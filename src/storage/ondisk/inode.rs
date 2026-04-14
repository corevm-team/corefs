// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Fixed-size on-disk inode record (256 bytes).
//!
//! Layout of a single record:
//!
//! ```text
//! offset  size  field
//!   0      2    version
//!   2      2    kind            (0 = unused, 1 = file, 2 = dir, 3 = symlink,
//!                                 4 = system payload)
//!   4      4    mode            (POSIX permission bits)
//!   8      4    uid
//!  12      4    gid
//!  16      4    link_count
//!  20      4    flags
//!  24      8    size_bytes
//!  32      8    blocks_allocated
//!  40      8    created_at
//!  48      8    modified_at
//!  56      8    changed_at
//!  64      8    accessed_at
//!  72      8    generation
//!  80      4    extent_count
//!  84      4    reserved0
//!  88    128    extents[8] × (u64 physical, u32 logical, u32 length_blocks)
//! 216     36    reserved_tail
//! 252      4    crc32c          (over the 256-byte record with crc bytes = 0)
//! ```

use super::checksum::Crc32c;
use crate::error::{CoreFsError, CoreFsResult};

/// Size of an on-disk inode record.
pub const INODE_RECORD_SIZE: usize = 256;
/// Byte offset of the CRC32C checksum field inside the record.
pub const INODE_CHECKSUM_OFFSET: usize = INODE_RECORD_SIZE - 4;
/// Maximum number of extents storable directly in one record.
pub const MAX_INLINE_EXTENTS: usize = 8;

/// Logical kind of a stored on-disk inode.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDiskKind {
    Unused = 0,
    File = 1,
    Directory = 2,
    Symlink = 3,
    SystemPayload = 4,
}

impl OnDiskKind {
    pub fn from_u16(value: u16) -> CoreFsResult<Self> {
        match value {
            0 => Ok(Self::Unused),
            1 => Ok(Self::File),
            2 => Ok(Self::Directory),
            3 => Ok(Self::Symlink),
            4 => Ok(Self::SystemPayload),
            v => Err(CoreFsError::State(format!(
                "unknown on-disk inode kind {v}"
            ))),
        }
    }
}

/// A contiguous physical extent owned by an inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extent {
    /// Logical starting block of this extent inside the inode's file data.
    pub logical_block: u32,
    /// Length of the extent in blocks.
    pub length_blocks: u32,
    /// Physical starting block on the underlying device.
    pub physical_block: u64,
}

/// Memory image of an on-disk inode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDiskInode {
    pub version: u16,
    pub kind: OnDiskKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub link_count: u32,
    pub flags: u32,
    pub size_bytes: u64,
    pub blocks_allocated: u64,
    pub created_at: i64,
    pub modified_at: i64,
    pub changed_at: i64,
    pub accessed_at: i64,
    pub generation: u64,
    pub extents: Vec<Extent>,
}

impl OnDiskInode {
    /// Construct an unused inode slot.
    pub fn unused() -> Self {
        Self {
            version: 1,
            kind: OnDiskKind::Unused,
            mode: 0,
            uid: 0,
            gid: 0,
            link_count: 0,
            flags: 0,
            size_bytes: 0,
            blocks_allocated: 0,
            created_at: 0,
            modified_at: 0,
            changed_at: 0,
            accessed_at: 0,
            generation: 0,
            extents: Vec::new(),
        }
    }

    /// Encode into a 256-byte record.  Rejects extent lists that exceed
    /// [`MAX_INLINE_EXTENTS`] — callers must spill to an indirect block
    /// before writing (not supported in ODF v1).
    pub fn encode(&self) -> CoreFsResult<[u8; INODE_RECORD_SIZE]> {
        if self.extents.len() > MAX_INLINE_EXTENTS {
            return Err(CoreFsError::InvalidInput(format!(
                "ODF v1 supports at most {} inline extents, got {}",
                MAX_INLINE_EXTENTS,
                self.extents.len()
            )));
        }
        let mut rec = [0u8; INODE_RECORD_SIZE];
        rec[0..2].copy_from_slice(&self.version.to_le_bytes());
        rec[2..4].copy_from_slice(&(self.kind as u16).to_le_bytes());
        rec[4..8].copy_from_slice(&self.mode.to_le_bytes());
        rec[8..12].copy_from_slice(&self.uid.to_le_bytes());
        rec[12..16].copy_from_slice(&self.gid.to_le_bytes());
        rec[16..20].copy_from_slice(&self.link_count.to_le_bytes());
        rec[20..24].copy_from_slice(&self.flags.to_le_bytes());
        rec[24..32].copy_from_slice(&self.size_bytes.to_le_bytes());
        rec[32..40].copy_from_slice(&self.blocks_allocated.to_le_bytes());
        rec[40..48].copy_from_slice(&self.created_at.to_le_bytes());
        rec[48..56].copy_from_slice(&self.modified_at.to_le_bytes());
        rec[56..64].copy_from_slice(&self.changed_at.to_le_bytes());
        rec[64..72].copy_from_slice(&self.accessed_at.to_le_bytes());
        rec[72..80].copy_from_slice(&self.generation.to_le_bytes());
        rec[80..84].copy_from_slice(&(self.extents.len() as u32).to_le_bytes());
        // reserved0 at 84..88 stays zero.
        let mut off = 88usize;
        for ext in &self.extents {
            rec[off..off + 8].copy_from_slice(&ext.physical_block.to_le_bytes());
            rec[off + 8..off + 12].copy_from_slice(&ext.logical_block.to_le_bytes());
            rec[off + 12..off + 16].copy_from_slice(&ext.length_blocks.to_le_bytes());
            off += 16;
        }
        // Tail stays zero.  Compute CRC over record with checksum slot zero.
        let checksum = Crc32c::hash(&rec);
        rec[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&checksum.to_le_bytes());
        Ok(rec)
    }

    /// Decode from a 256-byte record.  Validates the CRC32C checksum.
    pub fn decode(rec: &[u8]) -> CoreFsResult<Self> {
        if rec.len() != INODE_RECORD_SIZE {
            return Err(CoreFsError::InvalidInput(format!(
                "inode record must be {INODE_RECORD_SIZE} bytes, got {}",
                rec.len()
            )));
        }
        let stored = u32::from_le_bytes(
            rec[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        let mut buf = [0u8; INODE_RECORD_SIZE];
        buf.copy_from_slice(rec);
        buf[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&buf);
        if expected != stored {
            return Err(CoreFsError::State(format!(
                "inode checksum mismatch: stored=0x{stored:08X} expected=0x{expected:08X}"
            )));
        }

        let version = u16::from_le_bytes(rec[0..2].try_into().unwrap());
        let kind = OnDiskKind::from_u16(u16::from_le_bytes(rec[2..4].try_into().unwrap()))?;
        let mode = u32::from_le_bytes(rec[4..8].try_into().unwrap());
        let uid = u32::from_le_bytes(rec[8..12].try_into().unwrap());
        let gid = u32::from_le_bytes(rec[12..16].try_into().unwrap());
        let link_count = u32::from_le_bytes(rec[16..20].try_into().unwrap());
        let flags = u32::from_le_bytes(rec[20..24].try_into().unwrap());
        let size_bytes = u64::from_le_bytes(rec[24..32].try_into().unwrap());
        let blocks_allocated = u64::from_le_bytes(rec[32..40].try_into().unwrap());
        let created_at = i64::from_le_bytes(rec[40..48].try_into().unwrap());
        let modified_at = i64::from_le_bytes(rec[48..56].try_into().unwrap());
        let changed_at = i64::from_le_bytes(rec[56..64].try_into().unwrap());
        let accessed_at = i64::from_le_bytes(rec[64..72].try_into().unwrap());
        let generation = u64::from_le_bytes(rec[72..80].try_into().unwrap());
        let extent_count = u32::from_le_bytes(rec[80..84].try_into().unwrap()) as usize;
        if extent_count > MAX_INLINE_EXTENTS {
            return Err(CoreFsError::State(format!(
                "inode declares {extent_count} extents, more than the {MAX_INLINE_EXTENTS} inline slots"
            )));
        }
        let mut extents = Vec::with_capacity(extent_count);
        let mut off = 88usize;
        for _ in 0..extent_count {
            let physical_block = u64::from_le_bytes(rec[off..off + 8].try_into().unwrap());
            let logical_block =
                u32::from_le_bytes(rec[off + 8..off + 12].try_into().unwrap());
            let length_blocks =
                u32::from_le_bytes(rec[off + 12..off + 16].try_into().unwrap());
            extents.push(Extent {
                logical_block,
                length_blocks,
                physical_block,
            });
            off += 16;
        }

        Ok(Self {
            version,
            kind,
            mode,
            uid,
            gid,
            link_count,
            flags,
            size_bytes,
            blocks_allocated,
            created_at,
            modified_at,
            changed_at,
            accessed_at,
            generation,
            extents,
        })
    }
}

#[cfg(test)]
#[path = "inode_tests.rs"]
mod tests;
