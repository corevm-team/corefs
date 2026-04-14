// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::storage::block_store::BlockStore;
use crate::storage::volume_image;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "corefs-integrity-{name}-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ))
}

fn sample_state() -> PersistedState {
    PersistedState {
        config: CoreFsConfig::default(),
        volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: crate::storage::block_store::AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: crate::services::journal::JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: vec![Snapshot {
            id: 1,
            name: "baseline".to_string(),
            scope_root: "/".to_string(),
            created_at: SystemTime::now().into(),
            paths: vec!["/".to_string()],
            file_data: std::collections::BTreeMap::new(),
            inodes: std::collections::BTreeMap::new(),
        }],
        next_snapshot_id: 1,
    }
}

#[test]
fn scrub_counts_valid_and_invalid_blocks() {
    let service = IntegrityService;
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"ok".to_vec());

    let report = service.scrub([InodeId(1), InodeId(2)].into_iter(), &store);

    assert_eq!(report.checked_paths, 2);
    assert_eq!(report.valid_blocks, 1);
    assert_eq!(report.invalid_blocks, 1);
}

#[test]
fn fsck_image_reports_valid_volume_image() {
    let service = IntegrityService;
    let path = temp_path("fsck-ok");
    let state = sample_state();
    volume_image::save_volume_image(&path, &state).expect("volume image should be written");

    let report = service.fsck_image(&path).expect("fsck should succeed");

    assert_eq!(report.format_version, 7);
    assert_eq!(report.segment_count, 15);
    assert_eq!(report.valid_superblocks, 2);
    assert!(report.directory_checksum_valid);
    assert!(report.payload_checksum_valid);

    let _ = fs::remove_file(path);
}

#[test]
fn fsck_image_rejects_corrupted_volume_image() {
    let service = IntegrityService;
    let path = temp_path("fsck-corrupt");
    let state = sample_state();
    volume_image::save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    bytes[0] ^= 0xFF;
    fs::write(&path, bytes).expect("corrupted image should be written");

    let report = service.fsck_image(&path);

    assert!(report.is_err());

    let _ = fs::remove_file(path);
}

#[test]
fn repair_image_restores_missing_superblock_copy() {
    let service = IntegrityService;
    let path = temp_path("repair-superblock");
    let state = sample_state();
    volume_image::save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
    bytes[primary_offset] ^= 0xFF;
    fs::write(&path, bytes).expect("corrupted image should be written");

    let repaired = service
        .repair_image(&path)
        .expect("repair should succeed from secondary copy");

    assert_eq!(repaired.repaired_superblocks, 1);
    assert_eq!(repaired.resulting_valid_superblocks, 2);
    assert!(!repaired.recovered_without_valid_superblock);
    assert!(!repaired.reconstructed_segment_directory);
    assert!(!repaired.reconstructed_block_descriptors);
    assert_eq!(repaired.removed_orphan_blocks, 0);

    let report = service
        .fsck_image(&path)
        .expect("image should be healthy again");
    assert_eq!(report.valid_superblocks, 2);

    let _ = fs::remove_file(path);
}

#[test]
fn repair_image_can_recover_without_any_valid_superblock() {
    let service = IntegrityService;
    let path = temp_path("repair-no-superblocks");
    let state = sample_state();
    volume_image::save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
    let secondary_offset =
        u64::from_le_bytes(bytes[48..56].try_into().expect("fixed")) as usize;
    bytes[primary_offset] ^= 0xFF;
    bytes[secondary_offset] ^= 0xFF;
    fs::write(&path, bytes).expect("corrupted image should be written");

    let repaired = service
        .repair_image(&path)
        .expect("repair should succeed from header directory");

    assert!(repaired.recovered_without_valid_superblock);
    assert_eq!(repaired.resulting_valid_superblocks, 2);

    let report = service
        .fsck_image(&path)
        .expect("repaired image should be healthy");
    assert_eq!(report.valid_superblocks, 2);

    let _ = fs::remove_file(path);
}
