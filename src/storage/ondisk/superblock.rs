// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! On-disk superblock of the CoreFS ODF v1 format.
//!
//! The superblock is 4096 bytes (one full block).  Only the first
//! [`SUPERBLOCK_STRUCT_BYTES`] bytes carry structured data; the remainder is
//! zero-padded.  The last four bytes of the structured prefix hold a
//! CRC32C checksum that covers the entire 4096-byte block with those four
//! bytes set to zero.

use super::checksum::Crc32c;
use super::layout::{
    LayoutGeometry, ODF_MAGIC, ODF_VERSION_MAJOR, ODF_VERSION_MINOR, SUPPORTED_INCOMPAT,
    SUPPORTED_RO_COMPAT,
};
use crate::error::{CoreFsError, CoreFsResult};

/// Total on-disk length of the structured superblock prefix (bytes).
///
/// All fields occupy the first 256 bytes.  The window is widened to 512 bytes
/// so the CRC32C field lives well past the last payload field and leaves
/// headroom for future field additions without a format break.
pub const SUPERBLOCK_STRUCT_BYTES: usize = 512;
/// Offset inside the 4 KiB superblock block where the CRC32C checksum lives.
pub const SUPERBLOCK_CHECKSUM_OFFSET: usize = SUPERBLOCK_STRUCT_BYTES - 4;
/// Volume states — clean means the previous mount was properly shut down.
pub const STATE_CLEAN: u32 = 0;
/// Volume states — dirty means a session was open when the volume was last seen.
pub const STATE_DIRTY: u32 = 1;
/// Layout mode — the whole `PersistedState` is inlined as a bincode blob
/// into the system payload inode.  Compact, single-inode format.
pub const LAYOUT_MODE_BLOB: u32 = 0;
/// Layout mode — every domain inode gets its own on-disk inode slot and
/// directories live as dir-entry blocks.  Supports per-inode `fsck`,
/// xattrs and encryption flags, and per-data-block checksums.
pub const LAYOUT_MODE_NATIVE: u32 = 1;

/// Memory representation of the on-disk superblock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superblock {
    pub magic: u64,
    pub version_major: u16,
    pub version_minor: u16,
    pub block_size: u32,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
    pub block_bitmap_start: u64,
    pub block_bitmap_blocks: u64,
    pub inode_bitmap_start: u64,
    pub inode_bitmap_blocks: u64,
    pub inode_table_start: u64,
    pub inode_table_blocks: u64,
    pub journal_start: u64,
    pub journal_blocks: u64,
    pub data_start: u64,
    pub secondary_superblock_block: u64,
    pub tertiary_superblock_block: u64,
    pub feature_compat: u64,
    pub feature_incompat: u64,
    pub feature_ro_compat: u64,
    pub uuid: [u8; 16],
    pub label: [u8; 32],
    pub created_at: i64,
    pub last_mount_at: i64,
    pub last_write_at: i64,
    pub mount_count: u32,
    pub generation: u64,
    pub state: u32,
    pub payload_inode: u64,
    /// CRC32C of the full block-allocation bitmap buffer.
    pub block_bitmap_crc: u32,
    /// CRC32C of the full inode-allocation bitmap buffer.
    pub inode_bitmap_crc: u32,
    /// ODF layout mode — 0 = blob payload, 1 = native per-inode layout.
    pub layout_mode: u32,
    /// ODF inode number of the root directory (0 if not populated).
    pub root_inode: u64,
}

impl Superblock {
    /// Construct a fresh superblock for a newly formatted volume.
    pub fn for_new_volume(
        geom: &LayoutGeometry,
        uuid: [u8; 16],
        label: &str,
        now_secs: i64,
    ) -> Self {
        let mut label_bytes = [0u8; 32];
        let copy_len = label.as_bytes().len().min(label_bytes.len());
        label_bytes[..copy_len].copy_from_slice(&label.as_bytes()[..copy_len]);
        Self {
            magic: ODF_MAGIC,
            version_major: ODF_VERSION_MAJOR,
            version_minor: ODF_VERSION_MINOR,
            block_size: super::layout::BLOCK_SIZE as u32,
            total_blocks: geom.total_blocks,
            free_blocks: geom.data_blocks,
            total_inodes: geom.inode_count,
            free_inodes: geom.inode_count,
            block_bitmap_start: geom.block_bitmap_start,
            block_bitmap_blocks: geom.block_bitmap_blocks,
            inode_bitmap_start: geom.inode_bitmap_start,
            inode_bitmap_blocks: geom.inode_bitmap_blocks,
            inode_table_start: geom.inode_table_start,
            inode_table_blocks: geom.inode_table_blocks,
            journal_start: geom.journal_start,
            journal_blocks: geom.journal_blocks,
            data_start: geom.data_start,
            secondary_superblock_block: geom.secondary_superblock_block,
            tertiary_superblock_block: geom.tertiary_superblock_block,
            feature_compat: super::layout::FEATURE_COMPAT_BLOCK_CHECKSUMS
                | super::layout::FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS,
            feature_incompat: super::layout::FEATURE_INCOMPAT_PAYLOAD_INODE,
            feature_ro_compat: 0,
            uuid,
            label: label_bytes,
            created_at: now_secs,
            last_mount_at: now_secs,
            last_write_at: now_secs,
            mount_count: 0,
            generation: 1,
            state: STATE_CLEAN,
            payload_inode: 0,
            block_bitmap_crc: 0,
            inode_bitmap_crc: 0,
            layout_mode: 0,
            root_inode: 0,
        }
    }

    /// Decoded geometry derived from the superblock fields.
    pub fn geometry(&self) -> LayoutGeometry {
        LayoutGeometry {
            total_blocks: self.total_blocks,
            block_bitmap_start: self.block_bitmap_start,
            block_bitmap_blocks: self.block_bitmap_blocks,
            inode_bitmap_start: self.inode_bitmap_start,
            inode_bitmap_blocks: self.inode_bitmap_blocks,
            inode_table_start: self.inode_table_start,
            inode_table_blocks: self.inode_table_blocks,
            inode_count: self.total_inodes,
            journal_start: self.journal_start,
            journal_blocks: self.journal_blocks,
            data_start: self.data_start,
            data_blocks: self.total_blocks.saturating_sub(self.data_start),
            secondary_superblock_block: self.secondary_superblock_block,
            tertiary_superblock_block: self.tertiary_superblock_block,
        }
    }

    /// Encode the superblock into a full 4096-byte block, computing the
    /// CRC32C checksum over the block with the checksum field zeroed.
    pub fn encode_block(&self) -> Vec<u8> {
        let mut block = vec![0u8; super::layout::BLOCK_SIZE as usize];
        let mut c = std::io::Cursor::new(&mut block[..SUPERBLOCK_STRUCT_BYTES]);
        use std::io::Write;
        let w = &mut c;
        w.write_all(&self.magic.to_le_bytes()).unwrap();
        w.write_all(&self.version_major.to_le_bytes()).unwrap();
        w.write_all(&self.version_minor.to_le_bytes()).unwrap();
        w.write_all(&self.block_size.to_le_bytes()).unwrap();
        w.write_all(&self.total_blocks.to_le_bytes()).unwrap();
        w.write_all(&self.free_blocks.to_le_bytes()).unwrap();
        w.write_all(&self.total_inodes.to_le_bytes()).unwrap();
        w.write_all(&self.free_inodes.to_le_bytes()).unwrap();
        w.write_all(&self.block_bitmap_start.to_le_bytes()).unwrap();
        w.write_all(&self.block_bitmap_blocks.to_le_bytes()).unwrap();
        w.write_all(&self.inode_bitmap_start.to_le_bytes()).unwrap();
        w.write_all(&self.inode_bitmap_blocks.to_le_bytes()).unwrap();
        w.write_all(&self.inode_table_start.to_le_bytes()).unwrap();
        w.write_all(&self.inode_table_blocks.to_le_bytes()).unwrap();
        w.write_all(&self.journal_start.to_le_bytes()).unwrap();
        w.write_all(&self.journal_blocks.to_le_bytes()).unwrap();
        w.write_all(&self.data_start.to_le_bytes()).unwrap();
        w.write_all(&self.secondary_superblock_block.to_le_bytes())
            .unwrap();
        w.write_all(&self.tertiary_superblock_block.to_le_bytes())
            .unwrap();
        w.write_all(&self.feature_compat.to_le_bytes()).unwrap();
        w.write_all(&self.feature_incompat.to_le_bytes()).unwrap();
        w.write_all(&self.feature_ro_compat.to_le_bytes()).unwrap();
        w.write_all(&self.uuid).unwrap();
        w.write_all(&self.label).unwrap();
        w.write_all(&self.created_at.to_le_bytes()).unwrap();
        w.write_all(&self.last_mount_at.to_le_bytes()).unwrap();
        w.write_all(&self.last_write_at.to_le_bytes()).unwrap();
        w.write_all(&self.mount_count.to_le_bytes()).unwrap();
        w.write_all(&self.generation.to_le_bytes()).unwrap();
        w.write_all(&self.state.to_le_bytes()).unwrap();
        w.write_all(&self.payload_inode.to_le_bytes()).unwrap();
        w.write_all(&self.block_bitmap_crc.to_le_bytes()).unwrap();
        w.write_all(&self.inode_bitmap_crc.to_le_bytes()).unwrap();
        w.write_all(&self.layout_mode.to_le_bytes()).unwrap();
        w.write_all(&self.root_inode.to_le_bytes()).unwrap();
        // Remaining bytes up to SUPERBLOCK_CHECKSUM_OFFSET are padding (zero).
        let checksum = Crc32c::hash(&block);
        block[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&checksum.to_le_bytes());
        block
    }

    /// Decode a superblock from a 4096-byte block.  Performs magic and
    /// checksum validation plus feature-flag compatibility checks.
    pub fn decode_block(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != super::layout::BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "superblock block must be {} bytes, got {}",
                super::layout::BLOCK_SIZE,
                block.len()
            )));
        }
        // Verify checksum.
        let stored = u32::from_le_bytes(
            block[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        let mut block_copy = block.to_vec();
        block_copy[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4]
            .fill(0);
        let expected = Crc32c::hash(&block_copy);
        if expected != stored {
            return Err(CoreFsError::State(format!(
                "superblock checksum mismatch: stored=0x{stored:08X} expected=0x{expected:08X}"
            )));
        }

        let mut p = 0usize;
        fn take<const N: usize>(buf: &[u8], p: &mut usize) -> [u8; N] {
            let mut out = [0u8; N];
            out.copy_from_slice(&buf[*p..*p + N]);
            *p += N;
            out
        }
        let magic = u64::from_le_bytes(take::<8>(block, &mut p));
        if magic != ODF_MAGIC {
            return Err(CoreFsError::State(format!(
                "superblock magic mismatch: got 0x{magic:016X}"
            )));
        }
        let version_major = u16::from_le_bytes(take::<2>(block, &mut p));
        let version_minor = u16::from_le_bytes(take::<2>(block, &mut p));
        if version_major != ODF_VERSION_MAJOR {
            return Err(CoreFsError::State(format!(
                "unsupported ODF major version: {version_major}"
            )));
        }
        let block_size = u32::from_le_bytes(take::<4>(block, &mut p));
        let total_blocks = u64::from_le_bytes(take::<8>(block, &mut p));
        let free_blocks = u64::from_le_bytes(take::<8>(block, &mut p));
        let total_inodes = u64::from_le_bytes(take::<8>(block, &mut p));
        let free_inodes = u64::from_le_bytes(take::<8>(block, &mut p));
        let block_bitmap_start = u64::from_le_bytes(take::<8>(block, &mut p));
        let block_bitmap_blocks = u64::from_le_bytes(take::<8>(block, &mut p));
        let inode_bitmap_start = u64::from_le_bytes(take::<8>(block, &mut p));
        let inode_bitmap_blocks = u64::from_le_bytes(take::<8>(block, &mut p));
        let inode_table_start = u64::from_le_bytes(take::<8>(block, &mut p));
        let inode_table_blocks = u64::from_le_bytes(take::<8>(block, &mut p));
        let journal_start = u64::from_le_bytes(take::<8>(block, &mut p));
        let journal_blocks = u64::from_le_bytes(take::<8>(block, &mut p));
        let data_start = u64::from_le_bytes(take::<8>(block, &mut p));
        let secondary_superblock_block = u64::from_le_bytes(take::<8>(block, &mut p));
        let tertiary_superblock_block = u64::from_le_bytes(take::<8>(block, &mut p));
        let feature_compat = u64::from_le_bytes(take::<8>(block, &mut p));
        let feature_incompat = u64::from_le_bytes(take::<8>(block, &mut p));
        let feature_ro_compat = u64::from_le_bytes(take::<8>(block, &mut p));
        let uuid: [u8; 16] = take::<16>(block, &mut p);
        let label: [u8; 32] = take::<32>(block, &mut p);
        let created_at = i64::from_le_bytes(take::<8>(block, &mut p));
        let last_mount_at = i64::from_le_bytes(take::<8>(block, &mut p));
        let last_write_at = i64::from_le_bytes(take::<8>(block, &mut p));
        let mount_count = u32::from_le_bytes(take::<4>(block, &mut p));
        let generation = u64::from_le_bytes(take::<8>(block, &mut p));
        let state = u32::from_le_bytes(take::<4>(block, &mut p));
        let payload_inode = u64::from_le_bytes(take::<8>(block, &mut p));
        let block_bitmap_crc = u32::from_le_bytes(take::<4>(block, &mut p));
        let inode_bitmap_crc = u32::from_le_bytes(take::<4>(block, &mut p));
        let layout_mode = u32::from_le_bytes(take::<4>(block, &mut p));
        let root_inode = u64::from_le_bytes(take::<8>(block, &mut p));

        let unknown_incompat = feature_incompat & !SUPPORTED_INCOMPAT;
        if unknown_incompat != 0 {
            return Err(CoreFsError::State(format!(
                "superblock requires unsupported incompat features: 0x{unknown_incompat:016X}"
            )));
        }
        let unknown_ro = feature_ro_compat & !SUPPORTED_RO_COMPAT;
        if unknown_ro != 0 {
            return Err(CoreFsError::State(format!(
                "superblock requires unsupported ro-compat features: 0x{unknown_ro:016X}"
            )));
        }

        Ok(Self {
            magic,
            version_major,
            version_minor,
            block_size,
            total_blocks,
            free_blocks,
            total_inodes,
            free_inodes,
            block_bitmap_start,
            block_bitmap_blocks,
            inode_bitmap_start,
            inode_bitmap_blocks,
            inode_table_start,
            inode_table_blocks,
            journal_start,
            journal_blocks,
            data_start,
            secondary_superblock_block,
            tertiary_superblock_block,
            feature_compat,
            feature_incompat,
            feature_ro_compat,
            uuid,
            label,
            created_at,
            last_mount_at,
            last_write_at,
            mount_count,
            generation,
            state,
            payload_inode,
            block_bitmap_crc,
            inode_bitmap_crc,
            layout_mode,
            root_inode,
        })
    }
}

#[cfg(test)]
#[path = "superblock_tests.rs"]
mod tests;
