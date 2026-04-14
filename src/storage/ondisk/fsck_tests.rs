// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::volume::VolumeDescriptor;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::MemoryDevice;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord};
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::native::save_state_native;
use crate::storage::ondisk::volume::{FormatOptions, format_device, save_state};
use std::time::{SystemTime, UNIX_EPOCH};

fn make_dev() -> MemoryDevice {
    let mut dev = MemoryDevice::new(4096 * BLOCK_SIZE, 4096).unwrap();
    let opts = FormatOptions {
        label: "fsck".into(),
        uuid: [1u8; 16],
        inode_count: 1024,
        journal_blocks: 32,
    };
    format_device(&mut dev, &opts).unwrap();
    dev
}

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

#[test]
fn clean_blob_volume_has_no_errors() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let report = check(&dev).unwrap();
    assert!(report.is_clean(), "issues: {:?}", report.issues);
    assert!(report.inodes_checked >= 1); // slot 0
}

#[test]
fn clean_native_volume_has_no_errors() {
    let mut dev = make_dev();
    let mut state = empty_state();
    state.active_inodes.push(Inode {
        id: InodeId(42),
        kind: InodeKind::File,
        path: "/x".into(),
        size: 0,
        created_at: (UNIX_EPOCH + std::time::Duration::from_secs(1)).into(),
        modified_at: (UNIX_EPOCH + std::time::Duration::from_secs(2)).into(),
        changed_at: (UNIX_EPOCH + std::time::Duration::from_secs(3)).into(),
        metadata: FileMetadata::default(),
    });
    state.block_records.push(BlockRecord {
        inode: InodeId(42),
        bytes: b"abc".to_vec(),
        checksum: 0,
        device_block: 0,
        allocated_blocks: 0,
    });
    save_state_native(&mut dev, &state).unwrap();
    let report = check(&dev).unwrap();
    assert!(report.is_clean(), "issues: {:?}", report.issues);
    assert!(report.inodes_checked >= 2); // ancillary + user inode
}

#[test]
fn corrupt_block_bitmap_is_reported() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let mut bmp = dev.read_at(sb.block_bitmap_start * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    bmp[77] ^= 0x04;
    dev.write_at(sb.block_bitmap_start * BLOCK_SIZE, &bmp).unwrap();

    let report = check(&dev).unwrap();
    assert!(
        report.issues.iter().any(|i| i.code == "ODF.BBM.CRC"),
        "issues: {:?}",
        report.issues
    );
    assert!(!report.is_clean());
}

#[test]
fn stale_tertiary_superblock_warns() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    // Corrupt the tertiary (middle) superblock.
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let mid = sb.tertiary_superblock_block * BLOCK_SIZE;
    let mut buf = dev.read_at(mid, BLOCK_SIZE).unwrap();
    buf[100] ^= 0xFF;
    dev.write_at(mid, &buf).unwrap();

    let report = check(&dev).unwrap();
    assert!(
        report.issues.iter().any(|i| i.code == "ODF.SB.TERTIARY_UNREADABLE"),
        "issues: {:?}", report.issues
    );
}

#[test]
fn extent_pointing_outside_data_region_is_error() {
    let mut dev = make_dev();
    let mut state = empty_state();
    state.active_inodes.push(Inode {
        id: InodeId(5),
        kind: InodeKind::File,
        path: "/y".into(),
        size: 0,
        created_at: UNIX_EPOCH.into(),
        modified_at: UNIX_EPOCH.into(),
        changed_at: UNIX_EPOCH.into(),
        metadata: FileMetadata::default(),
    });
    state.block_records.push(BlockRecord {
        inode: InodeId(5),
        bytes: b"zz".to_vec(),
        checksum: 0,
        device_block: 0,
        allocated_blocks: 0,
    });
    save_state_native(&mut dev, &state).unwrap();

    // Tamper with slot 10's first extent to point at block 0 (reserved).
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let geom = sb.geometry();
    let (block, offset) = geom.inode_record_location(
        crate::storage::ondisk::native::FIRST_USER_INODE_SLOT,
    ).unwrap();
    let mut buf = dev.read_at(block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let slot_bytes = &mut buf[offset as usize..offset as usize + crate::storage::ondisk::inode::INODE_RECORD_SIZE];
    // First extent begins at offset 88 (physical u64 at 88..96).
    slot_bytes[88..96].copy_from_slice(&0u64.to_le_bytes());
    // Recompute CRC of the inode record.
    slot_bytes[crate::storage::ondisk::inode::INODE_RECORD_SIZE - 4..
               crate::storage::ondisk::inode::INODE_RECORD_SIZE].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(slot_bytes);
    slot_bytes[crate::storage::ondisk::inode::INODE_RECORD_SIZE - 4..
               crate::storage::ondisk::inode::INODE_RECORD_SIZE]
        .copy_from_slice(&csum.to_le_bytes());
    // Also update bitmap CRC by recomputing to keep bitmap check from
    // firing first — we want the extent check to be the one that fires.
    dev.write_at(block * BLOCK_SIZE, &buf).unwrap();

    let report = check(&dev).unwrap();
    assert!(
        report.issues.iter().any(|i| i.code == "ODF.INODE.EXTENT_OUT_OF_RANGE"),
        "issues: {:?}", report.issues
    );
}

#[test]
fn fsck_is_read_only() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let before = dev.data().to_vec();
    let _ = check(&dev).unwrap();
    let after = dev.data().to_vec();
    assert_eq!(before, after);
}

#[test]
fn report_counts_inodes_and_extents() {
    let mut dev = make_dev();
    let mut state = empty_state();
    for i in 1..5 {
        state.active_inodes.push(Inode {
            id: InodeId(i),
            kind: InodeKind::File,
            path: format!("/f{i}"),
            size: 0,
            created_at: SystemTime::now().into(),
            modified_at: SystemTime::now().into(),
            changed_at: SystemTime::now().into(),
            metadata: FileMetadata::default(),
        });
        state.block_records.push(BlockRecord {
            inode: InodeId(i),
            bytes: vec![i as u8; 10],
            checksum: 0,
            device_block: 0,
            allocated_blocks: 0,
        });
    }
    save_state_native(&mut dev, &state).unwrap();
    let report = check(&dev).unwrap();
    assert!(report.is_clean(), "issues: {:?}", report.issues);
    assert!(report.extents_checked >= 4);
    assert!(report.blocks_referenced >= 4);
}
