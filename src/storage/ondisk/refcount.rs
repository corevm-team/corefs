// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Physical copy-on-write infrastructure — per-data-block reference
//! counts (D.9).
//!
//! Today ODF treats every data block as owned by exactly one inode.
//! Physical CoW lifts that invariant by giving each data block an
//! on-disk `u16` reference counter; two inodes can then share a block
//! until one of them writes, at which point the writer allocates a
//! fresh block, copies the content, decrements the shared block's
//! refcount and retargets its extent to the new location.
//!
//! ## On-disk layout
//!
//! The refcount region is a contiguous run of 4 KiB blocks.  Each
//! block holds up to [`COUNTS_PER_BLOCK`] `u16` counters plus a 4-byte
//! CRC32C trailer:
//!
//! ```text
//! offset  size  meaning
//!   0    4088   u16 counters (little-endian, indexed by data-block
//!                offset within this block's slice)
//! 4088     4    reserved
//! 4092     4    crc32c over the full block with CRC slot zeroed
//! ```
//!
//! A volume that uses CoW carries the [`FEATURE_INCOMPAT_PHYSICAL_COW`]
//! flag in its superblock and reserves
//! [`RefCountTable::blocks_needed`] blocks for the counter region.
//!
//! ## In-memory model
//!
//! [`RefCountTable`] owns a flat `Vec<u16>` sized to the data-region
//! capacity, transparently loadable from or persistable to its on-disk
//! encoding via [`Self::encode`] / [`Self::decode`].  Every counter
//! change goes through [`Self::acquire`] / [`Self::release`], which
//! return the new value and surface overflow / underflow as errors.
//!
//! ## Feature flag
//!
//! [`FEATURE_INCOMPAT_PHYSICAL_COW`] is reserved bit `1 << 2` and is
//! included in [`crate::storage::ondisk::layout::SUPPORTED_INCOMPAT`]
//! so grouped or single-group volumes can opt in without a format
//! break.

use super::checksum::Crc32c;
use super::inode::Extent;
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};

/// Feature flag that marks a volume as using the physical-CoW
/// refcount table.  Without this flag no counter region is allocated
/// and the allocator treats every block as having an implicit refcount
/// of 1.
pub const FEATURE_INCOMPAT_PHYSICAL_COW: u64 = 1 << 2;

/// Number of `u16` counters that fit into one 4 KiB refcount block,
/// after the CRC trailer.
pub const COUNTS_PER_BLOCK: usize = 4088 / 2;
const CRC_OFFSET: usize = 4092;

/// Maximum shares per block (saturated — exceeding this returns a
/// clear `PolicyViolation` error).
pub const MAX_REFCOUNT: u16 = u16::MAX;

/// In-memory refcount table covering `capacity` data blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefCountTable {
    counters: Vec<u16>,
    capacity: u64,
}

impl RefCountTable {
    /// Fresh zero-filled table for `capacity` data blocks.
    pub fn new(capacity: u64) -> Self {
        Self {
            counters: vec![0u16; capacity as usize],
            capacity,
        }
    }

    /// Number of 4 KiB refcount blocks needed to persist `capacity`
    /// entries.
    pub fn blocks_needed(capacity: u64) -> u64 {
        (capacity as usize).div_ceil(COUNTS_PER_BLOCK) as u64
    }

    /// Logical capacity (highest addressable counter index + 1).
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Current refcount for `block`.  Returns 0 for out-of-range
    /// indices rather than erroring — callers expecting strict
    /// bounds checking should call [`Self::get_checked`].
    pub fn get(&self, block: u64) -> u16 {
        if block >= self.capacity {
            return 0;
        }
        self.counters[block as usize]
    }

    /// Strict variant of [`Self::get`] that returns an error for
    /// out-of-range indices.
    pub fn get_checked(&self, block: u64) -> CoreFsResult<u16> {
        self.check_index(block)?;
        Ok(self.counters[block as usize])
    }

    /// Mark `block` as referenced one more time.  Returns the new
    /// value.  Fails with `PolicyViolation` if the counter would
    /// overflow `MAX_REFCOUNT`.
    pub fn acquire(&mut self, block: u64) -> CoreFsResult<u16> {
        self.check_index(block)?;
        let cur = self.counters[block as usize];
        if cur == MAX_REFCOUNT {
            return Err(CoreFsError::PolicyViolation(format!(
                "refcount: block {block} would overflow u16"
            )));
        }
        self.counters[block as usize] = cur + 1;
        Ok(self.counters[block as usize])
    }

    /// Drop one reference on `block`.  Returns the new value.  Fails
    /// with `State` if the block's refcount was already zero — that
    /// is a double-free bug at the caller site.
    pub fn release(&mut self, block: u64) -> CoreFsResult<u16> {
        self.check_index(block)?;
        let cur = self.counters[block as usize];
        if cur == 0 {
            return Err(CoreFsError::State(format!(
                "refcount: block {block} released while refcount was zero"
            )));
        }
        self.counters[block as usize] = cur - 1;
        Ok(self.counters[block as usize])
    }

    /// Acquire every block in the extent.  Roll-back on failure.
    pub fn acquire_extent(&mut self, ext: Extent) -> CoreFsResult<()> {
        for i in 0..u64::from(ext.length_blocks) {
            if let Err(e) = self.acquire(ext.physical_block + i) {
                // Roll back the partial acquires we already did.
                for j in 0..i {
                    let _ = self.release(ext.physical_block + j);
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Release every block in the extent.  Blocks that fall to zero
    /// refcount are collected and returned — the caller is responsible
    /// for freeing them in the block bitmap (and optionally TRIM).
    pub fn release_extent(&mut self, ext: Extent) -> CoreFsResult<Vec<u64>> {
        let mut freed = Vec::new();
        for i in 0..u64::from(ext.length_blocks) {
            let b = ext.physical_block + i;
            let new = self.release(b)?;
            if new == 0 {
                freed.push(b);
            }
        }
        Ok(freed)
    }

    /// Total number of blocks with refcount > 0 (allocated).
    pub fn allocated(&self) -> u64 {
        self.counters.iter().filter(|c| **c > 0).count() as u64
    }

    /// Total number of blocks with refcount > 1 (shared between two
    /// or more inodes).
    pub fn shared(&self) -> u64 {
        self.counters.iter().filter(|c| **c > 1).count() as u64
    }

    /// Borrow the raw counter slice (read-only).
    pub fn counters(&self) -> &[u16] {
        &self.counters
    }

    /// Encode to a sequence of 4 KiB blocks.  Output length equals
    /// `Self::blocks_needed(capacity) * BLOCK_SIZE`.
    pub fn encode(&self) -> Vec<u8> {
        let blocks = Self::blocks_needed(self.capacity) as usize;
        let mut out = vec![0u8; blocks * BLOCK_SIZE as usize];
        for (block_idx, chunk) in self.counters.chunks(COUNTS_PER_BLOCK).enumerate() {
            let block_start = block_idx * BLOCK_SIZE as usize;
            let block = &mut out[block_start..block_start + BLOCK_SIZE as usize];
            for (i, count) in chunk.iter().enumerate() {
                block[i * 2..i * 2 + 2].copy_from_slice(&count.to_le_bytes());
            }
            let crc = Crc32c::hash(block);
            block[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        }
        out
    }

    /// Decode + verify a refcount region of length
    /// `Self::blocks_needed(capacity) * BLOCK_SIZE`.
    pub fn decode(bytes: &[u8], capacity: u64) -> CoreFsResult<Self> {
        let blocks = Self::blocks_needed(capacity) as usize;
        if bytes.len() != blocks * BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "refcount decode: expected {} bytes, got {}",
                blocks * BLOCK_SIZE as usize,
                bytes.len()
            )));
        }
        let mut counters = Vec::with_capacity(capacity as usize);
        let mut remaining = capacity as usize;
        for block_idx in 0..blocks {
            let start = block_idx * BLOCK_SIZE as usize;
            let block = &bytes[start..start + BLOCK_SIZE as usize];
            let stored =
                u32::from_le_bytes(block[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
            let mut zeroed = block.to_vec();
            zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
            let expected = Crc32c::hash(&zeroed);
            if expected != stored {
                return Err(CoreFsError::State(format!(
                    "refcount block {block_idx} CRC mismatch (stored=0x{stored:08X}, expected=0x{expected:08X})"
                )));
            }
            let take = remaining.min(COUNTS_PER_BLOCK);
            for i in 0..take {
                counters.push(u16::from_le_bytes(
                    block[i * 2..i * 2 + 2].try_into().unwrap(),
                ));
            }
            remaining -= take;
        }
        Ok(Self { counters, capacity })
    }

    fn check_index(&self, block: u64) -> CoreFsResult<()> {
        if block >= self.capacity {
            return Err(CoreFsError::InvalidInput(format!(
                "refcount: block {block} out of range (capacity {})",
                self.capacity
            )));
        }
        Ok(())
    }
}

/// High-level CoW-aware sharing helper.  Couples a [`RefCountTable`]
/// with the shared-extent semantics that the block store needs
/// (clone / cow_write / release).
#[derive(Debug)]
pub struct BlockSharing {
    table: RefCountTable,
}

/// Result of a [`BlockSharing::cow_write`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CowOutcome {
    /// The extent was the sole reference to its blocks — the caller
    /// may write in place.
    InPlace,
    /// The extent was shared — the caller must allocate the returned
    /// replacement extent, copy the data and retarget the inode
    /// there.  The old extent has already had its refcounts
    /// decremented; the `freed` list holds the blocks that dropped
    /// to refcount 0.
    MustCopy { freed: Vec<u64> },
}

impl BlockSharing {
    pub fn new(table: RefCountTable) -> Self {
        Self { table }
    }

    pub fn table(&self) -> &RefCountTable {
        &self.table
    }

    pub fn into_table(self) -> RefCountTable {
        self.table
    }

    /// Initially mark `ext` as having refcount 1 (the normal allocation
    /// path).  Fails if any block already had a non-zero refcount.
    pub fn register_fresh(&mut self, ext: Extent) -> CoreFsResult<()> {
        for i in 0..u64::from(ext.length_blocks) {
            let b = ext.physical_block + i;
            if self.table.get(b) != 0 {
                return Err(CoreFsError::State(format!(
                    "refcount: register_fresh on block {b} which already has refcount {}",
                    self.table.get(b)
                )));
            }
        }
        self.table.acquire_extent(ext)
    }

    /// Clone an extent — bump every block's refcount by one.  After
    /// the call both the source and destination inode reference the
    /// same physical blocks.
    pub fn clone_extent(&mut self, ext: Extent) -> CoreFsResult<()> {
        self.table.acquire_extent(ext)
    }

    /// Classify a pending write to `ext`:
    /// - if every block has refcount 1 → [`CowOutcome::InPlace`]
    /// - otherwise → [`CowOutcome::MustCopy`] and the *old* extent's
    ///   refcounts are decremented (releasing any blocks that were
    ///   only referenced by the writer).
    pub fn cow_write(&mut self, ext: Extent) -> CoreFsResult<CowOutcome> {
        let exclusive = (0..u64::from(ext.length_blocks))
            .all(|i| self.table.get(ext.physical_block + i) == 1);
        if exclusive {
            return Ok(CowOutcome::InPlace);
        }
        // Must copy — caller is detaching from the sharing group.
        let freed = self.table.release_extent(ext)?;
        Ok(CowOutcome::MustCopy { freed })
    }

    /// Caller-initiated release on delete / truncate.
    pub fn release(&mut self, ext: Extent) -> CoreFsResult<Vec<u64>> {
        self.table.release_extent(ext)
    }

    /// Diagnostic: how many blocks are shared by at least two inodes?
    pub fn shared_blocks(&self) -> u64 {
        self.table.shared()
    }

    /// Diagnostic: bytes saved by the current sharing pattern vs. a
    /// hypothetical "every reference is its own block" layout.
    pub fn bytes_saved(&self) -> u64 {
        self.table
            .counters()
            .iter()
            .filter(|c| **c > 1)
            .map(|c| u64::from(*c - 1) * BLOCK_SIZE)
            .sum()
    }
}

#[cfg(test)]
#[path = "refcount_tests.rs"]
mod tests;
