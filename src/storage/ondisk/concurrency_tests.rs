// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use std::sync::{Arc, Mutex};
use std::thread;

use crate::config::CoreFsConfig;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::bitmap::Bitmap;
use crate::storage::ondisk::checksum::Crc32c;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

// ----- pure primitives ------------------------------------------------------

#[test]
fn c1_crc32c_is_thread_safe_across_many_threads() {
    // CRC32C has no shared state, so N threads can hash simultaneously.
    let mut handles = Vec::new();
    for t in 0..8 {
        let handle = thread::spawn(move || {
            let bytes: Vec<u8> = (0..4096).map(|i| ((i + t) % 256) as u8).collect();
            let a = Crc32c::hash(&bytes);
            let b = Crc32c::hash(&bytes);
            assert_eq!(a, b, "deterministic");
            a
        });
        handles.push(handle);
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // All results are well-defined u32s (no panic across threads).
    assert_eq!(results.len(), 8);
}

#[test]
fn c2_disjoint_bitmaps_mutated_in_parallel_merge_correctly() {
    // Partition a 4096-bit space across 4 threads.  Each thread owns
    // its own Bitmap of the appropriate slice, sets bits concurrently,
    // then we merge the bitmaps back into a single view.
    const TOTAL: u64 = 4096;
    const SHARDS: usize = 4;
    const PER_SHARD: u64 = TOTAL / SHARDS as u64;

    let mut handles = Vec::new();
    for shard in 0..SHARDS {
        handles.push(thread::spawn(move || {
            let mut bm = Bitmap::new(PER_SHARD);
            // Mark every 7th bit.
            let mut i = 0u64;
            while i < PER_SHARD {
                bm.set(i).unwrap();
                i += 7;
            }
            (shard, bm)
        }));
    }

    let mut merged = Bitmap::new(TOTAL);
    for h in handles {
        let (shard, bm) = h.join().unwrap();
        for bit in 0..PER_SHARD {
            if bm.is_set(bit).unwrap() {
                merged.set(shard as u64 * PER_SHARD + bit).unwrap();
            }
        }
    }

    // Expected popcount: ceil(PER_SHARD / 7) bits set per shard × 4 shards.
    let per_shard_bits = PER_SHARD.div_ceil(7);
    assert_eq!(merged.popcount(), per_shard_bits * SHARDS as u64);
}

// ----- OdfReader under an Arc<Mutex> ---------------------------------------

fn formatted_device_with_files() -> Box<dyn BlockDevice> {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 8 * 1024 * 1024;
    opts.inode_count = 128;
    opts.journal_blocks = 32;
    opts.config = CoreFsConfig::default();
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| {
        for i in 0..20 {
            fs.create_file(&format!("/file{i:02}"), format!("v{i}").as_bytes(), &[])?;
        }
        Ok(())
    })
    .unwrap();
    sess.into_device()
}

#[test]
fn c3_multiple_readers_under_mutex_serve_consistent_results() {
    // Wrap a shared device in Arc<Mutex>, spawn N reader threads,
    // verify every thread sees the same set of inodes / contents.
    let dev = Arc::new(Mutex::new(formatted_device_with_files()));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let dev = Arc::clone(&dev);
        handles.push(thread::spawn(move || {
            let guard = dev.lock().unwrap();
            let reader = crate::storage::ondisk::reader::OdfReader::open(guard.as_ref()).unwrap();
            reader.list_inodes().unwrap()
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for r in &results {
        assert_eq!(r.len(), 20);
    }
    // All threads saw the same allocation.
    let first = &results[0];
    for r in &results[1..] {
        let a: std::collections::HashSet<_> = first.iter().map(|s| (s.slot, s.domain_id)).collect();
        let b: std::collections::HashSet<_> = r.iter().map(|s| (s.slot, s.domain_id)).collect();
        assert_eq!(a, b);
    }
}

#[test]
fn c4_single_writer_multiple_readers_pattern_works_via_reopen() {
    // Documented safe pattern: one owning writer session does a
    // mutate + flush, then hands off a *snapshot* device to readers
    // that each open their own OdfReader.  The writer resumes after
    // readers finish.  This is the pattern that a FUSE daemon would
    // use.
    let writer_dev = formatted_device_with_files();
    let snapshot = writer_dev.capacity();
    // Take a byte-level snapshot of the device for the readers.
    let capacity = snapshot;
    let snapshot_bytes = writer_dev
        .read_at(0, capacity)
        .expect("snapshot read should succeed");

    let mut handles = Vec::new();
    for _ in 0..4 {
        let bytes = snapshot_bytes.clone();
        handles.push(thread::spawn(move || {
            let snap = MemoryDevice::from_bytes(bytes, 4096).unwrap();
            let reader = crate::storage::ondisk::reader::OdfReader::open(&snap).unwrap();
            reader.allocated_user_inodes()
        }));
    }
    for h in handles {
        assert_eq!(h.join().unwrap(), 20);
    }
}

// ----- Session send/sync surface -------------------------------------------

#[test]
fn c5_session_can_be_moved_between_threads_but_not_shared() {
    // OdfDeviceSession is Send (so it can be *moved* to another
    // thread), but not Sync (so it can't be shared without external
    // synchronisation).  Demonstrate the Send-move path by running a
    // whole session on a worker thread.
    let opts = {
        let mut o = OdfSessionOptions::with_defaults();
        o.capacity_bytes = 4 * 1024 * 1024;
        o.inode_count = 64;
        o.journal_blocks = 32;
        o.config = CoreFsConfig::default();
        o.config.performance.compression_enabled = false;
        o.config.security.encryption_at_rest = false;
        o
    };
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let sess = OdfDeviceSession::format_new(dev, &opts).unwrap();

    let handle = thread::spawn(move || {
        let mut sess = sess;
        sess.mutate(|fs| fs.create_file("/worker-owned", b"payload", &[]))
            .unwrap();
        sess
    });

    let sess = handle.join().unwrap();
    // Back on the main thread: state is visible.
    assert!(
        sess.service()
            .list_paths()
            .contains(&"/worker-owned".to_string())
    );
}

#[test]
fn c6_concurrent_readers_on_snapshots_do_not_mutate_source() {
    // Start a session, hand each reader its own Arc<[u8]> snapshot,
    // and confirm none of the readers can see a write that happens on
    // the owner after the snapshot is taken.  This is the foundation
    // of a future "read-only FUSE view while RW writer runs" design.
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 4 * 1024 * 1024;
    opts.inode_count = 64;
    opts.journal_blocks = 32;
    opts.config = CoreFsConfig::default();
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;

    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| fs.create_file("/v1", b"first", &[]))
        .unwrap();

    let capacity = sess.device().capacity();
    let snapshot = sess.device().read_at(0, capacity).unwrap();

    // Continue mutating on the owner AFTER snapshot.
    sess.mutate(|fs| fs.create_file("/v2", b"second", &[]))
        .unwrap();

    let shared = Arc::new(snapshot);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let s = Arc::clone(&shared);
        handles.push(thread::spawn(move || {
            let snap_dev = MemoryDevice::from_bytes((*s).clone(), 4096).unwrap();
            let mut reader = crate::storage::ondisk::reader::OdfReader::open(&snap_dev).unwrap();
            // /v1 was in the snapshot; /v2 was not.
            assert!(reader.slot_for_path("/v1").unwrap().is_some());
            assert!(reader.slot_for_path("/v2").unwrap().is_none());
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

// ----- Smoke assertion on Send bounds (compile-time) -----------------------

#[test]
fn c7_send_bounds_hold_as_documented() {
    fn assert_send<T: Send>() {}
    assert_send::<crate::storage::ondisk::session::OdfDeviceSession>();
    assert_send::<Box<dyn BlockDevice>>();
    // Crc32c has no fields — nothing to assert beyond the fn calls above.
    let _ = BLOCK_SIZE;
}
