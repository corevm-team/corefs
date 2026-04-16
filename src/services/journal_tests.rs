// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::PersistedState;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use corefs_core::platform::Timestamp;

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
                logical_size: 5,
                extents: vec![],
                content_crc: 1,
                flags: 0,
            },
            BlockRecord {
                inode: crate::domain::inode::InodeId(99),
                logical_size: 6,
                extents: vec![],
                content_crc: 2,
                flags: 0,
            },
        ],
        journal_entries: vec![
            JournalEntry {
                timestamp: Timestamp::now(),
                operation: "delete".to_string(),
                target: "/a".to_string(),
                details: String::new(),
            },
            JournalEntry {
                timestamp: Timestamp::now(),
                operation: "restore".to_string(),
                target: "/b".to_string(),
                details: String::new(),
            },
            JournalEntry {
                timestamp: Timestamp::now(),
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
            created_at: Timestamp::now(),
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

// Silence unused-import warning if InodeId is not referenced directly.
#[allow(dead_code)]
fn _type_refs() -> (InodeId, InodeKind) {
    (InodeId(0), InodeKind::File)
}
