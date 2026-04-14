// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Per-inode attribute block — stores the serialized domain inode
//! metadata (path, FileMetadata, tags, ACLs, timestamps) alongside each
//! on-disk inode.  The attr block's physical address is referenced by
//! [`crate::storage::ondisk::inode::OnDiskInode::xattr_block_addr`].
//!
//! ## Layout (4 KiB)
//!
//! ```text
//! offset  size  field
//!   0      4    magic           (0xA771_B10C)
//!   4      4    payload_len
//!   8      8    next_attr_block (0 = terminal — for future chaining)
//!  16    var    payload bytes   (bincode-serialized `Inode` or similar)
//! 4092     4    crc32c          (over the full block with CRC zeroed)
//! ```

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::checksum::Crc32c;
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};

/// Magic value in the first 4 bytes of every attr block.
pub const ATTR_BLOCK_MAGIC: u32 = 0xA771_B10C;
const CRC_OFFSET: usize = 4092;
const HEADER_BYTES: usize = 16;
/// Maximum payload bytes that fit in a single attr block.
pub const ATTR_BLOCK_CAPACITY: usize = CRC_OFFSET - HEADER_BYTES;

/// Memory image of an attr block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrBlock {
    pub next_attr_block: u64,
    pub payload: Vec<u8>,
}

impl AttrBlock {
    pub fn new(payload: Vec<u8>) -> Self {
        Self {
            next_attr_block: 0,
            payload,
        }
    }

    pub fn encode(&self) -> CoreFsResult<Vec<u8>> {
        if self.payload.len() > ATTR_BLOCK_CAPACITY {
            return Err(CoreFsError::InvalidInput(format!(
                "attr block payload {} > capacity {}",
                self.payload.len(),
                ATTR_BLOCK_CAPACITY
            )));
        }
        let mut block = vec![0u8; BLOCK_SIZE as usize];
        block[0..4].copy_from_slice(&ATTR_BLOCK_MAGIC.to_le_bytes());
        block[4..8].copy_from_slice(&(self.payload.len() as u32).to_le_bytes());
        block[8..16].copy_from_slice(&self.next_attr_block.to_le_bytes());
        block[HEADER_BYTES..HEADER_BYTES + self.payload.len()].copy_from_slice(&self.payload);
        let crc = Crc32c::hash(&block);
        block[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(block)
    }

    pub fn decode(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "attr block: wrong length {}",
                block.len()
            )));
        }
        let stored = u32::from_le_bytes(block[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
        let mut zeroed = block.to_vec();
        zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&zeroed);
        if stored != expected {
            return Err(CoreFsError::State("attr block CRC mismatch".into()));
        }
        let magic = u32::from_le_bytes(block[0..4].try_into().unwrap());
        if magic != ATTR_BLOCK_MAGIC {
            return Err(CoreFsError::State(format!(
                "attr block: bad magic 0x{magic:08X}"
            )));
        }
        let payload_len = u32::from_le_bytes(block[4..8].try_into().unwrap()) as usize;
        if payload_len > ATTR_BLOCK_CAPACITY {
            return Err(CoreFsError::State(format!(
                "attr block: payload_len {payload_len} exceeds capacity"
            )));
        }
        let next_attr_block = u64::from_le_bytes(block[8..16].try_into().unwrap());
        let payload = block[HEADER_BYTES..HEADER_BYTES + payload_len].to_vec();
        Ok(Self {
            next_attr_block,
            payload,
        })
    }
}

#[cfg(test)]
#[path = "attr_block_tests.rs"]
mod tests;
