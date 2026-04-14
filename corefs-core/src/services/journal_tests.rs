// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::platform::Timestamp;

#[test]
fn record_appends_journal_entries() {
    let mut journal = JournalService::default();
    journal.record_at("create", "/tmp/file", "bytes=4", Timestamp::EPOCH);

    assert_eq!(journal.entries().len(), 1);
    assert_eq!(journal.entries()[0].operation, "create");
    assert_eq!(journal.entries()[0].target, "/tmp/file");
    assert_eq!(journal.entries()[0].details, "bytes=4");
}

#[test]
fn transactions_stage_entries_until_commit() {
    let mut journal = JournalService::default();
    let tx_id = journal.begin_transaction_at("rw-writeback", Timestamp::EPOCH);
    journal.record_at("write_file", "/a", "bytes=3", Timestamp::EPOCH);

    assert!(journal.entries().is_empty());
    assert!(journal.has_pending_transaction());

    let committed = journal.commit_transaction_at(Timestamp::EPOCH);
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
    let tx_id = journal.begin_transaction_at("rw-writeback", Timestamp::EPOCH);
    journal.record_at("write_file", "/a", "bytes=3", Timestamp::EPOCH);

    let summary = journal.recover_on_load_at(Timestamp::EPOCH);

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
    journal.record_at("create_file", "/a", "", Timestamp::EPOCH);
    journal.record_at("delete", "/a", "", Timestamp::EPOCH);
    journal.record_at("restore", "/a", "", Timestamp::EPOCH);
    journal.record_at("snapshot", "/", "name=one", Timestamp::EPOCH);
    journal.record_at("secure_delete", "/a", "", Timestamp::EPOCH);

    let replay = journal.replay();
    assert!(!replay.active_paths.contains("/a"));
    assert!(!replay.deleted_paths.contains("/a"));
    assert_eq!(replay.snapshot_count, 1);
}
