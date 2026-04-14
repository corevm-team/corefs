// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Repair pass that consumes an [`FsckReport`] and fixes every
//! auto-repairable issue through a single journaled transaction
//! ([`crate::storage::ondisk::journaled::JournaledSaveSession`]).
//!
//! Repairs are crash-consistent: a power loss during the repair is
//! either fully rolled forward (commit + replay on next mount) or
//! never took effect (commit record didn't land).  Issues that cannot
//! be fixed without risking data loss — double-allocations, out-of-
//! range extents, inode-record decode failures — are reported but
//! left untouched; the caller decides whether to escalate.
//!
//! ## Fix table
//!
//! | code                                | strategy                                            |
//! |-------------------------------------|-----------------------------------------------------|
//! | `ODF.SB.TERTIARY_*` / `..STALE`     | rewrite tertiary SB from primary                    |
//! | `ODF.SB.SECONDARY_*` / `..STALE`    | rewrite secondary SB from primary                   |
//! | `ODF.BBM.CRC`                       | recompute CRC from actual bytes, update SB          |
//! | `ODF.IBM.CRC`                       | recompute CRC from actual bytes, update SB          |
//! | `ODF.SB.FREE_BLOCKS`                | recompute from bitmap, patch SB                     |
//! | `ODF.SB.FREE_INODES`                | recompute from bitmap, patch SB                     |
//! | `ODF.INODE.UNUSED_BUT_BITMAP_SET`   | clear the inode bitmap bit                          |
//! | `ODF.INODE.EXTENT_UNALLOCATED`      | mark the referenced block as allocated              |
//! | `ODF.INODE.ATTR_UNALLOCATED`        | mark the attr block as allocated                    |
//! | anything else                       | reported in `unfixable`; caller-side intervention   |

use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::fsck::{FsckIssue, FsckReport, Severity};
use super::journaled::JournaledSaveSession;
use super::layout::{BLOCK_SIZE, LayoutGeometry, PRIMARY_SUPERBLOCK_BLOCK};
use super::superblock::Superblock;
use crate::error::CoreFsResult;
use crate::storage::block_device::BlockDevice;

/// Summary of a repair pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepairReport {
    /// Issue codes that were successfully fixed.
    pub fixed: Vec<&'static str>,
    /// Issues that the repair pass refused to touch.
    pub unfixable: Vec<FsckIssue>,
    /// Number of journaled metadata ops that were committed.
    pub ops_committed: usize,
}

/// Apply every auto-fixable repair hinted at by `report` as a single
/// journaled transaction against `device`.  No-op if the report is
/// already clean.
pub fn repair(device: &mut dyn BlockDevice, report: &FsckReport) -> CoreFsResult<RepairReport> {
    // Filter issues worth acting on.
    let actionable: Vec<&FsckIssue> = report
        .issues
        .iter()
        .filter(|i| matches!(i.severity, Severity::Error | Severity::Warning))
        .collect();
    if actionable.is_empty() {
        return Ok(RepairReport::default());
    }

    // Snapshot bitmaps + superblock before staging ops.
    let sb = super::volume::read_sb_with_fallbacks(device)?;
    let geom = sb.geometry();
    let mut bbm_bytes = device.read_at(
        geom.block_bitmap_start * BLOCK_SIZE,
        geom.block_bitmap_blocks * BLOCK_SIZE,
    )?;
    let mut ibm_bytes = device.read_at(
        geom.inode_bitmap_start * BLOCK_SIZE,
        geom.inode_bitmap_blocks * BLOCK_SIZE,
    )?;

    let mut block_bitmap = Bitmap::from_bytes(bbm_bytes.clone(), geom.total_blocks)?;
    let mut inode_bitmap = Bitmap::from_bytes(ibm_bytes.clone(), geom.inode_count)?;

    let mut patched_sb = sb.clone();
    let mut bitmap_touched = false;
    let mut inode_bitmap_touched = false;
    let mut sb_touched = false;
    let mut rewrite_tertiary = false;
    let mut rewrite_secondary = false;

    let mut fixed: Vec<&'static str> = Vec::new();
    let mut unfixable: Vec<FsckIssue> = Vec::new();

    for issue in &actionable {
        match issue.code {
            "ODF.SB.TERTIARY_STALE" | "ODF.SB.TERTIARY_UNREADABLE" => {
                rewrite_tertiary = true;
                fixed.push(issue.code);
            }
            "ODF.SB.SECONDARY_STALE" | "ODF.SB.SECONDARY_UNREADABLE" => {
                rewrite_secondary = true;
                fixed.push(issue.code);
            }
            "ODF.BBM.CRC" => {
                patched_sb.block_bitmap_crc = Crc32c::hash(&bbm_bytes);
                sb_touched = true;
                fixed.push(issue.code);
            }
            "ODF.IBM.CRC" => {
                patched_sb.inode_bitmap_crc = Crc32c::hash(&ibm_bytes);
                sb_touched = true;
                fixed.push(issue.code);
            }
            "ODF.SB.FREE_BLOCKS" => {
                let used = (geom.data_start..geom.total_blocks)
                    .filter(|b| block_bitmap.is_set(*b).unwrap_or(true))
                    .count() as u64;
                patched_sb.free_blocks = geom.data_blocks - used;
                sb_touched = true;
                fixed.push(issue.code);
            }
            "ODF.SB.FREE_INODES" => {
                patched_sb.free_inodes = geom.inode_count - inode_bitmap.popcount();
                sb_touched = true;
                fixed.push(issue.code);
            }
            "ODF.INODE.UNUSED_BUT_BITMAP_SET" => {
                // Message shape: "slot {N} marked allocated but record kind = Unused"
                if let Some(slot) = parse_slot_from_message(&issue.message) {
                    if inode_bitmap.is_set(slot).unwrap_or(false) {
                        let _ = inode_bitmap.clear(slot);
                        inode_bitmap_touched = true;
                        fixed.push(issue.code);
                    } else {
                        unfixable.push((*issue).clone());
                    }
                } else {
                    unfixable.push((*issue).clone());
                }
            }
            "ODF.INODE.EXTENT_UNALLOCATED" | "ODF.INODE.ATTR_UNALLOCATED" => {
                // Message shape: "slot {N} extent block {B} not marked allocated"
                //              or "slot {N} attr block {B} not marked allocated"
                if let Some(block) = parse_block_from_message(&issue.message) {
                    if geom.is_data_block(block) {
                        let _ = block_bitmap.set(block);
                        bitmap_touched = true;
                        fixed.push(issue.code);
                    } else {
                        unfixable.push((*issue).clone());
                    }
                } else {
                    unfixable.push((*issue).clone());
                }
            }
            _ => {
                unfixable.push((*issue).clone());
            }
        }
    }

    // If bitmap was touched, recompute the superblock's cached CRCs too.
    if bitmap_touched {
        bbm_bytes = block_bitmap.as_bytes().to_vec();
        patched_sb.block_bitmap_crc = Crc32c::hash(&bbm_bytes);
        // Recompute free_blocks as well so the SB stays consistent.
        let used = (geom.data_start..geom.total_blocks)
            .filter(|b| block_bitmap.is_set(*b).unwrap_or(true))
            .count() as u64;
        patched_sb.free_blocks = geom.data_blocks - used;
        sb_touched = true;
    }
    if inode_bitmap_touched {
        ibm_bytes = inode_bitmap.as_bytes().to_vec();
        patched_sb.inode_bitmap_crc = Crc32c::hash(&ibm_bytes);
        patched_sb.free_inodes = geom.inode_count - inode_bitmap.popcount();
        sb_touched = true;
    }

    // Nothing actionable changed → no transaction needed.
    if !bitmap_touched
        && !inode_bitmap_touched
        && !sb_touched
        && !rewrite_tertiary
        && !rewrite_secondary
    {
        return Ok(RepairReport {
            fixed,
            unfixable,
            ops_committed: 0,
        });
    }

    // Bump generation + last_write_at so observers see the repair.
    patched_sb.generation = patched_sb.generation.saturating_add(1);
    patched_sb.last_write_at = now_secs();

    // Stage every modified block as a journaled metadata write.
    let mut sess = JournaledSaveSession::open(device)?;
    if bitmap_touched {
        stage_bitmap_writes(&mut sess, geom.block_bitmap_start, &bbm_bytes)?;
    }
    if inode_bitmap_touched {
        stage_bitmap_writes(&mut sess, geom.inode_bitmap_start, &ibm_bytes)?;
    }
    // The generation bump forces every SB copy to be rewritten — otherwise
    // a stale redundant copy would immediately re-trigger the
    // ODF.SB.*_STALE warning on the next fsck run.  `rewrite_tertiary` /
    // `rewrite_secondary` remain as evidence of which copy was the
    // original repair target.
    let _ = rewrite_tertiary;
    let _ = rewrite_secondary;
    let sb_block = patched_sb.encode_block();
    sess.stage_metadata_block(PRIMARY_SUPERBLOCK_BLOCK, sb_block.clone())?;
    sess.stage_metadata_block(geom.tertiary_superblock_block, sb_block.clone())?;
    sess.stage_metadata_block(geom.secondary_superblock_block, sb_block.clone())?;
    let staged = sess.staged_ops();
    let commit = sess.commit()?;
    Ok(RepairReport {
        fixed,
        unfixable,
        ops_committed: staged.max(commit.ops_applied),
    })
}

fn stage_bitmap_writes(
    sess: &mut JournaledSaveSession<'_>,
    start_block: u64,
    bytes: &[u8],
) -> CoreFsResult<()> {
    debug_assert_eq!(bytes.len() as u64 % BLOCK_SIZE, 0);
    let block_count = bytes.len() as u64 / BLOCK_SIZE;
    for i in 0..block_count {
        let off = (i * BLOCK_SIZE) as usize;
        let slice = &bytes[off..off + BLOCK_SIZE as usize];
        sess.stage_metadata_block(start_block + i, slice.to_vec())?;
    }
    Ok(())
}

// Helper: parse "slot {N} …" out of a fsck message.
fn parse_slot_from_message(msg: &str) -> Option<u64> {
    let needle = "slot ";
    let start = msg.find(needle)? + needle.len();
    let rest = &msg[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse::<u64>().ok()
}

// Helper: parse "… block {B} …" out of a fsck message (first occurrence
// after a "slot N " prefix to avoid misparsing the slot number itself).
fn parse_block_from_message(msg: &str) -> Option<u64> {
    let after_slot = match msg.find("slot ") {
        Some(idx) => {
            let rest = &msg[idx + 5..];
            match rest.find(' ') {
                Some(j) => &rest[j + 1..],
                None => rest,
            }
        }
        None => msg,
    };
    let needle = "block ";
    let start = after_slot.find(needle)? + needle.len();
    let rest = &after_slot[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse::<u64>().ok()
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// Ensure LayoutGeometry import is used in release builds too.
#[allow(dead_code)]
fn _ensure_geometry_import_used() -> Option<LayoutGeometry> {
    None
}

// Superblock import used via super::volume::read_sb_with_fallbacks; keep
// the explicit import for clarity.
#[allow(dead_code)]
fn _ensure_superblock_import_used(sb: &Superblock) -> u64 {
    sb.generation
}

#[cfg(test)]
#[path = "fsck_repair_tests.rs"]
mod tests;
