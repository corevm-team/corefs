// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! On-demand read access to an ODF volume.
//!
//! The [`OdfSession`](super::session::OdfFileSession) API loads the
//! **full** state into a CoreFS service, suitable for mutate-then-
//! flush workflows.  `OdfReader` is the counterpart for read-heavy
//! scenarios where loading every inode is wasteful:
//!
//! * **FUSE mounts** that want to answer `readdir` / `getattr` /
//!   `read` without materialising every file's content.
//! * **Admin tooling** (`odf-inspect`, `odf-cat`) that wants to peek
//!   at individual slots without touching the rest.
//! * **Repair** / recovery tools that need to read a damaged volume
//!   slot-by-slot without the whole-image deserialise cost.
//!
//! The reader holds just the superblock and the inode bitmap in
//! memory (a few KiB for typical volumes); every other access is a
//! targeted block read against the device.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashMap;

use crate::domain::inode::{Inode, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;

use super::attr_block::AttrBlock;
use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::extent_tree::ExtentChain;
use super::inode::{
    FLAG_ENCRYPTED, FLAG_HAS_EXTENT_INDEX, INODE_RECORD_SIZE, OnDiskInode, OnDiskKind,
};
use super::layout::{BLOCK_SIZE, LayoutGeometry};
use super::native::{FIRST_USER_INODE_SLOT, FLAG_DELETED};
use super::superblock::{LAYOUT_MODE_NATIVE, Superblock};

/// Lightweight summary of a single inode, cheap to assemble from the
/// 256-byte on-disk record alone (no attr block read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InodeSummary {
    pub slot: u64,
    pub domain_id: u64,
    pub kind: InodeKind,
    pub size_bytes: u64,
    pub flags: u32,
    pub is_deleted: bool,
    pub is_encrypted: bool,
    pub data_crc: u64,
    pub extent_count: usize,
}

/// On-demand reader over an ODF volume.
pub struct OdfReader<'d> {
    device: &'d dyn BlockDevice,
    sb: Superblock,
    geom: LayoutGeometry,
    inode_bitmap: Bitmap,
    /// Memoised domain-id → slot mapping built on first lookup.
    id_to_slot: Option<HashMap<u64, u64>>,
    /// Memoised path → slot mapping built on first path lookup.
    path_to_slot: Option<HashMap<String, u64>>,
}

impl<'d> OdfReader<'d> {
    /// Open a reader.  Reads just the superblock and inode bitmap —
    /// `O(inode_bitmap_blocks)` block reads, usually one.
    pub fn open(device: &'d dyn BlockDevice) -> CoreFsResult<Self> {
        let sb = super::volume::read_sb_with_fallbacks(device)?;
        if sb.layout_mode != LAYOUT_MODE_NATIVE {
            return Err(CoreFsError::State(format!(
                "OdfReader: layout mode is {}, not NATIVE",
                sb.layout_mode
            )));
        }
        super::volume::verify_bitmap_integrity(device, &sb)?;
        let geom = sb.geometry();
        let ibm_bytes = device.read_at(
            geom.inode_bitmap_start * BLOCK_SIZE,
            geom.inode_bitmap_blocks * BLOCK_SIZE,
        )?;
        let inode_bitmap = Bitmap::from_bytes(ibm_bytes, geom.inode_count)?;
        Ok(Self {
            device,
            sb,
            geom,
            inode_bitmap,
            id_to_slot: None,
            path_to_slot: None,
        })
    }

    /// Borrow the decoded superblock.
    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    /// Total number of allocated user inodes (= non-system slots).
    pub fn allocated_user_inodes(&self) -> u64 {
        let mut count = 0u64;
        for slot in FIRST_USER_INODE_SLOT..self.geom.inode_count {
            if self.inode_bitmap.is_set(slot).unwrap_or(false) {
                count += 1;
            }
        }
        count
    }

    /// Walk every allocated user slot and return its summary.  One
    /// block read per slot (the record's containing block).
    pub fn list_inodes(&self) -> CoreFsResult<Vec<InodeSummary>> {
        let mut out = Vec::new();
        for slot in FIRST_USER_INODE_SLOT..self.geom.inode_count {
            if !self.inode_bitmap.is_set(slot)? {
                continue;
            }
            let rec = self.read_slot(slot)?;
            if rec.kind == OnDiskKind::Unused {
                continue;
            }
            let extents = if rec.flags & FLAG_HAS_EXTENT_INDEX != 0 {
                ExtentChain::read_chain(self.device, rec.index_block_addr)?.len()
            } else {
                rec.extents.len()
            };
            out.push(InodeSummary {
                slot,
                domain_id: rec.domain_inode_id,
                kind: odk_to_kind(rec.kind)?,
                size_bytes: rec.size_bytes,
                flags: rec.flags,
                is_deleted: rec.flags & FLAG_DELETED != 0,
                is_encrypted: rec.flags & FLAG_ENCRYPTED != 0,
                data_crc: rec.data_crc,
                extent_count: extents,
            });
        }
        Ok(out)
    }

    /// Look up an inode by its domain id.  Builds + caches a hash
    /// index on first call.  Returns `None` if no slot is assigned.
    pub fn slot_for_domain_id(&mut self, domain_id: u64) -> CoreFsResult<Option<u64>> {
        if self.id_to_slot.is_none() {
            let mut map = HashMap::new();
            for slot in FIRST_USER_INODE_SLOT..self.geom.inode_count {
                if !self.inode_bitmap.is_set(slot)? {
                    continue;
                }
                let rec = self.read_slot(slot)?;
                if rec.kind == OnDiskKind::Unused {
                    continue;
                }
                map.insert(rec.domain_inode_id, slot);
            }
            self.id_to_slot = Some(map);
        }
        Ok(self.id_to_slot.as_ref().unwrap().get(&domain_id).copied())
    }

    /// Look up an inode by its logical path.  Reads every allocated
    /// inode's attr block the first time, then memoises.  O(n) first
    /// call, O(1) thereafter.
    pub fn slot_for_path(&mut self, path: &str) -> CoreFsResult<Option<u64>> {
        if self.path_to_slot.is_none() {
            let mut map = HashMap::new();
            for slot in FIRST_USER_INODE_SLOT..self.geom.inode_count {
                if !self.inode_bitmap.is_set(slot)? {
                    continue;
                }
                let rec = self.read_slot(slot)?;
                if rec.kind == OnDiskKind::Unused {
                    continue;
                }
                if let Some(inode) = self.decode_attr(&rec)? {
                    map.insert(inode.path, slot);
                }
            }
            self.path_to_slot = Some(map);
        }
        Ok(self.path_to_slot.as_ref().unwrap().get(path).copied())
    }

    /// Read the full on-disk inode record for a specific slot.
    pub fn read_on_disk_inode(&self, slot: u64) -> CoreFsResult<OnDiskInode> {
        if slot >= self.geom.inode_count {
            return Err(CoreFsError::InvalidInput(format!(
                "OdfReader: slot {slot} out of range (max {})",
                self.geom.inode_count
            )));
        }
        if !self.inode_bitmap.is_set(slot)? {
            return Err(CoreFsError::NotFound(format!(
                "OdfReader: slot {slot} not allocated"
            )));
        }
        self.read_slot(slot)
    }

    /// Read the domain [`Inode`] attached to `slot` via its attr block.
    pub fn read_inode_metadata(&self, slot: u64) -> CoreFsResult<Inode> {
        let rec = self.read_on_disk_inode(slot)?;
        self.decode_attr(&rec)?
            .ok_or_else(|| CoreFsError::State(format!("OdfReader: slot {slot} has no attr block")))
    }

    /// Read the full content of the file at `slot` (returns an empty
    /// vec for directories and empty files).
    pub fn read_file_content(&self, slot: u64) -> CoreFsResult<Vec<u8>> {
        let rec = self.read_on_disk_inode(slot)?;
        let bytes = super::native::read_all_extent_bytes_public(self.device, &rec)?;
        // Verify data CRC if present.
        if rec.data_crc != 0 && !bytes.is_empty() {
            let actual = u64::from(Crc32c::hash(&bytes));
            if actual != rec.data_crc {
                return Err(CoreFsError::State(format!(
                    "OdfReader: slot {slot} data CRC mismatch (stored=0x{:016X}, actual=0x{:016X})",
                    rec.data_crc, actual
                )));
            }
        }
        Ok(bytes)
    }

    /// Convenience: look up by path and read the content.
    pub fn read_file_by_path(&mut self, path: &str) -> CoreFsResult<Vec<u8>> {
        match self.slot_for_path(path)? {
            Some(slot) => self.read_file_content(slot),
            None => Err(CoreFsError::NotFound(format!(
                "OdfReader: path {path} not found"
            ))),
        }
    }

    // ------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------

    fn read_slot(&self, slot: u64) -> CoreFsResult<OnDiskInode> {
        let (block, offset) = self.geom.inode_record_location(slot)?;
        let buf = self.device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
        OnDiskInode::decode(&buf[offset as usize..offset as usize + INODE_RECORD_SIZE])
    }

    fn decode_attr(&self, rec: &OnDiskInode) -> CoreFsResult<Option<Inode>> {
        if rec.xattr_block_addr == 0 {
            return Ok(None);
        }
        let buf = self
            .device
            .read_at(rec.xattr_block_addr * BLOCK_SIZE, BLOCK_SIZE)?;
        let attr = AttrBlock::decode(&buf)?;
        bincode::deserialize::<Inode>(&attr.payload)
            .map(Some)
            .map_err(|e| CoreFsError::State(format!("OdfReader: attr deserialize failed: {e}")))
    }
}

fn odk_to_kind(k: OnDiskKind) -> CoreFsResult<InodeKind> {
    Ok(match k {
        OnDiskKind::File => InodeKind::File,
        OnDiskKind::Directory => InodeKind::Directory,
        OnDiskKind::Symlink => InodeKind::Symlink,
        OnDiskKind::Unused | OnDiskKind::SystemPayload => {
            return Err(CoreFsError::State(format!(
                "OdfReader: unexpected on-disk kind {k:?}"
            )));
        }
    })
}

// Tests for `OdfReader` live in the main `corefs` crate because they
// depend on `session::OdfDeviceSession`, which is std-bound (pulls in
// filesystem-level services) and stays in the main crate.
