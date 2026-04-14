// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Physical layout of the CoreFS On-Disk Format (ODF v1).
//!
//! The volume is divided into fixed-size 4 KiB blocks.  The first blocks hold
//! control structures, followed by a reserved journal region and the data
//! area that is managed via a block allocation bitmap and an inode table.
//!
//! ```text
//! block 0        Reserved (boot sector, all zero)
//! block 1        Primary superblock
//! block 2..      Block allocation bitmap          (`block_bitmap_blocks` blocks)
//! block .. ..    Inode allocation bitmap          (`inode_bitmap_blocks` blocks)
//! block .. ..    Inode table                      (`inode_table_blocks` blocks)
//! block .. ..    Journal region                   (`journal_blocks` blocks)
//! block .. N-2   Data blocks                      (remainder)
//! block N/2      Tertiary superblock (redundant copy, read-only fallback)
//! block N-1      Secondary superblock (redundant copy, read-only fallback)
//! ```
//!
//! Multi-byte integers are stored in **little-endian** order.  Every persisted
//! structure carries a CRC32C checksum so corruption can be detected
//! independently of the payload format.

use crate::error::{CoreFsError, CoreFsResult};

/// Size of an on-disk block in bytes (4 KiB).
pub const BLOCK_SIZE: u64 = 4096;
/// Logarithm base 2 of [`BLOCK_SIZE`].
pub const BLOCK_SIZE_LOG2: u32 = 12;
/// Size of a single on-disk inode record in bytes.
pub const INODE_SIZE: u64 = 256;
/// Number of inode records that fit into one block.
pub const INODES_PER_BLOCK: u64 = BLOCK_SIZE / INODE_SIZE;
/// Number of bits represented by a single bitmap block.
pub const BITS_PER_BITMAP_BLOCK: u64 = BLOCK_SIZE * 8;
/// Minimum acceptable volume size in blocks (≈ 256 KiB).
pub const MIN_VOLUME_BLOCKS: u64 = 64;
/// Default inode count for a freshly formatted volume.
pub const DEFAULT_INODE_COUNT: u64 = 4096;
/// Default journal region size in blocks (256 KiB).
pub const DEFAULT_JOURNAL_BLOCKS: u64 = 64;
/// Reserved block 0 — the boot sector / signature area.
pub const RESERVED_BLOCK: u64 = 0;
/// Location of the primary (authoritative) superblock.
pub const PRIMARY_SUPERBLOCK_BLOCK: u64 = 1;
/// ODF magic value — ASCII "COREFSDF" in little-endian u64.
pub const ODF_MAGIC: u64 = u64::from_le_bytes(*b"COREFSDF");
/// Current format major version.
pub const ODF_VERSION_MAJOR: u16 = 1;
/// Current format minor version.
pub const ODF_VERSION_MINOR: u16 = 0;

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------

/// Feature flag — the volume stores the domain `PersistedState` payload in
/// the system inode #0 (inlined bincode blob).  Every ODF v1 volume carries
/// this flag.
pub const FEATURE_INCOMPAT_PAYLOAD_INODE: u64 = 1 << 0;
/// Feature flag — per-block CRC32C checksums are verified on read.
pub const FEATURE_COMPAT_BLOCK_CHECKSUMS: u64 = 1 << 0;
/// Feature flag — redundant superblocks are written at N/2 and N-1.
pub const FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS: u64 = 1 << 1;

/// Feature flag — the volume uses the block-group layout (data region
/// carved into N sub-regions, each with its own bitmap block).  This is
/// the activation flag for [`super::grouped`].
pub const FEATURE_INCOMPAT_BLOCK_GROUPS: u64 = 1 << 1;

/// Bit-wise union of all incompatible features this build understands.
pub const SUPPORTED_INCOMPAT: u64 =
    FEATURE_INCOMPAT_PAYLOAD_INODE | FEATURE_INCOMPAT_BLOCK_GROUPS;
/// Bit-wise union of all read-only-compat features this build understands.
pub const SUPPORTED_RO_COMPAT: u64 = 0;

// ---------------------------------------------------------------------------
// Parameters + computed geometry
// ---------------------------------------------------------------------------

/// User-controlled parameters for [`LayoutGeometry::plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutParams {
    /// Total number of 4 KiB blocks the volume occupies.
    pub total_blocks: u64,
    /// Maximum number of on-disk inodes the inode table must hold.
    pub inode_count: u64,
    /// Number of blocks reserved for the on-disk journal region.
    pub journal_blocks: u64,
}

impl LayoutParams {
    /// Sensible defaults derived solely from the volume size.
    pub fn with_defaults(total_blocks: u64) -> Self {
        Self {
            total_blocks,
            inode_count: DEFAULT_INODE_COUNT,
            journal_blocks: DEFAULT_JOURNAL_BLOCKS,
        }
    }
}

/// Resolved block positions of every layout region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutGeometry {
    pub total_blocks: u64,
    pub block_bitmap_start: u64,
    pub block_bitmap_blocks: u64,
    pub inode_bitmap_start: u64,
    pub inode_bitmap_blocks: u64,
    pub inode_table_start: u64,
    pub inode_table_blocks: u64,
    pub inode_count: u64,
    pub journal_start: u64,
    pub journal_blocks: u64,
    pub data_start: u64,
    pub data_blocks: u64,
    pub secondary_superblock_block: u64,
    pub tertiary_superblock_block: u64,
}

impl LayoutGeometry {
    /// Compute a valid layout for the given parameters or report why the
    /// volume is too small / misconfigured.
    pub fn plan(params: LayoutParams) -> CoreFsResult<Self> {
        if params.total_blocks < MIN_VOLUME_BLOCKS {
            return Err(CoreFsError::InvalidInput(format!(
                "volume too small: {} blocks, minimum is {}",
                params.total_blocks, MIN_VOLUME_BLOCKS
            )));
        }
        if params.inode_count == 0 {
            return Err(CoreFsError::InvalidInput(
                "inode_count must be greater than zero".to_string(),
            ));
        }
        let total_blocks = params.total_blocks;

        let block_bitmap_start = PRIMARY_SUPERBLOCK_BLOCK + 1;
        let block_bitmap_blocks = total_blocks.div_ceil(BITS_PER_BITMAP_BLOCK);

        let inode_bitmap_start = block_bitmap_start + block_bitmap_blocks;
        let inode_bitmap_blocks = params.inode_count.div_ceil(BITS_PER_BITMAP_BLOCK);

        let inode_table_start = inode_bitmap_start + inode_bitmap_blocks;
        let inode_table_blocks = params.inode_count.div_ceil(INODES_PER_BLOCK);

        let journal_start = inode_table_start + inode_table_blocks;
        let journal_blocks = params.journal_blocks;

        let data_start = journal_start + journal_blocks;
        if data_start + 4 > total_blocks {
            return Err(CoreFsError::InvalidInput(format!(
                "layout does not leave room for data blocks: data_start={}, total={}",
                data_start, total_blocks
            )));
        }
        let data_blocks = total_blocks - data_start;

        let secondary_superblock_block = total_blocks - 1;
        let tertiary_superblock_block = total_blocks / 2;

        Ok(Self {
            total_blocks,
            block_bitmap_start,
            block_bitmap_blocks,
            inode_bitmap_start,
            inode_bitmap_blocks,
            inode_table_start,
            inode_table_blocks,
            inode_count: params.inode_count,
            journal_start,
            journal_blocks,
            data_start,
            data_blocks,
            secondary_superblock_block,
            tertiary_superblock_block,
        })
    }

    /// Byte offset (from the start of the device) of a given block number.
    pub fn block_offset(&self, block: u64) -> u64 {
        block * BLOCK_SIZE
    }

    /// `true` if `block` lies inside the data region.
    pub fn is_data_block(&self, block: u64) -> bool {
        block >= self.data_start && block < self.total_blocks
    }

    /// Block number that stores the on-disk inode record for `index`.
    pub fn inode_record_location(&self, index: u64) -> CoreFsResult<(u64, u64)> {
        if index >= self.inode_count {
            return Err(CoreFsError::InvalidInput(format!(
                "inode index {index} out of range (max {})",
                self.inode_count
            )));
        }
        let block = self.inode_table_start + index / INODES_PER_BLOCK;
        let offset_in_block = (index % INODES_PER_BLOCK) * INODE_SIZE;
        Ok((block, offset_in_block))
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
