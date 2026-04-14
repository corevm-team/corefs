// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Locality-preserving allocator on top of a [`BlockGroupTable`] (P2.7).
//!
//! Each block group owns its own [`super::bitmap::Bitmap`] for data-block allocation;
//! the inode bitmap stays global (one allocator-wide [`super::bitmap::Bitmap`]).  The
//! allocator's `allocate_*_near` family of methods picks the home group
//! of an inode slot first and only spills to other groups when the home
//! group can't satisfy the request.  This minimises the seek distance
//! between an inode record (logical home group) and its data extents,
//! which is the core motivation for block groups in ext4 / XFS.
//!
//! The on-disk format is described in [`super::block_group`].  This allocator
//! operates entirely in memory; persistence is the caller's
//! responsibility (write each per-group bitmap back at
//! `descriptor.bitmap_block` and refresh the [`BlockGroupTable`]).

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::bitmap::Bitmap;
use super::block_group::{BlockGroupDescriptor, BlockGroupTable};
use super::checksum::Crc32c;
use super::inode::Extent;
use crate::error::{CoreFsError, CoreFsResult};

/// Stateful multi-group allocator.
#[derive(Debug)]
pub struct MultiGroupAllocator {
    /// One bitmap per group (same order as `table.groups`).
    group_bitmaps: Vec<Bitmap>,
    /// Single inode bitmap shared across all groups.
    inode_bitmap: Bitmap,
    table: BlockGroupTable,
    inode_hint: u64,
    reserved_inode_floor: u64,
}

impl MultiGroupAllocator {
    /// Create a new allocator.  `group_bitmaps[i]` must be sized for
    /// `table.groups[i].data_blocks` bits.
    pub fn new(
        table: BlockGroupTable,
        group_bitmaps: Vec<Bitmap>,
        inode_bitmap: Bitmap,
        reserved_inode_floor: u64,
    ) -> CoreFsResult<Self> {
        if table.groups.len() != group_bitmaps.len() {
            return Err(CoreFsError::InvalidInput(format!(
                "MultiGroupAllocator: table has {} groups but {} bitmaps were given",
                table.groups.len(),
                group_bitmaps.len()
            )));
        }
        for (i, (g, b)) in table.groups.iter().zip(group_bitmaps.iter()).enumerate() {
            if b.capacity() < g.data_blocks {
                return Err(CoreFsError::InvalidInput(format!(
                    "MultiGroupAllocator: group {i} bitmap capacity {} < data_blocks {}",
                    b.capacity(),
                    g.data_blocks
                )));
            }
        }
        Ok(Self {
            group_bitmaps,
            inode_bitmap,
            table,
            inode_hint: reserved_inode_floor,
            reserved_inode_floor,
        })
    }

    /// Allocate a contiguous extent of `count` blocks, preferring the
    /// home group of `inode_slot`.  Falls back to round-robin across the
    /// remaining groups when the home group is full.
    pub fn allocate_near(&mut self, count: u64, inode_slot: u64) -> CoreFsResult<Extent> {
        if count == 0 {
            return Err(CoreFsError::InvalidInput(
                "allocator: cannot allocate zero blocks".into(),
            ));
        }
        let home = self.table.group_for_inode(inode_slot).unwrap_or(0);
        let group_count = self.group_bitmaps.len();
        for offset in 0..group_count {
            let g = (home + offset) % group_count;
            if let Some(extent) = self.try_allocate_in_group(g, count) {
                return Ok(extent);
            }
        }
        Err(CoreFsError::State(format!(
            "MultiGroupAllocator: no group can fit {count} contiguous blocks"
        )))
    }

    /// Free a previously-allocated extent.  Determines its home group
    /// from the table.
    pub fn free_extent(&mut self, ext: Extent) -> CoreFsResult<()> {
        if ext.length_blocks == 0 {
            return Ok(());
        }
        let group = self
            .table
            .group_for_block(ext.physical_block)
            .ok_or_else(|| {
                CoreFsError::InvalidInput(format!(
                    "free_extent: block {} belongs to no group",
                    ext.physical_block
                ))
            })?;
        let g = &self.table.groups[group];
        let bm = &mut self.group_bitmaps[group];
        for i in 0..u64::from(ext.length_blocks) {
            let block = ext.physical_block + i;
            let local = block - g.data_start;
            bm.clear(local)?;
        }
        Ok(())
    }

    /// Allocate the next free inode slot >= the reserved floor.
    pub fn allocate_inode(&mut self) -> CoreFsResult<u64> {
        let idx = self
            .inode_bitmap
            .allocate_first(self.inode_hint)
            .ok_or_else(|| CoreFsError::State("MultiGroupAllocator: inode table full".into()))?;
        self.inode_hint = idx + 1;
        Ok(idx)
    }

    /// Total free data blocks across every group.
    pub fn total_free_data_blocks(&self) -> u64 {
        self.group_bitmaps
            .iter()
            .zip(self.table.groups.iter())
            .map(|(bm, g)| g.data_blocks - bm.popcount())
            .sum()
    }

    /// Free data blocks in a specific group.
    pub fn free_data_blocks_in(&self, group: usize) -> CoreFsResult<u64> {
        let g = self
            .table
            .groups
            .get(group)
            .ok_or_else(|| CoreFsError::InvalidInput(format!("unknown group {group}")))?;
        let bm = &self.group_bitmaps[group];
        Ok(g.data_blocks - bm.popcount())
    }

    /// Refresh the descriptor cache (`free_blocks` + `bitmap_crc`) for
    /// every group from the current bitmap state.  Callers do this
    /// before persisting the table back to disk.
    pub fn refresh_descriptors(&mut self) {
        for (g, bm) in self.table.groups.iter_mut().zip(self.group_bitmaps.iter()) {
            g.free_blocks = (g.data_blocks - bm.popcount()) as u32;
            g.bitmap_crc = Crc32c::hash(bm.as_bytes());
        }
    }

    /// Consume the allocator and return its components for persistence.
    pub fn into_parts(self) -> (BlockGroupTable, Vec<Bitmap>, Bitmap) {
        (self.table, self.group_bitmaps, self.inode_bitmap)
    }

    /// Borrow the table (for inspection / encoding).
    pub fn table(&self) -> &BlockGroupTable {
        &self.table
    }

    /// Borrow a single group's bitmap.
    pub fn group_bitmap(&self, group: usize) -> Option<&Bitmap> {
        self.group_bitmaps.get(group)
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    fn try_allocate_in_group(&mut self, group: usize, count: u64) -> Option<Extent> {
        let g = self.table.groups[group];
        let bm = &mut self.group_bitmaps[group];
        if bm.capacity() < g.data_blocks {
            return None;
        }
        let mut start = 0u64;
        while start + count <= g.data_blocks {
            let mut ok = true;
            for i in 0..count {
                if bm.is_set(start + i).unwrap_or(true) {
                    start = start + i + 1;
                    ok = false;
                    break;
                }
            }
            if ok {
                for i in 0..count {
                    let _ = bm.set(start + i);
                }
                return Some(Extent {
                    logical_block: 0,
                    length_blocks: count as u32,
                    physical_block: g.data_start + start,
                });
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "multi_group_allocator_tests.rs"]
mod tests;
