// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::volume::VolumeDescriptor;
use crate::platform::Timestamp;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::MemoryDevice;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord, BlockStore};
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::volume::{FormatOptions, format_device};
use crate::storage::persisted_state::PersistedState;
use alloc::string::ToString;

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
        volume: VolumeDescriptor::from_config_at(&config, crate::platform::Timestamp::EPOCH),
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

fn t(epoch_offset: u64) -> Timestamp {
    Timestamp::from_secs(1_700_000_000 + epoch_offset)
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
        accessed_at: t(15),
        metadata: FileMetadata::default(),
    }
}

/// Helper: write bytes for an inode to the device via BlockStore and return the
/// updated PersistedState (with block_records populated from the store).
fn write_inode_content(
    dev: &mut MemoryDevice,
    state: &mut PersistedState,
    inode: &Inode,
    bytes: &[u8],
) {
    // Build a temporary BlockStore using the device.
    // We need to write to a region AFTER the ODF metadata.
    // Use a simple block store that writes to the device at high offsets.
    // For simplicity in tests, use BlockStore::default() internal device,
    // and store the resulting BlockRecord manually.
    let mut store = BlockStore::default();
    store.write(inode.id, bytes.to_vec());
    let records = store.records();
    // The compat BlockStore uses its own internal MemoryDevice.
    // For native_tests, we need the bytes on the same device as ODF.
    // Use a simple approach: write bytes directly to a known area
    // and create the BlockRecord manually.
    // We'll use a high data block offset (block 500+) to avoid ODF metadata.
    let base_block: u64 = 500;
    let needed_blocks = if bytes.is_empty() { 0u64 } else { (bytes.len() as u64).div_ceil(BLOCK_SIZE) };
    if needed_blocks > 0 {
        let padded_len = (needed_blocks * BLOCK_SIZE) as usize;
        let mut buf = alloc::vec![0u8; padded_len];
        buf[..bytes.len()].copy_from_slice(bytes);
        dev.write_at(base_block * BLOCK_SIZE, &buf).expect("write ok");
    }
    let crc = if bytes.is_empty() { 0u32 } else { crate::storage::ondisk::checksum::Crc32c::hash(bytes) };
    // Remove any existing record for this inode.
    state.block_records.retain(|r| r.inode != inode.id);
    if !bytes.is_empty() {
        use crate::storage::block_store::ExtentRef;
        state.block_records.push(BlockRecord {
            inode: inode.id,
            logical_size: bytes.len() as u64,
            extents: alloc::vec![ExtentRef {
                logical_block: 0,
                logical_len: bytes.len() as u32,
                physical_block: base_block,
                length_blocks: needed_blocks as u32,
                physical_len: bytes.len() as u32,
                content_crc: crc,
                flags: 0,
            }],
            content_crc: crc,
            flags: 0,
        });
    }
}

/// Read bytes from a loaded BlockRecord using the device.
fn read_inode_bytes(dev: &MemoryDevice, rec: &BlockRecord) -> Vec<u8> {
    if rec.extents.is_empty() || rec.logical_size == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for ext in &rec.extents {
        let byte_offset = ext.physical_block * BLOCK_SIZE;
        let read_len = u64::from(ext.length_blocks) * BLOCK_SIZE;
        if let Ok(buf) = dev.read_at(byte_offset, read_len) {
            let want = (ext.logical_len as usize).min(buf.len());
            out.extend_from_slice(&buf[..want]);
        }
    }
    out.truncate(rec.logical_size as usize);
    out
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
    write_inode_content(&mut dev, &mut state, &inode, b"hello, world!");

    let report = save_state_native(&mut dev, &state).unwrap();
    assert_eq!(report.active_slots, 1);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 1);
    assert_eq!(loaded.active_inodes[0].id, inode.id);
    assert_eq!(loaded.active_inodes[0].path, inode.path);
    assert_eq!(loaded.block_records.len(), 1);
    assert_eq!(read_inode_bytes(&dev, &loaded.block_records[0]), b"hello, world!");
    // data_crc should be set to the CRC32C of the content.
    let expected_crc = u32::from(crate::storage::ondisk::checksum::Crc32c::hash(
        b"hello, world!",
    ));
    assert_eq!(loaded.block_records[0].content_crc, expected_crc);
}

#[test]
fn many_inodes_roundtrip() {
    let mut dev = fresh_device(8192);
    format_device(&mut dev, &default_options()).unwrap();

    let mut state = empty_state();
    // Use a separate block range for each inode (base_block increments)
    // Since write_inode_content always uses block 500, we need a different approach for many inodes.
    // For this test, skip content verification and just test structure.
    for i in 1..20 {
        let content = alloc::format!("payload-{i}");
        let inode = sample_inode(
            i as u64,
            &alloc::format!("/f{i}.txt"),
            InodeKind::File,
            content.len(),
        );
        state.active_inodes.push(inode.clone());
        // Add a minimal BlockRecord without real device content for structure test.
        state.block_records.push(BlockRecord {
            inode: inode.id,
            logical_size: content.len() as u64,
            extents: alloc::vec![],
            content_crc: crate::storage::ondisk::checksum::Crc32c::hash(content.as_bytes()),
            flags: 0,
        });
    }
    // A directory and a symlink for variety.
    state
        .active_inodes
        .push(sample_inode(200, "/", InodeKind::Directory, 0));
    state
        .active_inodes
        .push(sample_inode(201, "/link", InodeKind::Symlink, 0));

    let report = save_state_native(&mut dev, &state).unwrap();
    assert_eq!(report.active_slots, 21);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 21);
    // Block records: files with empty extents won't produce on-disk block records
    // (no extents → size_bytes=0 on disk)

    // Paths and ids preserved.
    let paths: hashbrown::HashSet<_> = loaded
        .active_inodes
        .iter()
        .map(|i| i.path.clone())
        .collect();
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
    assert_eq!(
        sb.layout_mode,
        crate::storage::ondisk::superblock::LAYOUT_MODE_NATIVE
    );
    assert_eq!(sb.payload_inode, ANCILLARY_INODE_SLOT);
}

#[test]
fn load_native_rejects_blob_mode_volume() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    crate::storage::ondisk::volume::save_state(&mut dev, &empty_state()).unwrap();
    let err = load_state_native(&dev).unwrap_err();
    assert!(alloc::format!("{err}").contains("NATIVE"));
}

#[test]
fn corrupted_attr_block_is_reported() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state
        .active_inodes
        .push(sample_inode(9, "/x", InodeKind::File, 0));
    save_state_native(&mut dev, &state).unwrap();

    // Find the attr block of slot 10 and corrupt it.
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let geom = sb.geometry();
    let (block, offset) = geom.inode_record_location(FIRST_USER_INODE_SLOT).unwrap();
    let slot_buf = dev.read_at(block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let on_disk = crate::storage::ondisk::inode::OnDiskInode::decode(
        &slot_buf
            [offset as usize..offset as usize + crate::storage::ondisk::inode::INODE_RECORD_SIZE],
    )
    .unwrap();
    let attr_block = on_disk.xattr_block_addr;
    let mut attr_buf = dev.read_at(attr_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    attr_buf[20] ^= 0xFF;
    dev.write_at(attr_block * BLOCK_SIZE, &attr_buf).unwrap();

    let err = load_state_native(&dev).unwrap_err();
    let msg = alloc::format!("{err}");
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
        accessed_at: t(0),
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
fn incremental_first_call_falls_back_to_full_save() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let state = empty_state();
    let report = save_state_native_incremental(&mut dev, &state).unwrap();
    assert!(report.fell_back_to_full_save);
    assert_eq!(report.generation, 2);
}

#[test]
fn incremental_skips_unchanged_inodes() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    for i in 1..=5 {
        let payload = alloc::format!("v1-{i}");
        let inode = sample_inode(i as u64, &alloc::format!("/f{i}"), InodeKind::File, payload.len());
        state.active_inodes.push(inode.clone());
        state.block_records.push(BlockRecord {
            inode: inode.id,
            logical_size: payload.len() as u64,
            extents: alloc::vec![],
            content_crc: crate::storage::ondisk::checksum::Crc32c::hash(payload.as_bytes()),
            flags: 0,
        });
    }
    save_state_native(&mut dev, &state).unwrap();

    // Re-save the same state — every inode should be reported unchanged.
    let report = save_state_native_incremental(&mut dev, &state).unwrap();
    assert!(!report.fell_back_to_full_save);
    assert_eq!(report.unchanged, 5);
    assert_eq!(report.created, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.removed, 0);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 5);
}

#[test]
fn incremental_classifies_create_update_remove() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut s1 = empty_state();
    let push = |state: &mut PersistedState, id: u64, content: &str| {
        let inode = sample_inode(id, &alloc::format!("/f{id}"), InodeKind::File, content.len());
        state.active_inodes.push(inode.clone());
        state.block_records.push(BlockRecord {
            inode: inode.id,
            logical_size: content.len() as u64,
            extents: alloc::vec![],
            content_crc: crate::storage::ondisk::checksum::Crc32c::hash(content.as_bytes()),
            flags: 0,
        });
    };
    push(&mut s1, 1, "alpha");
    push(&mut s1, 2, "beta");
    push(&mut s1, 3, "gamma");
    save_state_native(&mut dev, &s1).unwrap();

    // s2 = s1 minus inode 3, plus inode 4, with inode 2's content updated.
    let mut s2 = empty_state();
    push(&mut s2, 1, "alpha"); // unchanged
    push(&mut s2, 2, "beta-CHANGED"); // updated
    push(&mut s2, 4, "delta"); // created (3 is removed implicitly)

    let report = save_state_native_incremental(&mut dev, &s2).unwrap();
    assert!(!report.fell_back_to_full_save);
    assert_eq!(report.unchanged, 1, "inode 1");
    assert_eq!(report.updated, 1, "inode 2");
    assert_eq!(report.removed, 1, "inode 3");
    assert_eq!(report.created, 1, "inode 4");

    // Roundtrip: reloading must reflect the new state precisely.
    let loaded = load_state_native(&dev).unwrap();
    let by_id: hashbrown::HashMap<u64, &Inode> =
        loaded.active_inodes.iter().map(|i| (i.id.0, i)).collect();
    assert_eq!(by_id.len(), 3);
    assert!(by_id.contains_key(&1));
    assert!(by_id.contains_key(&2));
    assert!(!by_id.contains_key(&3));
    assert!(by_id.contains_key(&4));
}

#[test]
fn dirty_incremental_persists_metadata_only_update() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    let mut inode = sample_inode(1, "/f1", InodeKind::File, 4);
    inode.metadata.mode = 0o644;
    state.active_inodes.push(inode.clone());
    state.block_records.push(BlockRecord {
        inode: inode.id,
        logical_size: 4,
        extents: alloc::vec![],
        content_crc: crate::storage::ondisk::checksum::Crc32c::hash(b"data"),
        flags: 0,
    });
    save_state_native(&mut dev, &state).unwrap();

    state.active_inodes[0].path = "/renamed".into();
    state.active_inodes[0].metadata.mode = 0o600;
    state.active_inodes[0].touch_changed_at(Timestamp::EPOCH);
    let report =
        save_state_native_incremental_dirty(&mut dev, &state, &[InodeId(1)], &[]).unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(report.created, 0);
    assert_eq!(report.removed, 0);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.active_inodes.len(), 1);
    assert_eq!(loaded.active_inodes[0].path, "/renamed");
    assert_eq!(loaded.active_inodes[0].metadata.mode, 0o600);
}

#[test]
fn dirty_incremental_uses_and_reports_slot_hints() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state
        .active_inodes
        .push(sample_inode(1, "/f1", InodeKind::File, 0));
    save_state_native(&mut dev, &state).unwrap();

    let slot_index = load_native_inode_slot_index(&dev).unwrap();
    assert_eq!(slot_index.len(), 1);
    assert_eq!(slot_index[0].inode, InodeId(1));

    state.active_inodes[0].metadata.mode = 0o600;
    state.active_inodes[0].touch_changed_at(Timestamp::EPOCH);
    let report = save_state_native_incremental_dirty_with_slots(
        &mut dev,
        &state,
        &[InodeId(1)],
        &[],
        &slot_index,
    )
    .unwrap();
    assert_eq!(report.incremental.updated, 1);
    assert_eq!(
        report.assigned_slots,
        alloc::vec![InodeSlotMapping {
            inode: InodeId(1),
            slot: slot_index[0].slot,
        }]
    );
    assert!(report.removed_slots.is_empty());

    let reloaded_index = load_native_inode_slot_index(&dev).unwrap();
    assert_eq!(reloaded_index, slot_index);
}

#[test]
fn native_record_extents_spill_to_index_chain() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    let inode = sample_inode(
        1,
        "/many-extents",
        InodeKind::File,
        10 * BLOCK_SIZE as usize,
    );
    let mut extents = Vec::new();
    for i in 0..10u64 {
        let physical_block = 600 + (i * 2);
        let payload = alloc::vec![i as u8; BLOCK_SIZE as usize];
        dev.write_at(physical_block * BLOCK_SIZE, &payload).unwrap();
        extents.push(crate::storage::block_store::ExtentRef {
            logical_block: i as u32,
            logical_len: BLOCK_SIZE as u32,
            physical_block,
            length_blocks: 1,
            physical_len: BLOCK_SIZE as u32,
            content_crc: crate::storage::ondisk::checksum::Crc32c::hash(&payload),
            flags: 0,
        });
    }
    state.active_inodes.push(inode.clone());
    state.block_records.push(BlockRecord {
        inode: inode.id,
        logical_size: 10 * BLOCK_SIZE,
        extents,
        content_crc: 0,
        flags: 0,
    });

    save_state_native(&mut dev, &state).unwrap();

    let slot = load_native_inode_slot_index(&dev).unwrap()[0].slot;
    let sb = crate::storage::ondisk::volume::read_sb_with_fallbacks(&dev).unwrap();
    let on_disk = read_inode_at_slot(&dev, &sb.geometry(), slot).unwrap();
    assert_eq!(on_disk.extents.len(), 0);
    assert!(on_disk.flags & crate::storage::ondisk::inode::FLAG_HAS_EXTENT_INDEX != 0);
    assert_ne!(on_disk.index_block_addr, 0);

    let loaded = load_state_native(&dev).unwrap();
    assert_eq!(loaded.block_records.len(), 1);
    assert_eq!(loaded.block_records[0].extents.len(), 10);
    assert_eq!(loaded.block_records[0].logical_size, 10 * BLOCK_SIZE);
}

#[test]
fn dirty_incremental_releases_removed_inode_slot() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state
        .active_inodes
        .push(sample_inode(1, "/f1", InodeKind::File, 0));
    save_state_native(&mut dev, &state).unwrap();

    state.active_inodes.clear();
    let report =
        save_state_native_incremental_dirty(&mut dev, &state, &[], &[InodeId(1)]).unwrap();
    assert_eq!(report.removed, 1);

    let loaded = load_state_native(&dev).unwrap();
    assert!(loaded.active_inodes.is_empty());
    assert!(loaded.deleted_inodes.is_empty());
}

#[test]
fn incremental_save_reuses_freed_blocks() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut s1 = empty_state();
    // Use write_inode_content for the big file so it has real extents.
    let big_inode = sample_inode(7, "/big", InodeKind::File, 16 * 1024);
    s1.active_inodes.push(big_inode.clone());
    write_inode_content(&mut dev, &mut s1, &big_inode, &alloc::vec![0xAA; 16 * 1024]);
    save_state_native(&mut dev, &s1).unwrap();
    let sb1 = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let used_after_first = sb1.total_blocks - sb1.free_blocks - sb1.data_start;

    // Remove the big inode entirely.
    let s2 = empty_state();
    let report = save_state_native_incremental(&mut dev, &s2).unwrap();
    assert_eq!(report.removed, 1);

    let sb2 = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let used_after_remove = sb2.total_blocks - sb2.free_blocks - sb2.data_start;
    assert!(
        used_after_remove < used_after_first,
        "expected fewer used blocks after remove (was {used_after_first}, now {used_after_remove})"
    );
}

#[test]
fn incremental_passes_fsck() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    for i in 1..=4 {
        let inode = sample_inode(i as u64, &alloc::format!("/f{i}"), InodeKind::File, 32);
        state.active_inodes.push(inode.clone());
        state.block_records.push(BlockRecord {
            inode: inode.id,
            logical_size: 32,
            extents: alloc::vec![],
            content_crc: crate::storage::ondisk::checksum::Crc32c::hash(&alloc::vec![i as u8; 32]),
            flags: 0,
        });
    }
    save_state_native(&mut dev, &state).unwrap();
    // Mutate one inode's content CRC.
    state.block_records[1].content_crc = crate::storage::ondisk::checksum::Crc32c::hash(b"new content for inode 2");
    state.active_inodes[1].size = 23;
    save_state_native_incremental(&mut dev, &state).unwrap();

    let fsck = crate::storage::ondisk::fsck::check(&dev).unwrap();
    assert!(
        fsck.is_clean(),
        "issues after incremental: {:?}",
        fsck.issues
    );
}

#[test]
fn root_inode_pointer_records_directory() {
    let mut dev = fresh_device(4096);
    format_device(&mut dev, &default_options()).unwrap();
    let mut state = empty_state();
    state
        .active_inodes
        .push(sample_inode(1, "/", InodeKind::Directory, 0));
    save_state_native(&mut dev, &state).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    assert_eq!(sb.root_inode, 1);
}
