// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Structured extended-attributes (xattr) + ACL block.
//!
//! An xattr block is a stronger-typed sibling of [`super::attr_block`]:
//! it encodes a list of key/value pairs (like POSIX xattrs) together with
//! an ACL entry list.  The block is 4 KiB and CRC32C-protected.
//!
//! An inode advertises that it carries xattrs by setting the
//! [`super::inode::FLAG_HAS_XATTRS`] flag and pointing `xattr_block_addr`
//! at this block.  The [`super::native`] layout uses the simpler
//! [`super::attr_block::AttrBlock`] (which wraps a bincode blob) for its
//! per-inode metadata; this block type is available for callers that
//! want a format the kernel could parse directly without bincode.
//!
//! ## Layout (4 KiB)
//!
//! ```text
//! offset  size  field
//!   0      4    magic            (0x584A_4154 — 'XJAT' little-endian)
//!   4      4    flags
//!   8      2    xattr_count
//!  10      2    acl_count
//!  12      4    reserved
//!  16   var    xattr entries
//!  ..   var    acl entries
//! 4092     4    crc32c
//! ```
//!
//! ### xattr entry
//!
//! ```text
//!  0      2    rec_len        (8-byte-aligned)
//!  2      2    name_len
//!  4      4    value_len
//!  8    ...    name bytes (utf-8)
//! ...   ...    value bytes (opaque)
//! ...   pad    zero fill up to rec_len
//! ```
//!
//! ### acl entry
//!
//! ```text
//!  0      2    rec_len        (8-byte-aligned)
//!  2      1    principal_tag  (0 = user, 1 = group, 2 = everyone)
//!  3      1    permission     (bits: 1=read, 2=write, 4=execute)
//!  4      4    subject_len
//!  8    ...    subject bytes (utf-8; empty for Everyone)
//! ...   pad    zero fill up to rec_len
//! ```

use super::checksum::Crc32c;
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};

/// Magic value — ASCII `XJAT` (eXtended Attributes).
pub const XATTR_BLOCK_MAGIC: u32 = u32::from_le_bytes(*b"XJAT");
const CRC_OFFSET: usize = 4092;
const HEADER_BYTES: usize = 16;
const XATTR_HDR: usize = 8;
const ACL_HDR: usize = 8;

/// Permission bits inside an [`AclRecord`].
pub mod perm {
    pub const READ: u8 = 1;
    pub const WRITE: u8 = 2;
    pub const EXECUTE: u8 = 4;
}

/// Principal kind for an ACL entry.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclPrincipal {
    User = 0,
    Group = 1,
    Everyone = 2,
}

impl AclPrincipal {
    fn from_u8(v: u8) -> CoreFsResult<Self> {
        match v {
            0 => Ok(Self::User),
            1 => Ok(Self::Group),
            2 => Ok(Self::Everyone),
            x => Err(CoreFsError::State(format!("xattr: unknown principal {x}"))),
        }
    }
}

/// Single extended-attribute key/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XattrPair {
    pub name: String,
    pub value: Vec<u8>,
}

/// Single ACL record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclRecord {
    pub principal: AclPrincipal,
    pub subject: String,
    pub permission: u8,
}

/// In-memory image of an xattr block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XattrBlock {
    pub flags: u32,
    pub xattrs: Vec<XattrPair>,
    pub acls: Vec<AclRecord>,
}

impl XattrBlock {
    /// Encoded size of a single xattr entry (8-byte aligned).
    fn xattr_len(pair: &XattrPair) -> CoreFsResult<usize> {
        let raw = XATTR_HDR + pair.name.as_bytes().len() + pair.value.len();
        let rec = raw.div_ceil(8) * 8;
        if rec > 4000 {
            return Err(CoreFsError::InvalidInput("xattr entry too large".into()));
        }
        Ok(rec)
    }

    fn acl_len(acl: &AclRecord) -> CoreFsResult<usize> {
        let raw = ACL_HDR + acl.subject.as_bytes().len();
        let rec = raw.div_ceil(8) * 8;
        if rec > 512 {
            return Err(CoreFsError::InvalidInput("acl entry too large".into()));
        }
        Ok(rec)
    }

    /// Encode into a full 4 KiB block.
    pub fn encode(&self) -> CoreFsResult<Vec<u8>> {
        let mut block = vec![0u8; BLOCK_SIZE as usize];
        block[0..4].copy_from_slice(&XATTR_BLOCK_MAGIC.to_le_bytes());
        block[4..8].copy_from_slice(&self.flags.to_le_bytes());
        block[8..10].copy_from_slice(&(self.xattrs.len() as u16).to_le_bytes());
        block[10..12].copy_from_slice(&(self.acls.len() as u16).to_le_bytes());
        // reserved 12..16

        let mut cursor = HEADER_BYTES;
        for pair in &self.xattrs {
            let rec = Self::xattr_len(pair)?;
            if cursor + rec > CRC_OFFSET {
                return Err(CoreFsError::InvalidInput(
                    "xattr block: entries overflow capacity".into(),
                ));
            }
            let name_b = pair.name.as_bytes();
            block[cursor..cursor + 2].copy_from_slice(&(rec as u16).to_le_bytes());
            block[cursor + 2..cursor + 4].copy_from_slice(&(name_b.len() as u16).to_le_bytes());
            block[cursor + 4..cursor + 8].copy_from_slice(&(pair.value.len() as u32).to_le_bytes());
            block[cursor + XATTR_HDR..cursor + XATTR_HDR + name_b.len()].copy_from_slice(name_b);
            let v_off = cursor + XATTR_HDR + name_b.len();
            block[v_off..v_off + pair.value.len()].copy_from_slice(&pair.value);
            cursor += rec;
        }
        for acl in &self.acls {
            let rec = Self::acl_len(acl)?;
            if cursor + rec > CRC_OFFSET {
                return Err(CoreFsError::InvalidInput(
                    "xattr block: acl entries overflow capacity".into(),
                ));
            }
            let subj = acl.subject.as_bytes();
            block[cursor..cursor + 2].copy_from_slice(&(rec as u16).to_le_bytes());
            block[cursor + 2] = acl.principal as u8;
            block[cursor + 3] = acl.permission;
            block[cursor + 4..cursor + 8].copy_from_slice(&(subj.len() as u32).to_le_bytes());
            block[cursor + ACL_HDR..cursor + ACL_HDR + subj.len()].copy_from_slice(subj);
            cursor += rec;
        }

        let crc = Crc32c::hash(&block);
        block[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(block)
    }

    pub fn decode(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput(format!(
                "xattr block: wrong length {}",
                block.len()
            )));
        }
        let stored = u32::from_le_bytes(block[CRC_OFFSET..CRC_OFFSET + 4].try_into().unwrap());
        let mut zeroed = block.to_vec();
        zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&zeroed);
        if stored != expected {
            return Err(CoreFsError::State("xattr block CRC mismatch".into()));
        }
        let magic = u32::from_le_bytes(block[0..4].try_into().unwrap());
        if magic != XATTR_BLOCK_MAGIC {
            return Err(CoreFsError::State(format!(
                "xattr block: bad magic 0x{magic:08X}"
            )));
        }
        let flags = u32::from_le_bytes(block[4..8].try_into().unwrap());
        let xattr_count = u16::from_le_bytes(block[8..10].try_into().unwrap()) as usize;
        let acl_count = u16::from_le_bytes(block[10..12].try_into().unwrap()) as usize;

        let mut cursor = HEADER_BYTES;
        let mut xattrs = Vec::with_capacity(xattr_count);
        for _ in 0..xattr_count {
            if cursor + XATTR_HDR > CRC_OFFSET {
                return Err(CoreFsError::State("xattr: truncated entry".into()));
            }
            let rec = u16::from_le_bytes(block[cursor..cursor + 2].try_into().unwrap()) as usize;
            if rec < XATTR_HDR || rec % 8 != 0 || cursor + rec > CRC_OFFSET {
                return Err(CoreFsError::State("xattr: bad rec_len".into()));
            }
            let name_len = u16::from_le_bytes(block[cursor + 2..cursor + 4].try_into().unwrap()) as usize;
            let value_len = u32::from_le_bytes(block[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            if XATTR_HDR + name_len + value_len > rec {
                return Err(CoreFsError::State("xattr: lengths exceed rec".into()));
            }
            let name = std::str::from_utf8(
                &block[cursor + XATTR_HDR..cursor + XATTR_HDR + name_len],
            )
            .map_err(|e| CoreFsError::State(format!("xattr: name utf-8: {e}")))?
            .to_string();
            let v_off = cursor + XATTR_HDR + name_len;
            let value = block[v_off..v_off + value_len].to_vec();
            xattrs.push(XattrPair { name, value });
            cursor += rec;
        }

        let mut acls = Vec::with_capacity(acl_count);
        for _ in 0..acl_count {
            if cursor + ACL_HDR > CRC_OFFSET {
                return Err(CoreFsError::State("acl: truncated entry".into()));
            }
            let rec = u16::from_le_bytes(block[cursor..cursor + 2].try_into().unwrap()) as usize;
            if rec < ACL_HDR || rec % 8 != 0 || cursor + rec > CRC_OFFSET {
                return Err(CoreFsError::State("acl: bad rec_len".into()));
            }
            let principal = AclPrincipal::from_u8(block[cursor + 2])?;
            let permission = block[cursor + 3];
            let subj_len = u32::from_le_bytes(block[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            if ACL_HDR + subj_len > rec {
                return Err(CoreFsError::State("acl: subject exceeds rec".into()));
            }
            let subject = std::str::from_utf8(
                &block[cursor + ACL_HDR..cursor + ACL_HDR + subj_len],
            )
            .map_err(|e| CoreFsError::State(format!("acl: subject utf-8: {e}")))?
            .to_string();
            acls.push(AclRecord {
                principal,
                subject,
                permission,
            });
            cursor += rec;
        }

        Ok(Self {
            flags,
            xattrs,
            acls,
        })
    }
}

#[cfg(test)]
#[path = "xattr_tests.rs"]
mod tests;
