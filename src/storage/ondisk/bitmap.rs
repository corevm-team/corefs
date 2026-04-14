// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Block- and inode-allocation bitmaps.
//!
//! A bitmap spans one or more 4 KiB blocks; each bit represents one
//! addressable unit (a block in the block bitmap, an inode in the inode
//! bitmap).  Bit 0 of byte 0 represents unit 0.

use crate::error::{CoreFsError, CoreFsResult};

use super::layout::{BITS_PER_BITMAP_BLOCK, BLOCK_SIZE};

/// A mutable bitmap spanning `blocks` contiguous 4 KiB blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    bits: Vec<u8>,
    capacity: u64,
}

impl Bitmap {
    /// Create a new zero-filled bitmap large enough for `capacity` units
    /// (rounded up to the next bitmap block).
    pub fn new(capacity: u64) -> Self {
        let blocks = capacity.div_ceil(BITS_PER_BITMAP_BLOCK).max(1);
        let bytes = (blocks * BLOCK_SIZE) as usize;
        Self {
            bits: vec![0u8; bytes],
            capacity,
        }
    }

    /// Load a bitmap from a byte buffer whose length is a multiple of
    /// [`BLOCK_SIZE`].  `capacity` defines the highest addressable bit index
    /// (useful to avoid reporting ghost units from rounding).
    pub fn from_bytes(bytes: Vec<u8>, capacity: u64) -> CoreFsResult<Self> {
        if bytes.len() as u64 % BLOCK_SIZE != 0 {
            return Err(CoreFsError::InvalidInput(format!(
                "bitmap buffer length {} is not a multiple of block size",
                bytes.len()
            )));
        }
        if (bytes.len() as u64) * 8 < capacity {
            return Err(CoreFsError::InvalidInput(format!(
                "bitmap buffer too small: {} bytes cannot hold {} bits",
                bytes.len(),
                capacity
            )));
        }
        Ok(Self {
            bits: bytes,
            capacity,
        })
    }

    /// Underlying byte view for writing to disk.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    /// Number of bitmap blocks occupied on disk.
    pub fn block_count(&self) -> u64 {
        self.bits.len() as u64 / BLOCK_SIZE
    }

    /// Highest addressable unit index + 1.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// `true` if bit `index` is set.
    pub fn is_set(&self, index: u64) -> CoreFsResult<bool> {
        self.check_index(index)?;
        let (byte, bit) = split(index);
        Ok((self.bits[byte] >> bit) & 1 == 1)
    }

    /// Mark bit `index` as allocated.  Returns `true` if the bit changed
    /// from 0 to 1 (i.e. a fresh allocation).
    pub fn set(&mut self, index: u64) -> CoreFsResult<bool> {
        self.check_index(index)?;
        let (byte, bit) = split(index);
        let mask = 1u8 << bit;
        let was_set = self.bits[byte] & mask != 0;
        self.bits[byte] |= mask;
        Ok(!was_set)
    }

    /// Mark bit `index` as free.  Returns `true` if the bit changed from
    /// 1 to 0.
    pub fn clear(&mut self, index: u64) -> CoreFsResult<bool> {
        self.check_index(index)?;
        let (byte, bit) = split(index);
        let mask = 1u8 << bit;
        let was_set = self.bits[byte] & mask != 0;
        self.bits[byte] &= !mask;
        Ok(was_set)
    }

    /// Find the lowest free bit in `[start, capacity)` and mark it
    /// allocated.  Returns its index or `None` if the bitmap is full.
    pub fn allocate_first(&mut self, start: u64) -> Option<u64> {
        let limit = self.capacity;
        let mut i = start;
        while i < limit {
            let (byte, bit) = split(i);
            let b = self.bits[byte];
            // Fast path: skip a fully-allocated byte.
            if b == 0xFF && bit == 0 {
                i = i.saturating_add(8);
                continue;
            }
            if (b >> bit) & 1 == 0 {
                self.bits[byte] = b | (1u8 << bit);
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Count the number of set bits within the addressable capacity.
    pub fn popcount(&self) -> u64 {
        let mut count = 0u64;
        for i in 0..self.capacity {
            let (byte, bit) = split(i);
            if (self.bits[byte] >> bit) & 1 == 1 {
                count += 1;
            }
        }
        count
    }

    fn check_index(&self, index: u64) -> CoreFsResult<()> {
        if index >= self.capacity {
            return Err(CoreFsError::InvalidInput(format!(
                "bitmap index {index} out of range (capacity {})",
                self.capacity
            )));
        }
        Ok(())
    }
}

#[inline]
fn split(index: u64) -> (usize, u32) {
    ((index / 8) as usize, (index % 8) as u32)
}

#[cfg(test)]
#[path = "bitmap_tests.rs"]
mod tests;
