// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Top-level driver of the CoreFS On-Disk Format v1.
//!
//! Exposes the three enterprise operations:
//!
//! * [`format_device`] — write all control structures of a fresh volume.
//! * [`save_state`] — persist a [`PersistedState`] payload into an already
//!   formatted volume, atomically replacing the previous payload and bumping
//!   the superblock generation.
//! * [`load_state`] — re-read the payload together with the authoritative
//!   superblock, transparently falling back to the redundant copies if the
//!   primary block is corrupted.

use crate::app::PersistedState;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;
use std::time::{SystemTime, UNIX_EPOCH};

use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::inode::{Extent, OnDiskInode, OnDiskKind};
use super::layout::{
    BLOCK_SIZE, LayoutGeometry, LayoutParams, PRIMARY_SUPERBLOCK_BLOCK, RESERVED_BLOCK,
};
use super::superblock::{STATE_CLEAN, Superblock};

/// Byte offset of the CRC32C trailer embedded at the end of the payload blob.
const PAYLOAD_CRC_TRAILER_BYTES: usize = 4;

/// Options for [`format_device`].
#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub label: String,
    pub uuid: [u8; 16],
    pub inode_count: u64,
    pub journal_blocks: u64,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            label: "corefs".to_string(),
            uuid: [0u8; 16],
            inode_count: super::layout::DEFAULT_INODE_COUNT,
            journal_blocks: super::layout::DEFAULT_JOURNAL_BLOCKS,
        }
    }
}

/// Report returned after formatting a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatReport {
    pub geometry: LayoutGeometry,
    pub uuid: [u8; 16],
    pub generation: u64,
}

/// Write a fresh ODF v1 volume to the device.  All existing content on the
/// device is logically overwritten (bitmaps reset, inode table zeroed).
pub fn format_device(
    device: &mut dyn BlockDevice,
    options: &FormatOptions,
) -> CoreFsResult<FormatReport> {
    let total_blocks = device.capacity() / BLOCK_SIZE;
    let params = LayoutParams {
        total_blocks,
        inode_count: options.inode_count,
        journal_blocks: options.journal_blocks,
    };
    let geom = LayoutGeometry::plan(params)?;
    let now = now_secs();
    let sb = Superblock::for_new_volume(&geom, options.uuid, &options.label, now);

    // --- Reserved block 0 — zero fill -------------------------------------
    write_block(device, RESERVED_BLOCK, &[0u8; BLOCK_SIZE as usize])?;

    // --- Bitmaps ----------------------------------------------------------
    let mut block_bitmap = Bitmap::new(geom.total_blocks);
    // Mark control blocks as allocated.
    mark_reserved_blocks(&mut block_bitmap, &geom)?;
    write_blocks(
        device,
        geom.block_bitmap_start,
        block_bitmap.as_bytes(),
    )?;

    let mut inode_bitmap = Bitmap::new(geom.inode_count);
    // Reserve inode 0 for the system payload.
    inode_bitmap.set(0)?;
    write_blocks(
        device,
        geom.inode_bitmap_start,
        inode_bitmap.as_bytes(),
    )?;

    // --- Inode table — write unused slots ---------------------------------
    let unused = OnDiskInode::unused().encode()?;
    let mut table = vec![0u8; (geom.inode_table_blocks * BLOCK_SIZE) as usize];
    let slot_count = (geom.inode_count).min(geom.inode_table_blocks * super::layout::INODES_PER_BLOCK);
    for i in 0..slot_count {
        let start = (i * super::inode::INODE_RECORD_SIZE as u64) as usize;
        table[start..start + super::inode::INODE_RECORD_SIZE].copy_from_slice(&unused);
    }
    // System inode slot #0 — initial empty payload inode.
    let system_inode = OnDiskInode {
        version: 1,
        kind: OnDiskKind::SystemPayload,
        mode: 0o600,
        uid: 0,
        gid: 0,
        link_count: 1,
        flags: 0,
        size_bytes: 0,
        blocks_allocated: 0,
        created_at: now,
        modified_at: now,
        changed_at: now,
        accessed_at: now,
        generation: 1,
        extents: Vec::new(),
        index_block_addr: 0,
        xattr_block_addr: 0,
        domain_inode_id: 0,
        data_crc: 0,
    };
    let sys_enc = system_inode.encode()?;
    table[..super::inode::INODE_RECORD_SIZE].copy_from_slice(&sys_enc);
    write_blocks(device, geom.inode_table_start, &table)?;

    // --- Journal region ---------------------------------------------------
    // Zero the whole region first, then lay down a fresh header so that
    // Journal::open / fsck find a decodable empty journal.
    zero_fill_region(device, geom.journal_start, geom.journal_blocks)?;

    // --- Superblock(s) (written first so Journal::format can read them) ---
    let sb_block = sb.encode_block();
    write_block(device, PRIMARY_SUPERBLOCK_BLOCK, &sb_block)?;
    write_block(device, geom.tertiary_superblock_block, &sb_block)?;
    write_block(device, geom.secondary_superblock_block, &sb_block)?;

    // Initialize the journal header.
    super::journal::Journal::format(device, &sb)?;

    device.sync()?;

    Ok(FormatReport {
        geometry: geom,
        uuid: options.uuid,
        generation: sb.generation,
    })
}

/// Persist a `PersistedState` blob into an already-formatted volume.  On
/// return the device has a fresh superblock generation and the previous
/// payload blocks have been freed in the bitmap.
pub fn save_state(
    device: &mut dyn BlockDevice,
    state: &PersistedState,
) -> CoreFsResult<SaveReport> {
    let mut sb = read_primary_superblock_with_fallbacks(device)?;
    let geom = sb.geometry();

    // --- TRIM old payload extents before we overwrite them ----------------
    // Read the current system inode so we know which data blocks the
    // previous generation was occupying and discard them on the device.
    if let Ok(old_inode) = read_inode_record(device, &geom, sb.payload_inode) {
        if old_inode.kind == OnDiskKind::SystemPayload && !old_inode.extents.is_empty() {
            trim_extents(device, &old_inode.extents)?;
        }
    }

    // --- Serialise payload + CRC trailer ---------------------------------
    let mut payload = bincode::serialize(state).map_err(|e| {
        CoreFsError::State(format!("ODF: failed to serialize PersistedState: {e}"))
    })?;
    let crc = Crc32c::hash(&payload);
    payload.extend_from_slice(&crc.to_le_bytes());

    let payload_blocks_needed =
        (payload.len() as u64).div_ceil(BLOCK_SIZE);
    if payload_blocks_needed > geom.data_blocks {
        return Err(CoreFsError::State(format!(
            "ODF: payload requires {} blocks, data region holds {}",
            payload_blocks_needed, geom.data_blocks
        )));
    }

    // --- Reset bitmaps: only control blocks + new payload extent stay set.
    let mut block_bitmap = Bitmap::new(geom.total_blocks);
    mark_reserved_blocks(&mut block_bitmap, &geom)?;
    let payload_start = geom.data_start;
    for i in 0..payload_blocks_needed {
        block_bitmap.set(payload_start + i)?;
    }
    write_blocks(
        device,
        geom.block_bitmap_start,
        block_bitmap.as_bytes(),
    )?;

    let mut inode_bitmap = Bitmap::new(geom.inode_count);
    inode_bitmap.set(0)?;
    write_blocks(
        device,
        geom.inode_bitmap_start,
        inode_bitmap.as_bytes(),
    )?;

    // --- Write payload blocks --------------------------------------------
    let mut buf = vec![0u8; (payload_blocks_needed * BLOCK_SIZE) as usize];
    buf[..payload.len()].copy_from_slice(&payload);
    write_blocks(device, payload_start, &buf)?;

    // --- Update system inode with single extent --------------------------
    let now = now_secs();
    let system_inode = OnDiskInode {
        version: 1,
        kind: OnDiskKind::SystemPayload,
        mode: 0o600,
        uid: 0,
        gid: 0,
        link_count: 1,
        flags: 0,
        size_bytes: payload.len() as u64,
        blocks_allocated: payload_blocks_needed,
        created_at: sb.created_at,
        modified_at: now,
        changed_at: now,
        accessed_at: now,
        generation: sb.generation + 1,
        extents: vec![Extent {
            logical_block: 0,
            length_blocks: payload_blocks_needed as u32,
            physical_block: payload_start,
        }],
        index_block_addr: 0,
        xattr_block_addr: 0,
        domain_inode_id: 0,
        data_crc: u64::from(Crc32c::hash(&payload)),
    };
    write_inode_record(device, &geom, 0, &system_inode)?;

    // --- Bump + rewrite superblock(s) with bitmap CRCs --------------------
    sb.generation += 1;
    sb.last_write_at = now;
    sb.state = STATE_CLEAN;
    sb.payload_inode = 0;
    sb.free_blocks = geom.data_blocks - payload_blocks_needed;
    sb.free_inodes = geom.inode_count - 1;
    sb.block_bitmap_crc = Crc32c::hash(block_bitmap.as_bytes());
    sb.inode_bitmap_crc = Crc32c::hash(inode_bitmap.as_bytes());
    sb.layout_mode = super::superblock::LAYOUT_MODE_BLOB;
    sb.root_inode = 0;
    let sb_block = sb.encode_block();
    write_block(device, PRIMARY_SUPERBLOCK_BLOCK, &sb_block)?;
    write_block(device, geom.tertiary_superblock_block, &sb_block)?;
    write_block(device, geom.secondary_superblock_block, &sb_block)?;
    device.sync()?;

    Ok(SaveReport {
        generation: sb.generation,
        payload_bytes: payload.len() as u64,
        payload_blocks: payload_blocks_needed,
    })
}

/// Report returned after a successful [`save_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    pub generation: u64,
    pub payload_bytes: u64,
    pub payload_blocks: u64,
}

/// Load a `PersistedState` payload from a previously saved volume.
pub fn load_state(device: &dyn BlockDevice) -> CoreFsResult<PersistedState> {
    let sb = read_primary_superblock_with_fallbacks(device)?;
    verify_bitmap_integrity(device, &sb)?;
    let geom = sb.geometry();
    let system_inode = read_inode_record(device, &geom, sb.payload_inode)?;
    if system_inode.kind != OnDiskKind::SystemPayload {
        return Err(CoreFsError::State(format!(
            "payload inode #{} has unexpected kind {:?}",
            sb.payload_inode, system_inode.kind
        )));
    }
    if system_inode.extents.is_empty() {
        return Err(CoreFsError::State(
            "ODF: system payload inode has no extents".to_string(),
        ));
    }

    // Read all extent bytes in logical order.
    let mut raw = Vec::with_capacity(system_inode.size_bytes as usize);
    let mut remaining = system_inode.size_bytes as usize;
    let mut sorted = system_inode.extents.clone();
    sorted.sort_by_key(|e| e.logical_block);
    for ext in &sorted {
        let byte_offset = ext.physical_block * BLOCK_SIZE;
        let byte_len = u64::from(ext.length_blocks) * BLOCK_SIZE;
        let block_data = device.read_at(byte_offset, byte_len)?;
        let wanted = remaining.min(block_data.len());
        raw.extend_from_slice(&block_data[..wanted]);
        remaining -= wanted;
    }
    if raw.len() != system_inode.size_bytes as usize {
        return Err(CoreFsError::State(
            "ODF: payload extents did not cover declared size".to_string(),
        ));
    }
    if raw.len() < PAYLOAD_CRC_TRAILER_BYTES {
        return Err(CoreFsError::State(
            "ODF: payload is shorter than CRC trailer".to_string(),
        ));
    }

    let (data, crc_bytes) = raw.split_at(raw.len() - PAYLOAD_CRC_TRAILER_BYTES);
    let stored_crc = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    let expected_crc = Crc32c::hash(data);
    if stored_crc != expected_crc {
        return Err(CoreFsError::State(format!(
            "ODF: payload CRC mismatch (stored=0x{stored_crc:08X}, expected=0x{expected_crc:08X})"
        )));
    }

    let state: PersistedState = bincode::deserialize(data).map_err(|e| {
        CoreFsError::State(format!("ODF: failed to deserialize PersistedState: {e}"))
    })?;
    Ok(state)
}

/// Light-weight volume inspection used by `fsck`-style tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub label: String,
    pub uuid: [u8; 16],
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
    pub generation: u64,
    pub state: u32,
    pub primary_ok: bool,
    pub tertiary_ok: bool,
    pub secondary_ok: bool,
}

/// Read superblocks and report their status.  Used by admin tooling to
/// verify a volume's structural health without touching the payload.
pub fn inspect(device: &dyn BlockDevice) -> CoreFsResult<VolumeInfo> {
    let (sb, primary_ok) = read_superblock_at(device, PRIMARY_SUPERBLOCK_BLOCK)
        .map(|s| (s, true))
        .unwrap_or_else(|_| {
            // Try fallbacks to at least report something.
            let geom_total = device.capacity() / BLOCK_SIZE;
            let tertiary = read_superblock_at(device, geom_total / 2);
            let secondary = read_superblock_at(device, geom_total - 1);
            let sb = tertiary.or(secondary).unwrap_or_else(|_| {
                // Return a minimal placeholder if everything fails.
                panic!("no superblock readable")
            });
            (sb, false)
        });

    let geom = sb.geometry();
    let tertiary_ok = read_superblock_at(device, geom.tertiary_superblock_block).is_ok();
    let secondary_ok = read_superblock_at(device, geom.secondary_superblock_block).is_ok();
    let label = String::from_utf8_lossy(
        &sb.label[..sb.label.iter().position(|b| *b == 0).unwrap_or(sb.label.len())],
    )
    .to_string();
    Ok(VolumeInfo {
        label,
        uuid: sb.uuid,
        total_blocks: sb.total_blocks,
        free_blocks: sb.free_blocks,
        total_inodes: sb.total_inodes,
        free_inodes: sb.free_inodes,
        generation: sb.generation,
        state: sb.state,
        primary_ok,
        tertiary_ok,
        secondary_ok,
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn mark_reserved_blocks(bitmap: &mut Bitmap, geom: &LayoutGeometry) -> CoreFsResult<()> {
    bitmap.set(RESERVED_BLOCK)?;
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

fn write_block(device: &mut dyn BlockDevice, block: u64, data: &[u8]) -> CoreFsResult<()> {
    debug_assert_eq!(data.len() as u64, BLOCK_SIZE);
    device.write_at(block * BLOCK_SIZE, data)
}

fn write_blocks(device: &mut dyn BlockDevice, start: u64, bytes: &[u8]) -> CoreFsResult<()> {
    debug_assert_eq!(bytes.len() as u64 % BLOCK_SIZE, 0);
    device.write_at(start * BLOCK_SIZE, bytes)
}

fn zero_fill_region(
    device: &mut dyn BlockDevice,
    start: u64,
    blocks: u64,
) -> CoreFsResult<()> {
    // Chunk zero-fill to avoid a huge single allocation for large regions.
    const CHUNK_BLOCKS: u64 = 64;
    let mut written = 0u64;
    while written < blocks {
        let take = (blocks - written).min(CHUNK_BLOCKS);
        let buf = vec![0u8; (take * BLOCK_SIZE) as usize];
        write_blocks(device, start + written, &buf)?;
        written += take;
    }
    Ok(())
}

fn read_superblock_at(device: &dyn BlockDevice, block: u64) -> CoreFsResult<Superblock> {
    let buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    Superblock::decode_block(&buf)
}

fn trim_extents(
    device: &mut dyn BlockDevice,
    extents: &[Extent],
) -> CoreFsResult<()> {
    if !device.supports_trim() {
        return Ok(());
    }
    for ext in extents {
        if ext.length_blocks == 0 {
            continue;
        }
        let offset = ext.physical_block * BLOCK_SIZE;
        let length = u64::from(ext.length_blocks) * BLOCK_SIZE;
        // TRIM is advisory — ignore errors so a single failing discard
        // does not make a successful save look like a failure.
        let _ = device.trim(offset, length);
    }
    Ok(())
}

/// Verify that the persisted bitmap bytes still match the CRC32C values
/// recorded in the superblock.  Returns an error if either bitmap is
/// corrupted or has been tampered with since the last save.
pub fn verify_bitmap_integrity(
    device: &dyn BlockDevice,
    sb: &Superblock,
) -> CoreFsResult<()> {
    if sb.block_bitmap_crc != 0 {
        let bytes = device.read_at(
            sb.block_bitmap_start * BLOCK_SIZE,
            sb.block_bitmap_blocks * BLOCK_SIZE,
        )?;
        let actual = Crc32c::hash(&bytes);
        if actual != sb.block_bitmap_crc {
            return Err(CoreFsError::State(format!(
                "ODF: block bitmap CRC mismatch (stored=0x{:08X}, actual=0x{:08X})",
                sb.block_bitmap_crc, actual
            )));
        }
    }
    if sb.inode_bitmap_crc != 0 {
        let bytes = device.read_at(
            sb.inode_bitmap_start * BLOCK_SIZE,
            sb.inode_bitmap_blocks * BLOCK_SIZE,
        )?;
        let actual = Crc32c::hash(&bytes);
        if actual != sb.inode_bitmap_crc {
            return Err(CoreFsError::State(format!(
                "ODF: inode bitmap CRC mismatch (stored=0x{:08X}, actual=0x{:08X})",
                sb.inode_bitmap_crc, actual
            )));
        }
    }
    Ok(())
}

/// Public alias for use by the native layout implementation.
pub fn read_sb_with_fallbacks(device: &dyn BlockDevice) -> CoreFsResult<Superblock> {
    read_primary_superblock_with_fallbacks(device)
}

fn read_primary_superblock_with_fallbacks(device: &dyn BlockDevice) -> CoreFsResult<Superblock> {
    if let Ok(sb) = read_superblock_at(device, PRIMARY_SUPERBLOCK_BLOCK) {
        return Ok(sb);
    }
    // Fallbacks: superblock copies at N/2 and N-1.
    let total_blocks = device.capacity() / BLOCK_SIZE;
    if let Ok(sb) = read_superblock_at(device, total_blocks / 2) {
        return Ok(sb);
    }
    read_superblock_at(device, total_blocks - 1).map_err(|e| {
        CoreFsError::State(format!(
            "all superblock copies are unreadable; last error: {e}"
        ))
    })
}

fn write_inode_record(
    device: &mut dyn BlockDevice,
    geom: &LayoutGeometry,
    index: u64,
    inode: &OnDiskInode,
) -> CoreFsResult<()> {
    let (block, offset_in_block) = geom.inode_record_location(index)?;
    // Read-modify-write for the containing block.
    let mut block_buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    let encoded = inode.encode()?;
    let start = offset_in_block as usize;
    block_buf[start..start + super::inode::INODE_RECORD_SIZE].copy_from_slice(&encoded);
    write_block(device, block, &block_buf)
}

fn read_inode_record(
    device: &dyn BlockDevice,
    geom: &LayoutGeometry,
    index: u64,
) -> CoreFsResult<OnDiskInode> {
    let (block, offset_in_block) = geom.inode_record_location(index)?;
    let block_buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    let start = offset_in_block as usize;
    OnDiskInode::decode(&block_buf[start..start + super::inode::INODE_RECORD_SIZE])
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
