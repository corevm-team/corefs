use crate::app::PersistedState;
use crate::domain::inode::{Inode, InodeKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub timestamp: SystemTime,
    pub operation: String,
    pub target: String,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalTransaction {
    pub id: u64,
    pub label: String,
    pub started_at: SystemTime,
    pub operations: Vec<JournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JournalRuntimeState {
    pub next_transaction_id: u64,
    pub last_committed_transaction_id: Option<u64>,
    pub last_aborted_transaction_id: Option<u64>,
    pub last_replayed_transaction_id: Option<u64>,
    pub unclean_shutdown: bool,
    pub pending_transaction: Option<JournalTransaction>,
}

#[derive(Debug, Default)]
pub struct JournalService {
    entries: Vec<JournalEntry>,
    runtime: JournalRuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalReplayState {
    pub active_paths: BTreeSet<String>,
    pub deleted_paths: BTreeSet<String>,
    pub snapshot_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalRepairSummary {
    pub moved_to_deleted: usize,
    pub restored_to_active: usize,
    pub purged_deleted: usize,
    pub removed_orphan_blocks: usize,
    pub resized_inodes: usize,
    pub snapshot_id_adjusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalRecoverySummary {
    pub aborted_pending_transaction: bool,
    pub cleared_unclean_shutdown: bool,
    pub replay_recorded: bool,
}

impl JournalService {
    pub fn record(&mut self, operation: &str, target: &str, details: impl Into<String>) {
        let entry = JournalEntry {
            timestamp: SystemTime::now(),
            operation: operation.to_string(),
            target: target.to_string(),
            details: details.into(),
        };

        if let Some(transaction) = self.runtime.pending_transaction.as_mut() {
            transaction.operations.push(entry);
        } else {
            self.entries.push(entry);
        }
    }

    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    pub fn runtime_state(&self) -> &JournalRuntimeState {
        &self.runtime
    }

    pub fn has_pending_transaction(&self) -> bool {
        self.runtime.pending_transaction.is_some()
    }

    pub fn begin_transaction(&mut self, label: impl Into<String>) -> u64 {
        if let Some(transaction) = self.runtime.pending_transaction.as_ref() {
            return transaction.id;
        }

        self.runtime.next_transaction_id += 1;
        let id = self.runtime.next_transaction_id;
        self.runtime.pending_transaction = Some(JournalTransaction {
            id,
            label: label.into(),
            started_at: SystemTime::now(),
            operations: Vec::new(),
        });
        id
    }

    pub fn commit_transaction(&mut self) -> Option<u64> {
        let transaction = self.runtime.pending_transaction.take()?;
        let id = transaction.id;
        let operation_count = transaction.operations.len();
        self.entries.push(JournalEntry {
            timestamp: SystemTime::now(),
            operation: "tx_begin".to_string(),
            target: transaction.label.clone(),
            details: format!("id={id} ops={operation_count}"),
        });
        self.entries.extend(transaction.operations);
        self.entries.push(JournalEntry {
            timestamp: SystemTime::now(),
            operation: "tx_commit".to_string(),
            target: transaction.label,
            details: format!("id={id} ops={operation_count}"),
        });
        self.runtime.last_committed_transaction_id = Some(id);
        Some(id)
    }

    pub fn abort_transaction(&mut self, reason: &str) -> Option<u64> {
        let transaction = self.runtime.pending_transaction.take()?;
        let id = transaction.id;
        self.entries.push(JournalEntry {
            timestamp: SystemTime::now(),
            operation: "tx_abort".to_string(),
            target: transaction.label,
            details: format!("id={id} reason={reason}"),
        });
        self.runtime.last_aborted_transaction_id = Some(id);
        Some(id)
    }

    pub fn mark_unclean_shutdown(&mut self) {
        self.runtime.unclean_shutdown = true;
    }

    pub fn mark_clean_shutdown(&mut self) {
        self.runtime.unclean_shutdown = false;
    }

    pub fn recover_on_load(&mut self) -> JournalRecoverySummary {
        let mut summary = JournalRecoverySummary::default();

        if let Some(transaction_id) = self.abort_transaction("recovery_replay") {
            self.runtime.last_replayed_transaction_id = Some(transaction_id);
            summary.aborted_pending_transaction = true;
        }

        if self.runtime.unclean_shutdown {
            self.runtime.unclean_shutdown = false;
            summary.cleared_unclean_shutdown = true;
        }

        if summary.aborted_pending_transaction || summary.cleared_unclean_shutdown {
            self.entries.push(JournalEntry {
                timestamp: SystemTime::now(),
                operation: "recovery_replay".to_string(),
                target: "/".to_string(),
                details: format!(
                    "aborted_pending={} cleared_unclean={}",
                    summary.aborted_pending_transaction, summary.cleared_unclean_shutdown
                ),
            });
            summary.replay_recorded = true;
        }

        summary
    }

    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self {
            entries,
            runtime: JournalRuntimeState::default(),
        }
    }

    pub fn from_entries_with_runtime(
        entries: Vec<JournalEntry>,
        runtime: JournalRuntimeState,
    ) -> Self {
        Self { entries, runtime }
    }

    pub fn replay(&self) -> JournalReplayState {
        let mut state = JournalReplayState::default();

        for entry in &self.entries {
            match entry.operation.as_str() {
                "create_file" | "create_directory" | "create_symlink" | "write_file"
                | "restore" => {
                    state.active_paths.insert(entry.target.clone());
                    state.deleted_paths.remove(&entry.target);
                }
                "delete" => {
                    state.active_paths.remove(&entry.target);
                    state.deleted_paths.insert(entry.target.clone());
                }
                "secure_delete" => {
                    state.active_paths.remove(&entry.target);
                    state.deleted_paths.remove(&entry.target);
                }
                "snapshot" => {
                    state.snapshot_count += 1;
                }
                "format" | "sync" | "rename" | "tx_begin" | "tx_commit" | "tx_abort"
                | "recovery_replay" => {}
                _ => {}
            }
        }

        state
    }
}

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
mod tests {
    use super::*;

    #[test]
    fn record_appends_journal_entries() {
        let mut journal = JournalService::default();
        journal.record("create", "/tmp/file", "bytes=4");

        assert_eq!(journal.entries().len(), 1);
        assert_eq!(journal.entries()[0].operation, "create");
        assert_eq!(journal.entries()[0].target, "/tmp/file");
        assert_eq!(journal.entries()[0].details, "bytes=4");
    }

    #[test]
    fn transactions_stage_entries_until_commit() {
        let mut journal = JournalService::default();
        let tx_id = journal.begin_transaction("rw-writeback");
        journal.record("write_file", "/a", "bytes=3");

        assert!(journal.entries().is_empty());
        assert!(journal.has_pending_transaction());

        let committed = journal.commit_transaction();
        assert_eq!(committed, Some(tx_id));
        assert!(!journal.has_pending_transaction());
        assert_eq!(journal.entries().len(), 3);
        assert_eq!(journal.entries()[0].operation, "tx_begin");
        assert_eq!(journal.entries()[1].operation, "write_file");
        assert_eq!(journal.entries()[2].operation, "tx_commit");
    }

    #[test]
    fn recover_on_load_aborts_pending_transaction_and_clears_dirty_marker() {
        let mut journal = JournalService::default();
        journal.mark_unclean_shutdown();
        let tx_id = journal.begin_transaction("rw-writeback");
        journal.record("write_file", "/a", "bytes=3");

        let summary = journal.recover_on_load();

        assert!(summary.aborted_pending_transaction);
        assert!(summary.cleared_unclean_shutdown);
        assert!(summary.replay_recorded);
        assert_eq!(
            journal.runtime_state().last_replayed_transaction_id,
            Some(tx_id)
        );
        assert!(!journal.runtime_state().unclean_shutdown);
        assert!(
            journal
                .entries()
                .iter()
                .any(|entry| entry.operation == "tx_abort")
        );
        assert!(
            journal
                .entries()
                .iter()
                .any(|entry| entry.operation == "recovery_replay")
        );
    }

    #[test]
    fn replay_tracks_active_deleted_and_snapshots() {
        let mut journal = JournalService::default();
        journal.record("create_file", "/a", "");
        journal.record("delete", "/a", "");
        journal.record("restore", "/a", "");
        journal.record("snapshot", "/", "name=one");
        journal.record("secure_delete", "/a", "");

        let replay = journal.replay();
        assert!(!replay.active_paths.contains("/a"));
        assert!(!replay.deleted_paths.contains("/a"));
        assert_eq!(replay.snapshot_count, 1);
    }

    #[test]
    fn reconcile_persisted_state_normalizes_entries_and_blocks() {
        use crate::config::CoreFsConfig;
        use crate::domain::metadata::FileMetadata;
        use crate::domain::snapshot::Snapshot;
        use crate::domain::volume::VolumeDescriptor;
        use crate::storage::block_store::BlockRecord;

        let mut state = PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: vec![Inode::new(
                crate::domain::inode::InodeId(1),
                InodeKind::File,
                "/a".to_string(),
                FileMetadata::default(),
            )],
            deleted_inodes: vec![Inode::new(
                crate::domain::inode::InodeId(2),
                InodeKind::File,
                "/b".to_string(),
                FileMetadata::default(),
            )],
            allocator_policy: crate::storage::block_store::AllocatorPolicy::default(),
            free_extents: Vec::new(),
            block_records: vec![
                BlockRecord {
                    inode: crate::domain::inode::InodeId(1),
                    bytes: b"hello".to_vec(),
                    checksum: 1,
                    device_block: 0,
                    allocated_blocks: 1,
                },
                BlockRecord {
                    inode: crate::domain::inode::InodeId(99),
                    bytes: b"orphan".to_vec(),
                    checksum: 2,
                    device_block: 1,
                    allocated_blocks: 1,
                },
            ],
            journal_entries: vec![
                JournalEntry {
                    timestamp: SystemTime::now(),
                    operation: "delete".to_string(),
                    target: "/a".to_string(),
                    details: String::new(),
                },
                JournalEntry {
                    timestamp: SystemTime::now(),
                    operation: "restore".to_string(),
                    target: "/b".to_string(),
                    details: String::new(),
                },
                JournalEntry {
                    timestamp: SystemTime::now(),
                    operation: "snapshot".to_string(),
                    target: "/".to_string(),
                    details: String::new(),
                },
            ],
            journal_runtime: JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: vec![Snapshot {
                id: 1,
                name: "baseline".to_string(),
                scope_root: "/".to_string(),
                created_at: SystemTime::now(),
                paths: vec!["/".to_string()],
            }],
            next_snapshot_id: 0,
        };

        let summary = reconcile_persisted_state(&mut state);

        assert_eq!(summary.moved_to_deleted, 1);
        assert_eq!(summary.restored_to_active, 1);
        assert_eq!(summary.removed_orphan_blocks, 1);
        assert_eq!(summary.resized_inodes, 1);
        assert!(summary.snapshot_id_adjusted);
        assert_eq!(state.active_inodes.len(), 1);
        assert_eq!(state.active_inodes[0].path, "/b");
        assert_eq!(state.deleted_inodes.len(), 1);
        assert_eq!(state.deleted_inodes[0].path, "/a");
        assert_eq!(state.block_records.len(), 1);
        assert_eq!(state.next_snapshot_id, 1);
    }
}
