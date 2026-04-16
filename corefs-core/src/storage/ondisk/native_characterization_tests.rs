// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Characterization tests for native save / load (ODF per-inode layout).
//
// These tests pin the observable behaviour of `save_state_native` /
// `load_state_native` / `save_state_native_incremental`.  They must pass
// against the current implementation and must still pass — unchanged —
// after the extent-based refactor.  A pre-refactor green that turns red
// post-refactor signals a regression.

use super::*;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::volume::VolumeDescriptor;
use crate::platform::Timestamp;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::MemoryDevice;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord};
use crate::storage::ondisk::checksum::Crc32c;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::volume::{FormatOptions, format_device};
use crate::storage::persisted_state::PersistedState;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn fresh_dev(blocks: u64) -> MemoryDevice {
    MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap()
}

fn default_opts() -> FormatOptions {
    FormatOptions {
        label: "char".into(),
        uuid: *b"CHAR------------",
        inode_count: 1024,
        journal_blocks: 32,
    }
}

fn empty_state() -> PersistedState {
    let config = CoreFsConfig::default();
    PersistedState {
        volume: VolumeDescriptor::from_config_at(&config, Timestamp::EPOCH),
        config,
        clean_unmount: true,
        pending_wal: None,
        active_inodes: alloc::vec![],
        deleted_inodes: alloc::vec![],
        allocator_policy: AllocatorPolicy::default(),
        free_extents: alloc::vec![],
        hot_path_records: alloc::vec![],
        block_records: alloc::vec![],
        journal_entries: alloc::vec![],
        journal_runtime: JournalRuntimeState::default(),
        versions: alloc::vec![],
        sync_statuses: alloc::vec![],
        snapshots: alloc::vec![],
        next_snapshot_id: 0,
    }
}

fn file_inode(id: u64, path: &str, size: usize) -> Inode {
    Inode {
        id: InodeId(id),
        kind: InodeKind::File,
        path: path.into(),
        size,
        created_at: Timestamp::EPOCH,
        modified_at: Timestamp::EPOCH,
        changed_at: Timestamp::EPOCH,
        metadata: FileMetadata::default(),
    }
}

fn prng_bytes(seed: u64, len: usize) -> alloc::vec::Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

fn block_rec(inode: &Inode, bytes: alloc::vec::Vec<u8>) -> BlockRecord {
    BlockRecord {
        inode: inode.id,
        bytes,
        checksum: 0,
        device_block: 0,
        allocated_blocks: 0,
    }
}

// ---------------------------------------------------------------------------
// Characterization tests
// ---------------------------------------------------------------------------

#[test]
fn char_save_load_empty_state_roundtrips() {
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let state = empty_state();
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert!(loaded.active_inodes.is_empty());
    assert!(loaded.deleted_inodes.is_empty());
    assert!(loaded.block_records.is_empty());
    assert_eq!(loaded.clean_unmount, state.clean_unmount);
}

#[test]
fn char_save_load_one_file_preserves_bytes() {
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let content = b"hello world";
    let inode = file_inode(1, "/hello.txt", content.len());
    state.active_inodes.push(inode.clone());
    state.block_records.push(block_rec(&inode, content.to_vec()));
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.block_records.len(), 1);
    assert_eq!(loaded.block_records[0].bytes, content);
    assert_eq!(loaded.active_inodes[0].path, "/hello.txt");
}

#[test]
fn char_save_load_large_file_preserves_bytes() {
    // 2 MiB random payload — characterises that load reads back all bytes
    // without truncation regardless of how many extents were needed.
    let mut dev = fresh_dev(8192);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let content = prng_bytes(0xABCD_1234, 2 * 1024 * 1024);
    let inode = file_inode(1, "/big.bin", content.len());
    state.active_inodes.push(inode.clone());
    state.block_records.push(block_rec(&inode, content.clone()));
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.block_records[0].bytes, content);
}

#[test]
fn char_save_load_sparse_file_preserves_zero_fill() {
    // A file whose content is all-zero bytes round-trips correctly.
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let zeros = alloc::vec![0u8; 8192];
    let inode = file_inode(1, "/sparse.bin", zeros.len());
    state.active_inodes.push(inode.clone());
    state.block_records.push(block_rec(&inode, zeros.clone()));
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.block_records[0].bytes, zeros);
}

#[test]
fn char_save_load_preserves_allocator_policy() {
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let mut policy = AllocatorPolicy::default();
    policy.fragmentation_threshold_percent = 42;
    state.allocator_policy = policy;
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.allocator_policy.fragmentation_threshold_percent, 42);
}

#[test]
fn char_save_load_preserves_inode_metadata() {
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let mut meta = FileMetadata::default();
    meta.mode = 0o755;
    meta.uid = 1001;
    meta.gid = 1002;
    let inode = Inode {
        id: InodeId(5),
        kind: InodeKind::File,
        path: "/meta.txt".into(),
        size: 0,
        created_at: Timestamp::from_secs(1_700_000_100),
        modified_at: Timestamp::from_secs(1_700_000_200),
        changed_at: Timestamp::from_secs(1_700_000_300),
        metadata: meta,
    };
    state.active_inodes.push(inode);
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    let li = &loaded.active_inodes[0];
    assert_eq!(li.metadata.mode, 0o755);
    assert_eq!(li.metadata.uid, 1001);
    assert_eq!(li.metadata.gid, 1002);
}

#[test]
fn char_incremental_save_unchanged_inode_is_classified_unchanged() {
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let inode = file_inode(1, "/stable.txt", 5);
    state.active_inodes.push(inode.clone());
    state.block_records.push(block_rec(&inode, b"hello".to_vec()));
    // Initial full save.
    save_state_native(&mut dev, &state).unwrap();
    // Reload to get the CRC32C-keyed checksum that incremental save expects.
    let state2 = load_state_native(&dev).unwrap();
    // Incremental save of an unchanged state must report ≥1 unchanged inode.
    let report = save_state_native_incremental(&mut dev, &state2).unwrap();
    assert!(
        report.unchanged >= 1,
        "unchanged inode must be classified as unchanged (got {})",
        report.unchanged
    );
}

#[test]
fn char_save_load_with_deleted_inodes_preserves_them() {
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    state.deleted_inodes.push(file_inode(99, "/deleted.txt", 0));
    save_state_native(&mut dev, &state).unwrap();
    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.deleted_inodes.len(), 1);
    assert_eq!(loaded.deleted_inodes[0].path, "/deleted.txt");
}

#[test]
fn char_corrupted_attr_block_returns_error_on_load() {
    // Flipping a byte in an attr block's CRC region causes load to fail.
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let inode = file_inode(1, "/x.txt", 3);
    state.active_inodes.push(inode.clone());
    state.block_records.push(block_rec(&inode, b"abc".to_vec()));
    save_state_native(&mut dev, &state).unwrap();

    // Find the on-disk inode slot and corrupt its attr block.
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb =
        crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let geom = sb.geometry();
    let (block, offset) = geom.inode_record_location(FIRST_USER_INODE_SLOT).unwrap();
    let slot_buf = dev.read_at(block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let on_disk = crate::storage::ondisk::inode::OnDiskInode::decode(
        &slot_buf
            [offset as usize..offset as usize + crate::storage::ondisk::inode::INODE_RECORD_SIZE],
    )
    .unwrap();
    let attr_addr = on_disk.xattr_block_addr;
    let mut attr_buf = dev.read_at(attr_addr * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    attr_buf[20] ^= 0xFF; // corrupt CRC field
    dev.write_at(attr_addr * BLOCK_SIZE, &attr_buf).unwrap();

    assert!(
        load_state_native(&dev).is_err(),
        "corrupted attr block must cause a load error"
    );
}

#[test]
fn char_corrupted_data_block_yields_crc_mismatch_on_load() {
    // If a data extent's bytes are corrupted on device, the loaded
    // BlockRecord's CRC32C checksum no longer matches the content.
    // (load_state_native stores the on-disk data_crc without re-verifying.)
    let mut dev = fresh_dev(4096);
    format_device(&mut dev, &default_opts()).unwrap();
    let mut state = empty_state();
    let content = b"verify me";
    let inode = file_inode(1, "/v.txt", content.len());
    state.active_inodes.push(inode.clone());
    state.block_records.push(block_rec(&inode, content.to_vec()));
    save_state_native(&mut dev, &state).unwrap();

    // First load to discover the physical extent address.
    let initial = load_state_native(&dev).unwrap();
    let rec = &initial.block_records[0];
    let data_offset = rec.device_block * BLOCK_SIZE;

    // Corrupt one byte in the data extent.
    let mut buf = dev.read_at(data_offset, BLOCK_SIZE).unwrap();
    buf[0] ^= 0xFF;
    dev.write_at(data_offset, &buf).unwrap();

    // Reload: bytes are corrupted but the on-disk data_crc was not updated,
    // so the loaded record's CRC no longer matches the content.
    let corrupted = load_state_native(&dev).unwrap();
    let crec = &corrupted.block_records[0];
    let actual_crc = u64::from(Crc32c::hash(&crec.bytes));
    assert_ne!(
        actual_crc, crec.checksum,
        "corrupted bytes must not match the stored CRC32C"
    );
}
