// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Block-group descriptors and table (P2.7).
//!
//! In ODF v1 the data region is a single flat zone bitmap-managed by
//! one [`super::bitmap::Bitmap`].  The block-group extension partitions
//! the data region into multiple sub-zones, each with:
//!
//! * its own bitmap block (one 4 KiB block can address up to 32 768
//!   data blocks = 128 MiB per group),
//! * a "home" inode-slot range to drive locality-preserving allocation,
//! * cached `free_blocks` and `bitmap_crc` so admin tools can scan the
//!   table without touching every bitmap.
//!
//! The descriptor table itself is a single 4 KiB block holding up to
//! [`MAX_GROUPS_PER_TABLE`] descriptors and a CRC32C trailer.  It lives
//! at a position pointed to by a future superblock field
//! `block_group_table_block` and is only consulted when the volume
//! advertises a `FEATURE_INCOMPAT_BLOCK_GROUPS` flag — ODF v1 volumes
//! created by [`super::volume::format_device`] continue to operate in
//! single-group mode.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::checksum::Crc32c;
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};

/// Magic value at the head of a [`BlockGroupTable`].
pub const BLOCK_GROUP_TABLE_MAGIC: u32 = u32::from_le_bytes(*b"BGRP");
/// Bytes per encoded [`BlockGroupDescriptor`].
pub const DESCRIPTOR_BYTES: usize = 48;
/// Maximum number of groups encoded in one descriptor table block
/// (4 KiB minus a 16-byte header and a 4-byte CRC trailer, divided by
/// the 48-byte descriptor record).
pub const MAX_GROUPS_PER_TABLE: usize = (BLOCK_SIZE as usize - 20) / DESCRIPTOR_BYTES;

const HEADER_BYTES: usize = 16;
const CRC_OFFSET: usize = BLOCK_SIZE as usize - 4;

/// Per-group on-disk descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockGroupDescriptor {
    /// First data block managed by this group (absolute block address).
    pub data_start: u64,
    /// Number of data blocks in this group.
    pub data_blocks: u64,
    /// Absolute block number of this group's allocation bitmap.
    pub bitmap_block: u64,
    /// First inode-slot index whose "home" is this group (for locality).
    pub inode_range_start: u64,
    /// Number of inode slots that belong to this group.
    pub inode_range_count: u64,
    /// Cached free-block count for fast admin reporting.
    pub free_blocks: u32,
    /// CRC32C of the group's bitmap block.
    pub bitmap_crc: u32,
}

impl BlockGroupDescriptor {
    fn write_to(&self, buf: &mut [u8]) {
        buf[0..8].copy_from_slice(&self.data_start.to_le_bytes());
        buf[8..16].copy_from_slice(&self.data_blocks.to_le_bytes());
        buf[16..24].copy_from_slice(&self.bitmap_block.to_le_bytes());
        buf[24..32].copy_from_slice(&self.inode_range_start.to_le_bytes());
        buf[32..40].copy_from_slice(&self.inode_range_count.to_le_bytes());
        buf[40..44].copy_from_slice(&self.free_blocks.to_le_bytes());
        buf[44..48].copy_from_slice(&self.bitmap_crc.to_le_bytes());
    }

    fn read_from(buf: &[u8]) -> CoreFsResult<Self> {
        if buf.len() < DESCRIPTOR_BYTES {
            return Err(CoreFsError::State(
                "block group descriptor: buffer too short".into(),
            ));
        }
        Ok(Self {
            data_start: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            data_blocks: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            bitmap_block: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            inode_range_start: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            inode_range_count: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
            free_blocks: u32::from_le_bytes(buf[40..44].try_into().unwrap()),
            bitmap_crc: u32::from_le_bytes(buf[44..48].try_into().unwrap()),
        })
    }
}

/// 4 KiB on-disk block-group descriptor table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockGroupTable {
    pub groups: Vec<BlockGroupDescriptor>,
}

impl BlockGroupTable {
    pub fn new(groups: Vec<BlockGroupDescriptor>) -> CoreFsResult<Self> {
        if groups.len() > MAX_GROUPS_PER_TABLE {
            return Err(CoreFsError::InvalidInput(format!(
                "block group table: {} groups exceed {} max",
                groups.len(),
                MAX_GROUPS_PER_TABLE
            )));
        }
        // Sanity checks: groups must not overlap on disk.
        let mut sorted = groups.clone();
        sorted.sort_by_key(|g| g.data_start);
        for w in sorted.windows(2) {
            let prev_end = w[0].data_start + w[0].data_blocks;
            if prev_end > w[1].data_start {
                return Err(CoreFsError::InvalidInput(format!(
                    "block group table: overlapping groups at {} and {}",
                    w[0].data_start, w[1].data_start
                )));
            }
        }
        Ok(Self { groups })
    }

    /// Encode into a full 4 KiB block.
    pub fn encode(&self) -> CoreFsResult<Vec<u8>> {
        let mut block = vec![0u8; BLOCK_SIZE as usize];
        block[0..4].copy_from_slice(&BLOCK_GROUP_TABLE_MAGIC.to_le_bytes());
        block[4..8].copy_from_slice(&(self.groups.len() as u32).to_le_bytes());
        // bytes 8..16 reserved.
        for (i, g) in self.groups.iter().enumerate() {
            let off = HEADER_BYTES + i * DESCRIPTOR_BYTES;
            g.write_to(&mut block[off..off + DESCRIPTOR_BYTES]);
        }
        let crc = Crc32c::hash(&block);
        block[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(block)
    }

    /// Decode + validate.
    pub fn decode(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "block group table: wrong length {}",
                block.len()
            )));
        }
        let stored = u32::from_le_bytes(block[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
        let mut zeroed = block.to_vec();
        zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&zeroed);
        if stored != expected {
            return Err(CoreFsError::State("block group table: CRC mismatch".into()));
        }
        let magic = u32::from_le_bytes(block[0..4].try_into().unwrap());
        if magic != BLOCK_GROUP_TABLE_MAGIC {
            return Err(CoreFsError::State(format!(
                "block group table: bad magic 0x{magic:08X}"
            )));
        }
        let count = u32::from_le_bytes(block[4..8].try_into().unwrap()) as usize;
        if count > MAX_GROUPS_PER_TABLE {
            return Err(CoreFsError::State(format!(
                "block group table: claims {count} groups, more than {MAX_GROUPS_PER_TABLE}"
            )));
        }
        let mut groups = Vec::with_capacity(count);
        for i in 0..count {
            let off = HEADER_BYTES + i * DESCRIPTOR_BYTES;
            groups.push(BlockGroupDescriptor::read_from(
                &block[off..off + DESCRIPTOR_BYTES],
            )?);
        }
        Self::new(groups)
    }

    /// Index of the group whose home inode range covers `inode_slot`,
    /// or `None` if the slot is outside every group's range.
    pub fn group_for_inode(&self, inode_slot: u64) -> Option<usize> {
        self.groups.iter().position(|g| {
            inode_slot >= g.inode_range_start
                && inode_slot < g.inode_range_start + g.inode_range_count
        })
    }

    /// Index of the group that owns `data_block`, or `None` if the
    /// block is outside every group's range.
    pub fn group_for_block(&self, data_block: u64) -> Option<usize> {
        self.groups
            .iter()
            .position(|g| data_block >= g.data_start && data_block < g.data_start + g.data_blocks)
    }
}

#[cfg(test)]
#[path = "block_group_tests.rs"]
mod tests;
