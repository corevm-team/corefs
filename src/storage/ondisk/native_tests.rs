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
use crate::storage::ondisk::volume::{FormatOptions, format_device};
use std::time::{SystemTime, UNIX_EPOCH};

fn fresh_device(blocks: u64) -> MemoryDevice {
    MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap()
}

fn default_options() -> FormatOptions {
    FormatOptions {
        label: "native".into(),
        uuid: *b"NATIVE----------",
        inode_count: 1024,
        journal_blocks: 32,
    }
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

fn t(epoch_offset: u64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + epoch_offset)
}

fn sample_inode(id: u64, path: &str, kind: InodeKind, size: usize) -> Inode {
    Inode {
        id: InodeId(id),
        kind,
        path: path.to_string(),
        size,
        created_at: t(0),
        modified_at: t(10),
        changed_at: t(20),
        metadata: FileMetadata::default(),
    }
}

#[test]
fn empty_state_native_roundtrip() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let state = empty_state();
    let report = save_state_native(&mut dev, &state).unwrap();
    assert_eq!(report.active_slots, 0);
    assert_eq!(report.deleted_slots, 0);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded, state);
}

#[test]
fn single_file_roundtrip() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();

    let mut state = empty_state();
    let inode = sample_inode(100, "/foo.txt", InodeKind::File, 13);
    state.active_inodes.push(inode.clone());
    state.block_records.push(BlockRecord {
        inode: inode.id,
        bytes: b"hello, world!".to_vec(),
        checksum: 0, // will be recomputed as Crc32c on load
        device_block: 0,
        allocated_blocks: 0,
    });

    let report = save_state_native(&mut dev, &state).unwrap();
    assert_eq!(report.active_slots, 1);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 1);
    assert_eq!(loaded.active_inodes[0].id, inode.id);
    assert_eq!(loaded.active_inodes[0].path, inode.path);
    assert_eq!(loaded.block_records.len(), 1);
    assert_eq!(loaded.block_records[0].bytes, b"hello, world!");
    // data_crc should be set to the CRC32C of the content.
    let expected_crc = u64::from(
        crate::storage::ondisk::checksum::Crc32c::hash(b"hello, world!"),
    );
    assert_eq!(loaded.block_records[0].checksum, expected_crc);
}

#[test]
fn many_inodes_roundtrip() {
    let mut dev = fresh_device(8192);
    format_device(&mut dev, &default_options()).unwrap();

    let mut state = empty_state();
    for i in 1..20 {
        let content = format!("payload-{i}");
        let inode = sample_inode(i as u64, &format!("/f{i}.txt"), InodeKind::File, content.len());
        state.active_inodes.push(inode.clone());
        state.block_records.push(BlockRecord {
            inode: inode.id,
            bytes: content.into_bytes(),
            checksum: 0,
            device_block: 0,
            allocated_blocks: 0,
        });
    }
    // A directory and a symlink for variety.
    state.active_inodes.push(sample_inode(200, "/", InodeKind::Directory, 0));
    state.active_inodes.push(sample_inode(201, "/link", InodeKind::Symlink, 0));

    let report = save_state_native(&mut dev, &state).unwrap();
    assert_eq!(report.active_slots, 21);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 21);
    assert_eq!(loaded.block_records.len(), 19);

    // Paths and ids preserved.
    let paths: std::collections::HashSet<_> =
        loaded.active_inodes.iter().map(|i| i.path.clone()).collect();
    assert!(paths.contains("/f5.txt"));
    assert!(paths.contains("/"));
    assert!(paths.contains("/link"));
}

#[test]
fn deleted_inodes_round_trip_with_flag() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();

    let mut state = empty_state();
    let kept = sample_inode(1, "/kept", InodeKind::File, 0);
    let gone = sample_inode(2, "/gone", InodeKind::File, 0);
    state.active_inodes.push(kept);
    state.deleted_inodes.push(gone);

    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 1);
    assert_eq!(loaded.deleted_inodes.len(), 1);
    assert_eq!(loaded.active_inodes[0].path, "/kept");
    assert_eq!(loaded.deleted_inodes[0].path, "/gone");
}

#[test]
fn superblock_layout_mode_is_native_after_save() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    save_state_native(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    assert_eq!(sb.layout_mode, crate::storage::ondisk::superblock::LAYOUT_MODE_NATIVE);
    assert_eq!(sb.payload_inode, ANCILLARY_INODE_SLOT);
}

#[test]
fn load_native_rejects_blob_mode_volume() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    crate::storage::ondisk::volume::save_state(&mut dev, &empty_state()).unwrap();
    let err = load_state_native(&dev).unwrap_err();
    assert!(format!("{err}").contains("NATIVE"));
}

#[test]
fn corrupted_attr_block_is_reported() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state.active_inodes.push(sample_inode(9, "/x", InodeKind::File, 0));
    save_state_native(&mut dev, &state).unwrap();

    // Find the attr block of slot 10 and corrupt it.
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let geom = sb.geometry();
    let (block, offset) = geom.inode_record_location(FIRST_USER_INODE_SLOT).unwrap();
    let slot_buf = dev.read_at(block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let on_disk = crate::storage::ondisk::inode::OnDiskInode::decode(
        &slot_buf[offset as usize..offset as usize + crate::storage::ondisk::inode::INODE_RECORD_SIZE],
    ).unwrap();
    let attr_block = on_disk.xattr_block_addr;
    let mut attr_buf = dev.read_at(attr_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    attr_buf[20] ^= 0xFF;
    dev.write_at(attr_block * BLOCK_SIZE, &attr_buf).unwrap();

    let err = load_state_native(&dev).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("CRC") || msg.contains("attr"));
}

#[test]
fn encryption_flag_propagates_to_on_disk_inode() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    let mut meta = FileMetadata::default();
    meta.encrypted = true;
    meta.compressed = true;
    state.active_inodes.push(Inode {
        id: InodeId(77),
        kind: InodeKind::File,
        path: "/secret".into(),
        size: 0,
        created_at: t(0),
        modified_at: t(0),
        changed_at: t(0),
        metadata: meta,
    });
    save_state_native(&mut dev, &state).unwrap();

    // Read the on-disk inode for slot 10 (first user slot).
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let geom = sb.geometry();
    let (block, offset) = geom.inode_record_location(FIRST_USER_INODE_SLOT).unwrap();
    let buf = dev.read_at(block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let on_disk = crate::storage::ondisk::inode::OnDiskInode::decode(
        &buf[offset as usize..offset as usize + crate::storage::ondisk::inode::INODE_RECORD_SIZE],
    )
    .unwrap();
    assert!(on_disk.flags & crate::storage::ondisk::inode::FLAG_ENCRYPTED != 0);
    assert!(on_disk.flags & crate::storage::ondisk::inode::FLAG_COMPRESSED != 0);
    assert!(on_disk.flags & crate::storage::ondisk::inode::FLAG_HAS_XATTRS != 0);
    assert_eq!(on_disk.domain_inode_id, 77);
}

#[test]
fn root_inode_pointer_records_directory() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state.active_inodes.push(sample_inode(1, "/", InodeKind::Directory, 0));
    save_state_native(&mut dev, &state).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    assert_eq!(sb.root_inode, 1);
}
