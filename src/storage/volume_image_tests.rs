// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::services::journal::{JournalEntry, JournalRuntimeState};
use crate::storage::block_store::BlockRecord;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "corefs-{name}-{}-{}.img",
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
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: vec![HotPathRecord {
            path: "/hot.txt".to_string(),
            read_ops: 0,
            write_ops: 3,
            metadata_ops: 1,
            bytes_read: 0,
            bytes_written: 4096,
        }],
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: JournalRuntimeState::default(),
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
fn save_and_load_volume_image_round_trip() {
    let path = temp_path("roundtrip");
    let mut state = sample_state();
    state.allocator_policy = AllocatorPolicy {
        strategy: crate::storage::block_store::AllocationStrategy::FirstFit,
        split_threshold_blocks: 2,
        coalesce_on_release: true,
        tail_trim_enabled: true,
        background_compaction_enabled: true,
        fragmentation_threshold_percent: 40,
    };
    state.free_extents = vec![FreeExtentRecord {
        device_block: 0,
        allocated_blocks: 4,
    }];
    state.block_records = vec![BlockRecord {
        inode: InodeId(7),
        logical_size: 7,
        extents: vec![],
        content_crc: 0,
        flags: 0,
    }];

    save_volume_image(&path, &state).expect("volume image should be written");
    let loaded = load_volume_image(&path).expect("volume image should be loaded");

    assert_eq!(loaded.config, state.config);
    assert_eq!(loaded.next_snapshot_id, 1);
    assert_eq!(loaded.snapshots.len(), 1);
    assert_eq!(loaded.allocator_policy, state.allocator_policy);
    assert_eq!(loaded.hot_path_records, state.hot_path_records);
    assert_eq!(loaded.block_records.len(), state.block_records.len());
    assert_eq!(loaded.block_records[0].inode, InodeId(7));

    let _ = fs::remove_file(path);
}

#[test]
fn invalid_magic_is_rejected() {
    let path = temp_path("invalid-magic");
    fs::write(&path, b"not-a-corefs-image").expect("test fixture should be written");

    let result = load_volume_image(&path);
    assert!(matches!(result, Err(CoreFsError::State(_))));

    let _ = fs::remove_file(path);
}

#[test]
fn segmented_volume_image_contains_directory_and_segments() {
    let path = temp_path("segmented-layout");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let bytes = fs::read(&path).expect("image should exist");

    assert_eq!(&bytes[..8], MAGIC);
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().expect("fixed")),
        FORMAT_VERSION
    );
    assert_eq!(
        u32::from_le_bytes(bytes[12..16].try_into().expect("fixed")),
        EXPECTED_SEGMENT_KINDS.len() as u32
    );
    assert_eq!(&bytes[16..20], b"SUPR");
    assert_eq!(&bytes[40..44], b"SUP2");

    let _ = fs::remove_file(path);
}

#[test]
fn align_up_moves_offsets_to_alignment() {
    assert_eq!(align_up(64, 64), 64);
    assert_eq!(align_up(65, 64), 128);
}

#[test]
fn secondary_superblock_can_be_used_as_fallback() {
    let path = temp_path("superblock-fallback");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
    bytes[primary_offset] ^= 0xFF;
    fs::write(&path, bytes).expect("mutated image should be written");

    let loaded = load_volume_image(&path).expect("secondary superblock should still work");
    assert_eq!(loaded.next_snapshot_id, 1);

    let _ = fs::remove_file(path);
}

#[test]
fn inspect_volume_image_reports_segment_health() {
    let path = temp_path("inspect");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let report = inspect_volume_image(&path).expect("inspection should succeed");

    assert_eq!(report.format_version, FORMAT_VERSION);
    assert_eq!(report.segment_count, EXPECTED_SEGMENT_KINDS.len());
    assert_eq!(report.valid_superblocks, 2);
    assert!(report.directory_checksum_valid);
    assert!(report.payload_checksum_valid);
    assert!(report.segment_kinds.iter().any(|kind| kind == "DATA"));

    let _ = fs::remove_file(path);
}

#[test]
fn newer_valid_superblock_generation_is_preferred() {
    let path = temp_path("superblock-generation");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let entries = parse_directory(
        &bytes[HEADER_SIZE..HEADER_SIZE + (EXPECTED_SEGMENT_KINDS.len() * SEGMENT_ENTRY_SIZE)],
    )
    .expect("directory should parse");
    let secondary = find_segment(&entries, b"SUP2").expect("secondary superblock should exist");
    let generation_offset = secondary.offset as usize + 16;
    let old_generation = u64::from_le_bytes(
        bytes[generation_offset..generation_offset + 8]
            .try_into()
            .expect("fixed slice"),
    );
    bytes[generation_offset..generation_offset + 8]
        .copy_from_slice(&(old_generation + 7).to_le_bytes());
    fs::write(&path, bytes).expect("mutated image should be written");

    let report = inspect_volume_image(&path).expect("inspection should succeed");
    assert_eq!(report.valid_superblocks, 2);
    assert_eq!(report.selected_generation, old_generation + 7);

    let _ = fs::remove_file(path);
}

#[test]
fn repair_volume_image_rebuilds_both_superblock_copies() {
    let path = temp_path("repair-superblocks");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
    bytes[primary_offset] ^= 0xFF;
    fs::write(&path, bytes).expect("corrupted image should be written");

    let repaired =
        repair_volume_image_superblocks(&path).expect("repair should restore redundancy");
    assert_eq!(repaired.repaired_superblocks, 1);
    assert_eq!(repaired.resulting_valid_superblocks, 2);

    let report = inspect_volume_image(&path).expect("inspection should succeed");
    assert_eq!(report.valid_superblocks, 2);

    let _ = fs::remove_file(path);
}

#[test]
fn repair_volume_image_applies_journal_reconciliation() {
    let path = temp_path("repair-journal");
    let mut active_inode = Inode::new(
        InodeId(1),
        InodeKind::File,
        "/a".to_string(),
        FileMetadata::default(),
    );
    active_inode.size = 999;
    let deleted_inode = Inode::new(
        InodeId(2),
        InodeKind::File,
        "/b".to_string(),
        FileMetadata::default(),
    );
    let state = PersistedState {
        config: CoreFsConfig::default(),
        volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
        clean_unmount: true,
        pending_wal: None,
        active_inodes: vec![active_inode],
        deleted_inodes: vec![deleted_inode],
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: vec![
            BlockRecord {
                inode: InodeId(1),
                logical_size: 5,
                extents: vec![],
                content_crc: 123,
                flags: 0,
            },
            BlockRecord {
                inode: InodeId(99),
                logical_size: 6,
                extents: vec![],
                content_crc: 456,
                flags: 0,
            },
        ],
        journal_entries: vec![
            JournalEntry {
                timestamp: SystemTime::now().into(),
                operation: "delete".to_string(),
                target: "/a".to_string(),
                details: String::new(),
            },
            JournalEntry {
                timestamp: SystemTime::now().into(),
                operation: "restore".to_string(),
                target: "/b".to_string(),
                details: String::new(),
            },
        ],
        journal_runtime: JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    };

    save_volume_image(&path, &state).expect("volume image should be written");
    let repaired = repair_volume_image_superblocks(&path).expect("repair should succeed");

    assert_eq!(repaired.journal_repair.moved_to_deleted, 1);
    assert_eq!(repaired.journal_repair.restored_to_active, 1);
    assert_eq!(repaired.journal_repair.removed_orphan_blocks, 1);
    assert_eq!(repaired.journal_repair.resized_inodes, 1);

    let loaded = load_volume_image(&path).expect("repaired image should load");
    assert_eq!(loaded.active_inodes.len(), 1);
    assert_eq!(loaded.active_inodes[0].path, "/b");
    assert_eq!(loaded.deleted_inodes.len(), 1);
    assert_eq!(loaded.deleted_inodes[0].path, "/a");
    assert_eq!(loaded.block_records.len(), 1);

    let _ = fs::remove_file(path);
}

#[test]
fn repair_volume_image_recovers_from_missing_valid_superblocks_via_header_directory() {
    let path = temp_path("repair-header-fallback");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
    let secondary_offset = u64::from_le_bytes(bytes[48..56].try_into().expect("fixed")) as usize;
    bytes[primary_offset] ^= 0xFF;
    bytes[secondary_offset] ^= 0xFF;
    fs::write(&path, bytes).expect("corrupted image should be written");

    let repaired = repair_volume_image_superblocks(&path)
        .expect("repair should succeed via header directory recovery");

    assert!(repaired.recovered_without_valid_superblock);
    assert!(!repaired.reconstructed_segment_directory);
    assert!(repaired.reconstructed_block_descriptors);
    assert_eq!(repaired.resulting_valid_superblocks, 2);

    let report = inspect_volume_image(&path).expect("repaired image should inspect");
    assert_eq!(report.valid_superblocks, 2);

    let _ = fs::remove_file(path);
}

#[test]
fn repair_volume_image_reconstructs_corrupted_directory_entries() {
    let path = temp_path("repair-corrupted-directory");
    let state = sample_state();

    save_volume_image(&path, &state).expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    bytes[72..80].copy_from_slice(&u64::MAX.to_le_bytes());
    bytes[80..88].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, bytes).expect("corrupted directory should be written");

    let repaired =
        repair_volume_image_superblocks(&path).expect("repair should reconstruct the directory");

    assert!(repaired.reconstructed_segment_directory);
    assert!(repaired.reconstructed_block_descriptors);
    assert_eq!(repaired.resulting_valid_superblocks, 2);

    let loaded = load_volume_image(&path).expect("repaired image should load cleanly");
    assert_eq!(loaded.next_snapshot_id, 1);

    let _ = fs::remove_file(path);
}

#[test]
fn repair_volume_image_reconstructs_corrupted_block_descriptors() {
    use crate::app::CoreFsService;
    use crate::config::CoreFsConfig;
    let path = temp_path("repair-corrupted-blkd");
    // Use CoreFsService so bytes are in the compat device and can be
    // written to the DATA segment via save_image_to_path.
    let mut svc = CoreFsService::format(CoreFsConfig::default());
    svc.create_file("/payload.txt", b"hello", &[]).unwrap();
    svc.save_image_to_path(&path)
        .expect("volume image should be written");
    let mut bytes = fs::read(&path).expect("image should exist");
    let entries = parse_directory(
        &bytes[HEADER_SIZE..HEADER_SIZE + (EXPECTED_SEGMENT_KINDS.len() * SEGMENT_ENTRY_SIZE)],
    )
    .expect("directory should parse");
    let blkd_offset = find_segment(&entries, b"BLKD")
        .expect("blkd segment should exist")
        .offset as usize;
    if let Some(byte) = bytes.get_mut(blkd_offset) {
        *byte ^= 0xFF;
    }
    fs::write(&path, bytes).expect("corrupted image should be written");

    let repaired = repair_volume_image_superblocks(&path)
        .expect("repair should reconstruct block descriptors");

    assert!(repaired.reconstructed_block_descriptors);
    let loaded = load_volume_image(&path).expect("repaired image should load");
    assert_eq!(loaded.block_records.len(), 1);
    assert_eq!(loaded.block_records[0].logical_size, 5);

    let _ = fs::remove_file(path);
}

// -----------------------------------------------------------------------
// BlockDevice integration tests
// -----------------------------------------------------------------------

use crate::storage::block_device::MemoryDevice;

fn memory_device(capacity: u64) -> MemoryDevice {
    MemoryDevice::new(capacity, 4096).unwrap()
}

#[test]
fn save_and_load_from_device_round_trip() {
    let state = sample_state();
    let mut dev = memory_device(2 * 1024 * 1024);

    save_to_device(&mut dev, &state).unwrap();
    let loaded = load_from_device(&dev).unwrap();

    assert_eq!(loaded.config.volume_name, state.config.volume_name);
    assert_eq!(loaded.active_inodes.len(), state.active_inodes.len());
    assert_eq!(loaded.deleted_inodes.len(), state.deleted_inodes.len());
    assert_eq!(loaded.journal_entries.len(), state.journal_entries.len());
    assert_eq!(loaded.versions.len(), state.versions.len());
    assert_eq!(loaded.snapshots.len(), state.snapshots.len());
    assert_eq!(loaded.next_snapshot_id, state.next_snapshot_id);
    assert_eq!(loaded.block_records.len(), state.block_records.len());
    assert_eq!(loaded.free_extents.len(), state.free_extents.len());
}

#[test]
fn save_to_device_rejects_insufficient_capacity() {
    // Build a state large enough (via many inodes) to exceed a very small device.
    let mut state = sample_state();
    // Add many inodes so the AINO segment exceeds 4 KiB.
    for i in 0..200u64 {
        let inode = crate::domain::inode::Inode {
            id: InodeId(i + 100),
            kind: crate::domain::inode::InodeKind::File,
            path: format!("/filler/file_{i:04}.txt"),
            size: 0,
            created_at: std::time::SystemTime::UNIX_EPOCH.into(),
            modified_at: std::time::SystemTime::UNIX_EPOCH.into(),
            changed_at: std::time::SystemTime::UNIX_EPOCH.into(),
            accessed_at: std::time::SystemTime::UNIX_EPOCH.into(),
            metadata: crate::domain::metadata::FileMetadata::default(),
        };
        state.active_inodes.push(inode);
    }
    // 4 KiB device cannot fit the header + all segments.
    let mut dev = memory_device(4096);

    let err = save_to_device(&mut dev, &state).unwrap_err();
    assert!(err.to_string().contains("exceeds device capacity"));
}

#[test]
fn load_from_device_rejects_unformatted() {
    let dev = memory_device(2 * 1024 * 1024); // All zeros
    let err = load_from_device(&dev).unwrap_err();
    assert!(err.to_string().contains("invalid magic"));
}

#[test]
fn save_and_load_device_preserves_block_data() {
    let mut state = sample_state();
    state.block_records = vec![crate::storage::block_store::BlockRecord {
        inode: InodeId(1),
        logical_size: 19,
        extents: vec![],
        content_crc: 12345,
        flags: 0,
    }];
    let mut dev = memory_device(2 * 1024 * 1024);

    save_to_device(&mut dev, &state).unwrap();
    let loaded = load_from_device(&dev).unwrap();

    assert_eq!(loaded.block_records.len(), 1);
    assert_eq!(loaded.block_records[0].inode, InodeId(1));
    assert_eq!(loaded.block_records[0].logical_size, 19);
}

#[test]
fn save_and_load_device_empty_state() {
    let state = PersistedState {
        config: CoreFsConfig::default(),
        volume: crate::domain::volume::VolumeDescriptor::from_config(&CoreFsConfig::default()),
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: crate::services::journal::JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    };
    let mut dev = memory_device(2 * 1024 * 1024);

    save_to_device(&mut dev, &state).unwrap();
    let loaded = load_from_device(&dev).unwrap();

    assert_eq!(loaded.config.volume_name, "corefs");
    assert!(loaded.active_inodes.is_empty());
    assert!(loaded.block_records.is_empty());
}

#[test]
fn save_and_load_device_with_512_byte_sectors() {
    let state = sample_state();
    let mut dev = MemoryDevice::new(2 * 1024 * 1024, 512).unwrap();

    save_to_device(&mut dev, &state).unwrap();
    let loaded = load_from_device(&dev).unwrap();

    assert_eq!(loaded.config.volume_name, state.config.volume_name);
    assert_eq!(loaded.active_inodes.len(), state.active_inodes.len());
}

#[test]
fn build_volume_image_bytes_creates_valid_image() {
    let state = sample_state();
    let bytes = build_volume_image_bytes(&state).unwrap();

    // Should start with MAGIC
    assert_eq!(&bytes[..8], MAGIC);
    // Should have correct format version
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    assert_eq!(version, FORMAT_VERSION);
}

#[test]
fn device_round_trip_matches_file_round_trip() {
    let state = sample_state();

    // File path
    let file_path = temp_path("device-vs-file");
    save_volume_image(&file_path, &state).unwrap();
    let file_loaded = load_volume_image(&file_path).unwrap();

    // Device path
    let mut dev = memory_device(2 * 1024 * 1024);
    save_to_device(&mut dev, &state).unwrap();
    let device_loaded = load_from_device(&dev).unwrap();

    // Both should produce identical state
    assert_eq!(file_loaded.config, device_loaded.config);
    assert_eq!(
        file_loaded.active_inodes.len(),
        device_loaded.active_inodes.len()
    );
    assert_eq!(
        file_loaded.block_records.len(),
        device_loaded.block_records.len()
    );
    assert_eq!(file_loaded.snapshots.len(), device_loaded.snapshots.len());
    assert_eq!(file_loaded.next_snapshot_id, device_loaded.next_snapshot_id);
    for (f, d) in file_loaded
        .block_records
        .iter()
        .zip(device_loaded.block_records.iter())
    {
        assert_eq!(f.inode, d.inode);
        assert_eq!(f.logical_size, d.logical_size);
    }

    let _ = fs::remove_file(file_path);
}

#[test]
fn multiple_save_load_cycles_on_same_device() {
    let mut dev = memory_device(2 * 1024 * 1024);

    // Cycle 1: empty state
    let state1 = PersistedState {
        config: CoreFsConfig::default(),
        volume: crate::domain::volume::VolumeDescriptor::from_config(&CoreFsConfig::default()),
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: crate::services::journal::JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    };
    save_to_device(&mut dev, &state1).unwrap();

    // Cycle 2: overwrite with sample state
    let state2 = sample_state();
    save_to_device(&mut dev, &state2).unwrap();
    let loaded = load_from_device(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), state2.active_inodes.len());
    assert_eq!(loaded.next_snapshot_id, state2.next_snapshot_id);
}

// -----------------------------------------------------------------------
// Incremental persist
// -----------------------------------------------------------------------

#[test]
fn first_incremental_persist_does_full_write_and_populates_cache() {
    let state = sample_state();
    let mut dev = memory_device(2 * 1024 * 1024);
    let mut cache: Option<DeviceImageCache> = None;

    let report = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    assert!(!report.incremental, "first call should be full write");
    assert!(cache.is_some(), "cache should be populated");

    let loaded = load_from_device(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), state.active_inodes.len());
}

#[test]
fn incremental_persist_skips_unchanged_segments() {
    let state = sample_state();
    let mut dev = memory_device(2 * 1024 * 1024);
    let mut cache: Option<DeviceImageCache> = None;

    // First call: full write.
    let first = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    assert!(!first.incremental);
    assert!(first.bytes_written > 0);

    // Second call with identical state: incremental, zero segments written
    // (except the superblock contains a new generation counter, but only if
    // the bytes actually changed between builds — which they do, since
    // `current_generation()` returns nanosecond timestamps).
    let second = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    assert!(
        second.incremental,
        "second call should use incremental path"
    );
    // At most 2 segments change (SUPR + SUP2 with new generation).
    assert!(
        second.segments_written <= 2,
        "unchanged state should only rewrite superblocks, got {} segments",
        second.segments_written
    );
}

#[test]
fn incremental_persist_writes_only_changed_metadata_segment() {
    let mut state = sample_state();
    state.active_inodes.push(Inode {
        id: InodeId(1),
        kind: InodeKind::File,
        path: "/owned.txt".to_string(),
        size: 0,
        created_at: std::time::SystemTime::now().into(),
        modified_at: std::time::SystemTime::now().into(),
        changed_at: std::time::SystemTime::now().into(),
        accessed_at: std::time::SystemTime::now().into(),
        metadata: crate::domain::metadata::FileMetadata::default(),
    });

    let mut dev = memory_device(2 * 1024 * 1024);
    let mut cache: Option<DeviceImageCache> = None;

    // First call: full write.
    persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();

    // Modify only the inode's metadata (simulates chown).
    state.active_inodes[0].metadata.uid = 1234;
    state.active_inodes[0].metadata.gid = 5678;
    state.active_inodes[0].metadata.mode = 0o600;

    let report = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    assert!(report.incremental);
    // SUPR + SUP2 + AINO = 3 at most.  Implementations may write BLKD/DATA
    // if split_blocks emits anything different — but for a chown, it
    // shouldn't.  We allow up to 3 to stay robust against future tweaks.
    assert!(
        report.segments_written <= 3,
        "chown should only rewrite a few segments, got {}",
        report.segments_written
    );
    assert!(
        report.segments_written >= 1,
        "at least superblock should be written"
    );

    // Verify the change persisted.
    let loaded = load_from_device(&dev).unwrap();
    assert_eq!(loaded.active_inodes[0].metadata.uid, 1234);
    assert_eq!(loaded.active_inodes[0].metadata.gid, 5678);
    assert_eq!(loaded.active_inodes[0].metadata.mode, 0o600);
}

#[test]
fn incremental_persist_tolerates_small_size_changes() {
    // Phase-2 change: segment alignment bumped from 64 to 4096 bytes,
    // and the `can_incremental` predicate now requires only that
    // segment *offsets* stay stable (not lengths).  A mutation that
    // adds one inode grows the AINO payload by a few hundred bytes —
    // well within a 4 KiB alignment slot — so the next segment's
    // offset is unchanged and the incremental fast path still fires.
    //
    // The old test expected a fallback to full write in this case.
    // That was correct under the old 64-byte alignment + strict
    // size-match `can_incremental`, but the very behaviour we wanted
    // to break in Phase 2 was that every small mutation triggered a
    // full image rewrite.
    let mut state = sample_state();
    let mut dev = memory_device(2 * 1024 * 1024);
    let mut cache: Option<DeviceImageCache> = None;

    persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();

    state.active_inodes.push(Inode {
        id: InodeId(999),
        kind: InodeKind::File,
        path: "/new-file.txt".to_string(),
        size: 0,
        created_at: std::time::SystemTime::now().into(),
        modified_at: std::time::SystemTime::now().into(),
        changed_at: std::time::SystemTime::now().into(),
        accessed_at: std::time::SystemTime::now().into(),
        metadata: crate::domain::metadata::FileMetadata::default(),
    });

    let report = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    assert!(
        report.incremental,
        "a single-inode growth fits inside the 4 KiB alignment slot and \
         should stay on the incremental fast path"
    );

    // Correctness: the loaded image must reflect the new inode count.
    let loaded = load_from_device(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), state.active_inodes.len());
}

#[test]
fn incremental_persist_bytes_written_is_much_less_than_full() {
    // Construct a state with substantial DATA so the full image is large.
    let mut state = sample_state();
    state.active_inodes.push(Inode {
        id: InodeId(1),
        kind: InodeKind::File,
        path: "/big.bin".to_string(),
        size: 64 * 1024,
        created_at: std::time::SystemTime::now().into(),
        modified_at: std::time::SystemTime::now().into(),
        changed_at: std::time::SystemTime::now().into(),
        accessed_at: std::time::SystemTime::now().into(),
        metadata: crate::domain::metadata::FileMetadata::default(),
    });
    state.block_records = vec![crate::storage::block_store::BlockRecord {
        inode: InodeId(1),
        logical_size: 64 * 1024,
        extents: vec![],
        content_crc: 1,
        flags: 0,
    }];

    let mut dev = memory_device(2 * 1024 * 1024);
    let mut cache: Option<DeviceImageCache> = None;

    let full = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    let full_bytes = full.bytes_written;

    // Modify only the inode's metadata (chown-like).
    state.active_inodes[0].metadata.uid = 42;

    let incr = persist_to_device_incremental(&mut dev, &state, &mut cache).unwrap();
    assert!(incr.incremental);
    // Incremental bytes should be dramatically smaller than full.
    assert!(
        incr.bytes_written * 10 < full_bytes,
        "expected incremental ({}) to be >10x smaller than full ({})",
        incr.bytes_written,
        full_bytes
    );
}
