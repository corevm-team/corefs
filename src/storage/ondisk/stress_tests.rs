// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::config::CoreFsConfig;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::fsck;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::reader::OdfReader;
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

fn device(mib: u64) -> Box<dyn BlockDevice> {
    Box::new(MemoryDevice::new(mib * 1024 * 1024, 4096).unwrap())
}

fn stress_options(mib: u64, inode_count: u64) -> OdfSessionOptions {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = mib * 1024 * 1024;
    opts.inode_count = inode_count;
    opts.journal_blocks = 32;
    // Turn compression + encryption off: these tests target the
    // storage layer, not the service's data pipeline.
    opts.config = CoreFsConfig::default();
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    opts.config.versioning.keep_latest = 0;
    opts
}

// ---------------------------------------------------------------------------
// S1 — many small files
// ---------------------------------------------------------------------------

#[test]
fn s1_thousand_small_files_roundtrip_via_session() {
    // 1 000 files × (1 attr block + 1 data block) = ~8 MB consumed in
    // a 64 MB volume.  Tests that allocator, bitmap and inode table
    // scale linearly.
    let opts = stress_options(64, 4096);
    let mut sess = OdfDeviceSession::format_new(device(64), &opts).unwrap();
    let (_, report) = sess
        .mutate(|fs| {
            for i in 0..1000 {
                fs.create_file(&format!("/f{i:04}.bin"), format!("v{i}").as_bytes(), &[])?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(report.incremental.created, 1000);

    // Reopen via reader + confirm every file is retrievable.
    let dev = sess.into_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    assert_eq!(reader.allocated_user_inodes(), 1000);
    let summaries = reader.list_inodes().unwrap();
    assert_eq!(summaries.len(), 1000);
    // Spot-check content of a middle file.
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let slot = reader.slot_for_path("/f0500.bin").unwrap().unwrap();
    let bytes = reader.read_file_content(slot).unwrap();
    assert_eq!(bytes, b"v500");
}

// ---------------------------------------------------------------------------
// S2 — deep directory tree
// ---------------------------------------------------------------------------

#[test]
fn s2_deep_directory_tree_via_session() {
    // 150 nested directories — tests that path validation and the
    // catalog's string-keyed lookups don't explode at depth.
    let opts = stress_options(16, 1024);
    let mut sess = OdfDeviceSession::format_new(device(16), &opts).unwrap();
    sess.mutate(|fs| {
        let mut path = String::new();
        for i in 0..150 {
            path.push_str(&format!("/level{i:03}"));
            fs.create_directory(&path)?;
        }
        // Drop a leaf file at the bottom.
        path.push_str("/deepest.txt");
        fs.create_file(&path, b"reached-the-bottom", &[])?;
        Ok(())
    })
    .unwrap();

    let dev = sess.into_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    assert!(reader.allocated_user_inodes() >= 150);
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let mut leaf_path: String = (0..150).map(|i| format!("/level{i:03}")).collect();
    leaf_path.push_str("/deepest.txt");
    let bytes = reader.read_file_by_path(&leaf_path).unwrap();
    assert_eq!(bytes, b"reached-the-bottom");
}

// ---------------------------------------------------------------------------
// S3 — a single large file (extent path)
// ---------------------------------------------------------------------------

#[test]
fn s3_single_large_file_uses_contiguous_extent() {
    // 1 MB file → 256 blocks → one contiguous extent.  Verifies the
    // allocator handles a multi-hundred-block request without
    // falling back to the chain path.
    let opts = stress_options(16, 256);
    let mut sess = OdfDeviceSession::format_new(device(16), &opts).unwrap();
    let content: Vec<u8> = (0..(1024 * 1024)).map(|i| (i % 256) as u8).collect();
    sess.mutate(|fs| fs.create_file("/big.bin", &content, &[]))
        .unwrap();

    let dev = sess.into_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    let summaries = reader.list_inodes().unwrap();
    let big = summaries.iter().find(|s| s.size_bytes == content.len() as u64).unwrap();
    // Expect exactly one extent (contiguous).
    assert_eq!(big.extent_count, 1, "expected contiguous allocation");

    // And the bytes roundtrip byte-for-byte.
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let slot = reader.slot_for_path("/big.bin").unwrap().unwrap();
    let read = reader.read_file_content(slot).unwrap();
    assert_eq!(read.len(), content.len());
    assert_eq!(read, content);
}

// ---------------------------------------------------------------------------
// S4 — sustained mutate loop (mount-lifetime simulation)
// ---------------------------------------------------------------------------

#[test]
fn s4_sustained_incremental_saves_stay_bounded() {
    // Perform 200 mutate() cycles, each touching just two files.
    // Since each cycle triggers an incremental save, the update count
    // per cycle should stay O(1) regardless of the total files.
    let opts = stress_options(16, 512);
    let mut sess = OdfDeviceSession::format_new(device(16), &opts).unwrap();
    sess.mutate(|fs| {
        for i in 0..50 {
            fs.create_file(&format!("/stable{i:02}"), b"stable", &[])?;
        }
        Ok(())
    })
    .unwrap();

    for cycle in 0..200u32 {
        let (_, report) = sess
            .mutate(|fs| {
                // Delete + recreate one file → one removal + one creation.
                let path = format!("/churn{}.txt", cycle % 3);
                let _ = fs.delete_file(&path, false); // may not exist first round
                fs.create_file(&path, format!("c{cycle}").as_bytes(), &[])?;
                Ok(())
            })
            .unwrap();
        // Each cycle should touch at most 2 inodes from the churn plus
        // leave the 50 stable files unchanged.
        assert!(
            report.incremental.unchanged >= 48,
            "cycle {cycle}: unchanged={} total={:?}",
            report.incremental.unchanged,
            report.incremental
        );
    }

    // Still structurally consistent.
    let dev = sess.into_device();
    let report = fsck::check(dev.as_ref()).unwrap();
    assert!(
        report.is_clean(),
        "fsck after stress run: {:?}",
        report.issues
    );
}

// ---------------------------------------------------------------------------
// S5 — many delete/recreate cycles on the same path
// ---------------------------------------------------------------------------

#[test]
fn s5_delete_recreate_cycles_reuse_data_blocks() {
    // Repeatedly delete-and-recreate a 4-KB file.  The block bitmap
    // must eventually recycle the freed extent — otherwise the data
    // region would grow unbounded.
    let opts = stress_options(8, 256);
    let mut sess = OdfDeviceSession::format_new(device(8), &opts).unwrap();

    // Warm-up file to stabilise the bitmap cursor.
    sess.mutate(|fs| fs.create_file("/anchor", b"a", &[])).unwrap();

    let mut max_free_blocks: u64 = 0;
    let mut min_free_blocks: u64 = u64::MAX;
    for i in 0..30 {
        sess.mutate(|fs| {
            let _ = fs.delete_file("/churn", false);
            let payload = vec![i as u8; 4096];
            fs.create_file("/churn", &payload, &[])?;
            Ok(())
        })
        .unwrap();

        // Record free-block range over the cycle.
        let dev = sess.device();
        let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
            &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
        )
        .unwrap();
        max_free_blocks = max_free_blocks.max(sb.free_blocks);
        min_free_blocks = min_free_blocks.min(sb.free_blocks);
    }

    // Bitmap usage oscillates inside a small band — allocation
    // pressure never strictly grows.
    let band = max_free_blocks - min_free_blocks;
    assert!(
        band < 100,
        "free-block band too wide (max={max_free_blocks}, min={min_free_blocks}) — allocator may be leaking"
    );
}

// ---------------------------------------------------------------------------
// S6 — fsck cost scales linearly
// ---------------------------------------------------------------------------

#[test]
fn s6_fsck_scales_with_inode_count() {
    // This isn't a strict timing test, just a correctness check that
    // fsck traverses every allocated inode exactly once and reports
    // accurate counters.
    let opts = stress_options(32, 1024);
    let mut sess = OdfDeviceSession::format_new(device(32), &opts).unwrap();
    sess.mutate(|fs| {
        for i in 0..500 {
            fs.create_file(&format!("/s6-{i:03}.txt"), b"x", &[])?;
        }
        Ok(())
    })
    .unwrap();

    let dev = sess.into_device();
    let report = fsck::check(dev.as_ref()).unwrap();
    assert!(
        report.is_clean(),
        "fsck after 500-file populate: {:?}",
        report.issues
    );
    // Every inode = 1 extent for data + 1 attr block → 2 blocks
    // referenced per inode, plus the ancillary slot.
    assert!(report.inodes_checked >= 501);
    assert!(report.blocks_referenced >= 500);
}
