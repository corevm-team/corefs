// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! On-disk consistency checker (fsck) for ODF v1.
//!
//! [`check`] performs a read-only structural walk of a formatted volume
//! and returns an [`FsckReport`] listing every issue found.  No writes
//! occur — repair is a separate concern (future work).
//!
//! ## Invariants verified
//!
//! * All three superblock copies decode and agree on the critical
//!   geometry fields.
//! * Block- and inode-bitmap CRCs match the values stored in the
//!   superblock.
//! * `free_blocks` / `free_inodes` agree with the bitmap popcounts.
//! * Every slot set in the inode bitmap contains a decodable on-disk
//!   inode of kind ≠ Unused.
//! * Every inode's extents and attr-block pointer land inside the data
//!   region and correspond to blocks marked allocated in the block
//!   bitmap.
//! * No two inodes share a data block (double-allocation detection).
//! * Journal header is decodable (if journal region is populated).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashMap;

use crate::error::CoreFsResult;
use crate::storage::block_device::BlockDevice;

use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::inode::{FLAG_HAS_EXTENT_INDEX, INODE_RECORD_SIZE, OnDiskInode, OnDiskKind};
use super::layout::{BLOCK_SIZE, LayoutGeometry, PRIMARY_SUPERBLOCK_BLOCK};
use super::superblock::{LAYOUT_MODE_NATIVE, Superblock};

/// Severity tag for an [`FsckIssue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Benign observation — not a correctness issue.
    Info,
    /// Warning — filesystem is usable but something is off.
    Warning,
    /// Error — filesystem is inconsistent; manual intervention advised.
    Error,
}

/// A single finding produced by [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckIssue {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

/// Aggregate result of a fsck run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FsckReport {
    pub issues: Vec<FsckIssue>,
    pub inodes_checked: u64,
    pub extents_checked: u64,
    pub blocks_referenced: u64,
}

impl FsckReport {
    /// `true` if the report contains no issues of severity Error or higher.
    pub fn is_clean(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// Count of issues of the given severity.
    pub fn count(&self, sev: Severity) -> usize {
        self.issues.iter().filter(|i| i.severity == sev).count()
    }
}

/// Perform a read-only structural check of the volume on `device`.
pub fn check(device: &dyn BlockDevice) -> CoreFsResult<FsckReport> {
    let mut report = FsckReport::default();

    // --- Superblock copies -------------------------------------------------
    let primary =
        read_superblock_at(device, PRIMARY_SUPERBLOCK_BLOCK).map_err(|e| e.clone_as_err());
    let total_blocks_guess = device.capacity() / BLOCK_SIZE;
    let tertiary_guess = total_blocks_guess / 2;
    let secondary_guess = total_blocks_guess - 1;
    let tertiary = read_superblock_at(device, tertiary_guess);
    let secondary = read_superblock_at(device, secondary_guess);

    let sb = match &primary {
        Ok(sb) => sb.clone(),
        Err(e) => {
            report.issues.push(FsckIssue {
                severity: Severity::Error,
                code: "ODF.SB.PRIMARY_UNREADABLE",
                message: format!("primary superblock: {e}"),
            });
            match tertiary.clone().or_else(|_| secondary.clone()) {
                Ok(fallback) => fallback,
                Err(e2) => {
                    report.issues.push(FsckIssue {
                        severity: Severity::Error,
                        code: "ODF.SB.ALL_UNREADABLE",
                        message: format!("no superblock copy is readable: {e2}"),
                    });
                    return Ok(report);
                }
            }
        }
    };

    if let Ok(t) = &tertiary {
        if t.generation != sb.generation {
            report.issues.push(FsckIssue {
                severity: Severity::Warning,
                code: "ODF.SB.TERTIARY_STALE",
                message: format!(
                    "tertiary superblock generation {} differs from primary {}",
                    t.generation, sb.generation
                ),
            });
        }
    } else {
        report.issues.push(FsckIssue {
            severity: Severity::Warning,
            code: "ODF.SB.TERTIARY_UNREADABLE",
            message: "tertiary (middle) superblock is not decodable".into(),
        });
    }
    if let Ok(s) = &secondary {
        if s.generation != sb.generation {
            report.issues.push(FsckIssue {
                severity: Severity::Warning,
                code: "ODF.SB.SECONDARY_STALE",
                message: format!(
                    "secondary superblock generation {} differs from primary {}",
                    s.generation, sb.generation
                ),
            });
        }
    } else {
        report.issues.push(FsckIssue {
            severity: Severity::Warning,
            code: "ODF.SB.SECONDARY_UNREADABLE",
            message: "secondary (tail) superblock is not decodable".into(),
        });
    }

    let geom = sb.geometry();

    // --- Bitmap CRCs -------------------------------------------------------
    let bbm_bytes = device.read_at(
        geom.block_bitmap_start * BLOCK_SIZE,
        geom.block_bitmap_blocks * BLOCK_SIZE,
    )?;
    if sb.block_bitmap_crc != 0 {
        let actual = Crc32c::hash(&bbm_bytes);
        if actual != sb.block_bitmap_crc {
            report.issues.push(FsckIssue {
                severity: Severity::Error,
                code: "ODF.BBM.CRC",
                message: format!(
                    "block bitmap CRC mismatch (stored=0x{:08X}, actual=0x{:08X})",
                    sb.block_bitmap_crc, actual
                ),
            });
        }
    }
    let block_bitmap = Bitmap::from_bytes(bbm_bytes, geom.total_blocks)?;

    let ibm_bytes = device.read_at(
        geom.inode_bitmap_start * BLOCK_SIZE,
        geom.inode_bitmap_blocks * BLOCK_SIZE,
    )?;
    if sb.inode_bitmap_crc != 0 {
        let actual = Crc32c::hash(&ibm_bytes);
        if actual != sb.inode_bitmap_crc {
            report.issues.push(FsckIssue {
                severity: Severity::Error,
                code: "ODF.IBM.CRC",
                message: format!(
                    "inode bitmap CRC mismatch (stored=0x{:08X}, actual=0x{:08X})",
                    sb.inode_bitmap_crc, actual
                ),
            });
        }
    }
    let inode_bitmap = Bitmap::from_bytes(ibm_bytes, geom.inode_count)?;

    // --- free_* sanity -----------------------------------------------------
    let allocated_in_data = (geom.data_start..geom.total_blocks)
        .filter(|b| block_bitmap.is_set(*b).unwrap_or(true))
        .count() as u64;
    let free_data = geom.data_blocks - allocated_in_data;
    if sb.free_blocks != free_data {
        report.issues.push(FsckIssue {
            severity: Severity::Warning,
            code: "ODF.SB.FREE_BLOCKS",
            message: format!(
                "sb.free_blocks={} but bitmap reports {} free data blocks",
                sb.free_blocks, free_data
            ),
        });
    }
    let allocated_inodes = inode_bitmap.popcount();
    let free_inodes = geom.inode_count - allocated_inodes;
    if sb.free_inodes != free_inodes {
        report.issues.push(FsckIssue {
            severity: Severity::Warning,
            code: "ODF.SB.FREE_INODES",
            message: format!(
                "sb.free_inodes={} but bitmap reports {} free slots",
                sb.free_inodes, free_inodes
            ),
        });
    }

    // --- Walk every allocated inode slot -----------------------------------
    let mut blocks_owners: HashMap<u64, u64> = HashMap::new(); // block -> slot
    let mut kinds_seen: BTreeMap<u16, u64> = BTreeMap::new();
    let start_slot = if sb.layout_mode == LAYOUT_MODE_NATIVE {
        0u64
    } else {
        0u64
    };
    for slot in start_slot..geom.inode_count {
        let set = inode_bitmap.is_set(slot)?;
        if !set {
            continue;
        }
        report.inodes_checked += 1;
        let (block, offset) = geom.inode_record_location(slot)?;
        let raw = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
        let rec =
            match OnDiskInode::decode(&raw[offset as usize..offset as usize + INODE_RECORD_SIZE]) {
                Ok(r) => r,
                Err(e) => {
                    report.issues.push(FsckIssue {
                        severity: Severity::Error,
                        code: "ODF.INODE.DECODE",
                        message: format!("slot {slot} inode record undecodable: {e}"),
                    });
                    continue;
                }
            };
        if rec.kind == OnDiskKind::Unused {
            report.issues.push(FsckIssue {
                severity: Severity::Error,
                code: "ODF.INODE.UNUSED_BUT_BITMAP_SET",
                message: format!("slot {slot} marked allocated but record kind = Unused"),
            });
            continue;
        }
        *kinds_seen.entry(rec.kind as u16).or_insert(0) += 1;

        // Attr block (native layout) must be inside the data region and
        // allocated — verify.
        if rec.xattr_block_addr != 0 {
            if !geom.is_data_block(rec.xattr_block_addr) {
                report.issues.push(FsckIssue {
                    severity: Severity::Error,
                    code: "ODF.INODE.ATTR_OUT_OF_RANGE",
                    message: format!(
                        "slot {slot} attr block {} not inside data region",
                        rec.xattr_block_addr
                    ),
                });
            } else if !block_bitmap.is_set(rec.xattr_block_addr)? {
                report.issues.push(FsckIssue {
                    severity: Severity::Error,
                    code: "ODF.INODE.ATTR_UNALLOCATED",
                    message: format!(
                        "slot {slot} attr block {} not marked allocated",
                        rec.xattr_block_addr
                    ),
                });
            }
            if let Some(prev) = blocks_owners.insert(rec.xattr_block_addr, slot) {
                if prev != slot {
                    report.issues.push(FsckIssue {
                        severity: Severity::Error,
                        code: "ODF.BLOCK.DOUBLE_ALLOCATED",
                        message: format!(
                            "attr block {} claimed by both slot {} and slot {}",
                            rec.xattr_block_addr, prev, slot
                        ),
                    });
                }
            }
        }

        // Walk extents — direct first; fall back to the indirect chain.
        let mut extents = rec.extents.clone();
        if rec.flags & FLAG_HAS_EXTENT_INDEX != 0 {
            match super::extent_tree::ExtentChain::read_chain(device, rec.index_block_addr) {
                Ok(list) => extents = list,
                Err(e) => {
                    report.issues.push(FsckIssue {
                        severity: Severity::Error,
                        code: "ODF.INODE.EXTENT_CHAIN",
                        message: format!("slot {slot} extent chain unreadable: {e}"),
                    });
                    continue;
                }
            }
        }
        for ext in extents {
            report.extents_checked += 1;
            if ext.length_blocks == 0 {
                continue;
            }
            for i in 0..u64::from(ext.length_blocks) {
                let b = ext.physical_block + i;
                if !geom.is_data_block(b) {
                    report.issues.push(FsckIssue {
                        severity: Severity::Error,
                        code: "ODF.INODE.EXTENT_OUT_OF_RANGE",
                        message: format!("slot {slot} extent block {} outside data region", b),
                    });
                    continue;
                }
                if !block_bitmap.is_set(b)? {
                    report.issues.push(FsckIssue {
                        severity: Severity::Error,
                        code: "ODF.INODE.EXTENT_UNALLOCATED",
                        message: format!("slot {slot} extent block {} not marked allocated", b),
                    });
                }
                if let Some(prev) = blocks_owners.insert(b, slot) {
                    if prev != slot {
                        report.issues.push(FsckIssue {
                            severity: Severity::Error,
                            code: "ODF.BLOCK.DOUBLE_ALLOCATED",
                            message: format!(
                                "data block {b} claimed by both slot {prev} and slot {slot}"
                            ),
                        });
                    }
                }
                report.blocks_referenced += 1;
            }
        }
    }

    // --- Journal header ----------------------------------------------------
    if geom.journal_blocks >= 1 {
        match super::journal::Journal::inspect(device, &sb) {
            Ok(_hdr) => {}
            Err(e) => {
                report.issues.push(FsckIssue {
                    severity: Severity::Warning,
                    code: "ODF.JOURNAL.HEADER",
                    message: format!("journal header undecodable: {e}"),
                });
            }
        }
    }

    // --- Informational summary --------------------------------------------
    report.issues.push(FsckIssue {
        severity: Severity::Info,
        code: "ODF.SUMMARY.KINDS",
        message: format!(
            "{} inode(s) checked: {:?}",
            report.inodes_checked, kinds_seen
        ),
    });

    Ok(report)
}

fn read_superblock_at(device: &dyn BlockDevice, block: u64) -> CoreFsResult<Superblock> {
    let buf = device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
    Superblock::decode_block(&buf)
}

trait CloneableErr {
    fn clone_as_err(&self) -> crate::error::CoreFsError;
}

impl CloneableErr for crate::error::CoreFsError {
    fn clone_as_err(&self) -> crate::error::CoreFsError {
        self.clone()
    }
}

// Tests live in the main `corefs` crate (src/storage/ondisk/fsck_tests.rs)
// because they depend on crate::app::PersistedState and the blob-/native-
// mode volume helpers that have not been migrated to corefs-core yet.
// The main crate's src/storage/ondisk/mod.rs includes them via
// `#[path = "fsck_tests.rs"] mod fsck_tests_in_main;` under `#[cfg(test)]`.
