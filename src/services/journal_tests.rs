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
        hot_path_records: Vec::new(),
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
            file_data: std::collections::BTreeMap::new(),
            inodes: std::collections::BTreeMap::new(),
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
