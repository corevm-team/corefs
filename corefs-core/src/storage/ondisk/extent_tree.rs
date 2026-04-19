// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Indirect-block extent chains for large files.
//!
//! An on-disk inode can describe up to [`super::inode::MAX_INLINE_EXTENTS`] extents
//! directly.  Files that need more extents set [`super::inode::FLAG_HAS_EXTENT_INDEX`]
//! and point at a chain of [`ExtentIndexBlock`]s via `index_block_addr`.
//!
//! ## Index-block layout (4 KiB)
//!
//! ```text
//! offset  size  field
//!   0      4    magic           (EEBE_EEBE)
//!   4      4    entry_count
//!   8      8    next_index_block (0 = terminal)
//!  16   4064    entries[254] × (u64 physical, u32 logical, u32 length_blocks)
//! 4080    12    reserved
//! 4092     4    crc32c          (over the full 4 KiB block with CRC zeroed)
//! ```
//!
//! A chain reads linearly — the order of extents across the chain is the
//! order the file exposes them.  `ExtentChain::read` and
//! `ExtentChain::write` encapsulate the stitching.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashSet;

use super::checksum::Crc32c;
use super::inode::Extent;
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;

/// Magic value in the first 4 bytes of each index block.
pub const INDEX_BLOCK_MAGIC: u32 = 0xEEBE_EEBE;
/// Maximum extents per index block.
pub const EXTENTS_PER_INDEX_BLOCK: usize = 254;
const CRC_OFFSET: usize = 4092;

/// In-memory image of a single index block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentIndexBlock {
    pub next_index_block: u64,
    pub extents: Vec<Extent>,
}

impl ExtentIndexBlock {
    pub fn empty() -> Self {
        Self {
            next_index_block: 0,
            extents: Vec::new(),
        }
    }

    /// Encode into a full 4 KiB block with CRC32C trailer.
    pub fn encode(&self) -> CoreFsResult<Vec<u8>> {
        if self.extents.len() > EXTENTS_PER_INDEX_BLOCK {
            return Err(CoreFsError::InvalidInput(format!(
                "extent index block holds at most {EXTENTS_PER_INDEX_BLOCK} entries, got {}",
                self.extents.len()
            )));
        }
        let mut block = vec![0u8; BLOCK_SIZE as usize];
        block[0..4].copy_from_slice(&INDEX_BLOCK_MAGIC.to_le_bytes());
        block[4..8].copy_from_slice(&(self.extents.len() as u32).to_le_bytes());
        block[8..16].copy_from_slice(&self.next_index_block.to_le_bytes());
        let mut off = 16usize;
        for ext in &self.extents {
            block[off..off + 8].copy_from_slice(&ext.physical_block.to_le_bytes());
            block[off + 8..off + 12].copy_from_slice(&ext.logical_block.to_le_bytes());
            block[off + 12..off + 16].copy_from_slice(&ext.length_blocks.to_le_bytes());
            off += 16;
        }
        let crc = Crc32c::hash(&block);
        block[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(block)
    }

    /// Decode + validate a 4 KiB index block.
    pub fn decode(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "index block must be {BLOCK_SIZE} bytes, got {}",
                block.len()
            )));
        }
        let stored = u32::from_le_bytes(block[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
        let mut zeroed = block.to_vec();
        zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&zeroed);
        if stored != expected {
            return Err(CoreFsError::State(format!(
                "extent index block CRC mismatch (stored=0x{stored:08X}, expected=0x{expected:08X})"
            )));
        }
        let magic = u32::from_le_bytes(block[0..4].try_into().unwrap());
        if magic != INDEX_BLOCK_MAGIC {
            return Err(CoreFsError::State(format!(
                "extent index block bad magic 0x{magic:08X}"
            )));
        }
        let count = u32::from_le_bytes(block[4..8].try_into().unwrap()) as usize;
        if count > EXTENTS_PER_INDEX_BLOCK {
            return Err(CoreFsError::State(format!(
                "extent index block declares {count} entries, more than {EXTENTS_PER_INDEX_BLOCK}"
            )));
        }
        let next_index_block = u64::from_le_bytes(block[8..16].try_into().unwrap());
        let mut extents = Vec::with_capacity(count);
        let mut off = 16usize;
        for _ in 0..count {
            let physical_block = u64::from_le_bytes(block[off..off + 8].try_into().unwrap());
            let logical_block = u32::from_le_bytes(block[off + 8..off + 12].try_into().unwrap());
            let length_blocks = u32::from_le_bytes(block[off + 12..off + 16].try_into().unwrap());
            extents.push(Extent {
                logical_block,
                length_blocks,
                physical_block,
            });
            off += 16;
        }
        Ok(Self {
            next_index_block,
            extents,
        })
    }
}

/// Walks the full extent chain of an inode.
pub struct ExtentChain;

impl ExtentChain {
    /// Read every extent reachable from `root_block` (inclusive).  Returns
    /// a flat list in on-disk order.
    pub fn read_chain(device: &dyn BlockDevice, root_block: u64) -> CoreFsResult<Vec<Extent>> {
        if root_block == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut cursor = root_block;
        let mut visited = HashSet::new();
        while cursor != 0 {
            if !visited.insert(cursor) {
                return Err(CoreFsError::State(format!(
                    "extent chain: loop detected at block {cursor}"
                )));
            }
            let buf = device.read_at(cursor * BLOCK_SIZE, BLOCK_SIZE)?;
            let blk = ExtentIndexBlock::decode(&buf)?;
            out.extend(blk.extents);
            cursor = blk.next_index_block;
        }
        Ok(out)
    }

    /// Return every index-block address reachable from `root_block`,
    /// including the root block.
    pub fn read_chain_blocks(device: &dyn BlockDevice, root_block: u64) -> CoreFsResult<Vec<u64>> {
        if root_block == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut cursor = root_block;
        let mut visited = HashSet::new();
        while cursor != 0 {
            if !visited.insert(cursor) {
                return Err(CoreFsError::State(format!(
                    "extent chain: loop detected at block {cursor}"
                )));
            }
            out.push(cursor);
            let buf = device.read_at(cursor * BLOCK_SIZE, BLOCK_SIZE)?;
            let blk = ExtentIndexBlock::decode(&buf)?;
            cursor = blk.next_index_block;
        }
        Ok(out)
    }

    /// Write `extents` as a fresh chain using blocks pre-allocated by the
    /// caller via `reserve_blocks`.  The first block in `reserve_blocks`
    /// becomes the chain root; subsequent entries hold overflow.  Returns
    /// the list of blocks that ended up in use so callers can TRIM the
    /// rest.
    pub fn write_chain(
        device: &mut dyn BlockDevice,
        extents: &[Extent],
        reserve_blocks: &[u64],
    ) -> CoreFsResult<Vec<u64>> {
        if extents.is_empty() {
            return Ok(Vec::new());
        }
        let needed = extents.len().div_ceil(EXTENTS_PER_INDEX_BLOCK);
        if reserve_blocks.len() < needed {
            return Err(CoreFsError::InvalidInput(format!(
                "write_chain: need {needed} index blocks, got {}",
                reserve_blocks.len()
            )));
        }
        let mut used = Vec::with_capacity(needed);
        for i in 0..needed {
            let start = i * EXTENTS_PER_INDEX_BLOCK;
            let end = (start + EXTENTS_PER_INDEX_BLOCK).min(extents.len());
            let next = if i + 1 < needed {
                reserve_blocks[i + 1]
            } else {
                0
            };
            let blk = ExtentIndexBlock {
                next_index_block: next,
                extents: extents[start..end].to_vec(),
            };
            let bytes = blk.encode()?;
            device.write_at(reserve_blocks[i] * BLOCK_SIZE, &bytes)?;
            used.push(reserve_blocks[i]);
        }
        Ok(used)
    }

    /// Compute how many index blocks are needed to hold `extent_count`
    /// extents in a chain.
    pub fn index_blocks_needed(extent_count: usize) -> usize {
        extent_count.div_ceil(EXTENTS_PER_INDEX_BLOCK)
    }
}

#[cfg(test)]
#[path = "extent_tree_tests.rs"]
mod tests;
