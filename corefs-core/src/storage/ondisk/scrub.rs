// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Self-healing background scrubber (D.10).
//!
//! [`super::fsck::check`] is a structural check: it
//! validates the superblock chain, bitmap CRCs, inode records and
//! extent addressing — but it does **not** read every single data
//! block.  Silent bit-rot in the data region goes undetected by fsck
//! alone.
//!
//! `scrub::run` walks every allocated inode's data extents, reads
//! each block, and verifies the per-inode `data_crc` that
//! [`native`][super::native] wrote when the file was saved.  Blocks
//! that fail verification are recorded in a [`ScrubReport`].  The
//! scrubber also opportunistically invokes [`fsck_repair::repair`] if
//! `ScrubPlan::auto_repair` is set, so a single call can both detect
//! and fix structural damage in one pass.
//!
//! ## Limitations
//!
//! Bit-rot in a data block is **detected** (via `data_crc`) but not
//! automatically recovered from: ODF v1 carries no data-level
//! redundancy — there is no mirror copy to fall back to.  A data-CRC
//! failure is therefore reported as `unrecoverable` and the caller
//! must decide whether to quarantine the file, restore from backup,
//! or unlink it.  Future ODF versions with mirror or parity
//! redundancy can extend the scrubber to use the redundant source
//! automatically.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::CoreFsResult;
use crate::storage::block_device::BlockDevice;

use super::checksum::Crc32c;
use super::fsck;
use super::fsck_repair;
use super::inode::{FLAG_HAS_EXTENT_INDEX, OnDiskKind};
use super::layout::BLOCK_SIZE;
use super::reader::OdfReader;

/// Configuration for a [`run`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubPlan {
    /// If `true`, invoke [`fsck_repair::repair`] after the structural
    /// walk so that auto-fixable issues (stale superblock copies,
    /// bitmap-CRC drift, inode-bitmap inconsistencies) are corrected
    /// in the same call.
    pub auto_repair: bool,
    /// If `true`, verify every file's content against its stored
    /// `data_crc`.  Set to false to limit a scrub to structural checks
    /// only — useful for a fast initial pass on very large volumes.
    pub verify_data_crc: bool,
}

impl ScrubPlan {
    /// The default behaviour: structural fsck, auto-repair, and
    /// per-file data-CRC verification.  Matches what an enterprise
    /// storage array's weekly scrub does.
    pub fn full() -> Self {
        Self {
            auto_repair: true,
            verify_data_crc: true,
        }
    }

    /// Fast structural-only scrub: fsck + auto-repair, no content
    /// reads.  Appropriate for a per-boot check on multi-TB volumes.
    pub fn structural_only() -> Self {
        Self {
            auto_repair: true,
            verify_data_crc: false,
        }
    }

    /// Read-only observer: no writes, no repair attempts.
    pub fn read_only() -> Self {
        Self {
            auto_repair: false,
            verify_data_crc: true,
        }
    }
}

/// Outcome of a scrub pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScrubReport {
    /// Number of data extents read during verification.
    pub extents_verified: u64,
    /// Number of data blocks that had their CRC re-computed.
    pub blocks_verified: u64,
    /// Data-CRC failures — `(slot, domain_inode_id)`.  These are
    /// unrecoverable without data redundancy.
    pub data_corruptions: Vec<(u64, u64)>,
    /// fsck issues that remained after an auto-repair pass, or would
    /// have been fixed (when `auto_repair` is false).
    pub residual_issues: Vec<fsck::FsckIssue>,
    /// Number of fsck repair-ops committed.
    pub repair_ops_committed: usize,
    /// Structural issues that the initial fsck pass found.
    pub fsck_issues_before: usize,
}

impl ScrubReport {
    /// `true` if the volume is structurally clean and no data-CRC
    /// failure was observed.
    pub fn is_clean(&self) -> bool {
        self.data_corruptions.is_empty()
            && !self
                .residual_issues
                .iter()
                .any(|i| i.severity == fsck::Severity::Error)
    }
}

/// Run a scrub pass.  Read-only when `plan.auto_repair == false`.
pub fn run(device: &mut dyn BlockDevice, plan: &ScrubPlan) -> CoreFsResult<ScrubReport> {
    let mut report = ScrubReport::default();

    // --- Structural fsck -------------------------------------------------
    let initial = fsck::check(device)?;
    report.fsck_issues_before = initial
        .issues
        .iter()
        .filter(|i| matches!(i.severity, fsck::Severity::Error | fsck::Severity::Warning))
        .count();

    if plan.auto_repair {
        let repair = fsck_repair::repair(device, &initial)?;
        report.repair_ops_committed = repair.ops_committed;
        let after = fsck::check(device)?;
        report.residual_issues = after
            .issues
            .iter()
            .filter(|i| matches!(i.severity, fsck::Severity::Error | fsck::Severity::Warning))
            .cloned()
            .collect();
    } else {
        report.residual_issues = initial
            .issues
            .iter()
            .filter(|i| matches!(i.severity, fsck::Severity::Error | fsck::Severity::Warning))
            .cloned()
            .collect();
    }

    // --- Data-CRC verification ------------------------------------------
    if plan.verify_data_crc {
        let reader = OdfReader::open(device)?;
        let summaries = reader.list_inodes()?;
        for summary in summaries {
            // Only files carry meaningful data_crc; directories and
            // symlinks often have size_bytes = 0.
            if summary.size_bytes == 0 {
                continue;
            }
            let on_disk = reader.read_on_disk_inode(summary.slot)?;
            if on_disk.kind != OnDiskKind::File {
                continue;
            }
            let extents = if on_disk.flags & FLAG_HAS_EXTENT_INDEX != 0 {
                super::extent_tree::ExtentChain::read_chain(device, on_disk.index_block_addr)?
            } else {
                on_disk.extents.clone()
            };
            if extents.is_empty() || on_disk.data_crc == 0 {
                continue;
            }
            report.extents_verified += extents.len() as u64;
            // Read + cat + verify.  Matches what OdfReader::read_file_content does
            // but without panicking on mismatch.
            let mut bytes = Vec::with_capacity(on_disk.size_bytes as usize);
            let mut remaining = on_disk.size_bytes as usize;
            let mut sorted = extents.clone();
            sorted.sort_by_key(|e| e.logical_block);
            for ext in &sorted {
                let byte_offset = ext.physical_block * BLOCK_SIZE;
                let byte_len = u64::from(ext.length_blocks) * BLOCK_SIZE;
                let block_data = device.read_at(byte_offset, byte_len)?;
                let wanted = remaining.min(block_data.len());
                bytes.extend_from_slice(&block_data[..wanted]);
                remaining -= wanted;
                report.blocks_verified += u64::from(ext.length_blocks);
            }
            let actual = u64::from(Crc32c::hash(&bytes));
            if actual != on_disk.data_crc {
                report
                    .data_corruptions
                    .push((summary.slot, summary.domain_id));
            }
        }
    }

    Ok(report)
}

// Tests live in the main `corefs` crate — they depend on
// `session::OdfDeviceSession`, which is std-bound and not migrated.
