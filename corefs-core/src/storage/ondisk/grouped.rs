// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Block-group-aware volume driver (A.2 / P2.7 activated).
//!
//! Activates the [`BlockGroupTable`] + [`MultiGroupAllocator`] pair
//! introduced by [`super::block_group`] and [`super::multi_group_allocator`].  A
//! grouped volume carries the [`FEATURE_INCOMPAT_BLOCK_GROUPS`] flag in
//! its superblock; readers without support for that flag refuse to
//! open the volume per ODF's standard incompat-flag contract.
//!
//! ## Layout
//!
//! ```text
//! block 0            reserved (boot sector)
//! block 1            primary superblock
//! block 2            block-group descriptor table   ← new in grouped mode
//! block 3..          inode bitmap (global)
//! block ..           inode table
//! block ..           journal region
//! block ..           data region, partitioned into G groups
//!     group 0  →  block bitmap | data blocks
//!     group 1  →  block bitmap | data blocks
//!     …
//! ```
//!
//! The global block bitmap from single-group mode is **replaced** by
//! N per-group bitmaps.  Each group's bitmap is the first block of its
//! slice and addresses the remaining `blocks_per_group - 1` data blocks.
//! Inode allocation stays global (a single inode bitmap) because the
//! inode table is also global; the descriptors carry an
//! `inode_range_start` / `inode_range_count` pair to tell the allocator
//! which inode slots should prefer each group for locality.
//!
//! Existing single-group volumes created by [`super::volume::format_device`]
//! are untouched.  The two paths coexist via the incompat flag.

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

use super::attr_block::AttrBlock;
use super::bitmap::Bitmap;
use super::block_group::{BlockGroupDescriptor, BlockGroupTable};
use super::checksum::Crc32c;
use super::inode::{Extent, FLAG_HAS_EXTENT_INDEX, INODE_RECORD_SIZE, OnDiskInode, OnDiskKind};
use super::layout::{
    BITS_PER_BITMAP_BLOCK, BLOCK_SIZE, INODES_PER_BLOCK, LayoutGeometry, ODF_MAGIC,
    ODF_VERSION_MAJOR, ODF_VERSION_MINOR, PRIMARY_SUPERBLOCK_BLOCK, RESERVED_BLOCK,
};
use super::multi_group_allocator::MultiGroupAllocator;
use super::superblock::{LAYOUT_MODE_NATIVE, STATE_CLEAN, Superblock};

/// Incompat feature flag that announces block-group mode.
pub const FEATURE_INCOMPAT_BLOCK_GROUPS: u64 = 1 << 1;
/// Fixed block number where the [`BlockGroupTable`] lives in grouped layout.
pub const BLOCK_GROUP_TABLE_BLOCK: u64 = 2;
/// First inode slot usable for domain inodes (reserved 0..10 as per native layout).
pub const FIRST_USER_INODE_SLOT: u64 = super::native::FIRST_USER_INODE_SLOT;
/// Ancillary inode slot number (shared with the non-grouped native layout).
pub const ANCILLARY_INODE_SLOT: u64 = super::native::ANCILLARY_INODE_SLOT;

/// Grouped-format options.
#[derive(Debug, Clone)]
pub struct GroupedFormatOptions {
    pub label: String,
    pub uuid: [u8; 16],
    pub inode_count: u64,
    pub journal_blocks: u64,
    /// Number of block groups to carve out of the data region.
    pub group_count: usize,
}

impl Default for GroupedFormatOptions {
    fn default() -> Self {
        Self {
            label: "corefs-grouped".into(),
            uuid: [0u8; 16],
            inode_count: super::layout::DEFAULT_INODE_COUNT,
            journal_blocks: super::layout::DEFAULT_JOURNAL_BLOCKS,
            group_count: 4,
        }
    }
}

/// Report from [`format_device_grouped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedFormatReport {
    pub total_blocks: u64,
    pub group_count: usize,
    pub blocks_per_group: u64,
    pub data_blocks_total: u64,
    pub uuid: [u8; 16],
}

/// Write a fresh grouped volume.  The data region is carved into
/// `group_count` equally-sized slices; each slice reserves its first
/// block for the group bitmap.
pub fn format_device_grouped(
    device: &mut dyn BlockDevice,
    opts: &GroupedFormatOptions,
) -> CoreFsResult<GroupedFormatReport> {
    if opts.group_count == 0 {
        return Err(CoreFsError::InvalidInput(
            "grouped format: group_count must be > 0".into(),
        ));
    }
    if opts.group_count > super::block_group::MAX_GROUPS_PER_TABLE {
        return Err(CoreFsError::InvalidInput(format!(
            "grouped format: group_count {} exceeds MAX_GROUPS_PER_TABLE {}",
            opts.group_count,
            super::block_group::MAX_GROUPS_PER_TABLE
        )));
    }
    let total_blocks = device.capacity() / BLOCK_SIZE;
    if total_blocks < super::layout::MIN_VOLUME_BLOCKS {
        return Err(CoreFsError::InvalidInput(format!(
            "grouped format: volume too small ({total_blocks} blocks)"
        )));
    }

    // Place control structures: block 2 = descriptor table, then inode bitmap,
    // inode table, journal, then the data region carved into groups.
    let descriptor_table_block = BLOCK_GROUP_TABLE_BLOCK;
    let inode_bitmap_start = descriptor_table_block + 1;
    let inode_bitmap_blocks = opts.inode_count.div_ceil(BITS_PER_BITMAP_BLOCK);
    let inode_table_start = inode_bitmap_start + inode_bitmap_blocks;
    let inode_table_blocks = opts.inode_count.div_ceil(INODES_PER_BLOCK);
    let journal_start = inode_table_start + inode_table_blocks;
    let journal_blocks = opts.journal_blocks;
    let data_region_start = journal_start + journal_blocks;
    let data_region_blocks = total_blocks
        .checked_sub(data_region_start + 2) // leave room for tertiary/secondary SB copies
        .ok_or_else(|| {
            CoreFsError::InvalidInput(
                "grouped format: no room for data region after control blocks".into(),
            )
        })?;
    let blocks_per_group = data_region_blocks / opts.group_count as u64;
    if blocks_per_group < 4 {
        return Err(CoreFsError::InvalidInput(format!(
            "grouped format: {} blocks per group is too small",
            blocks_per_group
        )));
    }

    // Build the group descriptors.
    let mut groups = Vec::with_capacity(opts.group_count);
    let user_inode_floor = FIRST_USER_INODE_SLOT;
    let user_inode_count = opts.inode_count.saturating_sub(user_inode_floor);
    let inodes_per_group = user_inode_count / opts.group_count as u64;
    for i in 0..opts.group_count {
        let group_start = data_region_start + i as u64 * blocks_per_group;
        let bitmap_block = group_start;
        let data_start = group_start + 1;
        let data_blocks = blocks_per_group - 1;
        let inode_range_start = user_inode_floor + i as u64 * inodes_per_group;
        let inode_range_count = if i + 1 == opts.group_count {
            user_inode_count - i as u64 * inodes_per_group
        } else {
            inodes_per_group
        };
        groups.push(BlockGroupDescriptor {
            data_start,
            data_blocks,
            bitmap_block,
            inode_range_start,
            inode_range_count,
            free_blocks: data_blocks as u32,
            bitmap_crc: 0,
        });
    }
    let table = BlockGroupTable::new(groups.clone())?;

    // --- Write all control regions ---------------------------------------
    let now = now_secs();
    let mut sb = build_superblock(
        total_blocks,
        opts,
        inode_bitmap_start,
        inode_bitmap_blocks,
        inode_table_start,
        inode_table_blocks,
        journal_start,
        journal_blocks,
        data_region_start,
        now,
    );

    // Block 0 — reserved.
    device.write_at(RESERVED_BLOCK * BLOCK_SIZE, &[0u8; BLOCK_SIZE as usize])?;
    // Block 2 — descriptor table (fresh, free_blocks=data_blocks, bitmap_crc=crc-of-zero).
    let zero_bitmap = vec![0u8; BLOCK_SIZE as usize];
    let zero_crc = Crc32c::hash(&zero_bitmap);
    let mut table_with_zero_crc = table.clone();
    for g in &mut table_with_zero_crc.groups {
        g.bitmap_crc = zero_crc;
    }
    let table_block = table_with_zero_crc.encode()?;
    device.write_at(descriptor_table_block * BLOCK_SIZE, &table_block)?;

    // Inode bitmap (all zero).
    let zero_ibm = vec![0u8; (inode_bitmap_blocks * BLOCK_SIZE) as usize];
    device.write_at(inode_bitmap_start * BLOCK_SIZE, &zero_ibm)?;

    // Inode table (all slots initialised to Unused).
    let unused = OnDiskInode::unused().encode()?;
    let mut table_buf = vec![0u8; (inode_table_blocks * BLOCK_SIZE) as usize];
    let slot_count = opts.inode_count.min(inode_table_blocks * INODES_PER_BLOCK);
    for i in 0..slot_count {
        let start = (i * INODE_RECORD_SIZE as u64) as usize;
        table_buf[start..start + INODE_RECORD_SIZE].copy_from_slice(&unused);
    }
    device.write_at(inode_table_start * BLOCK_SIZE, &table_buf)?;

    // Journal region — zero then init header.
    let j_buf = vec![0u8; (journal_blocks * BLOCK_SIZE) as usize];
    device.write_at(journal_start * BLOCK_SIZE, &j_buf)?;

    // Each group's bitmap block — zero fill.
    for g in &groups {
        device.write_at(g.bitmap_block * BLOCK_SIZE, &zero_bitmap)?;
    }

    // Superblock copies.
    sb.block_bitmap_crc = 0;
    sb.inode_bitmap_crc = Crc32c::hash(&zero_ibm);
    let sb_block = sb.encode_block();
    device.write_at(PRIMARY_SUPERBLOCK_BLOCK * BLOCK_SIZE, &sb_block)?;
    device.write_at(sb.tertiary_superblock_block * BLOCK_SIZE, &sb_block)?;
    device.write_at(sb.secondary_superblock_block * BLOCK_SIZE, &sb_block)?;

    // Journal header.
    super::journal::Journal::format(device, &sb)?;

    device.sync()?;

    Ok(GroupedFormatReport {
        total_blocks,
        group_count: opts.group_count,
        blocks_per_group,
        data_blocks_total: groups.iter().map(|g| g.data_blocks).sum(),
        uuid: opts.uuid,
    })
}

fn build_superblock(
    total_blocks: u64,
    opts: &GroupedFormatOptions,
    inode_bitmap_start: u64,
    inode_bitmap_blocks: u64,
    inode_table_start: u64,
    inode_table_blocks: u64,
    journal_start: u64,
    journal_blocks: u64,
    data_start: u64,
    now: i64,
) -> Superblock {
    let mut label_bytes = [0u8; 32];
    let copy_len = opts.label.as_bytes().len().min(label_bytes.len());
    label_bytes[..copy_len].copy_from_slice(&opts.label.as_bytes()[..copy_len]);
    Superblock {
        magic: ODF_MAGIC,
        version_major: ODF_VERSION_MAJOR,
        version_minor: ODF_VERSION_MINOR,
        block_size: BLOCK_SIZE as u32,
        total_blocks,
        free_blocks: 0,
        total_inodes: opts.inode_count,
        free_inodes: opts.inode_count,
        // In grouped mode, "block_bitmap_start/blocks" are set to the
        // descriptor table block.  Tools check the incompat flag to
        // understand the real layout.
        block_bitmap_start: BLOCK_GROUP_TABLE_BLOCK,
        block_bitmap_blocks: 1,
        inode_bitmap_start,
        inode_bitmap_blocks,
        inode_table_start,
        inode_table_blocks,
        journal_start,
        journal_blocks,
        data_start,
        secondary_superblock_block: total_blocks - 1,
        tertiary_superblock_block: total_blocks / 2,
        feature_compat: super::layout::FEATURE_COMPAT_BLOCK_CHECKSUMS
            | super::layout::FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS,
        feature_incompat: super::layout::FEATURE_INCOMPAT_PAYLOAD_INODE
            | FEATURE_INCOMPAT_BLOCK_GROUPS,
        feature_ro_compat: 0,
        uuid: opts.uuid,
        label: label_bytes,
        created_at: now,
        last_mount_at: now,
        last_write_at: now,
        mount_count: 0,
        generation: 1,
        state: STATE_CLEAN,
        payload_inode: ANCILLARY_INODE_SLOT,
        block_bitmap_crc: 0,
        inode_bitmap_crc: 0,
        layout_mode: LAYOUT_MODE_NATIVE,
        root_inode: 0,
    }
}

/// Save a `PersistedState` into a grouped volume.  Uses the
/// [`MultiGroupAllocator`] so extents land in the home group of their
/// owning inode slot.
pub fn save_state_native_grouped(
    device: &mut dyn BlockDevice,
    state: &PersistedState,
) -> CoreFsResult<GroupedSaveReport> {
    let mut sb = super::volume::read_sb_with_fallbacks(device)?;
    if sb.feature_incompat & FEATURE_INCOMPAT_BLOCK_GROUPS == 0 {
        return Err(CoreFsError::State(
            "grouped save: volume is not in grouped layout".into(),
        ));
    }

    // Read descriptor table + per-group bitmaps.
    let table_bytes = device.read_at(BLOCK_GROUP_TABLE_BLOCK * BLOCK_SIZE, BLOCK_SIZE)?;
    let mut table = BlockGroupTable::decode(&table_bytes)?;
    // Reset all per-group bitmaps to empty — we're doing a full save.
    let mut group_bitmaps = Vec::with_capacity(table.groups.len());
    for g in &table.groups {
        group_bitmaps.push(Bitmap::new(g.data_blocks));
    }
    // Fresh inode bitmap; reserve slots 0 and 1.
    let mut inode_bitmap = Bitmap::new(sb.total_inodes);
    inode_bitmap.set(0)?;
    inode_bitmap.set(ANCILLARY_INODE_SLOT)?;

    let mut alloc = MultiGroupAllocator::new(
        BlockGroupTable::new(table.groups.clone())?,
        group_bitmaps,
        inode_bitmap,
        FIRST_USER_INODE_SLOT,
    )?;

    // --- Ancillary at slot #1 --------------------------------------------
    let anc = super::native::build_ancillary_bytes(state)?;
    let anc_blocks = (anc.len() as u64).div_ceil(BLOCK_SIZE);
    let anc_extent = alloc.allocate_near(anc_blocks, ANCILLARY_INODE_SLOT)?;
    let mut anc_buf = vec![0u8; (anc_blocks * BLOCK_SIZE) as usize];
    anc_buf[..anc.len()].copy_from_slice(&anc);
    device.write_at(anc_extent.physical_block * BLOCK_SIZE, &anc_buf)?;
    let now = now_secs();
    let anc_crc = {
        let (payload, crc_bytes) = anc.split_at(anc.len() - 4);
        let _ = payload;
        u32::from_le_bytes(crc_bytes.try_into().unwrap())
    };
    let anc_inode = OnDiskInode {
        version: 1,
        kind: OnDiskKind::SystemPayload,
        mode: 0o600,
        uid: 0,
        gid: 0,
        link_count: 1,
        flags: 0,
        size_bytes: anc.len() as u64,
        blocks_allocated: anc_blocks,
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
    write_inode_at_slot(device, &sb, ANCILLARY_INODE_SLOT, &anc_inode)?;

    // Bytes-per-inode map.
    let mut bytes_by_inode: HashMap<InodeId, &[u8]> =
        HashMap::new();
    for rec in &state.block_records {
        bytes_by_inode.insert(rec.inode, rec.bytes.as_slice());
    }

    // Write each inode.
    let mut active_slots = 0usize;
    let mut deleted_slots = 0usize;
    for inode in &state.active_inodes {
        let bytes = bytes_by_inode.get(&inode.id).copied().unwrap_or(&[]);
        write_user_inode(device, &mut alloc, &sb, inode, bytes, false)?;
        active_slots += 1;
    }
    for inode in &state.deleted_inodes {
        let bytes = bytes_by_inode.get(&inode.id).copied().unwrap_or(&[]);
        write_user_inode(device, &mut alloc, &sb, inode, bytes, true)?;
        deleted_slots += 1;
    }

    // --- Persist per-group bitmaps + descriptor table --------------------
    alloc.refresh_descriptors();
    let data_blocks_used =
        table.groups.iter().map(|g| g.data_blocks).sum::<u64>() - alloc.total_free_data_blocks();
    let (refreshed_table, group_bitmaps, final_inode_bitmap) = alloc.into_parts();
    table = refreshed_table;
    for (g, bm) in table.groups.iter().zip(group_bitmaps.iter()) {
        device.write_at(g.bitmap_block * BLOCK_SIZE, bm.as_bytes())?;
    }
    let table_bytes = table.encode()?;
    device.write_at(BLOCK_GROUP_TABLE_BLOCK * BLOCK_SIZE, &table_bytes)?;
    device.write_at(
        sb.inode_bitmap_start * BLOCK_SIZE,
        final_inode_bitmap.as_bytes(),
    )?;

    // Bump superblock.
    sb.generation += 1;
    sb.last_write_at = now;
    sb.state = STATE_CLEAN;
    sb.payload_inode = ANCILLARY_INODE_SLOT;
    sb.free_blocks = table.groups.iter().map(|g| g.data_blocks).sum::<u64>() - data_blocks_used;
    sb.free_inodes = sb.total_inodes - final_inode_bitmap.popcount();
    sb.inode_bitmap_crc = Crc32c::hash(final_inode_bitmap.as_bytes());
    sb.block_bitmap_crc = 0; // not meaningful in grouped mode
    sb.root_inode = state
        .active_inodes
        .iter()
        .find(|i| i.path == "/" && matches!(i.kind, InodeKind::Directory))
        .map(|i| i.id.0)
        .unwrap_or(0);
    let sb_block = sb.encode_block();
    device.write_at(PRIMARY_SUPERBLOCK_BLOCK * BLOCK_SIZE, &sb_block)?;
    device.write_at(sb.tertiary_superblock_block * BLOCK_SIZE, &sb_block)?;
    device.write_at(sb.secondary_superblock_block * BLOCK_SIZE, &sb_block)?;
    device.sync()?;

    Ok(GroupedSaveReport {
        generation: sb.generation,
        active_slots,
        deleted_slots,
        group_count: table.groups.len(),
        data_blocks_used,
    })
}

/// Load a `PersistedState` from a grouped volume.
pub fn load_state_native_grouped(device: &dyn BlockDevice) -> CoreFsResult<PersistedState> {
    let sb = super::volume::read_sb_with_fallbacks(device)?;
    if sb.feature_incompat & FEATURE_INCOMPAT_BLOCK_GROUPS == 0 {
        return Err(CoreFsError::State(
            "grouped load: volume is not in grouped layout".into(),
        ));
    }
    // --- Verify per-group bitmap CRCs ------------------------------------
    let table_bytes = device.read_at(BLOCK_GROUP_TABLE_BLOCK * BLOCK_SIZE, BLOCK_SIZE)?;
    let table = BlockGroupTable::decode(&table_bytes)?;
    for g in &table.groups {
        let bm_bytes = device.read_at(g.bitmap_block * BLOCK_SIZE, BLOCK_SIZE)?;
        let actual = Crc32c::hash(&bm_bytes);
        if actual != g.bitmap_crc {
            return Err(CoreFsError::State(format!(
                "grouped load: group bitmap CRC mismatch at block {} (stored=0x{:08X}, actual=0x{:08X})",
                g.bitmap_block, g.bitmap_crc, actual
            )));
        }
    }

    // --- Ancillary blob --------------------------------------------------
    let anc_inode = read_inode_at_slot(device, &sb, ANCILLARY_INODE_SLOT)?;
    if anc_inode.kind != OnDiskKind::SystemPayload {
        return Err(CoreFsError::State(
            "grouped load: ancillary slot has wrong kind".into(),
        ));
    }
    let anc_bytes = super::native::read_all_extent_bytes_public(device, &anc_inode)?;
    let (payload, crc_bytes) = anc_bytes.split_at(anc_bytes.len() - 4);
    let stored_crc = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    if stored_crc != Crc32c::hash(payload) {
        return Err(CoreFsError::State(
            "grouped load: ancillary CRC mismatch".into(),
        ));
    }
    let ancillary = super::native::decode_ancillary(payload)?;

    // --- Walk inode bitmap ----------------------------------------------
    let ibm_bytes = device.read_at(
        sb.inode_bitmap_start * BLOCK_SIZE,
        sb.inode_bitmap_blocks * BLOCK_SIZE,
    )?;
    let inode_bitmap = Bitmap::from_bytes(ibm_bytes, sb.total_inodes)?;

    let mut active_inodes = Vec::new();
    let mut deleted_inodes = Vec::new();
    let mut block_records = Vec::new();

    for slot in FIRST_USER_INODE_SLOT..sb.total_inodes {
        if !inode_bitmap.is_set(slot)? {
            continue;
        }
        let on_disk = read_inode_at_slot(device, &sb, slot)?;
        if on_disk.kind == OnDiskKind::Unused {
            continue;
        }
        if on_disk.xattr_block_addr == 0 {
            return Err(CoreFsError::State(format!(
                "grouped load: inode slot {slot} has no attr block"
            )));
        }
        let attr_buf = device.read_at(on_disk.xattr_block_addr * BLOCK_SIZE, BLOCK_SIZE)?;
        let attr = AttrBlock::decode(&attr_buf)?;
        let inode: Inode = bincode::deserialize(&attr.payload).map_err(|e| {
            CoreFsError::State(format!("grouped load: inode deserialize failed: {e}"))
        })?;
        let bytes = super::native::read_all_extent_bytes_public(device, &on_disk)?;
        if !bytes.is_empty() {
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
                checksum: on_disk.data_crc,
                device_block: first_ext.physical_block,
                allocated_blocks: total_alloc,
            });
        }
        if on_disk.flags & super::native::FLAG_DELETED != 0 {
            deleted_inodes.push(inode);
        } else {
            active_inodes.push(inode);
        }
    }

    let mut state = super::native::ancillary_into_state(ancillary);
    state.active_inodes = active_inodes;
    state.deleted_inodes = deleted_inodes;
    state.block_records = block_records;
    Ok(state)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedSaveReport {
    pub generation: u64,
    pub active_slots: usize,
    pub deleted_slots: usize,
    pub group_count: usize,
    pub data_blocks_used: u64,
}

fn write_user_inode(
    device: &mut dyn BlockDevice,
    alloc: &mut MultiGroupAllocator,
    sb: &Superblock,
    inode: &Inode,
    content: &[u8],
    deleted: bool,
) -> CoreFsResult<()> {
    let slot = alloc.allocate_inode()?;
    let (extents, index_block_addr, has_index) = if content.is_empty() {
        (Vec::new(), 0u64, false)
    } else {
        let blocks = (content.len() as u64).div_ceil(BLOCK_SIZE);
        let ext = alloc.allocate_near(blocks, slot)?;
        let mut buf = vec![0u8; (blocks * BLOCK_SIZE) as usize];
        buf[..content.len()].copy_from_slice(content);
        device.write_at(ext.physical_block * BLOCK_SIZE, &buf)?;
        (vec![ext], 0u64, false)
    };

    let attr_bytes = bincode::serialize(inode)
        .map_err(|e| CoreFsError::State(format!("grouped: inode serialize failed: {e}")))?;
    if attr_bytes.len() > super::attr_block::ATTR_BLOCK_CAPACITY {
        return Err(CoreFsError::State(format!(
            "grouped: serialized inode {} exceeds attr block capacity",
            attr_bytes.len()
        )));
    }
    let attr_ext = alloc.allocate_near(1, slot)?;
    let attr_block_addr = attr_ext.physical_block;
    let encoded = AttrBlock::new(attr_bytes).encode()?;
    device.write_at(attr_block_addr * BLOCK_SIZE, &encoded)?;

    let kind = match inode.kind {
        InodeKind::File => OnDiskKind::File,
        InodeKind::Directory => OnDiskKind::Directory,
        InodeKind::Symlink => OnDiskKind::Symlink,
    };
    let mut flags = super::inode::FLAG_HAS_XATTRS;
    if has_index {
        flags |= FLAG_HAS_EXTENT_INDEX;
    }
    if deleted {
        flags |= super::native::FLAG_DELETED;
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
        accessed_at: systime_to_secs(inode.modified_at),
        generation: 1,
        extents: if has_index { Vec::new() } else { extents },
        index_block_addr,
        xattr_block_addr: attr_block_addr,
        domain_inode_id: inode.id.0,
        data_crc,
    };
    write_inode_at_slot(device, sb, slot, &on_disk)
}

fn write_inode_at_slot(
    device: &mut dyn BlockDevice,
    sb: &Superblock,
    slot: u64,
    inode: &OnDiskInode,
) -> CoreFsResult<()> {
    let geom = LayoutGeometry {
        total_blocks: sb.total_blocks,
        block_bitmap_start: sb.block_bitmap_start,
        block_bitmap_blocks: sb.block_bitmap_blocks,
        inode_bitmap_start: sb.inode_bitmap_start,
        inode_bitmap_blocks: sb.inode_bitmap_blocks,
        inode_table_start: sb.inode_table_start,
        inode_table_blocks: sb.inode_table_blocks,
        inode_count: sb.total_inodes,
        journal_start: sb.journal_start,
        journal_blocks: sb.journal_blocks,
        data_start: sb.data_start,
        data_blocks: sb.total_blocks.saturating_sub(sb.data_start),
        secondary_superblock_block: sb.secondary_superblock_block,
        tertiary_superblock_block: sb.tertiary_superblock_block,
    };
    let (block, offset) = geom.inode_record_location(slot)?;
    let mut buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    let encoded = inode.encode()?;
    buf[offset as usize..offset as usize + INODE_RECORD_SIZE].copy_from_slice(&encoded);
    device.write_at(block * BLOCK_SIZE, &buf)
}

fn read_inode_at_slot(
    device: &dyn BlockDevice,
    sb: &Superblock,
    slot: u64,
) -> CoreFsResult<OnDiskInode> {
    let geom = LayoutGeometry {
        total_blocks: sb.total_blocks,
        block_bitmap_start: sb.block_bitmap_start,
        block_bitmap_blocks: sb.block_bitmap_blocks,
        inode_bitmap_start: sb.inode_bitmap_start,
        inode_bitmap_blocks: sb.inode_bitmap_blocks,
        inode_table_start: sb.inode_table_start,
        inode_table_blocks: sb.inode_table_blocks,
        inode_count: sb.total_inodes,
        journal_start: sb.journal_start,
        journal_blocks: sb.journal_blocks,
        data_start: sb.data_start,
        data_blocks: sb.total_blocks.saturating_sub(sb.data_start),
        secondary_superblock_block: sb.secondary_superblock_block,
        tertiary_superblock_block: sb.tertiary_superblock_block,
    };
    let (block, offset) = geom.inode_record_location(slot)?;
    let buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    OnDiskInode::decode(&buf[offset as usize..offset as usize + INODE_RECORD_SIZE])
}

fn systime_to_secs(t: Timestamp) -> i64 {
    t.as_secs() as i64
}

fn now_secs() -> i64 {
    systime_to_secs(Timestamp::now())
}

#[cfg(test)]
#[path = "grouped_tests.rs"]
mod tests;
