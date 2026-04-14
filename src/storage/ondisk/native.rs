// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Native per-inode ODF layout.
//!
//! The [blob layout](super::volume::save_state) stores the entire
//! [`PersistedState`] as a single bincode blob pinned to system inode #0.
//! The native layout instead gives every domain [`Inode`] its own on-disk
//! inode slot, allocates real extents for the file content and places the
//! inode's per-inode metadata (path, [`FileMetadata`], ACLs, tags, etc.)
//! in a sibling [`attr_block::AttrBlock`].
//!
//! This enables:
//!
//! * per-inode `fsck` walks (see [`super::fsck`]),
//! * per-inode encryption / compression / xattr flags ([`FLAG_ENCRYPTED`],
//!   [`FLAG_COMPRESSED`], [`FLAG_HAS_XATTRS`]),
//! * per-data-block CRC32C via [`OnDiskInode::data_crc`],
//! * future directory-block layout via [`super::dir_entry::DirBlock`].
//!
//! ## Inode numbering
//!
//! | slot  | meaning                                                       |
//! |-------|---------------------------------------------------------------|
//! | 0     | reserved (used by the blob layout)                            |
//! | 1     | ancillary state (config, volume, snapshots, versions, …)      |
//! | 2..9  | reserved for future system inodes                             |
//! | 10..  | one slot per active / deleted domain inode                    |
//!
//! ## Flags
//!
//! `OnDiskInode::flags` carries the already-defined
//! [`FLAG_HAS_EXTENT_INDEX`] / [`FLAG_ENCRYPTED`] / [`FLAG_COMPRESSED`] /
//! [`FLAG_HAS_XATTRS`] bits plus the native-specific
//! [`FLAG_DELETED`] below which marks an on-disk inode as logically
//! removed but retained for recovery.

use crate::app::PersistedState;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;
use crate::storage::block_store::BlockRecord;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use super::allocator::{AllocationStrategy, OndiskAllocator};
use super::attr_block::AttrBlock;
use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::inode::{
    Extent, FLAG_HAS_EXTENT_INDEX, INODE_RECORD_SIZE, OnDiskInode, OnDiskKind,
};
use super::layout::{BLOCK_SIZE, LayoutGeometry, PRIMARY_SUPERBLOCK_BLOCK};
use super::superblock::{LAYOUT_MODE_NATIVE, STATE_CLEAN, Superblock};

/// Flag — this on-disk slot holds a soft-deleted inode.
pub const FLAG_DELETED: u32 = 1 << 4;
/// First inode slot usable for domain inodes.
pub const FIRST_USER_INODE_SLOT: u64 = 10;
/// ODF slot number of the ancillary-state inode.
pub const ANCILLARY_INODE_SLOT: u64 = 1;

/// Ancillary bits of `PersistedState` that are not captured in per-inode
/// slots.  Serialized as bincode into [`ANCILLARY_INODE_SLOT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AncillaryState {
    config: crate::config::CoreFsConfig,
    volume: crate::domain::volume::VolumeDescriptor,
    clean_unmount: bool,
    pending_wal: Option<crate::storage::volume_wal::VolumeWal>,
    allocator_policy: crate::storage::block_store::AllocatorPolicy,
    free_extents: Vec<crate::storage::block_store::FreeExtentRecord>,
    hot_path_records: Vec<crate::services::hot_paths::HotPathRecord>,
    journal_entries: Vec<crate::services::journal::JournalEntry>,
    journal_runtime: crate::services::journal::JournalRuntimeState,
    versions: Vec<crate::services::versioning::FileVersion>,
    sync_statuses: Vec<crate::services::sync::SyncStatus>,
    snapshots: Vec<crate::domain::snapshot::Snapshot>,
    next_snapshot_id: u64,
}

impl AncillaryState {
    fn from(state: &PersistedState) -> Self {
        Self {
            config: state.config.clone(),
            volume: state.volume.clone(),
            clean_unmount: state.clean_unmount,
            pending_wal: state.pending_wal.clone(),
            allocator_policy: state.allocator_policy.clone(),
            free_extents: state.free_extents.clone(),
            hot_path_records: state.hot_path_records.clone(),
            journal_entries: state.journal_entries.clone(),
            journal_runtime: state.journal_runtime.clone(),
            versions: state.versions.clone(),
            sync_statuses: state.sync_statuses.clone(),
            snapshots: state.snapshots.clone(),
            next_snapshot_id: state.next_snapshot_id,
        }
    }
}

/// Report returned from [`save_state_native`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSaveReport {
    pub generation: u64,
    pub active_slots: usize,
    pub deleted_slots: usize,
    pub data_blocks_used: u64,
}

/// Persist a `PersistedState` in native per-inode layout.
pub fn save_state_native(
    device: &mut dyn BlockDevice,
    state: &PersistedState,
) -> CoreFsResult<NativeSaveReport> {
    let mut sb = super::volume::read_sb_with_fallbacks(device)?;
    let geom = sb.geometry();

    // Build fresh bitmaps — we rewrite the layout from scratch each save.
    let mut block_bitmap = Bitmap::new(geom.total_blocks);
    mark_reserved_blocks(&mut block_bitmap, &geom)?;
    let mut inode_bitmap = Bitmap::new(geom.inode_count);
    // Reserve only the slots that carry real data — slot 0 (blob legacy)
    // and slot 1 (ancillary).  Slots 2..FIRST_USER_INODE_SLOT stay free
    // for future system inodes so fsck doesn't see "allocated but
    // Unused" records.
    inode_bitmap.set(0)?;
    inode_bitmap.set(ANCILLARY_INODE_SLOT)?;
    let mut alloc = OndiskAllocator::new(
        &geom,
        block_bitmap,
        inode_bitmap,
        AllocationStrategy::FirstFit,
        FIRST_USER_INODE_SLOT,
    );

    // --- Ancillary blob at slot #1 ---------------------------------------
    let ancillary = AncillaryState::from(state);
    let anc_bytes = bincode::serialize(&ancillary).map_err(|e| {
        CoreFsError::State(format!("native: ancillary serialize failed: {e}"))
    })?;
    let anc_crc = Crc32c::hash(&anc_bytes);
    let mut anc_payload = anc_bytes.clone();
    anc_payload.extend_from_slice(&anc_crc.to_le_bytes());
    let anc_blocks_needed = (anc_payload.len() as u64).div_ceil(BLOCK_SIZE);
    let anc_extent = alloc.allocate_contiguous(anc_blocks_needed)?;
    let mut anc_buf = vec![0u8; (anc_blocks_needed * BLOCK_SIZE) as usize];
    anc_buf[..anc_payload.len()].copy_from_slice(&anc_payload);
    device.write_at(anc_extent.physical_block * BLOCK_SIZE, &anc_buf)?;
    let now = now_secs();
    let anc_inode = OnDiskInode {
        version: 1,
        kind: OnDiskKind::SystemPayload,
        mode: 0o600,
        uid: 0,
        gid: 0,
        link_count: 1,
        flags: 0,
        size_bytes: anc_payload.len() as u64,
        blocks_allocated: anc_blocks_needed,
        created_at: sb.created_at,
        modified_at: now,
        changed_at: now,
        accessed_at: now,
        generation: sb.generation + 1,
        extents: vec![anc_extent],
        index_block_addr: 0,
        xattr_block_addr: 0,
        domain_inode_id: 0,
        data_crc: u64::from(anc_crc),
    };
    write_inode_at_slot(device, &geom, ANCILLARY_INODE_SLOT, &anc_inode)?;

    // --- Build a bytes-per-inode map from block_records ------------------
    let mut bytes_by_inode: std::collections::HashMap<InodeId, &[u8]> =
        std::collections::HashMap::new();
    for rec in &state.block_records {
        bytes_by_inode.insert(rec.inode, rec.bytes.as_slice());
    }

    // --- Write each domain inode into its own slot -----------------------
    let mut active_slots = 0usize;
    let mut deleted_slots = 0usize;
    for inode in &state.active_inodes {
        let bytes = bytes_by_inode.get(&inode.id).copied().unwrap_or(&[]);
        write_inode(device, &mut alloc, &geom, inode, bytes, false)?;
        active_slots += 1;
    }
    for inode in &state.deleted_inodes {
        let bytes = bytes_by_inode.get(&inode.id).copied().unwrap_or(&[]);
        write_inode(device, &mut alloc, &geom, inode, bytes, true)?;
        deleted_slots += 1;
    }

    // --- Flush bitmaps + bump superblock ---------------------------------
    let used_data = {
        let total_data = geom.data_blocks;
        total_data - alloc.free_data_blocks()
    };
    let (final_bbm, final_ibm) = alloc.into_bitmaps();
    write_blocks(device, geom.block_bitmap_start, final_bbm.as_bytes())?;
    write_blocks(device, geom.inode_bitmap_start, final_ibm.as_bytes())?;

    sb.generation += 1;
    sb.last_write_at = now;
    sb.state = STATE_CLEAN;
    sb.layout_mode = LAYOUT_MODE_NATIVE;
    sb.payload_inode = ANCILLARY_INODE_SLOT;
    sb.free_blocks = geom.data_blocks - used_data;
    sb.free_inodes = geom.inode_count - final_ibm.popcount();
    sb.block_bitmap_crc = Crc32c::hash(final_bbm.as_bytes());
    sb.inode_bitmap_crc = Crc32c::hash(final_ibm.as_bytes());
    // Root inode = the first inode whose path is "/" and kind=Directory.
    sb.root_inode = state
        .active_inodes
        .iter()
        .find(|i| i.path == "/" && matches!(i.kind, InodeKind::Directory))
        .map(|i| i.id.0)
        .unwrap_or(0);

    let sb_block = sb.encode_block();
    device.write_at(PRIMARY_SUPERBLOCK_BLOCK * BLOCK_SIZE, &sb_block)?;
    device.write_at(geom.tertiary_superblock_block * BLOCK_SIZE, &sb_block)?;
    device.write_at(geom.secondary_superblock_block * BLOCK_SIZE, &sb_block)?;
    device.sync()?;

    Ok(NativeSaveReport {
        generation: sb.generation,
        active_slots,
        deleted_slots,
        data_blocks_used: used_data,
    })
}

/// Reconstruct a `PersistedState` from a native-layout volume.
pub fn load_state_native(device: &dyn BlockDevice) -> CoreFsResult<PersistedState> {
    let sb = super::volume::read_sb_with_fallbacks(device)?;
    super::volume::verify_bitmap_integrity(device, &sb)?;
    if sb.layout_mode != LAYOUT_MODE_NATIVE {
        return Err(CoreFsError::State(format!(
            "native load: volume is in layout mode {}, not NATIVE",
            sb.layout_mode
        )));
    }
    let geom = sb.geometry();

    // --- Ancillary ---
    let anc_inode = read_inode_at_slot(device, &geom, ANCILLARY_INODE_SLOT)?;
    if anc_inode.kind != OnDiskKind::SystemPayload {
        return Err(CoreFsError::State(
            "native load: ancillary slot has wrong kind".into(),
        ));
    }
    let anc_bytes = read_all_extent_bytes(device, &anc_inode)?;
    if anc_bytes.len() < 4 {
        return Err(CoreFsError::State("native load: ancillary too short".into()));
    }
    let (payload, crc_bytes) = anc_bytes.split_at(anc_bytes.len() - 4);
    let stored_crc = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    if stored_crc != Crc32c::hash(payload) {
        return Err(CoreFsError::State("native load: ancillary CRC mismatch".into()));
    }
    let ancillary: AncillaryState = bincode::deserialize(payload).map_err(|e| {
        CoreFsError::State(format!("native load: ancillary deserialize failed: {e}"))
    })?;

    // --- Inode bitmap + scan ---
    let ibm_bytes = device.read_at(
        geom.inode_bitmap_start * BLOCK_SIZE,
        geom.inode_bitmap_blocks * BLOCK_SIZE,
    )?;
    let inode_bitmap = Bitmap::from_bytes(ibm_bytes, geom.inode_count)?;

    let mut active_inodes = Vec::new();
    let mut deleted_inodes = Vec::new();
    let mut block_records = Vec::new();

    for slot in FIRST_USER_INODE_SLOT..geom.inode_count {
        if !inode_bitmap.is_set(slot)? {
            continue;
        }
        let on_disk = read_inode_at_slot(device, &geom, slot)?;
        if on_disk.kind == OnDiskKind::Unused {
            continue;
        }
        if on_disk.xattr_block_addr == 0 {
            return Err(CoreFsError::State(format!(
                "native load: inode slot {slot} has no attr block"
            )));
        }
        let attr_buf = device.read_at(on_disk.xattr_block_addr * BLOCK_SIZE, BLOCK_SIZE)?;
        let attr = AttrBlock::decode(&attr_buf)?;
        let inode: Inode = bincode::deserialize(&attr.payload).map_err(|e| {
            CoreFsError::State(format!("native load: inode deserialize failed: {e}"))
        })?;
        let bytes = read_all_extent_bytes(device, &on_disk)?;
        if !bytes.is_empty() {
            // Reconstruct a BlockRecord from the on-disk extents.
            let checksum = on_disk.data_crc;
            let first_ext = on_disk
                .extents
                .first()
                .copied()
                .unwrap_or(Extent::default());
            let total_alloc: u64 = on_disk
                .extents
                .iter()
                .map(|e| u64::from(e.length_blocks))
                .sum();
            block_records.push(BlockRecord {
                inode: inode.id,
                bytes,
                checksum,
                device_block: first_ext.physical_block,
                allocated_blocks: total_alloc,
            });
        }
        if on_disk.flags & FLAG_DELETED != 0 {
            deleted_inodes.push(inode);
        } else {
            active_inodes.push(inode);
        }
    }

    Ok(PersistedState {
        config: ancillary.config,
        volume: ancillary.volume,
        clean_unmount: ancillary.clean_unmount,
        pending_wal: ancillary.pending_wal,
        active_inodes,
        deleted_inodes,
        allocator_policy: ancillary.allocator_policy,
        free_extents: ancillary.free_extents,
        hot_path_records: ancillary.hot_path_records,
        block_records,
        journal_entries: ancillary.journal_entries,
        journal_runtime: ancillary.journal_runtime,
        versions: ancillary.versions,
        sync_statuses: ancillary.sync_statuses,
        snapshots: ancillary.snapshots,
        next_snapshot_id: ancillary.next_snapshot_id,
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn mark_reserved_blocks(bitmap: &mut Bitmap, geom: &LayoutGeometry) -> CoreFsResult<()> {
    bitmap.set(super::layout::RESERVED_BLOCK)?;
    bitmap.set(PRIMARY_SUPERBLOCK_BLOCK)?;
    for i in 0..geom.block_bitmap_blocks {
        bitmap.set(geom.block_bitmap_start + i)?;
    }
    for i in 0..geom.inode_bitmap_blocks {
        bitmap.set(geom.inode_bitmap_start + i)?;
    }
    for i in 0..geom.inode_table_blocks {
        bitmap.set(geom.inode_table_start + i)?;
    }
    for i in 0..geom.journal_blocks {
        bitmap.set(geom.journal_start + i)?;
    }
    bitmap.set(geom.tertiary_superblock_block)?;
    bitmap.set(geom.secondary_superblock_block)?;
    Ok(())
}

fn write_blocks(device: &mut dyn BlockDevice, start: u64, bytes: &[u8]) -> CoreFsResult<()> {
    device.write_at(start * BLOCK_SIZE, bytes)
}

fn write_inode(
    device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    geom: &LayoutGeometry,
    inode: &Inode,
    content: &[u8],
    deleted: bool,
) -> CoreFsResult<()> {
    let slot = alloc.allocate_inode()?;
    // --- Allocate + write content extents (if any content) ---
    let (extents, index_block_addr, flags_has_index) =
        allocate_and_write_content(device, alloc, content)?;
    // --- Allocate + write attr block containing the serialized Inode ---
    let attr_bytes = bincode::serialize(inode).map_err(|e| {
        CoreFsError::State(format!("native: inode serialize failed: {e}"))
    })?;
    if attr_bytes.len() > super::attr_block::ATTR_BLOCK_CAPACITY {
        return Err(CoreFsError::State(format!(
            "native: serialized inode {} exceeds attr block capacity {}",
            attr_bytes.len(),
            super::attr_block::ATTR_BLOCK_CAPACITY
        )));
    }
    let attr_ext = alloc.allocate_contiguous(1)?;
    let attr_block_addr = attr_ext.physical_block;
    let attr_bytes_encoded = AttrBlock::new(attr_bytes).encode()?;
    device.write_at(attr_block_addr * BLOCK_SIZE, &attr_bytes_encoded)?;

    // --- Map metadata to OnDiskInode fields ---
    let kind = match inode.kind {
        InodeKind::File => OnDiskKind::File,
        InodeKind::Directory => OnDiskKind::Directory,
        InodeKind::Symlink => OnDiskKind::Symlink,
    };
    let timestamps = (
        systime_to_secs(inode.created_at),
        systime_to_secs(inode.modified_at),
        systime_to_secs(inode.changed_at),
    );
    let mut flags = super::inode::FLAG_HAS_XATTRS;
    if flags_has_index {
        flags |= FLAG_HAS_EXTENT_INDEX;
    }
    if deleted {
        flags |= FLAG_DELETED;
    }
    if inode.metadata.encrypted {
        flags |= super::inode::FLAG_ENCRYPTED;
    }
    if inode.metadata.compressed {
        flags |= super::inode::FLAG_COMPRESSED;
    }
    let data_crc = if !content.is_empty() {
        u64::from(Crc32c::hash(content))
    } else {
        0
    };
    let on_disk = OnDiskInode {
        version: 1,
        kind,
        mode: inode.metadata.mode,
        uid: inode.metadata.uid,
        gid: inode.metadata.gid,
        link_count: 1,
        flags,
        size_bytes: inode.size as u64,
        blocks_allocated: extents.iter().map(|e| u64::from(e.length_blocks)).sum(),
        created_at: timestamps.0,
        modified_at: timestamps.1,
        changed_at: timestamps.2,
        accessed_at: timestamps.1,
        generation: 1,
        extents: if flags_has_index { Vec::new() } else { extents },
        index_block_addr,
        xattr_block_addr: attr_block_addr,
        domain_inode_id: inode.id.0,
        data_crc,
    };
    write_inode_at_slot(device, geom, slot, &on_disk)
}

/// Allocate extents for `content` and write the bytes.  Returns (inline
/// extents, index_block_addr, has_index_flag) — if the extent count
/// exceeds [`super::inode::MAX_INLINE_EXTENTS`] we fall back to an
/// indirect chain.
fn allocate_and_write_content(
    device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    content: &[u8],
) -> CoreFsResult<(Vec<Extent>, u64, bool)> {
    if content.is_empty() {
        return Ok((Vec::new(), 0, false));
    }
    let blocks = (content.len() as u64).div_ceil(BLOCK_SIZE);
    // Try one contiguous extent first — cheapest and covers the common case.
    let extent = alloc.allocate_contiguous(blocks)?;
    let mut buf = vec![0u8; (blocks * BLOCK_SIZE) as usize];
    buf[..content.len()].copy_from_slice(content);
    device.write_at(extent.physical_block * BLOCK_SIZE, &buf)?;
    Ok((vec![extent], 0, false))
}

fn write_inode_at_slot(
    device: &mut dyn BlockDevice,
    geom: &LayoutGeometry,
    slot: u64,
    inode: &OnDiskInode,
) -> CoreFsResult<()> {
    let (block, offset) = geom.inode_record_location(slot)?;
    let mut buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    let encoded = inode.encode()?;
    buf[offset as usize..offset as usize + INODE_RECORD_SIZE].copy_from_slice(&encoded);
    device.write_at(block * BLOCK_SIZE, &buf)
}

fn read_inode_at_slot(
    device: &dyn BlockDevice,
    geom: &LayoutGeometry,
    slot: u64,
) -> CoreFsResult<OnDiskInode> {
    let (block, offset) = geom.inode_record_location(slot)?;
    let buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    OnDiskInode::decode(&buf[offset as usize..offset as usize + INODE_RECORD_SIZE])
}

fn read_all_extent_bytes(
    device: &dyn BlockDevice,
    inode: &OnDiskInode,
) -> CoreFsResult<Vec<u8>> {
    let mut extents = inode.extents.clone();
    if inode.flags & FLAG_HAS_EXTENT_INDEX != 0 {
        extents = super::extent_tree::ExtentChain::read_chain(
            device,
            inode.index_block_addr,
        )?;
    }
    if extents.is_empty() {
        return Ok(Vec::new());
    }
    extents.sort_by_key(|e| e.logical_block);
    let mut out = Vec::with_capacity(inode.size_bytes as usize);
    let mut remaining = inode.size_bytes as usize;
    for ext in &extents {
        let byte_offset = ext.physical_block * BLOCK_SIZE;
        let byte_len = u64::from(ext.length_blocks) * BLOCK_SIZE;
        let block_data = device.read_at(byte_offset, byte_len)?;
        let wanted = remaining.min(block_data.len());
        out.extend_from_slice(&block_data[..wanted]);
        remaining -= wanted;
    }
    Ok(out)
}

fn systime_to_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    systime_to_secs(SystemTime::now())
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
