// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Native per-inode ODF layout.
//!
//! The [blob layout](super::volume::save_state) stores the entire
//! [`PersistedState`] as a single bincode blob pinned to system inode #0.
//! The native layout instead gives every domain [`Inode`] its own on-disk
//! inode slot, allocates real extents for the file content and places the
//! inode's per-inode metadata (path, [`crate::domain::metadata::FileMetadata`], ACLs, tags, etc.)
//! in a sibling [`super::attr_block::AttrBlock`].
//!
//! This enables:
//!
//! * per-inode `fsck` walks (see [`super::fsck`]),
//! * per-inode encryption / compression / xattr flags ([`super::inode::FLAG_ENCRYPTED`],
//!   [`super::inode::FLAG_COMPRESSED`], [`super::inode::FLAG_HAS_XATTRS`]),
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
//! [`super::inode::FLAG_HAS_EXTENT_INDEX`] / [`super::inode::FLAG_ENCRYPTED`] / [`super::inode::FLAG_COMPRESSED`] /
//! [`super::inode::FLAG_HAS_XATTRS`] bits plus the native-specific
//! [`FLAG_DELETED`] below which marks an on-disk inode as logically
//! removed but retained for recovery.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::Timestamp;
use crate::storage::block_device::BlockDevice;
use crate::storage::block_store::BlockRecord;
use crate::storage::persisted_state::PersistedState;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use super::allocator::{AllocationStrategy, OndiskAllocator};
use super::attr_block::AttrBlock;
use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::inode::{Extent, FLAG_HAS_EXTENT_INDEX, INODE_RECORD_SIZE, OnDiskInode, OnDiskKind};
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

    // Mark every block currently claimed by a `BlockStore` data extent as
    // reserved, so the per-inode Attr-Block/Index-Block allocator below
    // doesn't hand those blocks back out and overwrite file contents.
    //
    // Without this, `mkfs-corefs-host --populate` (which writes file
    // bytes via `BlockStore::write_at` first, then calls
    // `save_state_native` to persist the resulting records) ends up with
    // Attr-Blocks being allocated right on top of the data extents — the
    // file reads then surface `ATTR_BLOCK_MAGIC` (0xA771_B10C) instead
    // of the original bytes.  See
    // anyOS memory/corefs-blockstore-attr-collision.md for the repro.
    for rec in &state.block_records {
        for ext in &rec.extents {
            for i in 0..u64::from(ext.length_blocks) {
                let block = ext.physical_block + i;
                if block < geom.total_blocks {
                    // `set` returns Ok(true) if the bit was already set.
                    // We treat "already set" as fine (reserved regions
                    // and dedup'd extents can legitimately overlap).
                    let _ = block_bitmap.set(block)?;
                }
            }
        }
    }

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
    let anc_bytes = crate::bincode_compat::serialize(&ancillary)
        .map_err(|e| CoreFsError::State(format!("native: ancillary serialize failed: {e}")))?;
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

    // --- Build a record-per-inode map from block_records -----------------
    let mut records_by_inode: HashMap<InodeId, &crate::storage::block_store::BlockRecord> =
        HashMap::new();
    for rec in &state.block_records {
        records_by_inode.insert(rec.inode, rec);
    }

    // --- Write each domain inode into its own slot -----------------------
    let mut active_slots = 0usize;
    let mut deleted_slots = 0usize;
    for inode in &state.active_inodes {
        let rec = records_by_inode.get(&inode.id).copied();
        write_inode_from_record(device, &mut alloc, &geom, inode, rec, false)?;
        active_slots += 1;
    }
    for inode in &state.deleted_inodes {
        let rec = records_by_inode.get(&inode.id).copied();
        write_inode_from_record(device, &mut alloc, &geom, inode, rec, true)?;
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
        return Err(CoreFsError::State(
            "native load: ancillary too short".into(),
        ));
    }
    let (payload, crc_bytes) = anc_bytes.split_at(anc_bytes.len() - 4);
    let stored_crc = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    if stored_crc != Crc32c::hash(payload) {
        return Err(CoreFsError::State(
            "native load: ancillary CRC mismatch".into(),
        ));
    }
    let ancillary: AncillaryState = crate::bincode_compat::deserialize(payload).map_err(|e| {
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
        let inode: Inode = crate::bincode_compat::deserialize(&attr.payload).map_err(|e| {
            CoreFsError::State(format!("native load: inode deserialize failed: {e}"))
        })?;
        // Reconstruct a BlockRecord (metadata-only) from on-disk extents.
        // Data remains on the device — no bytes loaded into RAM.
        if on_disk.size_bytes > 0 || !on_disk.extents.is_empty() {
            use crate::storage::block_store::{ExtentRef, EXTENT_COMPRESSED, EXTENT_ENCRYPTED};
            let extent_refs: Vec<ExtentRef> = on_disk
                .extents
                .iter()
                .map(|e| ExtentRef {
                    logical_block: e.logical_block,
                    logical_len: e.length_blocks.saturating_mul(BLOCK_SIZE as u32),
                    physical_block: e.physical_block,
                    length_blocks: e.length_blocks,
                    physical_len: e.length_blocks.saturating_mul(BLOCK_SIZE as u32),
                    content_crc: on_disk.data_crc as u32,
                    flags: 0,
                })
                .collect();
            let mut flags = 0u32;
            if on_disk.flags & super::inode::FLAG_COMPRESSED != 0 {
                flags |= EXTENT_COMPRESSED;
            }
            if on_disk.flags & super::inode::FLAG_ENCRYPTED != 0 {
                flags |= EXTENT_ENCRYPTED;
            }
            block_records.push(BlockRecord {
                inode: inode.id,
                logical_size: on_disk.size_bytes,
                extents: extent_refs,
                content_crc: on_disk.data_crc as u32,
                flags,
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
// Incremental save (P2.10)
// ---------------------------------------------------------------------------

/// Per-inode change classification produced by [`save_state_native_incremental`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IncrementalSaveReport {
    pub generation: u64,
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub fell_back_to_full_save: bool,
}

/// Persist `state` by rewriting only the inode slots whose content or
/// deletion status has changed since the last save.
///
/// Falls back to a full [`save_state_native`] when the volume isn't yet
/// in native layout.  In native mode the incremental path:
///
/// 1. Reads the existing inode bitmap and walks each allocated user slot
///    to capture (`domain_inode_id`, `data_crc`, `FLAG_DELETED`,
///    extents, attr block).
/// 2. Compares against the desired set built from `state.active_inodes`
///    + `state.deleted_inodes`.
/// 3. For every inode classified as **created**, allocates a slot and
///    writes content extents + attr block.  For **updated** inodes, the
///    old extents and attr block are returned to the allocator and a
///    fresh allocation is performed (in-place at the same slot).  For
///    **removed** inodes the extents, attr block and slot are freed.
///    **Unchanged** inodes are not touched at all.
/// 4. Always rewrites the ancillary slot (#1) and bumps the superblock
///    generation + bitmap CRCs.
pub fn save_state_native_incremental(
    device: &mut dyn BlockDevice,
    state: &PersistedState,
) -> CoreFsResult<IncrementalSaveReport> {
    let sb = super::volume::read_sb_with_fallbacks(device)?;
    if sb.layout_mode != LAYOUT_MODE_NATIVE {
        let r = save_state_native(device, state)?;
        return Ok(IncrementalSaveReport {
            generation: r.generation,
            created: r.active_slots + r.deleted_slots,
            updated: 0,
            removed: 0,
            unchanged: 0,
            fell_back_to_full_save: true,
        });
    }
    let geom = sb.geometry();

    // Capture the on-disk state per allocated slot.
    let ibm_bytes = device.read_at(
        geom.inode_bitmap_start * BLOCK_SIZE,
        geom.inode_bitmap_blocks * BLOCK_SIZE,
    )?;
    let ibm = Bitmap::from_bytes(ibm_bytes, geom.inode_count)?;
    let bbm_bytes = device.read_at(
        geom.block_bitmap_start * BLOCK_SIZE,
        geom.block_bitmap_blocks * BLOCK_SIZE,
    )?;
    let bbm = Bitmap::from_bytes(bbm_bytes, geom.total_blocks)?;

    #[derive(Debug, Clone)]
    struct ExistingSlot {
        slot: u64,
        on_disk: OnDiskInode,
    }
    let mut existing: HashMap<u64, ExistingSlot> =
        HashMap::new();
    for slot in FIRST_USER_INODE_SLOT..geom.inode_count {
        if !ibm.is_set(slot)? {
            continue;
        }
        let on_disk = read_inode_at_slot(device, &geom, slot)?;
        if on_disk.kind == OnDiskKind::Unused {
            continue;
        }
        existing.insert(on_disk.domain_inode_id, ExistingSlot { slot, on_disk });
    }

    // Build a record-per-inode map (new extent-based BlockRecord).
    let mut records_by_inode: HashMap<InodeId, &crate::storage::block_store::BlockRecord> =
        HashMap::new();
    for rec in &state.block_records {
        records_by_inode.insert(rec.inode, rec);
    }

    // Build the desired set with computed CRCs.
    #[derive(Clone)]
    struct DesiredSlot<'a> {
        inode: &'a Inode,
        rec: Option<&'a crate::storage::block_store::BlockRecord>,
        deleted: bool,
        crc: u64,
    }
    let mut desired: HashMap<u64, DesiredSlot<'_>> = HashMap::new();
    fn build_desired<'a>(
        inode: &'a Inode,
        deleted: bool,
        records_by_inode: &HashMap<InodeId, &'a crate::storage::block_store::BlockRecord>,
    ) -> DesiredSlot<'a> {
        let rec = records_by_inode.get(&inode.id).copied();
        let crc = rec.map(|r| u64::from(r.content_crc)).unwrap_or(0);
        DesiredSlot {
            inode,
            rec,
            deleted,
            crc,
        }
    }
    for inode in &state.active_inodes {
        desired.insert(inode.id.0, build_desired(inode, false, &records_by_inode));
    }
    for inode in &state.deleted_inodes {
        desired.insert(inode.id.0, build_desired(inode, true, &records_by_inode));
    }

    let mut alloc = OndiskAllocator::new(
        &geom,
        bbm,
        ibm,
        AllocationStrategy::FirstFit,
        FIRST_USER_INODE_SLOT,
    );

    let mut report = IncrementalSaveReport::default();

    // Pass 1: removals.  Frees blocks first so updates / creates can reuse.
    for (id, slot) in &existing {
        if !desired.contains_key(id) {
            release_slot(device, &mut alloc, &geom, slot.slot, &slot.on_disk)?;
            report.removed += 1;
        }
    }

    // Pass 2: updates + creates.
    for (id, want) in &desired {
        if let Some(cur) = existing.get(id) {
            let same_crc = cur.on_disk.data_crc == want.crc;
            let same_deleted = (cur.on_disk.flags & FLAG_DELETED != 0) == want.deleted;
            if same_crc && same_deleted {
                // Still need to mark existing data blocks as used in the allocator
                // so they don't get overwritten by other allocations.
                for ext in &cur.on_disk.extents {
                    for b in 0..u64::from(ext.length_blocks) {
                        let _ = alloc.mark_used(ext.physical_block + b);
                    }
                }
                report.unchanged += 1;
                continue;
            }
            // UPDATE: free old extents + attr block, rewrite at the same slot.
            free_inode_payload(&mut alloc, &cur.on_disk)?;
            write_inode_record_from_record_at(
                device,
                &mut alloc,
                &geom,
                cur.slot,
                want.inode,
                want.rec,
                want.deleted,
            )?;
            report.updated += 1;
        } else {
            // CREATE: take a fresh slot.
            write_inode_from_record(
                device,
                &mut alloc,
                &geom,
                want.inode,
                want.rec,
                want.deleted,
            )?;
            report.created += 1;
        }
    }

    // Always rewrite ancillary slot (it's tiny relative to data).
    rewrite_ancillary(device, &mut alloc, &geom, state, sb.created_at)?;

    // Flush bitmaps + superblock.
    let used_data = geom.data_blocks - alloc.free_data_blocks();
    let (final_bbm, final_ibm) = alloc.into_bitmaps();
    write_blocks(device, geom.block_bitmap_start, final_bbm.as_bytes())?;
    write_blocks(device, geom.inode_bitmap_start, final_ibm.as_bytes())?;

    let mut sb = sb;
    sb.generation += 1;
    sb.last_write_at = now_secs();
    sb.state = STATE_CLEAN;
    sb.layout_mode = LAYOUT_MODE_NATIVE;
    sb.payload_inode = ANCILLARY_INODE_SLOT;
    sb.free_blocks = geom.data_blocks - used_data;
    sb.free_inodes = geom.inode_count - final_ibm.popcount();
    sb.block_bitmap_crc = Crc32c::hash(final_bbm.as_bytes());
    sb.inode_bitmap_crc = Crc32c::hash(final_ibm.as_bytes());
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

    report.generation = sb.generation;
    Ok(report)
}

fn free_inode_payload(alloc: &mut OndiskAllocator, on_disk: &OnDiskInode) -> CoreFsResult<()> {
    for ext in &on_disk.extents {
        if ext.length_blocks > 0 {
            alloc.free_extent(*ext)?;
        }
    }
    if on_disk.xattr_block_addr != 0 {
        alloc.free_extent(Extent {
            logical_block: 0,
            length_blocks: 1,
            physical_block: on_disk.xattr_block_addr,
        })?;
    }
    Ok(())
}

fn release_slot(
    _device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    _geom: &LayoutGeometry,
    slot: u64,
    on_disk: &OnDiskInode,
) -> CoreFsResult<()> {
    free_inode_payload(alloc, on_disk)?;
    alloc.free_inode(slot)?;
    // Note: we deliberately don't zero the inode record on disk — the
    // bitmap is the source of truth, and a future allocate will overwrite
    // the slot.
    Ok(())
}

fn rewrite_ancillary(
    device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    geom: &LayoutGeometry,
    state: &PersistedState,
    created_at: i64,
) -> CoreFsResult<()> {
    // Free the previous ancillary's extents (read it first).
    if let Ok(prev) = read_inode_at_slot(device, geom, ANCILLARY_INODE_SLOT) {
        if prev.kind == OnDiskKind::SystemPayload {
            for ext in &prev.extents {
                if ext.length_blocks > 0 {
                    let _ = alloc.free_extent(*ext);
                }
            }
        }
    }
    let ancillary = AncillaryState::from(state);
    let anc_bytes = crate::bincode_compat::serialize(&ancillary)
        .map_err(|e| CoreFsError::State(format!("native: ancillary serialize failed: {e}")))?;
    let anc_crc = Crc32c::hash(&anc_bytes);
    let mut payload = anc_bytes;
    payload.extend_from_slice(&anc_crc.to_le_bytes());
    let blocks = (payload.len() as u64).div_ceil(BLOCK_SIZE);
    let extent = alloc.allocate_contiguous(blocks)?;
    let mut buf = vec![0u8; (blocks * BLOCK_SIZE) as usize];
    buf[..payload.len()].copy_from_slice(&payload);
    device.write_at(extent.physical_block * BLOCK_SIZE, &buf)?;
    let now = now_secs();
    let inode = OnDiskInode {
        version: 1,
        kind: OnDiskKind::SystemPayload,
        mode: 0o600,
        uid: 0,
        gid: 0,
        link_count: 1,
        flags: 0,
        size_bytes: payload.len() as u64,
        blocks_allocated: blocks,
        created_at,
        modified_at: now,
        changed_at: now,
        accessed_at: now,
        generation: 1,
        extents: vec![extent],
        index_block_addr: 0,
        xattr_block_addr: 0,
        domain_inode_id: 0,
        data_crc: u64::from(anc_crc),
    };
    write_inode_at_slot(device, geom, ANCILLARY_INODE_SLOT, &inode)
}

/// Variant of [`write_inode`] that targets a pre-determined slot
/// (used by the incremental updater to keep the slot-id stable).
fn write_inode_record_at(
    device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    geom: &LayoutGeometry,
    slot: u64,
    inode: &Inode,
    content: &[u8],
    deleted: bool,
) -> CoreFsResult<()> {
    let (extents, index_block_addr, flags_has_index) =
        allocate_and_write_content(device, alloc, content)?;
    let attr_bytes = crate::bincode_compat::serialize(inode)
        .map_err(|e| CoreFsError::State(format!("native: inode serialize failed: {e}")))?;
    if attr_bytes.len() > super::attr_block::ATTR_BLOCK_CAPACITY {
        return Err(CoreFsError::State(format!(
            "native: serialized inode {} exceeds attr block capacity {}",
            attr_bytes.len(),
            super::attr_block::ATTR_BLOCK_CAPACITY
        )));
    }
    let attr_ext = alloc.allocate_contiguous(1)?;
    let attr_block_addr = attr_ext.physical_block;
    let attr_block = AttrBlock::new(attr_bytes).encode()?;
    device.write_at(attr_block_addr * BLOCK_SIZE, &attr_block)?;

    let kind = match inode.kind {
        InodeKind::File => OnDiskKind::File,
        InodeKind::Directory => OnDiskKind::Directory,
        InodeKind::Symlink => OnDiskKind::Symlink,
    };
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
        size_bytes: content.len() as u64,
        blocks_allocated: extents.iter().map(|e| u64::from(e.length_blocks)).sum(),
        created_at: systime_to_secs(inode.created_at),
        modified_at: systime_to_secs(inode.modified_at),
        changed_at: systime_to_secs(inode.changed_at),
        accessed_at: systime_to_secs(inode.accessed_at),
        generation: 1,
        extents: if flags_has_index { Vec::new() } else { extents },
        index_block_addr,
        xattr_block_addr: attr_block_addr,
        domain_inode_id: inode.id.0,
        data_crc,
    };
    write_inode_at_slot(device, geom, slot, &on_disk)
}

/// Extent-aware variant of [`write_inode_record_at`] for the incremental
/// save path.  Data is already on the device.
fn write_inode_record_from_record_at(
    device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    geom: &LayoutGeometry,
    slot: u64,
    inode: &Inode,
    rec: Option<&crate::storage::block_store::BlockRecord>,
    deleted: bool,
) -> CoreFsResult<()> {
    let (on_disk_extents, _index_block_addr, size_bytes, data_crc, blocks_alloc) =
        if let Some(rec) = rec {
            if rec.extents.is_empty() {
                (Vec::new(), 0u64, rec.logical_size, rec.content_crc as u64, 0u64)
            } else {
                let mut on_disk_extents: Vec<Extent> = Vec::new();
                let mut total_blocks = 0u64;
                for ext in &rec.extents {
                    let e = Extent {
                        logical_block: ext.logical_block,
                        length_blocks: ext.length_blocks,
                        physical_block: ext.physical_block,
                    };
                    for b in 0..u64::from(ext.length_blocks) {
                        let _ = alloc.mark_used(ext.physical_block + b);
                    }
                    total_blocks += u64::from(ext.length_blocks);
                    on_disk_extents.push(e);
                }
                if on_disk_extents.len() > super::inode::MAX_INLINE_EXTENTS {
                    on_disk_extents.truncate(super::inode::MAX_INLINE_EXTENTS);
                }
                (on_disk_extents, 0u64, rec.logical_size, rec.content_crc as u64, total_blocks)
            }
        } else {
            (Vec::new(), 0u64, 0u64, 0u64, 0u64)
        };

    let attr_bytes = crate::bincode_compat::serialize(inode)
        .map_err(|e| CoreFsError::State(format!("native: inode serialize failed: {e}")))?;
    if attr_bytes.len() > super::attr_block::ATTR_BLOCK_CAPACITY {
        return Err(CoreFsError::State(format!(
            "native: serialized inode {} exceeds attr block capacity {}",
            attr_bytes.len(),
            super::attr_block::ATTR_BLOCK_CAPACITY
        )));
    }
    let attr_ext = alloc.allocate_contiguous(1)?;
    let attr_block_addr = attr_ext.physical_block;
    let attr_block = AttrBlock::new(attr_bytes).encode()?;
    device.write_at(attr_block_addr * BLOCK_SIZE, &attr_block)?;

    let kind = match inode.kind {
        InodeKind::File => OnDiskKind::File,
        InodeKind::Directory => OnDiskKind::Directory,
        InodeKind::Symlink => OnDiskKind::Symlink,
    };
    let mut flags = super::inode::FLAG_HAS_XATTRS;
    if deleted {
        flags |= FLAG_DELETED;
    }
    if inode.metadata.encrypted {
        flags |= super::inode::FLAG_ENCRYPTED;
    }
    if inode.metadata.compressed {
        flags |= super::inode::FLAG_COMPRESSED;
    }
    let on_disk = OnDiskInode {
        version: 1,
        kind,
        mode: inode.metadata.mode,
        uid: inode.metadata.uid,
        gid: inode.metadata.gid,
        link_count: 1,
        flags,
        size_bytes,
        blocks_allocated: blocks_alloc,
        created_at: systime_to_secs(inode.created_at),
        modified_at: systime_to_secs(inode.modified_at),
        changed_at: systime_to_secs(inode.changed_at),
        accessed_at: systime_to_secs(inode.accessed_at),
        generation: 1,
        extents: on_disk_extents,
        index_block_addr: 0,
        xattr_block_addr: attr_block_addr,
        domain_inode_id: inode.id.0,
        data_crc,
    };
    write_inode_at_slot(device, geom, slot, &on_disk)
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
    let attr_bytes = crate::bincode_compat::serialize(inode)
        .map_err(|e| CoreFsError::State(format!("native: inode serialize failed: {e}")))?;
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
        size_bytes: content.len() as u64,
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

/// New-style variant of [`write_inode`] that works from a [`BlockRecord`]
/// (metadata-only — no bytes in RAM).  Data is already on device.
///
/// If `rec` is `Some`, its extents are already on the device and are used
/// directly (no extra allocation / write needed).  The OndiskAllocator
/// bitmap is updated to mark the corresponding blocks as used.
/// If `rec` is `None`, the inode is written with no data extents.
fn write_inode_from_record(
    device: &mut dyn BlockDevice,
    alloc: &mut OndiskAllocator,
    geom: &LayoutGeometry,
    inode: &Inode,
    rec: Option<&crate::storage::block_store::BlockRecord>,
    deleted: bool,
) -> CoreFsResult<()> {
    // If the record has populated extents (data already on device), use them.
    // Otherwise fall back to allocate_and_write_content with empty content.
    let (on_disk_extents, index_block_addr, flags_has_index, size_bytes, data_crc, blocks_alloc) =
        if let Some(rec) = rec {
            if rec.extents.is_empty() {
                (Vec::new(), 0u64, false, rec.logical_size, rec.content_crc as u64, 0u64)
            } else {
                // Map ExtentRef → Extent and mark blocks used in the allocator.
                let mut on_disk_extents: Vec<Extent> = Vec::new();
                let mut total_blocks = 0u64;
                for ext in &rec.extents {
                    let e = Extent {
                        logical_block: ext.logical_block,
                        length_blocks: ext.length_blocks,
                        physical_block: ext.physical_block,
                    };
                    // Mark these blocks as used so OndiskAllocator doesn't overwrite them.
                    for b in 0..u64::from(ext.length_blocks) {
                        let _ = alloc.mark_used(ext.physical_block + b);
                    }
                    total_blocks += u64::from(ext.length_blocks);
                    on_disk_extents.push(e);
                }
                if on_disk_extents.len() > super::inode::MAX_INLINE_EXTENTS {
                    on_disk_extents.truncate(super::inode::MAX_INLINE_EXTENTS);
                }
                (on_disk_extents, 0u64, false, rec.logical_size, rec.content_crc as u64, total_blocks)
            }
        } else {
            (Vec::new(), 0u64, false, 0u64, 0u64, 0u64)
        };

    let slot = alloc.allocate_inode()?;

    // Allocate + write attr block containing the serialized Inode.
    let attr_bytes = crate::bincode_compat::serialize(inode)
        .map_err(|e| CoreFsError::State(format!("native: inode serialize failed: {e}")))?;
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

    let kind = match inode.kind {
        InodeKind::File => OnDiskKind::File,
        InodeKind::Directory => OnDiskKind::Directory,
        InodeKind::Symlink => OnDiskKind::Symlink,
    };
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
    let on_disk = OnDiskInode {
        version: 1,
        kind,
        mode: inode.metadata.mode,
        uid: inode.metadata.uid,
        gid: inode.metadata.gid,
        link_count: 1,
        flags,
        size_bytes,
        blocks_allocated: blocks_alloc,
        created_at: systime_to_secs(inode.created_at),
        modified_at: systime_to_secs(inode.modified_at),
        changed_at: systime_to_secs(inode.changed_at),
        accessed_at: systime_to_secs(inode.accessed_at),
        generation: 1,
        extents: if flags_has_index { Vec::new() } else { on_disk_extents },
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

/// Public variant of the internal `read_all_extent_bytes` helper,
/// used by the grouped layout.  Reads every extent of an inode (or
/// walks its indirect extent chain) and concatenates the bytes,
/// capped at `inode.size_bytes`.
pub fn read_all_extent_bytes_public(
    device: &dyn BlockDevice,
    inode: &OnDiskInode,
) -> CoreFsResult<Vec<u8>> {
    read_all_extent_bytes(device, inode)
}

/// Serialize the ancillary bincode blob (everything except per-inode
/// state) and append a CRC32C trailer.  Exposed for the grouped
/// implementation.
pub fn build_ancillary_bytes(state: &PersistedState) -> CoreFsResult<Vec<u8>> {
    let ancillary = AncillaryState::from(state);
    let anc_bytes = crate::bincode_compat::serialize(&ancillary)
        .map_err(|e| CoreFsError::State(format!("native: ancillary serialize failed: {e}")))?;
    let anc_crc = Crc32c::hash(&anc_bytes);
    let mut out = anc_bytes;
    out.extend_from_slice(&anc_crc.to_le_bytes());
    Ok(out)
}

/// Decode the ancillary payload (sans CRC trailer) back into its
/// opaque internal struct.  Exposed for the grouped implementation.
pub fn decode_ancillary(payload: &[u8]) -> CoreFsResult<AncillaryStateRef> {
    crate::bincode_compat::deserialize::<AncillaryState>(payload)
        .map_err(|e| CoreFsError::State(format!("native: ancillary deserialize failed: {e}")))
        .map(AncillaryStateRef)
}

/// Convert an ancillary struct back into a bare `PersistedState`
/// skeleton (without per-inode data).  Callers then fill in
/// `active_inodes`, `deleted_inodes`, and `block_records`.
pub fn ancillary_into_state(anc: AncillaryStateRef) -> PersistedState {
    let a = anc.0;
    PersistedState {
        config: a.config,
        volume: a.volume,
        clean_unmount: a.clean_unmount,
        pending_wal: a.pending_wal,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: a.allocator_policy,
        free_extents: a.free_extents,
        hot_path_records: a.hot_path_records,
        block_records: Vec::new(),
        journal_entries: a.journal_entries,
        journal_runtime: a.journal_runtime,
        versions: a.versions,
        sync_statuses: a.sync_statuses,
        snapshots: a.snapshots,
        next_snapshot_id: a.next_snapshot_id,
    }
}

/// Opaque wrapper exposing the internal `AncillaryState` struct to
/// grouped-layout callers without leaking its fields.
pub struct AncillaryStateRef(AncillaryState);

fn read_all_extent_bytes(device: &dyn BlockDevice, inode: &OnDiskInode) -> CoreFsResult<Vec<u8>> {
    let mut extents = inode.extents.clone();
    if inode.flags & FLAG_HAS_EXTENT_INDEX != 0 {
        extents = super::extent_tree::ExtentChain::read_chain(device, inode.index_block_addr)?;
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

fn systime_to_secs(t: Timestamp) -> i64 {
    t.as_secs() as i64
}

#[cfg(feature = "std")]
fn now_secs() -> i64 {
    systime_to_secs(Timestamp::now())
}

/// Im no_std-Build steht keine Wallclock zur Verfügung — der Treiber gibt
/// `Timestamp::EPOCH` zurück. Hostseitige Aufrufer sollten Zeitstempel
/// stattdessen explizit über die ohnehin bevorzugten `*_at(now: Timestamp)`-
/// Pfade vorgeben.
#[cfg(not(feature = "std"))]
fn now_secs() -> i64 {
    systime_to_secs(Timestamp::EPOCH)
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_characterization_tests.rs"]
mod char_tests;
