// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Crash-consistent save pipeline on top of the [`super::journal`] WAL.
//!
//! The classic "ordered-mode" strategy from ext4/JBD2: data blocks
//! (payload content, attr blocks, ancillary blob) are written directly
//! to the device, metadata blocks (bitmaps, inode records, superblock
//! copies) are **staged** as journal [`Op`]s and applied via
//! [`Journal::replay`] only after the commit record has become durable.
//!
//! ```text
//!   Phase 1: plan           (in-memory only)
//!   Phase 2: write data     (direct device writes — idempotent)
//!   Phase 3: stage metadata ops into a transaction
//!   Phase 4: commit txn     (durable on sync)
//!   Phase 5: replay ops     (overwrite target blocks — idempotent)
//!   Phase 6: checkpoint     (reset journal head/tail)
//! ```
//!
//! ## Crash windows
//!
//! | crash between … and …            | recovery behaviour                        |
//! |----------------------------------|-------------------------------------------|
//! | phase 2 / phase 4 (no commit)    | orphaned data blocks; old metadata intact |
//! | phase 4 (commit) / phase 5       | next mount: replay applies ops → new SB   |
//! | phase 5 partial                  | replay re-applies (idempotent)            |
//! | phase 6 partial                  | no effect — journal already empty         |
//!
//! This matches what file-system literature calls **crash-consistent
//! metadata** — data itself may be "newer" or "older" depending on the
//! crash window, but the pointer structure never ends up referring to
//! half-written data.  Callers who need data-journal semantics can route
//! data writes through [`JournaledSaveSession::stage_metadata_block`]
//! instead, at the cost of doubling the journal footprint.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::journal::{Journal, Op};
use super::layout::BLOCK_SIZE;
use super::superblock::Superblock;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;

/// Handle onto a staged-metadata save in progress.  Created by
/// [`JournaledSaveSession::open`]; finished by [`Self::commit`] or
/// [`Self::abort`].
pub struct JournaledSaveSession<'d> {
    device: &'d mut dyn BlockDevice,
    sb: Superblock,
    ops: Vec<Op>,
}

/// Summary of a completed journaled save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournaledSaveReport {
    pub ops_applied: usize,
    pub txn_id: u64,
}

impl<'d> JournaledSaveSession<'d> {
    /// Open a new session.  Reads (and retains) the current primary
    /// superblock so the journal location is known even if callers
    /// mutate the superblock later.
    pub fn open(device: &'d mut dyn BlockDevice) -> CoreFsResult<Self> {
        let sb = super::volume::read_sb_with_fallbacks(device)?;
        Ok(Self {
            device,
            sb,
            ops: Vec::new(),
        })
    }

    /// Direct data-block write — bypasses the journal.  Bytes must be a
    /// multiple of the device's sector size.
    pub fn write_data(&mut self, block: u64, bytes: &[u8]) -> CoreFsResult<()> {
        self.device.write_at(block * BLOCK_SIZE, bytes)
    }

    /// Stage a full 4 KiB metadata block for journaled application.
    /// `data` **must** be exactly [`BLOCK_SIZE`] bytes.
    pub fn stage_metadata_block(&mut self, block: u64, data: Vec<u8>) -> CoreFsResult<()> {
        if data.len() as u64 != BLOCK_SIZE {
            return Err(CoreFsError::InvalidInput(format!(
                "journaled save: metadata block payload must be {BLOCK_SIZE} bytes, got {}",
                data.len()
            )));
        }
        self.ops.push(Op::BlockWrite { block, data });
        Ok(())
    }

    /// Stage a single on-disk inode record update at `slot`.
    /// `record` must be exactly [`INODE_RECORD_SIZE`] bytes.
    ///
    /// [`INODE_RECORD_SIZE`]: super::inode::INODE_RECORD_SIZE
    pub fn stage_inode_update(&mut self, slot: u64, record: Vec<u8>) -> CoreFsResult<()> {
        if record.len() != super::inode::INODE_RECORD_SIZE {
            return Err(CoreFsError::InvalidInput(format!(
                "journaled save: inode record must be {} bytes, got {}",
                super::inode::INODE_RECORD_SIZE,
                record.len()
            )));
        }
        self.ops.push(Op::InodeUpdate {
            index: slot,
            record,
        });
        Ok(())
    }

    /// Number of ops currently staged.
    pub fn staged_ops(&self) -> usize {
        self.ops.len()
    }

    /// Underlying device (so callers can read current state for
    /// planning purposes).
    pub fn device(&self) -> &dyn BlockDevice {
        self.device
    }

    /// Cached superblock as read at [`Self::open`] time.
    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    /// Discard staged ops and end the session.  No writes are
    /// journalled; data-block writes already performed stay on disk.
    pub fn abort(self) -> CoreFsResult<()> {
        Ok(())
    }

    /// Commit all staged metadata ops in a single transaction, apply
    /// them, and checkpoint the journal.  Returns a report describing
    /// the commit.
    pub fn commit(self) -> CoreFsResult<JournaledSaveReport> {
        let ops_count = self.ops.len();
        if ops_count == 0 {
            return Err(CoreFsError::InvalidInput(
                "journaled save: no metadata ops staged — use abort() instead".into(),
            ));
        }
        // Sync data writes before the journal commit so that
        // crash-after-commit can safely re-apply metadata against the
        // already-persistent data blocks.
        self.device.sync()?;

        let mut journal = Journal::open(self.device, &self.sb)?;
        let mut txn = journal.begin();
        let txn_id = txn.txn_id();
        for op in self.ops {
            txn.push(op);
        }
        txn.commit(&mut journal)?;
        // Apply ops to the target locations.  replay() iterates the
        // newly-committed transaction and writes each op to the device.
        let applied = journal.replay()?;
        let ops_applied = applied.iter().map(|r| r.ops).sum::<usize>();
        journal.checkpoint()?;
        Ok(JournaledSaveReport {
            ops_applied,
            txn_id,
        })
    }

    /// Variant of [`Self::commit`] that commits the txn but stops
    /// **before** calling [`Journal::replay`] / [`Journal::checkpoint`].
    /// Used exclusively by tests to simulate a crash between the commit
    /// record landing on disk and the metadata being fully applied —
    /// the next mount must transparently recover via replay.
    #[doc(hidden)]
    pub fn commit_without_apply(self) -> CoreFsResult<u64> {
        if self.ops.is_empty() {
            return Err(CoreFsError::InvalidInput(
                "journaled save: no metadata ops staged".into(),
            ));
        }
        self.device.sync()?;
        let mut journal = Journal::open(self.device, &self.sb)?;
        let mut txn = journal.begin();
        let txn_id = txn.txn_id();
        for op in self.ops {
            txn.push(op);
        }
        txn.commit(&mut journal)?;
        Ok(txn_id)
    }
}

/// Recovery helper — opens the journal at the primary superblock's
/// `journal_start`, replays every committed pending transaction and
/// checkpoints.  Idempotent: calling on an empty journal is a no-op.
pub fn recover_pending_transactions(
    device: &mut dyn BlockDevice,
) -> CoreFsResult<JournalRecoveryReport> {
    let sb = super::volume::read_sb_with_fallbacks(device)?;
    let mut journal = Journal::open(device, &sb)?;
    let applied = journal.replay()?;
    let total_ops: usize = applied.iter().map(|r| r.ops).sum();
    let txns: Vec<u64> = applied.iter().map(|r| r.txn_id).collect();
    journal.checkpoint()?;
    Ok(JournalRecoveryReport {
        txns_replayed: txns,
        ops_applied: total_ops,
    })
}

/// Outcome of [`recover_pending_transactions`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalRecoveryReport {
    pub txns_replayed: Vec<u64>,
    pub ops_applied: usize,
}

#[cfg(test)]
#[path = "journaled_tests.rs"]
mod tests;
