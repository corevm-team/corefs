// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Directory-entry blocks.
//!
//! A directory inode in native layout owns a chain of 4 KiB dir-entry
//! blocks instead of raw file content.  Each block holds a sequence of
//! variable-length records describing the directory's children.
//!
//! ## Block layout (4 KiB)
//!
//! ```text
//! offset  size  field
//!   0      4    magic              (0xD12E_D12E)
//!   4      4    entry_count
//!   8      8    next_dir_block     (0 = terminal)
//!  16   4076    entries
//! 4092     4    crc32c             (over the full block with CRC zeroed)
//! ```
//!
//! ## Entry layout
//!
//! ```text
//!  0     4    rec_len          (8-byte-aligned, includes this header)
//!  4     2    name_len
//!  6     2    kind             (1=file, 2=dir, 3=symlink, 4=system)
//!  8     8    inode
//! 16   ...    name bytes (UTF-8, name_len long, zero-padded to rec_len)
//! ```
//!
//! `rec_len` is always a multiple of 8, so a scanner can walk the block
//! by adding `rec_len` after each decoded entry.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use core::str;

use super::checksum::Crc32c;
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};

/// Magic value at the start of every dir-entry block.
pub const DIR_BLOCK_MAGIC: u32 = 0xD12E_D12E;
/// Maximum length of a single entry's name in bytes.
pub const MAX_NAME_BYTES: usize = 255;
const HEADER_BYTES: usize = 16;
const CRC_OFFSET: usize = 4092;
const USABLE_BYTES: usize = CRC_OFFSET - HEADER_BYTES;

/// Kind tag used inside a dir-entry record.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirEntryKind {
    File = 1,
    Directory = 2,
    Symlink = 3,
    System = 4,
}

impl DirEntryKind {
    fn from_u16(v: u16) -> CoreFsResult<Self> {
        match v {
            1 => Ok(Self::File),
            2 => Ok(Self::Directory),
            3 => Ok(Self::Symlink),
            4 => Ok(Self::System),
            x => Err(CoreFsError::State(format!("dir entry: unknown kind {x}"))),
        }
    }
}

/// In-memory record for a single directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub inode: u64,
    pub kind: DirEntryKind,
    pub name: String,
}

impl DirEntry {
    /// Encoded length rounded up to 8-byte alignment.
    pub fn encoded_len(&self) -> CoreFsResult<usize> {
        let name_bytes = self.name.as_bytes().len();
        if name_bytes > MAX_NAME_BYTES {
            return Err(CoreFsError::InvalidInput(format!(
                "dir entry name too long ({name_bytes} > {MAX_NAME_BYTES})"
            )));
        }
        let raw = HEADER_BYTES + name_bytes;
        Ok(raw.div_ceil(8) * 8)
    }

    fn write_to(&self, buf: &mut [u8]) -> CoreFsResult<usize> {
        let rec_len = self.encoded_len()?;
        if buf.len() < rec_len {
            return Err(CoreFsError::InvalidInput(
                "dir entry: destination buffer too small".into(),
            ));
        }
        buf[0..4].copy_from_slice(&(rec_len as u32).to_le_bytes());
        buf[4..6].copy_from_slice(&(self.name.as_bytes().len() as u16).to_le_bytes());
        buf[6..8].copy_from_slice(&(self.kind as u16).to_le_bytes());
        buf[8..16].copy_from_slice(&self.inode.to_le_bytes());
        let nb = self.name.as_bytes();
        buf[16..16 + nb.len()].copy_from_slice(nb);
        // Zero-fill the padding between the name and rec_len.
        for b in &mut buf[16 + nb.len()..rec_len] {
            *b = 0;
        }
        Ok(rec_len)
    }

    fn read_from(buf: &[u8]) -> CoreFsResult<(Self, usize)> {
        if buf.len() < HEADER_BYTES {
            return Err(CoreFsError::State("dir entry: buffer too short".into()));
        }
        let rec_len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        if rec_len < HEADER_BYTES || rec_len % 8 != 0 || rec_len > buf.len() {
            return Err(CoreFsError::State(format!(
                "dir entry: invalid rec_len {rec_len}"
            )));
        }
        let name_len = u16::from_le_bytes(buf[4..6].try_into().unwrap()) as usize;
        let kind = DirEntryKind::from_u16(u16::from_le_bytes(buf[6..8].try_into().unwrap()))?;
        let inode = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        if HEADER_BYTES + name_len > rec_len {
            return Err(CoreFsError::State(
                "dir entry: name_len overflows rec_len".into(),
            ));
        }
        let name = str::from_utf8(&buf[16..16 + name_len])
            .map_err(|e| CoreFsError::State(format!("dir entry: invalid utf-8 name: {e}")))?
            .to_string();
        Ok((Self { inode, kind, name }, rec_len))
    }
}

/// Memory image of a single directory block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirBlock {
    pub next_dir_block: u64,
    pub entries: Vec<DirEntry>,
}

impl DirBlock {
    pub fn empty() -> Self {
        Self {
            next_dir_block: 0,
            entries: Vec::new(),
        }
    }

    /// Bytes consumed by the current entries (excluding header and CRC).
    pub fn encoded_entries_bytes(&self) -> CoreFsResult<usize> {
        let mut total = 0usize;
        for e in &self.entries {
            total += e.encoded_len()?;
        }
        Ok(total)
    }

    /// Encode into a full 4 KiB block with CRC32C trailer.
    pub fn encode(&self) -> CoreFsResult<Vec<u8>> {
        let mut block = vec![0u8; BLOCK_SIZE as usize];
        block[0..4].copy_from_slice(&DIR_BLOCK_MAGIC.to_le_bytes());
        block[4..8].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        block[8..16].copy_from_slice(&self.next_dir_block.to_le_bytes());
        let mut cursor = HEADER_BYTES;
        for e in &self.entries {
            let rec_len = e.encoded_len()?;
            if cursor + rec_len > CRC_OFFSET {
                return Err(CoreFsError::InvalidInput(
                    "dir block: entries overflow the 4 KiB capacity".into(),
                ));
            }
            e.write_to(&mut block[cursor..cursor + rec_len])?;
            cursor += rec_len;
        }
        let crc = Crc32c::hash(&block);
        block[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(block)
    }

    /// Decode + validate a 4 KiB directory block.
    pub fn decode(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "dir block: wrong length {}",
                block.len()
            )));
        }
        let stored = u32::from_le_bytes(block[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
        let mut zeroed = block.to_vec();
        zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&zeroed);
        if stored != expected {
            return Err(CoreFsError::State("dir block CRC mismatch".into()));
        }
        let magic = u32::from_le_bytes(block[0..4].try_into().unwrap());
        if magic != DIR_BLOCK_MAGIC {
            return Err(CoreFsError::State(format!(
                "dir block: bad magic 0x{magic:08X}"
            )));
        }
        let count = u32::from_le_bytes(block[4..8].try_into().unwrap()) as usize;
        let next_dir_block = u64::from_le_bytes(block[8..16].try_into().unwrap());
        let mut entries = Vec::with_capacity(count);
        let mut cursor = HEADER_BYTES;
        for _ in 0..count {
            if cursor >= CRC_OFFSET {
                return Err(CoreFsError::State(
                    "dir block: entry_count exceeds available bytes".into(),
                ));
            }
            let (entry, rec_len) = DirEntry::read_from(&block[cursor..CRC_OFFSET])?;
            entries.push(entry);
            cursor += rec_len;
        }
        Ok(Self {
            next_dir_block,
            entries,
        })
    }

    /// Estimate the number of dir blocks required to hold `entries`.
    /// Returns 1 for an empty list (every directory keeps at least one
    /// header block so walkers never see a chain that starts at 0).
    pub fn blocks_needed(entries: &[DirEntry]) -> CoreFsResult<usize> {
        let mut blocks = 1usize;
        let mut used = 0usize;
        for e in entries {
            let rec = e.encoded_len()?;
            if used + rec > USABLE_BYTES {
                blocks += 1;
                used = 0;
            }
            used += rec;
        }
        Ok(blocks)
    }

    /// Split a list of entries into sequential [`DirBlock`]s linked via
    /// `next_dir_block` pointers drawn from `reserve`.  The first block's
    /// physical address is `reserve[0]`.
    pub fn pack(entries: &[DirEntry], reserve: &[u64]) -> CoreFsResult<Vec<DirBlock>> {
        if reserve.is_empty() {
            return Err(CoreFsError::InvalidInput(
                "dir pack: reserve must contain at least one block".into(),
            ));
        }
        let mut blocks: Vec<DirBlock> = Vec::new();
        let mut current = DirBlock::empty();
        let mut current_bytes = 0usize;
        for e in entries {
            let rec = e.encoded_len()?;
            if current_bytes + rec > USABLE_BYTES {
                blocks.push(mem::replace(&mut current, DirBlock::empty()));
                current_bytes = 0;
            }
            current.entries.push(e.clone());
            current_bytes += rec;
        }
        blocks.push(current);
        if blocks.len() > reserve.len() {
            return Err(CoreFsError::InvalidInput(format!(
                "dir pack: need {} blocks, reserve has {}",
                blocks.len(),
                reserve.len()
            )));
        }
        for i in 0..blocks.len() - 1 {
            blocks[i].next_dir_block = reserve[i + 1];
        }
        Ok(blocks)
    }
}

#[cfg(test)]
#[path = "dir_entry_tests.rs"]
mod tests;
