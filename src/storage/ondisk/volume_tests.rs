// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::volume::VolumeDescriptor;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::MemoryDevice;
use crate::storage::block_store::AllocatorPolicy;
use crate::storage::ondisk::inode::OnDiskKind;
use crate::storage::ondisk::layout::BLOCK_SIZE;

fn empty_state() -> PersistedState {
    let config = CoreFsConfig::default();
    PersistedState {
        volume: VolumeDescriptor::from_config(&config),
        config,
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    }
}

fn fresh_device(blocks: u64) -> MemoryDevice {
    MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap()
}

fn default_options() -> FormatOptions {
    FormatOptions {
        label: "odf-test".to_string(),
        uuid: *b"0123456789ABCDEF",
        inode_count: 1024,
        journal_blocks: 32,
    }
}

#[test]
fn format_initialises_all_control_regions() {
    let mut dev = fresh_device(4096);
    let report = format_device(&mut dev, &default_options()).unwrap();

    // Superblock is decodable.
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    assert_eq!(sb.total_blocks, report.geometry.total_blocks);
    assert_eq!(sb.uuid, report.uuid);
    assert_eq!(&sb.label[..8], b"odf-test");
    assert_eq!(sb.generation, 1);

    // Redundant superblocks carry the same bytes.
    let t = dev
        .read_at(report.geometry.tertiary_superblock_block * BLOCK_SIZE, BLOCK_SIZE)
        .unwrap();
    assert_eq!(t, sb_bytes);
    let s = dev
        .read_at(report.geometry.secondary_superblock_block * BLOCK_SIZE, BLOCK_SIZE)
        .unwrap();
    assert_eq!(s, sb_bytes);

    // System inode #0 exists and is of SystemPayload kind.
    let info = inspect(&dev).unwrap();
    assert_eq!(info.free_inodes, report.geometry.inode_count); // no payload saved yet
}

#[test]
fn save_and_load_empty_state_roundtrip() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let state = empty_state();
    let report = save_state(&mut dev, &state).unwrap();
    assert_eq!(report.generation, 2);
    assert!(report.payload_blocks >= 1);

    let loaded = load_state(&dev).unwrap();
    assert_eq!(loaded, state);
}

#[test]
fn redundant_superblock_fallback_recovers_payload() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let state = empty_state();
    save_state(&mut dev, &state).unwrap();

    // Corrupt primary superblock — loader must fall back.
    let mut sb = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    sb[0] ^= 0xFF;
    dev.write_at(BLOCK_SIZE, &sb).unwrap();

    let loaded = load_state(&dev).unwrap();
    assert_eq!(loaded, state);
}

#[test]
fn payload_crc_mismatch_is_reported() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    save_state(&mut dev, &empty_state()).unwrap();

    // Corrupt a byte inside the payload region (data_start block).
    let info = inspect(&dev).unwrap();
    let data_block = info.total_blocks - info.free_blocks - 1; // last allocated data block (actually, data_start is simpler; we recompute via superblock)
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let mut payload = dev.read_at(sb.data_start * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    payload[10] ^= 0x5A;
    dev.write_at(sb.data_start * BLOCK_SIZE, &payload).unwrap();

    let err = load_state(&dev).unwrap_err();
    assert!(format!("{err}").contains("CRC"));
    let _ = data_block;
}

#[test]
fn inspect_reports_redundant_copy_health() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let info = inspect(&dev).unwrap();
    assert!(info.primary_ok);
    assert!(info.tertiary_ok);
    assert!(info.secondary_ok);
    assert_eq!(info.label, "odf-test");
}

#[test]
fn save_updates_bitmap_to_reflect_payload_allocation() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let state = empty_state();
    let report = save_state(&mut dev, &state).unwrap();

    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let bitmap_bytes = dev
        .read_at(
            sb.block_bitmap_start * BLOCK_SIZE,
            sb.block_bitmap_blocks * BLOCK_SIZE,
        )
        .unwrap();
    let bitmap = crate::storage::ondisk::bitmap::Bitmap::from_bytes(
        bitmap_bytes,
        sb.total_blocks,
    )
    .unwrap();
    // The payload_blocks at data_start must be marked allocated.
    for i in 0..report.payload_blocks {
        assert!(bitmap.is_set(sb.data_start + i).unwrap());
    }
    // A further data block must be free.
    let next = sb.data_start + report.payload_blocks;
    if next < sb.total_blocks - 1 {
        assert!(!bitmap.is_set(next).unwrap());
    }
}

#[test]
fn reject_volume_too_small() {
    let mut dev = MemoryDevice::new(32 * BLOCK_SIZE, 4096).unwrap();
    assert!(format_device(&mut dev, &default_options()).is_err());
}

#[test]
fn generation_advances_across_saves() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let s = empty_state();
    let r1 = save_state(&mut dev, &s).unwrap();
    let r2 = save_state(&mut dev, &s).unwrap();
    let r3 = save_state(&mut dev, &s).unwrap();
    assert_eq!(r1.generation, 2);
    assert_eq!(r2.generation, 3);
    assert_eq!(r3.generation, 4);
}

#[test]
fn inode_record_is_system_payload_after_save() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    save_state(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let block = dev
        .read_at(sb.inode_table_start * BLOCK_SIZE, BLOCK_SIZE)
        .unwrap();
    let inode = crate::storage::ondisk::inode::OnDiskInode::decode(
        &block[..crate::storage::ondisk::inode::INODE_RECORD_SIZE],
    )
    .unwrap();
    assert_eq!(inode.kind, OnDiskKind::SystemPayload);
    assert_eq!(inode.extents.len(), 1);
    assert!(inode.size_bytes > 0);
}

#[test]
fn reports_error_when_unformatted_device_is_loaded() {
    let dev = fresh_device(4096);
    let err = load_state(&dev).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("magic") || msg.contains("superblock"));
}

#[test]
fn state_with_snapshot_roundtrips() {
    use crate::domain::snapshot::Snapshot;
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state.snapshots.push(Snapshot {
        id: 1,
        name: "v1".to_string(),
        scope_root: "/".to_string(),
        created_at: SystemTime::UNIX_EPOCH,
        paths: vec!["/".to_string()],
        file_data: BTreeMap::new(),
        inodes: BTreeMap::new(),
    });
    state.next_snapshot_id = 2;

    save_state(&mut dev, &state).unwrap();
    let loaded = load_state(&dev).unwrap();
    assert_eq!(loaded.snapshots.len(), 1);
    assert_eq!(loaded.next_snapshot_id, 2);
    assert_eq!(loaded, state);
}
