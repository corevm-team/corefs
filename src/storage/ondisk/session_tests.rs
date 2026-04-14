// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::CoreFsConfig;
use crate::storage::block_device::MemoryDevice;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str) -> std::path::PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("corefs-odf-sess-{name}-{suffix}.odf"))
}

fn small_options() -> OdfSessionOptions {
    OdfSessionOptions {
        capacity_bytes: 16 * 1024 * 1024,
        label: "sess".into(),
        uuid: [0u8; 16],
        inode_count: 256,
        journal_blocks: 32,
        config: CoreFsConfig::default(),
    }
}

// ------------------------------------------------------------
// OdfFileSession
// ------------------------------------------------------------

#[test]
fn file_session_format_creates_image_file() {
    let path = temp_path("format");
    let sess = OdfFileSession::format_new(&path, &small_options()).unwrap();
    assert!(path.exists());
    assert_eq!(sess.image_path(), path.as_path());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_rejects_tiny_capacity() {
    let path = temp_path("tiny");
    let mut opts = small_options();
    opts.capacity_bytes = 1024;
    let err = OdfFileSession::format_new(&path, &opts).unwrap_err();
    assert!(format!("{err}").contains("capacity"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_mutate_persists_across_reopen() {
    let path = temp_path("mutate-persist");
    {
        let mut sess = OdfFileSession::format_new(&path, &small_options()).unwrap();
        let (_value, report) = sess
            .mutate(|fs| {
                fs.create_file("/a.txt", b"first", &[])?;
                fs.create_file("/b.txt", b"second", &[])?;
                Ok(())
            })
            .unwrap();
        assert!(report.incremental.created >= 2);
    }
    // Reopen and confirm files are there.
    let sess = OdfFileSession::open(&path).unwrap();
    let paths = sess.service().list_paths();
    assert!(paths.contains(&"/a.txt".to_string()));
    assert!(paths.contains(&"/b.txt".to_string()));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_open_or_format_creates_when_missing() {
    let path = temp_path("oof-new");
    assert!(!path.exists());
    let sess = OdfFileSession::open_or_format(&path, &small_options()).unwrap();
    assert!(sess.image_path().exists());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_open_or_format_opens_when_present() {
    let path = temp_path("oof-existing");
    {
        let mut sess = OdfFileSession::format_new(&path, &small_options()).unwrap();
        sess.mutate(|fs| fs.create_file("/seen.txt", b"hi", &[])).unwrap();
    }
    let sess = OdfFileSession::open_or_format(&path, &small_options()).unwrap();
    assert!(sess.service().list_paths().contains(&"/seen.txt".to_string()));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_open_nonexistent_errors() {
    let path = temp_path("missing");
    let err = OdfFileSession::open(&path).unwrap_err();
    assert!(matches!(err, crate::error::CoreFsError::NotFound(_)));
}

#[test]
fn incremental_flush_only_rewrites_changed_inodes() {
    let path = temp_path("increment");
    let mut sess = OdfFileSession::format_new(&path, &small_options()).unwrap();
    sess.mutate(|fs| {
        for i in 0..5 {
            fs.create_file(&format!("/f{i}"), format!("v1-{i}").as_bytes(), &[])?;
        }
        Ok(())
    })
    .unwrap();
    // Second mutate: touch only /f2.
    let (_r, report) = sess
        .mutate(|fs| {
            fs.delete_file("/f2", false)?;
            fs.create_file("/f2", b"v2-2", &[])?;
            Ok(())
        })
        .unwrap();
    // Delete + create → at most 2 inodes written, not 5.
    assert!(
        report.incremental.updated + report.incremental.created + report.incremental.removed
            <= 3,
        "too many inodes touched: {:?}",
        report.incremental
    );
    assert!(report.incremental.unchanged >= 4);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_uuid_zero_gets_time_seeded_uuid() {
    let path = temp_path("uuid");
    let sess = OdfFileSession::format_new(&path, &small_options()).unwrap();
    let sb_bytes = sess
        .device()
        .read_at(BLOCK_SIZE, BLOCK_SIZE)
        .unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    assert_ne!(sb.uuid, [0u8; 16], "time-seeded uuid should not be zero");
    let _ = std::fs::remove_file(&path);
}

// ------------------------------------------------------------
// OdfDeviceSession
// ------------------------------------------------------------

fn memory_device(blocks: u64) -> Box<dyn crate::storage::block_device::BlockDevice> {
    Box::new(MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap())
}

#[test]
fn device_session_format_and_flush_roundtrip() {
    let device = memory_device(4096);
    let mut sess = OdfDeviceSession::format_new(device, &small_options()).unwrap();
    sess.mutate(|fs| fs.create_file("/dev-a.txt", b"hello", &[])).unwrap();

    // Flush must leave the device readable via load_state_native.
    let dev_ref = sess.device();
    let state = crate::storage::ondisk::native::load_state_native(dev_ref).unwrap();
    let paths: Vec<String> = state.active_inodes.iter().map(|i| i.path.clone()).collect();
    assert!(paths.contains(&"/dev-a.txt".to_string()));
}

#[test]
fn device_session_open_reloads_state() {
    let device = memory_device(4096);
    let sess = OdfDeviceSession::format_new(device, &small_options()).unwrap();
    let mut sess = sess;
    sess.mutate(|fs| fs.create_file("/keep", b"x", &[])).unwrap();
    let dev = sess.into_device();
    let reopened = OdfDeviceSession::open(dev).unwrap();
    assert!(reopened.service().list_paths().contains(&"/keep".to_string()));
}

#[test]
fn device_session_open_recovers_pending_journal_transactions() {
    use crate::storage::ondisk::journaled::JournaledSaveSession;
    let mut device = memory_device(4096);
    // Format + save an empty state via an OdfDeviceSession, then leave
    // a pending (committed-but-not-applied) journal txn behind.
    {
        let sess = OdfDeviceSession::format_new(
            std::mem::replace(&mut device, Box::new(MemoryDevice::new(BLOCK_SIZE, 4096).unwrap())),
            &small_options(),
        )
        .unwrap();
        device = sess.into_device();
    }

    // Read primary SB, pick a data block as our target.
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &device.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let target = sb.data_start + 5;
    let payload = vec![0xA5u8; BLOCK_SIZE as usize];

    // Stage a metadata write, commit it without applying — simulates a
    // crash between commit-record-on-disk and replay.
    {
        let mut sess = JournaledSaveSession::open(device.as_mut()).unwrap();
        sess.stage_metadata_block(target, payload.clone()).unwrap();
        sess.commit_without_apply().unwrap();
    }

    // Target block is NOT yet updated.
    assert!(
        device.read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap()
            != payload
    );

    // Now OdfDeviceSession::open should replay the pending txn.
    let sess = OdfDeviceSession::open(device).unwrap();
    let read = sess.device().read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert_eq!(read, payload);
}

#[test]
fn mutate_returns_value_and_flush_report() {
    let device = memory_device(4096);
    let mut sess = OdfDeviceSession::format_new(device, &small_options()).unwrap();
    let (count, report) = sess
        .mutate(|fs| {
            fs.create_file("/m1", b"a", &[])?;
            fs.create_file("/m2", b"b", &[])?;
            Ok(2usize)
        })
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(report.incremental.created, 2);
}

#[test]
fn with_defaults_produces_working_options() {
    let opts = OdfSessionOptions::with_defaults();
    assert!(opts.capacity_bytes >= MIN_ODF_CAPACITY_BYTES);
    // Don't actually allocate 64 MiB in a unit test — just validate shape.
    assert_eq!(opts.label, "corefs");
}
