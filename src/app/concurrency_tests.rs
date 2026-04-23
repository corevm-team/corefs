// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Concurrency tests for the App/Service layer (CoreFsService).
//
// The ODF storage layer has its own concurrency suite in
// `storage::ondisk::concurrency_tests`.  These tests cover the
// *service-level* patterns that a real FUSE daemon or multi-threaded
// application would exercise.

use std::sync::{Arc, Mutex};
use std::thread;

use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

// ----- helpers --------------------------------------------------------------

fn test_fs() -> CoreFsService {
    CoreFsService::format(CoreFsConfig::default())
}

fn test_fs_no_crypto() -> CoreFsService {
    let mut config = CoreFsConfig::default();
    config.performance.compression_enabled = false;
    config.security.encryption_at_rest = false;
    CoreFsService::format(config)
}

fn odf_session_opts() -> OdfSessionOptions {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 8 * 1024 * 1024;
    opts.inode_count = 256;
    opts.journal_blocks = 32;
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    opts
}

fn odf_session_with_files(count: usize) -> OdfDeviceSession {
    let opts = odf_session_opts();
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| {
        for i in 0..count {
            fs.create_file(
                &format!("/file{i:04}"),
                format!("content-{i}").as_bytes(),
                &[],
            )?;
        }
        Ok(())
    })
    .unwrap();
    sess
}

// ----- Send/Sync compile-time bounds ----------------------------------------

#[test]
fn cs1_corefs_service_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<CoreFsService>();
}

#[test]
fn cs2_odf_device_session_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<OdfDeviceSession>();
}

// ----- Service move between threads -----------------------------------------

#[test]
fn cs3_service_can_be_moved_to_worker_thread() {
    let mut fs = test_fs_no_crypto();
    fs.create_file("/before", b"main-thread", &[]).unwrap();

    let handle = thread::spawn(move || {
        fs.create_file("/after", b"worker-thread", &[]).unwrap();
        fs
    });

    let fs = handle.join().unwrap();
    assert_eq!(fs.read_file("/before").unwrap(), b"main-thread");
    assert_eq!(fs.read_file("/after").unwrap(), b"worker-thread");
}

// ----- Concurrent reads via export_state ------------------------------------

#[test]
fn cs4_concurrent_readers_on_exported_state() {
    // A common pattern: the writer exports a PersistedState snapshot,
    // and N readers each hydrate their own CoreFsService from it.
    let mut fs = test_fs_no_crypto();
    for i in 0..20 {
        fs.create_file(&format!("/f{i:02}"), format!("v{i}").as_bytes(), &[])
            .unwrap();
    }

    let state = Arc::new(fs.export_state());
    let block_bytes = Arc::new(fs.read_all_block_bytes());
    let mut handles = Vec::new();
    for _ in 0..8 {
        let s = Arc::clone(&state);
        let bb = Arc::clone(&block_bytes);
        handles.push(thread::spawn(move || {
            let mut reader = CoreFsService::from_persisted_state((*s).clone());
            reader.restore_block_bytes((*bb).clone());
            let paths = reader.list_paths();
            assert_eq!(paths.len(), 20);
            // Every reader sees identical content.
            for i in 0..20 {
                let data = reader.read_file(&format!("/f{i:02}")).unwrap();
                assert_eq!(data, format!("v{i}").as_bytes());
            }
            paths
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // All threads observed the same path set.
    for r in &results[1..] {
        assert_eq!(r, &results[0]);
    }
}

// ----- Snapshot isolation across threads ------------------------------------

#[test]
fn cs5_snapshot_readers_isolated_from_writer_mutations() {
    let mut fs = test_fs_no_crypto();
    fs.create_file("/v1", b"first", &[]).unwrap();

    // Take a state snapshot before further mutations.
    let snapshot_state = Arc::new(fs.export_state());
    let snapshot_bytes = Arc::new(fs.read_all_block_bytes());

    // Writer continues mutating.
    fs.create_file("/v2", b"second", &[]).unwrap();
    fs.write_file("/v1", b"overwritten").unwrap();

    let mut handles = Vec::new();
    for _ in 0..4 {
        let s = Arc::clone(&snapshot_state);
        let bb = Arc::clone(&snapshot_bytes);
        handles.push(thread::spawn(move || {
            let mut reader = CoreFsService::from_persisted_state((*s).clone());
            reader.restore_block_bytes((*bb).clone());
            // Snapshot readers see the OLD state: /v1 exists with original content,
            // /v2 does not exist.
            assert_eq!(reader.read_file("/v1").unwrap(), b"first");
            assert!(reader.read_file("/v2").is_err());
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Writer still sees its own mutations.
    assert_eq!(fs.read_file("/v1").unwrap(), b"overwritten");
    assert_eq!(fs.read_file("/v2").unwrap(), b"second");
}

// ----- Arc<Mutex> serialised access pattern ---------------------------------

#[test]
fn cs6_arc_mutex_serialises_concurrent_mutations() {
    let fs = Arc::new(Mutex::new(test_fs_no_crypto()));
    let mut handles = Vec::new();

    for t in 0..8 {
        let fs = Arc::clone(&fs);
        handles.push(thread::spawn(move || {
            let mut guard = fs.lock().unwrap();
            guard
                .create_file(&format!("/t{t}"), format!("thread-{t}").as_bytes(), &[])
                .unwrap();
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let guard = fs.lock().unwrap();
    let paths = guard.list_paths();
    assert_eq!(paths.len(), 8);
    for t in 0..8 {
        assert_eq!(
            guard.read_file(&format!("/t{t}")).unwrap(),
            format!("thread-{t}").as_bytes()
        );
    }
}

// ----- ODF session moved between threads ------------------------------------

#[test]
fn cs7_odf_session_can_be_handed_between_threads() {
    let sess = odf_session_with_files(5);

    // Move the entire session to a worker, mutate there, then bring it back.
    let handle = thread::spawn(move || {
        let mut sess = sess;
        sess.mutate(|fs| fs.create_file("/from-worker", b"hello", &[]))
            .unwrap();
        sess
    });

    let sess = handle.join().unwrap();
    assert!(
        sess.service()
            .list_paths()
            .contains(&"/from-worker".to_string())
    );
}

// ----- Parallel reads on ODF device snapshot --------------------------------

#[test]
fn cs8_parallel_reads_on_odf_device_snapshot() {
    let sess = odf_session_with_files(10);
    let dev = sess.into_device();
    let capacity = dev.capacity();
    let snapshot_bytes = Arc::new(dev.read_at(0, capacity).unwrap());

    let mut handles = Vec::new();
    for _ in 0..4 {
        let bytes = Arc::clone(&snapshot_bytes);
        handles.push(thread::spawn(move || {
            let snap_dev: Box<dyn BlockDevice> =
                Box::new(MemoryDevice::from_bytes((*bytes).clone(), 4096).unwrap());
            let sess = OdfDeviceSession::open(snap_dev).unwrap();
            let paths = sess.service().list_paths();
            assert_eq!(paths.len(), 10);
            for i in 0..10 {
                let data = sess.service().read_file(&format!("/file{i:04}")).unwrap();
                assert_eq!(data, format!("content-{i}").as_bytes());
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}

// ----- Snapshot + fsck in parallel ------------------------------------------

#[test]
fn cs9_fsck_on_snapshot_while_writer_mutates() {
    let mut fs = test_fs_no_crypto();
    for i in 0..10 {
        fs.create_file(&format!("/f{i}"), b"data", &[]).unwrap();
    }

    // Snapshot for the fsck reader.
    let state = fs.export_state();

    // Writer keeps going.
    let writer = thread::spawn(move || {
        for i in 10..30 {
            fs.create_file(&format!("/f{i}"), b"more", &[]).unwrap();
        }
        fs
    });

    // Independent fsck on the snapshot.
    let reader = CoreFsService::from_persisted_state(state);
    let report = reader.fsck();
    assert_eq!(
        report.missing_blocks,
        Vec::<String>::new(),
        "fsck on snapshot should be clean"
    );

    let fs = writer.join().unwrap();
    assert_eq!(fs.list_paths().len(), 30);
}

// ----- Multiple snapshot captures in parallel ---------------------------------

#[test]
fn cs10_snapshot_creation_is_deterministic_across_threads() {
    // Create the same initial state, then have multiple threads each
    // create a snapshot independently.  The snapshot metadata should be
    // identical (minus timing).
    let mut fs = test_fs_no_crypto();
    fs.create_file("/a", b"alpha", &[]).unwrap();
    fs.create_file("/b", b"beta", &[]).unwrap();

    let state = Arc::new(fs.export_state());
    let mut handles = Vec::new();
    for t in 0..4 {
        let s = Arc::clone(&state);
        handles.push(thread::spawn(move || {
            let mut local = CoreFsService::from_persisted_state((*s).clone());
            local.create_snapshot(&format!("snap-{t}"));
            let names = local.snapshot_names();
            assert!(names.contains(&format!("snap-{t}")));
            local.snapshots().len()
        }));
    }

    let counts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // Each thread created exactly one snapshot on its own copy.
    for c in counts {
        assert_eq!(c, 1);
    }
}
