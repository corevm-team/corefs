// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! `PersistedState`-spezifische Journal-Reparaturen.
//!
//! Der Kern des Journal-Services liegt in [`corefs_core::services::journal`]
//! und ist plattformneutral (`no_std + alloc`). Diese Datei ergänzt
//! main-Crate-spezifische Funktionen, die den `PersistedState` aus `crate::app`
//! kennen.

pub use corefs_core::services::journal::{
    JournalEntry, JournalRecoverySummary, JournalReplayState, JournalRepairSummary,
    JournalRuntimeState, JournalService, JournalTransaction,
};

use crate::app::PersistedState;
use crate::domain::inode::{Inode, InodeKind};
use std::collections::BTreeSet;

/// Reconziliert einen [`PersistedState`] gegen sein eigenes Journal:
/// aktive/gelöschte Inode-Listen werden mit den Journal-Effekten abgeglichen,
/// Waisen-Block-Records entfernt, Inode-Größen korrigiert und der
/// Snapshot-Counter nachgeführt.
pub fn reconcile_persisted_state(state: &mut PersistedState) -> JournalRepairSummary {
    let replay = JournalService::from_entries(state.journal_entries.clone()).replay();
    let mut summary = JournalRepairSummary::default();

    let mut next_active = Vec::new();
    let mut next_deleted = state.deleted_inodes.clone();

    for inode in state.active_inodes.drain(..) {
        if replay.deleted_paths.contains(&inode.path) {
            next_deleted.push(inode);
            summary.moved_to_deleted += 1;
        } else {
            next_active.push(inode);
        }
    }

    let mut restored = Vec::new();
    next_deleted.retain(|inode| {
        if replay.active_paths.contains(&inode.path) {
            restored.push(inode.clone());
            summary.restored_to_active += 1;
            false
        } else if replay.deleted_paths.contains(&inode.path) {
            true
        } else {
            summary.purged_deleted += 1;
            false
        }
    });

    next_active.extend(restored);
    state.active_inodes = dedupe_inodes_by_path(next_active);
    state.deleted_inodes = dedupe_inodes_by_path(next_deleted);

    let known_inodes: BTreeSet<_> = state
        .active_inodes
        .iter()
        .chain(state.deleted_inodes.iter())
        .map(|inode| inode.id)
        .collect();
    let before_blocks = state.block_records.len();
    state
        .block_records
        .retain(|record| known_inodes.contains(&record.inode));
    summary.removed_orphan_blocks = before_blocks.saturating_sub(state.block_records.len());

    for inode in state
        .active_inodes
        .iter_mut()
        .chain(state.deleted_inodes.iter_mut())
    {
        if matches!(inode.kind, InodeKind::File | InodeKind::Symlink) {
            let block_len = state
                .block_records
                .iter()
                .find(|record| record.inode == inode.id)
                .map(|record| record.bytes.len())
                .unwrap_or(0);
            if inode.size != block_len {
                inode.size = block_len;
                summary.resized_inodes += 1;
            }
        }
    }

    if state.next_snapshot_id < replay.snapshot_count as u64 {
        state.next_snapshot_id = replay.snapshot_count as u64;
        summary.snapshot_id_adjusted = true;
    }

    summary
}

fn dedupe_inodes_by_path(mut inodes: Vec<Inode>) -> Vec<Inode> {
    inodes.sort_by(|left, right| left.path.cmp(&right.path));
    inodes.dedup_by(|left, right| left.path == right.path);
    inodes
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
