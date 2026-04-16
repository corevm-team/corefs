// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! On-disk block- and inode-allocator layered on top of [`super::bitmap::Bitmap`].
//!
//! The allocator operates entirely in memory: callers load the bitmaps
//! from disk, perform one or more `allocate_*` / `free_*` operations
//! against the allocator, then flush the mutated bitmap bytes back to the
//! device (together with the superblock CRC fields).  This keeps the
//! persistence envelope explicit and fsync-friendly.
//!
//! ## Strategies
//!
//! * [`AllocationStrategy::FirstFit`] — cheapest, scans from the hint
//!   forward.
//! * [`AllocationStrategy::BestFit`]  — scans the whole free map and picks
//!   the smallest gap that still fits the requested run.  Higher CPU cost,
//!   lower fragmentation under pressure.
//!
//! Both strategies allocate **contiguous** extents only.  Callers that
//! need to fill a fragmented volume request a list of smaller extents
//! instead via [`OndiskAllocator::allocate_any`].

use alloc::format;
use alloc::vec::Vec;

use super::bitmap::Bitmap;
use super::inode::Extent;
use super::layout::LayoutGeometry;
use crate::error::{CoreFsError, CoreFsResult};

/// Policy that decides how an extent is selected from the free map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationStrategy {
    FirstFit,
    BestFit,
}

/// Stateful allocator operating on an owned [`super::bitmap::Bitmap`] pair.
#[derive(Debug)]
pub struct OndiskAllocator {
    block_bitmap: Bitmap,
    inode_bitmap: Bitmap,
    data_start: u64,
    total_blocks: u64,
    strategy: AllocationStrategy,
    block_hint: u64,
    inode_hint: u64,
    /// Lowest allocatable inode slot — system inodes live below this.
    reserved_inode_floor: u64,
}

impl OndiskAllocator {
    /// Create a fresh allocator backed by the provided bitmaps.
    pub fn new(
        geom: &LayoutGeometry,
        block_bitmap: Bitmap,
        inode_bitmap: Bitmap,
        strategy: AllocationStrategy,
        reserved_inode_floor: u64,
    ) -> Self {
        Self {
            block_bitmap,
            inode_bitmap,
            data_start: geom.data_start,
            total_blocks: geom.total_blocks,
            strategy,
            block_hint: geom.data_start,
            inode_hint: reserved_inode_floor,
            reserved_inode_floor,
        }
    }

    /// Allocate a contiguous run of `count` data blocks.
    pub fn allocate_contiguous(&mut self, count: u64) -> CoreFsResult<Extent> {
        if count == 0 {
            return Err(CoreFsError::InvalidInput(
                "allocator: cannot allocate zero blocks".into(),
            ));
        }
        let start = match self.strategy {
            AllocationStrategy::FirstFit => self.find_first_fit(count)?,
            AllocationStrategy::BestFit => self.find_best_fit(count)?,
        };
        for i in 0..count {
            self.block_bitmap.set(start + i)?;
        }
        self.block_hint = start + count;
        Ok(Extent {
            logical_block: 0,
            length_blocks: count as u32,
            physical_block: start,
        })
    }

    /// Allocate `count` blocks, possibly in multiple extents if no
    /// contiguous run is available.  Returned extents have empty logical
    /// numbering; callers assign logical positions.
    pub fn allocate_any(&mut self, count: u64) -> CoreFsResult<Vec<Extent>> {
        let mut extents = Vec::new();
        let mut remaining = count;
        while remaining > 0 {
            let take = self.largest_free_run_from(self.data_start).min(remaining);
            if take == 0 {
                // Roll back anything allocated so far.
                for ext in &extents {
                    self.free_extent(*ext)?;
                }
                return Err(CoreFsError::State(format!(
                    "allocator: cannot satisfy {count}-block request"
                )));
            }
            let ext = self.allocate_contiguous(take)?;
            extents.push(ext);
            remaining -= take;
        }
        Ok(extents)
    }

    /// Return an extent to the free pool.
    pub fn free_extent(&mut self, ext: Extent) -> CoreFsResult<()> {
        for i in 0..u64::from(ext.length_blocks) {
            let block = ext.physical_block + i;
            if block < self.data_start || block >= self.total_blocks {
                return Err(CoreFsError::InvalidInput(format!(
                    "allocator: block {block} is outside the data region"
                )));
            }
            self.block_bitmap.clear(block)?;
        }
        if ext.physical_block < self.block_hint {
            self.block_hint = ext.physical_block;
        }
        Ok(())
    }

    /// Allocate a fresh inode slot >= the reserved floor.
    pub fn allocate_inode(&mut self) -> CoreFsResult<u64> {
        let idx = self
            .inode_bitmap
            .allocate_first(self.inode_hint)
            .ok_or_else(|| CoreFsError::State("allocator: inode table exhausted".into()))?;
        self.inode_hint = idx + 1;
        Ok(idx)
    }

    /// Mark a specific inode slot as allocated.  Used during recovery /
    /// mount to anchor system inodes at deterministic indices.
    pub fn reserve_inode(&mut self, index: u64) -> CoreFsResult<()> {
        self.inode_bitmap.set(index)?;
        Ok(())
    }

    /// Return an inode slot to the free pool.
    pub fn free_inode(&mut self, index: u64) -> CoreFsResult<()> {
        if index < self.reserved_inode_floor {
            return Err(CoreFsError::PolicyViolation(format!(
                "allocator: inode slot {index} is reserved and cannot be freed"
            )));
        }
        self.inode_bitmap.clear(index)?;
        if index < self.inode_hint {
            self.inode_hint = index;
        }
        Ok(())
    }

    /// Mark a specific data block as used (e.g. pre-marking data blocks
    /// that were written by `BlockStore::write_device` before calling
    /// `save_state_native`).  Silently ignores blocks outside the data
    /// region so callers can iterate extent lists safely.
    pub fn mark_used(&mut self, block: u64) -> CoreFsResult<()> {
        if block < self.data_start || block >= self.total_blocks {
            // Outside data region — skip silently.
            return Ok(());
        }
        self.block_bitmap.set(block)?;
        Ok(())
    }

    /// Current free-data-block count.
    pub fn free_data_blocks(&self) -> u64 {
        let used = self.block_bitmap.popcount().saturating_sub(self.data_start);
        (self.total_blocks - self.data_start).saturating_sub(used)
    }

    /// Current free-inode-slot count.
    pub fn free_inode_slots(&self) -> u64 {
        self.inode_bitmap.capacity() - self.inode_bitmap.popcount()
    }

    /// Consume the allocator and return the mutated bitmaps.
    pub fn into_bitmaps(self) -> (Bitmap, Bitmap) {
        (self.block_bitmap, self.inode_bitmap)
    }

    /// Borrow the bitmaps for persistence purposes.
    pub fn bitmaps(&self) -> (&Bitmap, &Bitmap) {
        (&self.block_bitmap, &self.inode_bitmap)
    }

    // ------------------------------------------------------------------
    // Scan helpers
    // ------------------------------------------------------------------

    fn find_first_fit(&self, count: u64) -> CoreFsResult<u64> {
        let mut start = self.block_hint.max(self.data_start);
        while start + count <= self.total_blocks {
            let run = self.free_run_starting_at(start, count);
            if run >= count {
                return Ok(start);
            }
            start += run + 1;
        }
        // Fall back to scan from data_start, in case hint was past the gap.
        let mut start = self.data_start;
        while start + count <= self.total_blocks {
            let run = self.free_run_starting_at(start, count);
            if run >= count {
                return Ok(start);
            }
            start += run + 1;
        }
        Err(CoreFsError::State(format!(
            "allocator: no contiguous run of {count} blocks"
        )))
    }

    fn find_best_fit(&self, count: u64) -> CoreFsResult<u64> {
        let mut best_start: Option<u64> = None;
        let mut best_len: u64 = u64::MAX;
        let mut cursor = self.data_start;
        while cursor < self.total_blocks {
            let run = self.free_run_starting_at(cursor, u64::MAX);
            if run >= count && run < best_len {
                best_len = run;
                best_start = Some(cursor);
                if run == count {
                    break; // Perfect fit — no need to keep scanning.
                }
            }
            cursor += if run == 0 { 1 } else { run + 1 };
        }
        best_start.ok_or_else(|| {
            CoreFsError::State(format!(
                "allocator: no contiguous run of {count} blocks (best-fit)"
            ))
        })
    }

    /// Length of the free run starting at `start`, capped at `cap`.
    fn free_run_starting_at(&self, start: u64, cap: u64) -> u64 {
        let mut len = 0u64;
        let mut i = start;
        while i < self.total_blocks && len < cap {
            if self.block_bitmap.is_set(i).unwrap_or(true) {
                break;
            }
            len += 1;
            i += 1;
        }
        len
    }

    fn largest_free_run_from(&self, start: u64) -> u64 {
        let mut best = 0u64;
        let mut cursor = start;
        while cursor < self.total_blocks {
            let run = self.free_run_starting_at(cursor, u64::MAX);
            best = best.max(run);
            cursor += if run == 0 { 1 } else { run + 1 };
        }
        best
    }
}

#[cfg(test)]
#[path = "allocator_tests.rs"]
mod tests;
