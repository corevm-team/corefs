use super::*;
use crate::services::journal::{JournalRuntimeState, JournalTransaction};

fn test_fs() -> CoreFsService {
    CoreFsService::format(CoreFsConfig::default())
}

#[test]
fn format_initializes_enterprise_services() {
    let fs = test_fs();

    assert_eq!(fs.volume_name(), "corefs");
    assert_eq!(fs.journal_entries(), 1);
    assert_eq!(fs.synced_paths(), 0);
    assert!(fs.list_paths().is_empty());
}

#[test]
fn create_and_read_file_round_trips_content() {
    let mut fs = test_fs();
    fs.create_file("/notes.txt", b"hello", &["docs".to_string()])
        .expect("file creation should succeed");

    assert_eq!(
        fs.read_file("/notes.txt").expect("file should exist"),
        b"hello".to_vec()
    );
    assert!(fs.inode_for_path("/notes.txt").is_some());
}

#[test]
fn duplicate_paths_are_rejected_for_file_directory_and_symlink() {
    let mut fs = test_fs();
    fs.create_file("/dup", b"a", &[]).expect("first file");
    assert!(matches!(
        fs.create_file("/dup", b"b", &[]),
        Err(CoreFsError::AlreadyExists(_))
    ));

    fs.create_directory("/dir").expect("dir");
    assert!(matches!(
        fs.create_directory("/dir"),
        Err(CoreFsError::AlreadyExists(_))
    ));

    fs.create_symlink("/ln", "/dup").expect("symlink");
    assert!(matches!(
        fs.create_symlink("/ln", "/dup"),
        Err(CoreFsError::AlreadyExists(_))
    ));
}

#[test]
fn write_file_updates_existing_file_and_rejects_non_files() {
    let mut fs = test_fs();
    fs.create_file("/file.txt", b"old", &[]).expect("file");
    fs.write_file("/file.txt", b"new")
        .expect("write should work");
    assert_eq!(fs.read_file("/file.txt").expect("file"), b"new".to_vec());

    fs.create_directory("/dir").expect("dir");
    assert!(matches!(
        fs.write_file("/dir", b"bad"),
        Err(CoreFsError::InvalidInput(_))
    ));
    assert!(matches!(
        fs.write_file("/missing", b"bad"),
        Err(CoreFsError::NotFound(_))
    ));
}

#[test]
fn read_file_returns_errors_for_missing_paths() {
    let fs = test_fs();
    assert!(matches!(
        fs.read_file("/missing"),
        Err(CoreFsError::NotFound(_))
    ));
}

#[test]
fn delete_restore_and_secure_delete_follow_policies() {
    let mut fs = test_fs();
    fs.create_file("/recover.txt", b"data", &[]).expect("file");
    fs.delete_file("/recover.txt", false).expect("soft delete");
    assert!(fs.read_file("/recover.txt").is_err());
    assert_eq!(fs.recoverable_paths(), vec!["/recover.txt".to_string()]);
    fs.restore_file("/recover.txt").expect("restore");
    assert_eq!(
        fs.read_file("/recover.txt").expect("restored"),
        b"data".to_vec()
    );

    fs.delete_file("/recover.txt", true).expect("secure delete");
    assert!(matches!(
        fs.restore_file("/recover.txt"),
        Err(CoreFsError::NotFound(_))
    ));
    assert!(matches!(
        fs.delete_file("/recover.txt", true),
        Err(CoreFsError::NotFound(_))
    ));
}

#[test]
fn snapshot_scrub_sync_and_reporting_are_available() {
    let mut fs = test_fs();
    fs.create_directory("/etc").expect("dir");
    fs.create_file("/etc/config.txt", b"cfg", &["config".to_string()])
        .expect("file");

    let snapshot = fs.create_snapshot("baseline");
    assert_eq!(snapshot.id, 1);
    assert!(snapshot.paths.iter().any(|path| path == "/etc/config.txt"));
    assert_eq!(fs.snapshot_names(), vec!["baseline".to_string()]);

    let scrub = fs.scrub();
    assert_eq!(scrub.checked_paths, 1);
    assert_eq!(scrub.valid_blocks, 1);
    assert_eq!(scrub.invalid_blocks, 0);

    fs.mark_synced("/etc/config.txt", "node-a").expect("sync");
    assert_eq!(fs.synced_paths(), 1);
    assert!(matches!(
        fs.mark_synced("/missing", "node-a"),
        Err(CoreFsError::NotFound(_))
    ));

    let stats = fs.stats();
    assert_eq!(stats.files, 2);
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(stats.versions, 1);
    assert_eq!(stats.snapshots, 1);
    assert!(stats.journal_entries >= 5);

    let report = fs.admin_report();
    assert_eq!(report.volume.name, "corefs");
    assert!(
        report
            .runtime
            .compatibility_targets
            .iter()
            .any(|item| item == "native-os")
    );
    assert_eq!(report.tools.mkfs, "corefs mkfs");
}

#[test]
fn state_can_be_saved_and_loaded_as_binary_image() {
    let path = std::env::temp_dir().join(format!(
        "corefs-image-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_directory("/var").expect("dir");
    fs.create_file("/var/log.bin", b"log", &["logs".to_string()])
        .expect("file");
    fs.create_snapshot("binary");
    fs.save_image_to_path(&path)
        .expect("image save should succeed");

    let loaded = CoreFsService::load_image_from_path(&path).expect("image load should succeed");

    assert!(
        loaded
            .list_paths()
            .iter()
            .any(|path| path == "/var/log.bin")
    );
    assert_eq!(loaded.snapshot_names(), vec!["binary".to_string()]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn journal_replay_reconciles_deleted_entries_on_load() {
    let path = std::env::temp_dir().join(format!(
        "corefs-journal-replay-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_file("/replay.txt", b"data", &[]).expect("file");
    fs.delete_file("/replay.txt", false).expect("delete");
    fs.save_image_to_path(&path).expect("save image");

    let loaded = CoreFsService::load_image_from_path(&path).expect("load image");

    assert!(!loaded.list_paths().iter().any(|path| path == "/replay.txt"));
    assert_eq!(loaded.recoverable_paths(), vec!["/replay.txt".to_string()]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn load_image_recovers_unclean_runtime_state_and_aborts_pending_transaction() {
    let path = std::env::temp_dir().join(format!(
        "corefs-journal-runtime-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_file("/runtime.txt", b"stable", &[])
        .expect("file");
    let mut state = fs.export_state();
    state.clean_unmount = false;
    state.journal_runtime = JournalRuntimeState {
        next_transaction_id: 1,
        unclean_shutdown: true,
        pending_transaction: Some(JournalTransaction {
            id: 1,
            label: "rw-writeback".to_string(),
            started_at: SystemTime::now(),
            operations: vec![crate::services::journal::JournalEntry {
                timestamp: SystemTime::now(),
                operation: "write_file".to_string(),
                target: "/runtime.txt".to_string(),
                details: "bytes=7".to_string(),
            }],
        }),
        ..JournalRuntimeState::default()
    };
    volume_image::save_volume_image(&path, &state).expect("save image");

    let loaded = CoreFsService::load_image_from_path(&path).expect("load image");

    assert!(!loaded.had_unclean_shutdown());
    assert_eq!(
        loaded.read_file("/runtime.txt").expect("file"),
        b"stable".to_vec()
    );
    assert!(loaded.journal_entries() >= 4);

    let _ = std::fs::remove_file(path);
}

#[test]
fn save_image_to_path_is_atomic_and_leaves_no_tmp_file() {
    let path = std::env::temp_dir().join(format!(
        "corefs-atomic-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time should be after unix epoch")
            .as_nanos()
    ));
    let tmp_path = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));

    let mut fs = test_fs();
    fs.create_file("/atomic.txt", b"hello", &[]).expect("file");
    fs.save_image_to_path(&path).expect("save");

    // The final image exists and the tmp file is gone.
    assert!(path.exists(), "image should exist");
    assert!(!tmp_path.exists(), "tmp file should be cleaned up");

    // Reload to confirm the image is readable.
    let loaded = CoreFsService::load_image_from_path(&path).expect("load");
    assert_eq!(
        loaded.read_file("/atomic.txt").expect("read"),
        b"hello".to_vec()
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rename_entry_moves_file_to_new_path() {
    let mut fs = test_fs();
    fs.create_file("/old.txt", b"content", &[]).expect("file");
    fs.rename_entry("/old.txt", "/new.txt").expect("rename");

    assert!(fs.read_file("/old.txt").is_err());
    assert_eq!(
        fs.read_file("/new.txt").expect("new path"),
        b"content".to_vec()
    );
}

#[test]
fn rename_entry_cascades_to_directory_children() {
    let mut fs = test_fs();
    fs.create_directory("/src").expect("dir");
    fs.create_file("/src/main.rs", b"fn main(){}", &[])
        .expect("file");
    fs.create_directory("/src/utils").expect("subdir");
    fs.create_file("/src/utils/helper.rs", b"// helper", &[])
        .expect("file");

    fs.rename_entry("/src", "/lib").expect("rename dir");

    assert!(!fs.list_paths().iter().any(|p| p.starts_with("/src")));
    assert_eq!(
        fs.read_file("/lib/main.rs").expect("main.rs"),
        b"fn main(){}".to_vec()
    );
    assert_eq!(
        fs.read_file("/lib/utils/helper.rs").expect("helper.rs"),
        b"// helper".to_vec()
    );
}

#[test]
fn rename_entry_overwrites_existing_target() {
    let mut fs = test_fs();
    fs.create_file("/a.txt", b"aaa", &[]).expect("file a");
    fs.create_file("/b.txt", b"bbb", &[]).expect("file b");

    fs.rename_entry("/a.txt", "/b.txt").expect("rename over");

    assert!(fs.read_file("/a.txt").is_err());
    assert_eq!(fs.read_file("/b.txt").expect("b.txt"), b"aaa".to_vec());
    // overwritten entry is soft-deleted and recoverable
    assert!(fs.recoverable_paths().contains(&"/b.txt".to_string()));
}

#[test]
fn rename_entry_fails_for_missing_source() {
    let mut fs = test_fs();
    assert!(matches!(
        fs.rename_entry("/missing.txt", "/target.txt"),
        Err(CoreFsError::NotFound(_))
    ));
}

#[test]
fn validate_path_rejects_invalid_inputs() {
    assert!(matches!(
        validate_path(""),
        Err(CoreFsError::InvalidInput(_))
    ));
    assert!(matches!(
        validate_path("relative"),
        Err(CoreFsError::InvalidInput(_))
    ));
    assert!(validate_path("/valid").is_ok());
}

#[test]
fn validate_path_rejects_excessively_long_paths() {
    let too_long = format!("/{}", "a".repeat(16_384));
    assert!(matches!(
        validate_path(&too_long),
        Err(CoreFsError::InvalidInput(_))
    ));
}

#[test]
fn data_layout_for_inode_tracks_data_segment_offsets() {
    // Disable encryption and compression so stored bytes == raw bytes.
    let mut fs = CoreFsService::format(CoreFsConfig {
        security: crate::config::SecurityPolicy {
            encryption_at_rest: false,
            ..CoreFsConfig::default().security
        },
        performance: crate::config::PerformancePolicy {
            compression_enabled: false,
            ..CoreFsConfig::default().performance
        },
        ..CoreFsConfig::default()
    });
    fs.create_file("/a.txt", b"abc", &[]).expect("file");
    fs.create_file("/b.txt", b"hello", &[]).expect("file");

    let first = fs
        .inode_for_path("/a.txt")
        .and_then(|inode| fs.data_layout_for_inode(inode))
        .expect("first layout");
    let second = fs
        .inode_for_path("/b.txt")
        .and_then(|inode| fs.data_layout_for_inode(inode))
        .expect("second layout");

    assert_eq!(first.data_offset, 0);
    assert_eq!(first.length, 3);
    assert_eq!(second.data_offset, fs.block_size() as u64);
    assert_eq!(second.length, 5);
}

#[test]
fn data_extents_for_inode_follow_block_boundaries() {
    // Disable encryption and compression so stored bytes == raw bytes.
    let mut fs = CoreFsService::format(CoreFsConfig {
        block_size: 4,
        security: crate::config::SecurityPolicy {
            encryption_at_rest: false,
            ..CoreFsConfig::default().security
        },
        performance: crate::config::PerformancePolicy {
            compression_enabled: false,
            ..CoreFsConfig::default().performance
        },
        ..CoreFsConfig::default()
    });
    fs.create_file("/payload.bin", b"abcdefghij", &[])
        .expect("file");

    let inode = fs.inode_for_path("/payload.bin").expect("inode");
    let extents = fs.data_extents_for_inode(inode);

    assert_eq!(extents.len(), 3);
    assert_eq!(extents[0].device_block, 0);
    assert_eq!(extents[0].inode_offset, 0);
    assert_eq!(extents[0].length, 4);
    assert_eq!(extents[1].device_block, 1);
    assert_eq!(extents[1].inode_offset, 4);
    assert_eq!(extents[1].length, 4);
    assert_eq!(extents[2].device_block, 2);
    assert_eq!(extents[2].inode_offset, 8);
    assert_eq!(extents[2].length, 2);
}

#[test]
fn defragment_compacts_device_blocks_and_records_journal_entry() {
    let mut fs = CoreFsService::format(CoreFsConfig {
        block_size: 4,
        ..CoreFsConfig::default()
    });
    fs.create_file("/a", b"aaaa", &[]).expect("file");
    fs.create_file("/b", b"bbbb", &[]).expect("file");
    fs.create_file("/c", b"cccc", &[]).expect("file");
    fs.delete_file("/b", true).expect("delete");

    let before = fs
        .inode_for_path("/c")
        .and_then(|inode| fs.data_layout_for_inode(inode))
        .expect("layout")
        .data_offset;
    let report = fs.defragment();
    let after = fs
        .inode_for_path("/c")
        .and_then(|inode| fs.data_layout_for_inode(inode))
        .expect("layout")
        .data_offset;

    assert!(report.moved_entries >= 1);
    assert!(after < before);
    assert!(fs.journal_entries() >= 1);
}

#[test]
fn optimize_storage_reports_fragmentation_and_compacts_when_needed() {
    let mut fs = CoreFsService::format(CoreFsConfig {
        block_size: 4,
        ..CoreFsConfig::default()
    });
    fs.set_allocator_policy(AllocatorPolicy {
        background_compaction_enabled: false,
        fragmentation_threshold_percent: 25,
        coalesce_on_release: false,
        ..AllocatorPolicy::default()
    });
    fs.create_file("/a", b"aaaa", &[]).expect("file");
    fs.create_file("/b", b"bbbb", &[]).expect("file");
    fs.create_file("/c", b"cccc", &[]).expect("file");
    fs.create_file("/d", b"dddd", &[]).expect("file");
    fs.create_file("/e", b"eeee", &[]).expect("file");
    fs.delete_file("/b", true).expect("delete");
    fs.delete_file("/d", true).expect("delete");

    let report = fs.optimize_storage();

    assert!(report.heat_reallocation.is_some());
    assert!(report.defragmentation.is_none());
    assert!(report.before.fragmentation_percent >= 25);
    assert_eq!(report.after.fragmentation_percent, 0);
}

#[test]
fn auto_optimize_runs_when_policy_requests_background_compaction() {
    let mut fs = CoreFsService::format(CoreFsConfig {
        block_size: 4,
        ..CoreFsConfig::default()
    });
    fs.set_allocator_policy(AllocatorPolicy {
        background_compaction_enabled: true,
        fragmentation_threshold_percent: 25,
        coalesce_on_release: false,
        ..AllocatorPolicy::default()
    });
    fs.create_file("/a", b"aaaa", &[]).expect("file");
    fs.create_file("/b", b"bbbb", &[]).expect("file");
    fs.create_file("/c", b"cccc", &[]).expect("file");
    fs.delete_file("/b", true).expect("delete");
    fs.write_file("/c", b"ccccdddd").expect("write");

    let report = fs.fragmentation_report();

    assert_eq!(report.fragmentation_percent, 0);
    assert!(
        fs.journal_entries() >= 1,
        "auto optimization should leave a journal trail"
    );
}

#[test]
fn persisted_state_round_trips_hot_path_records() {
    let mut fs = test_fs();
    fs.create_file("/hot.txt", b"hello", &[]).expect("file");
    fs.write_file("/hot.txt", b"hello-world").expect("write");

    let state = fs.export_state();

    assert!(
        state.hot_path_records.iter().any(|record| {
            record.path == "/hot.txt" && record.write_ops >= 2 && record.bytes_written >= 16
        }),
        "expected hot path telemetry to be exported"
    );
}

// ── Copy-on-Write ───────────────────────────────────────────────────────────

#[test]
fn snapshot_captures_file_data_at_creation_time() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/notes.txt", b"hello snapshot", &[])
        .expect("file");

    let snap = fs.create_snapshot("v1");

    assert!(
        snap.file_data.contains_key("/notes.txt"),
        "snapshot must include file_data for every regular file"
    );
    assert_eq!(
        snap.file_data["/notes.txt"],
        b"hello snapshot".to_vec(),
        "captured bytes must match what was written"
    );
}

#[test]
fn snapshot_captures_historical_metadata() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/doc.txt", b"hi", &[]).expect("file");
    fs.set_mode("/doc.txt", 0o640).expect("chmod");
    fs.create_directory("/dir").expect("mkdir");
    fs.create_symlink("/link", "/doc.txt").expect("symlink");

    let snap_id = fs.create_snapshot("meta").id;

    // Mutate metadata after the snapshot.
    fs.set_mode("/doc.txt", 0o600).expect("chmod");

    let file_meta = fs
        .snapshot_inode(snap_id, "/doc.txt")
        .expect("file entry in snapshot");
    assert_eq!(file_meta.kind, crate::domain::inode::InodeKind::File);
    assert_eq!(
        file_meta.metadata.mode, 0o640,
        "historical mode must be preserved even after chmod"
    );

    let dir_meta = fs
        .snapshot_inode(snap_id, "/dir")
        .expect("dir entry in snapshot");
    assert_eq!(dir_meta.kind, crate::domain::inode::InodeKind::Directory);

    let link_meta = fs
        .snapshot_inode(snap_id, "/link")
        .expect("symlink entry in snapshot");
    assert_eq!(link_meta.kind, crate::domain::inode::InodeKind::Symlink);
    assert_eq!(link_meta.symlink_target.as_deref(), Some("/doc.txt"));
}

#[test]
fn snapshot_restore_reapplies_captured_metadata() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/doc.txt", b"v1", &[]).expect("file");
    fs.set_mode("/doc.txt", 0o640).expect("chmod");
    fs.create_directory("/dir").expect("mkdir");
    fs.set_mode("/dir", 0o750).expect("chmod dir");

    let snap_id = fs.create_snapshot("pre").id;

    // Mutate after snapshot.
    fs.set_mode("/doc.txt", 0o600).expect("chmod");
    fs.set_mode("/dir", 0o700).expect("chmod dir");
    fs.write_file("/doc.txt", b"v2").expect("write");

    fs.restore_snapshot(snap_id).expect("restore");

    let file = fs.get_inode("/doc.txt").expect("file restored");
    assert_eq!(file.metadata.mode, 0o640, "file mode must be restored");
    let dir = fs.get_inode("/dir").expect("dir still exists");
    assert_eq!(dir.metadata.mode, 0o750, "dir mode must be restored");
    assert_eq!(fs.read_file("/doc.txt").unwrap(), b"v1".to_vec());
}

#[test]
fn snapshot_restore_recreates_missing_directory_with_metadata() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/keep").expect("mkdir");
    fs.set_mode("/keep", 0o711).expect("chmod");

    let snap_id = fs.create_snapshot("pre").id;

    // Directory removed after snapshot — rename out of the way since there is
    // no direct rmdir; simulate absence by renaming.
    fs.rename_entry("/keep", "/gone").expect("rename");

    fs.restore_snapshot(snap_id).expect("restore");

    let dir = fs
        .get_inode("/keep")
        .expect("directory recreated by restore");
    assert_eq!(dir.metadata.mode, 0o711);
}

#[test]
fn snapshot_captures_uncompressed_bytes_for_compressed_files() {
    let config = CoreFsConfig {
        performance: crate::config::PerformancePolicy {
            compression_enabled: true,
            copy_on_write: true,
            journaling_enabled: true,
            deduplication_enabled: false,
            trim_enabled: true,
        },
        ..CoreFsConfig::default()
    };
    let payload = b"compressible content ".repeat(50);
    let mut fs = CoreFsService::format(config);
    fs.create_file("/big.txt", &payload, &[]).expect("file");

    let snap = fs.create_snapshot("compressed-snap");

    assert_eq!(
        snap.file_data.get("/big.txt").cloned().unwrap_or_default(),
        payload,
        "snapshot must store uncompressed bytes"
    );
}

#[test]
fn snapshot_restore_reverts_file_to_captured_state() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/doc.txt", b"original", &[]).expect("file");
    let snap = fs.create_snapshot("before-change");

    // Modify the file after the snapshot.
    fs.write_file("/doc.txt", b"modified").expect("write");
    assert_eq!(fs.read_file("/doc.txt").expect("read"), b"modified".to_vec());

    // Restore the snapshot — file must revert.
    let report = fs
        .restore_snapshot(snap.id)
        .expect("restore should succeed");

    assert_eq!(report.restored_files, 1);
    assert!(report.skipped_paths.is_empty());
    assert_eq!(
        fs.read_file("/doc.txt").expect("read"),
        b"original".to_vec(),
        "file must be restored to snapshot state"
    );
}

#[test]
fn snapshot_restore_recreates_deleted_files() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/gone.txt", b"was here", &[]).expect("file");
    let snap = fs.create_snapshot("before-delete");

    fs.delete_file("/gone.txt", false).expect("delete");

    let report = fs.restore_snapshot(snap.id).expect("restore");

    assert_eq!(report.restored_files, 1, "deleted file must be recreated");
    assert_eq!(
        fs.read_file("/gone.txt").expect("read"),
        b"was here".to_vec()
    );
}

#[test]
fn delete_snapshot_removes_it_from_the_list() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    let snap = fs.create_snapshot("ephemeral");
    assert_eq!(fs.snapshots().len(), 1);

    fs.delete_snapshot(snap.id).expect("delete should succeed");
    assert!(fs.snapshots().is_empty());
}

#[test]
fn delete_snapshot_returns_error_for_unknown_id() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    assert!(fs.delete_snapshot(999).is_err());
}

#[test]
fn restore_snapshot_returns_error_for_unknown_id() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    assert!(fs.restore_snapshot(999).is_err());
}

#[test]
fn clone_file_shares_blob_and_allows_independent_divergence() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/src.txt", b"shared data", &[]).expect("file");

    fs.clone_file("/src.txt", "/dst.txt").expect("clone");

    // Both files read the same content immediately after cloning.
    assert_eq!(
        fs.read_file("/src.txt").expect("read"),
        fs.read_file("/dst.txt").expect("read"),
        "clone must initially equal source"
    );

    // Overwrite source — clone must remain independent.
    fs.write_file("/src.txt", b"diverged").expect("write");
    assert_eq!(
        fs.read_file("/dst.txt").expect("read"),
        b"shared data".to_vec(),
        "clone must not be affected by source write"
    );
    assert_eq!(
        fs.read_file("/src.txt").expect("read"),
        b"diverged".to_vec()
    );
}

#[test]
fn clone_file_fails_for_nonexistent_source() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    assert!(fs.clone_file("/ghost.txt", "/copy.txt").is_err());
}

#[test]
fn clone_file_fails_when_target_already_exists() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/a.txt", b"a", &[]).expect("file");
    fs.create_file("/b.txt", b"b", &[]).expect("file");
    assert!(fs.clone_file("/a.txt", "/b.txt").is_err());
}

#[test]
fn expunge_file_permanently_removes_soft_deleted_file() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/temp.txt", b"temporary", &[]).expect("file");
    fs.delete_file("/temp.txt", false).expect("soft delete");

    // File is in deleted catalog, not active.
    assert!(fs.get_inode("/temp.txt").is_none());
    assert!(fs.recoverable_paths().contains(&"/temp.txt".to_string()));

    fs.expunge_file("/temp.txt").expect("expunge");

    // File is now gone from both catalogs.
    assert!(!fs.recoverable_paths().contains(&"/temp.txt".to_string()));
    assert!(
        fs.restore_file("/temp.txt").is_err(),
        "cannot restore expunged file"
    );
}

#[test]
fn expunge_file_returns_error_for_active_or_missing_path() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/active.txt", b"x", &[]).expect("file");
    // Active files cannot be expunged — only soft-deleted ones.
    assert!(fs.expunge_file("/active.txt").is_err());
    assert!(fs.expunge_file("/nonexistent.txt").is_err());
}

#[test]
fn cow_report_reflects_sharing_and_config() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/a.txt", b"abc", &[]).expect("file");
    fs.clone_file("/a.txt", "/b.txt").expect("clone");
    fs.create_file("/c.txt", b"unique", &[]).expect("file");

    let report = fs.cow_report();

    assert!(report.copy_on_write_enabled);
    assert_eq!(report.snapshot_count, 0);
    // At least one shared blob must be detected (a.txt and b.txt share a blob).
    assert!(
        report.stats.shared_blobs >= 1,
        "shared blob must be reported"
    );
    assert!(
        report.stats.bytes_saved_by_sharing > 0,
        "savings must be nonzero for shared blobs"
    );
}

#[test]
fn cow_report_snapshot_count_matches_live_snapshots() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_snapshot("snap1");
    fs.create_snapshot("snap2");

    let report = fs.cow_report();
    assert_eq!(report.snapshot_count, 2);
}

// ── Config enforcement ──────────────────────────────────────────────────────

#[test]
fn clone_file_with_cow_disabled_creates_independent_copy() {
    let config = CoreFsConfig {
        performance: crate::config::PerformancePolicy {
            copy_on_write: false,
            compression_enabled: false,
            journaling_enabled: true,
            deduplication_enabled: false,
            trim_enabled: true,
        },
        ..CoreFsConfig::default()
    };
    let mut fs = CoreFsService::format(config);
    fs.create_file("/a.txt", b"data", &[]).expect("file");

    fs.clone_file("/a.txt", "/b.txt").expect("clone");

    // Both files exist with the same content.
    assert_eq!(fs.read_file("/a.txt").unwrap(), b"data".to_vec());
    assert_eq!(fs.read_file("/b.txt").unwrap(), b"data".to_vec());

    // With CoW disabled there should be no shared blobs (eager full copy).
    let report = fs.cow_report();
    assert!(
        !report.copy_on_write_enabled,
        "CoW flag should be off"
    );
}

// ── Recursive directory cloning ─────────────────────────────────────────────

#[test]
fn clone_tree_copies_directory_recursively() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/src").expect("dir");
    fs.create_directory("/src/sub").expect("sub");
    fs.create_file("/src/a.txt", b"alpha", &[]).expect("file");
    fs.create_file("/src/sub/b.txt", b"beta", &[]).expect("file");

    let report = fs.clone_tree("/src", "/dst").expect("clone_tree");

    assert_eq!(report.cloned_directories, 2, "root + sub");
    assert_eq!(report.cloned_files, 2, "two regular files");
    assert!(report.skipped_paths.is_empty());

    assert_eq!(fs.read_file("/dst/a.txt").unwrap(), b"alpha".to_vec());
    assert_eq!(fs.read_file("/dst/sub/b.txt").unwrap(), b"beta".to_vec());
}

#[test]
fn clone_tree_diverges_independently() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/orig").expect("dir");
    fs.create_file("/orig/data.bin", b"shared", &[]).expect("file");
    fs.clone_tree("/orig", "/copy").expect("clone_tree");

    // Modify the copy — original must not change.
    fs.write_file("/copy/data.bin", b"changed").expect("write");
    assert_eq!(fs.read_file("/orig/data.bin").unwrap(), b"shared".to_vec());
    assert_eq!(fs.read_file("/copy/data.bin").unwrap(), b"changed".to_vec());
}

#[test]
fn clone_tree_rejects_non_directory_source() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/file.txt", b"x", &[]).expect("file");
    assert!(fs.clone_tree("/file.txt", "/copy").is_err());
}

#[test]
fn clone_tree_rejects_existing_target() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/a").expect("dir");
    fs.create_directory("/b").expect("dir");
    assert!(fs.clone_tree("/a", "/b").is_err());
}

#[test]
fn clone_tree_handles_symlinks() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/src").expect("dir");
    fs.create_symlink("/src/link", "/some/target").expect("symlink");

    let report = fs.clone_tree("/src", "/dst").expect("clone_tree");

    // Symlink counts as a cloned file.
    assert_eq!(report.cloned_files, 1);
    assert_eq!(report.cloned_directories, 1);
}

// ── Scoped snapshots ────────────────────────────────────────────────────────

#[test]
fn scoped_snapshot_captures_only_subtree() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/docs").expect("dir");
    fs.create_file("/docs/readme.md", b"hello", &[]).expect("file");
    fs.create_file("/root.txt", b"root", &[]).expect("file");

    let snap = fs.create_snapshot_scoped("docs-only", "/docs");

    assert!(
        snap.file_data.contains_key("/docs/readme.md"),
        "scoped file must be captured"
    );
    assert!(
        !snap.file_data.contains_key("/root.txt"),
        "out-of-scope file must not be captured"
    );
    assert_eq!(snap.scope_root, "/docs");
}

#[test]
fn scoped_snapshot_restore_only_restores_scoped_files() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/proj").expect("dir");
    fs.create_file("/proj/code.rs", b"fn main() {}", &[]).expect("file");
    fs.create_file("/unrelated.txt", b"keep", &[]).expect("file");
    let snap = fs.create_snapshot_scoped("proj-snap", "/proj");

    // Modify both files.
    fs.write_file("/proj/code.rs", b"fn changed() {}").expect("write");
    fs.write_file("/unrelated.txt", b"also changed").expect("write");

    let report = fs.restore_snapshot(snap.id).expect("restore");

    assert_eq!(report.restored_files, 1);
    assert_eq!(
        fs.read_file("/proj/code.rs").unwrap(),
        b"fn main() {}".to_vec(),
        "scoped file must be restored"
    );
    assert_eq!(
        fs.read_file("/unrelated.txt").unwrap(),
        b"also changed".to_vec(),
        "out-of-scope file must remain unchanged"
    );
}

// ── Snapshot diff ───────────────────────────────────────────────────────────

#[test]
fn diff_snapshots_detects_all_change_categories() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/stable.txt", b"same", &[]).expect("file");
    fs.create_file("/changed.txt", b"old", &[]).expect("file");
    fs.create_file("/removed.txt", b"gone", &[]).expect("file");

    let snap_a = fs.create_snapshot("before");

    // Modify, delete, and add between snapshots.
    fs.write_file("/changed.txt", b"new").expect("write");
    fs.delete_file("/removed.txt", false).expect("delete");
    fs.create_file("/added.txt", b"fresh", &[]).expect("file");

    let snap_b = fs.create_snapshot("after");

    let diff = fs.diff_snapshots(snap_a.id, snap_b.id).expect("diff");

    assert_eq!(diff.added, vec!["/added.txt".to_string()]);
    assert_eq!(diff.removed, vec!["/removed.txt".to_string()]);
    assert_eq!(diff.modified, vec!["/changed.txt".to_string()]);
    assert_eq!(diff.unchanged, vec!["/stable.txt".to_string()]);
}

#[test]
fn diff_snapshots_returns_error_for_unknown_snapshot() {
    let fs = CoreFsService::format(CoreFsConfig::default());
    assert!(fs.diff_snapshots(1, 2).is_err());
}

#[test]
fn diff_snapshots_identical_snapshots_shows_all_unchanged() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/x.txt", b"data", &[]).expect("file");

    let a = fs.create_snapshot("a");
    let b = fs.create_snapshot("b");

    let diff = fs.diff_snapshots(a.id, b.id).expect("diff");

    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert!(diff.modified.is_empty());
    assert_eq!(diff.unchanged.len(), 1);
}

// ── Encryption at rest ──────────────────────────────────────────────────────

#[test]
fn encrypted_file_round_trips_through_read_write() {
    // Default config has encryption_at_rest: true.
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/secret.txt", b"classified", &[])
        .expect("create");

    let inode = fs.get_inode("/secret.txt").expect("inode");
    assert!(inode.metadata.encrypted, "file should be marked encrypted");

    assert_eq!(
        fs.read_file("/secret.txt").unwrap(),
        b"classified".to_vec(),
        "read_file must transparently decrypt"
    );
}

#[test]
fn encrypted_file_write_updates_and_reads_back() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/doc.txt", b"v1", &[]).expect("create");
    fs.write_file("/doc.txt", b"v2-encrypted").expect("write");

    assert_eq!(
        fs.read_file("/doc.txt").unwrap(),
        b"v2-encrypted".to_vec()
    );
}

#[test]
fn encryption_plus_compression_round_trips() {
    let payload = b"compress and encrypt me ".repeat(50);
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/both.bin", &payload, &[]).expect("create");

    let inode = fs.get_inode("/both.bin").expect("inode");
    assert!(inode.metadata.encrypted);
    assert!(inode.metadata.compressed);

    assert_eq!(fs.read_file("/both.bin").unwrap(), payload);
}

#[test]
fn snapshot_captures_plaintext_bytes_even_when_encrypted() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/enc.txt", b"plaintext content", &[])
        .expect("create");

    let snap = fs.create_snapshot("encrypted-snap");

    assert_eq!(
        snap.file_data.get("/enc.txt").cloned().unwrap_or_default(),
        b"plaintext content".to_vec(),
        "snapshot must capture unencrypted content"
    );
}

#[test]
fn encryption_disabled_stores_and_reads_plaintext() {
    let config = CoreFsConfig {
        security: crate::config::SecurityPolicy {
            encryption_at_rest: false,
            ..CoreFsConfig::default().security
        },
        ..CoreFsConfig::default()
    };
    let mut fs = CoreFsService::format(config);
    fs.create_file("/plain.txt", b"no encryption", &[])
        .expect("create");

    let inode = fs.get_inode("/plain.txt").expect("inode");
    assert!(!inode.metadata.encrypted);
    assert_eq!(fs.read_file("/plain.txt").unwrap(), b"no encryption".to_vec());
}

// ── Dedup pass ──────────────────────────────────────────────────────────────

#[test]
fn run_dedup_requires_config_flag() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    // Default has deduplication_enabled: false.
    assert!(
        fs.run_dedup().is_err(),
        "dedup should be rejected when disabled"
    );
}

#[test]
fn run_dedup_reports_clean_state() {
    let config = CoreFsConfig {
        performance: crate::config::PerformancePolicy {
            deduplication_enabled: true,
            ..CoreFsConfig::default().performance
        },
        ..CoreFsConfig::default()
    };
    let mut fs = CoreFsService::format(config);
    fs.create_file("/a.txt", b"data", &[]).expect("file");
    fs.create_file("/b.txt", b"other", &[]).expect("file");

    let report = fs.run_dedup().expect("dedup should succeed");

    assert!(report.blobs_scanned >= 2);
    assert_eq!(report.hash_collisions, 0);
    assert_eq!(report.duplicates_consolidated, 0);
}

// ── Deep fsck ───────────────────────────────────────────────────────────────

#[test]
fn fsck_clean_filesystem_reports_no_errors() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/docs").expect("dir");
    fs.create_file("/docs/readme.md", b"hello", &[]).expect("file");
    fs.create_file("/data.bin", b"binary payload", &[]).expect("file");

    let report = fs.fsck();

    assert!(report.checked_inodes >= 3);
    assert_eq!(report.missing_blocks, Vec::<String>::new());
    assert_eq!(report.checksum_failures, Vec::<String>::new());
    assert_eq!(report.compression_errors, Vec::<String>::new());
    assert_eq!(report.encryption_errors, Vec::<String>::new());
    assert_eq!(report.size_mismatches, Vec::<(String, usize, usize)>::new());
    assert_eq!(report.orphaned_blocks, Vec::<crate::domain::inode::InodeId>::new());
}

#[test]
fn fsck_with_encryption_and_compression_validates_all_layers() {
    let payload = b"fsck test payload ".repeat(10);
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/layered.bin", &payload, &[]).expect("file");

    let inode = fs.get_inode("/layered.bin").expect("inode");
    assert!(inode.metadata.encrypted);
    assert!(inode.metadata.compressed);

    let report = fs.fsck();

    // All layers valid: decrypt → decompress → size match.
    assert_eq!(report.encryption_errors, Vec::<String>::new());
    assert_eq!(report.compression_errors, Vec::<String>::new());
    assert_eq!(report.size_mismatches, Vec::<(String, usize, usize)>::new());
}

#[test]
fn fsck_detects_missing_blocks() {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_file("/orphan.txt", b"data", &[]).expect("file");

    // Manually remove the block to simulate corruption.
    let inode_id = fs.inode_for_path("/orphan.txt").expect("inode");
    fs.blocks.remove(inode_id);

    let report = fs.fsck();

    assert!(
        report.missing_blocks.contains(&"/orphan.txt".to_string()),
        "fsck must detect missing blocks"
    );
}

#[test]
fn set_owner_updates_uid_and_gid_independently() {
    let mut fs = test_fs();
    fs.create_file("/a.txt", b"x", &[]).expect("file");

    // Set both
    fs.set_owner("/a.txt", Some(1000), Some(1000)).expect("chown");
    let inode = fs.get_inode("/a.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 1000);
    assert_eq!(inode.metadata.gid, 1000);

    // Update only uid
    fs.set_owner("/a.txt", Some(2000), None).expect("chown uid");
    let inode = fs.get_inode("/a.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 2000);
    assert_eq!(inode.metadata.gid, 1000);

    // Update only gid
    fs.set_owner("/a.txt", None, Some(2500)).expect("chown gid");
    let inode = fs.get_inode("/a.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 2000);
    assert_eq!(inode.metadata.gid, 2500);
}

#[test]
fn set_owner_rejects_missing_path() {
    let mut fs = test_fs();
    let err = fs.set_owner("/missing", Some(1000), Some(1000)).unwrap_err();
    assert!(matches!(err, CoreFsError::NotFound(_)));
}

#[test]
fn set_mode_masks_to_permission_bits() {
    let mut fs = test_fs();
    fs.create_file("/b.txt", b"x", &[]).expect("file");

    fs.set_mode("/b.txt", 0o755).expect("chmod");
    let inode = fs.get_inode("/b.txt").expect("inode");
    assert_eq!(inode.metadata.mode, 0o755);

    // Higher bits outside 0o7777 get masked away.
    fs.set_mode("/b.txt", 0o177777).expect("chmod");
    let inode = fs.get_inode("/b.txt").expect("inode");
    assert_eq!(inode.metadata.mode, 0o7777);
}

#[test]
fn chown_and_chmod_do_not_create_new_versions() {
    let mut fs = test_fs();
    fs.create_file("/v.txt", b"first", &[]).expect("file");
    let initial_versions = fs.versioning.list_versions("/v.txt").len();

    // Writing content creates a new version.
    fs.write_file("/v.txt", b"second").expect("write");
    let after_write = fs.versioning.list_versions("/v.txt").len();
    assert!(
        after_write > initial_versions,
        "content write should create a new version"
    );

    // chown/chmod should NOT create versions.
    fs.set_owner("/v.txt", Some(1000), Some(1000)).expect("chown");
    fs.set_mode("/v.txt", 0o600).expect("chmod");
    fs.set_owner("/v.txt", None, Some(2000)).expect("chgrp");

    let after_metadata = fs.versioning.list_versions("/v.txt").len();
    assert_eq!(
        after_metadata, after_write,
        "metadata-only changes must not create versions"
    );
}

#[test]
fn chown_preserves_modified_at() {
    let mut fs = test_fs();
    fs.create_file("/t.txt", b"data", &[]).expect("file");
    let before = fs.get_inode("/t.txt").expect("inode").modified_at;

    // Pause briefly to ensure SystemTime::now() would differ.
    std::thread::sleep(std::time::Duration::from_millis(10));

    fs.set_owner("/t.txt", Some(1000), Some(1000)).expect("chown");
    fs.set_mode("/t.txt", 0o600).expect("chmod");

    let after = fs.get_inode("/t.txt").expect("inode").modified_at;
    assert_eq!(
        before, after,
        "chown/chmod must not update modified_at (POSIX mtime)"
    );
}

// =====================================================================
// POSIX timestamp tests (mtime, ctime, crtime)
// =====================================================================

/// Helper: force a measurable gap between SystemTime::now() calls.
fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

#[test]
fn new_file_has_equal_timestamps() {
    let mut fs = test_fs();
    fs.create_file("/new.txt", b"hello", &[]).expect("create");
    let inode = fs.get_inode("/new.txt").expect("inode");
    // At creation, created == modified == changed.
    assert_eq!(inode.created_at, inode.modified_at);
    assert_eq!(inode.created_at, inode.changed_at);
}

#[test]
fn write_file_bumps_mtime_and_ctime() {
    let mut fs = test_fs();
    fs.create_file("/w.txt", b"v1", &[]).expect("create");
    let before = {
        let i = fs.get_inode("/w.txt").expect("inode");
        (i.created_at, i.modified_at, i.changed_at)
    };

    sleep_ms(10);
    fs.write_file("/w.txt", b"v2-longer").expect("write");

    let after = {
        let i = fs.get_inode("/w.txt").expect("inode");
        (i.created_at, i.modified_at, i.changed_at)
    };

    // created_at never changes.
    assert_eq!(before.0, after.0, "created_at must be immutable");
    // Content change → mtime bumped.
    assert!(after.1 > before.1, "modified_at must advance on write");
    // Any change → ctime bumped.
    assert!(after.2 > before.2, "changed_at must advance on write");
}

#[test]
fn chown_bumps_ctime_only_not_mtime() {
    let mut fs = test_fs();
    fs.create_file("/c.txt", b"x", &[]).expect("create");
    let before = {
        let i = fs.get_inode("/c.txt").expect("inode");
        (i.created_at, i.modified_at, i.changed_at)
    };

    sleep_ms(10);
    fs.set_owner("/c.txt", Some(1000), Some(1000)).expect("chown");

    let after = {
        let i = fs.get_inode("/c.txt").expect("inode");
        (i.created_at, i.modified_at, i.changed_at)
    };

    assert_eq!(before.0, after.0, "created_at immutable under chown");
    assert_eq!(before.1, after.1, "modified_at must NOT bump on chown");
    assert!(after.2 > before.2, "changed_at must bump on chown");
}

#[test]
fn chmod_bumps_ctime_only_not_mtime() {
    let mut fs = test_fs();
    fs.create_file("/m.txt", b"x", &[]).expect("create");
    let before = {
        let i = fs.get_inode("/m.txt").expect("inode");
        (i.modified_at, i.changed_at)
    };

    sleep_ms(10);
    fs.set_mode("/m.txt", 0o600).expect("chmod");

    let after = {
        let i = fs.get_inode("/m.txt").expect("inode");
        (i.modified_at, i.changed_at)
    };

    assert_eq!(before.0, after.0, "modified_at must NOT bump on chmod");
    assert!(after.1 > before.1, "changed_at must bump on chmod");
}

#[test]
fn rename_bumps_ctime_only_not_mtime() {
    let mut fs = test_fs();
    fs.create_file("/r1.txt", b"x", &[]).expect("create");
    let before = {
        let i = fs.get_inode("/r1.txt").expect("inode");
        (i.modified_at, i.changed_at)
    };

    sleep_ms(10);
    fs.rename_entry("/r1.txt", "/r2.txt").expect("rename");

    let after = {
        let i = fs.get_inode("/r2.txt").expect("inode");
        (i.modified_at, i.changed_at)
    };

    assert_eq!(before.0, after.0, "rename must NOT bump modified_at");
    assert!(after.1 > before.1, "rename must bump changed_at");
}

#[test]
fn timestamps_survive_image_roundtrip() {
    use crate::storage::volume_image;
    let path = std::env::temp_dir().join(format!(
        "corefs-timestamps-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_file("/ts.txt", b"v1", &[]).expect("create");
    sleep_ms(10);
    fs.write_file("/ts.txt", b"v2").expect("write");
    sleep_ms(10);
    fs.set_mode("/ts.txt", 0o600).expect("chmod");

    let before = {
        let i = fs.get_inode("/ts.txt").expect("inode");
        (i.created_at, i.modified_at, i.changed_at)
    };
    fs.save_image_to_path(&path).expect("save");

    let loaded = CoreFsService::load_image_from_path(&path).expect("load");
    let after = {
        let i = loaded.get_inode("/ts.txt").expect("inode");
        (i.created_at, i.modified_at, i.changed_at)
    };

    assert_eq!(before, after, "all three timestamps must round-trip");
    // Sanity: ctime >= mtime >= created (created set at start, mtime at write,
    // ctime bumped by chmod which came after the write).
    assert!(after.0 <= after.1);
    assert!(after.1 <= after.2);

    let _ = std::fs::remove_file(path);
    let _ = volume_image::build_volume_image_bytes;
}

// =====================================================================
// Content-modification tests
// =====================================================================

#[test]
fn write_creates_version_per_content_change() {
    let mut fs = test_fs();
    fs.create_file("/v.txt", b"one", &[]).expect("create");
    let v0 = fs.versioning.list_versions("/v.txt").len();

    fs.write_file("/v.txt", b"two").expect("write v2");
    let v1 = fs.versioning.list_versions("/v.txt").len();

    fs.write_file("/v.txt", b"three").expect("write v3");
    let v2 = fs.versioning.list_versions("/v.txt").len();

    assert!(v1 > v0, "first write should add a version");
    assert!(v2 > v1, "second write should add another version");
}

#[test]
fn write_updates_size_to_new_content_length() {
    let mut fs = test_fs();
    fs.create_file("/s.txt", b"abc", &[]).expect("create");
    assert_eq!(fs.get_inode("/s.txt").expect("inode").size, 3);

    fs.write_file("/s.txt", b"abcdefghij").expect("grow");
    assert_eq!(fs.get_inode("/s.txt").expect("inode").size, 10);

    fs.write_file("/s.txt", b"x").expect("shrink");
    assert_eq!(fs.get_inode("/s.txt").expect("inode").size, 1);
}

#[test]
fn write_preserves_inode_id() {
    let mut fs = test_fs();
    fs.create_file("/id.txt", b"one", &[]).expect("create");
    let id_before = fs.inode_for_path("/id.txt").expect("id");

    fs.write_file("/id.txt", b"two").expect("write");
    fs.write_file("/id.txt", b"three").expect("write");

    let id_after = fs.inode_for_path("/id.txt").expect("id");
    assert_eq!(id_before, id_after, "inode id must be stable across writes");
}

#[test]
fn write_preserves_owner_and_mode() {
    let mut fs = test_fs();
    fs.create_file("/p.txt", b"one", &[]).expect("create");
    fs.set_owner("/p.txt", Some(1337), Some(42)).expect("chown");
    fs.set_mode("/p.txt", 0o640).expect("chmod");

    fs.write_file("/p.txt", b"two").expect("write");

    let inode = fs.get_inode("/p.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 1337, "uid survives content write");
    assert_eq!(inode.metadata.gid, 42, "gid survives content write");
    assert_eq!(inode.metadata.mode, 0o640, "mode survives content write");
}

// =====================================================================
// Attribute change tests (uid/gid/mode)
// =====================================================================

#[test]
fn new_file_has_default_mode() {
    let mut fs = test_fs();
    fs.create_file("/d.txt", b"x", &[]).expect("create");
    let inode = fs.get_inode("/d.txt").expect("inode");
    assert_eq!(inode.metadata.mode, 0o644, "files default to 0o644");
}

#[test]
fn chmod_masks_upper_bits() {
    let mut fs = test_fs();
    fs.create_file("/k.txt", b"x", &[]).expect("create");

    // setuid + setgid + sticky + rwx all set → still valid under 0o7777.
    fs.set_mode("/k.txt", 0o7777).expect("chmod");
    assert_eq!(fs.get_inode("/k.txt").expect("inode").metadata.mode, 0o7777);

    // Bits above 0o7777 get masked away.
    fs.set_mode("/k.txt", 0o170777).expect("chmod");
    assert_eq!(fs.get_inode("/k.txt").expect("inode").metadata.mode, 0o777);
}

#[test]
fn chown_uid_only_preserves_gid() {
    let mut fs = test_fs();
    fs.create_file("/u.txt", b"x", &[]).expect("create");
    fs.set_owner("/u.txt", Some(100), Some(200)).expect("chown both");

    fs.set_owner("/u.txt", Some(999), None).expect("chown uid");
    let inode = fs.get_inode("/u.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 999);
    assert_eq!(inode.metadata.gid, 200, "gid preserved");
}

#[test]
fn chown_gid_only_preserves_uid() {
    let mut fs = test_fs();
    fs.create_file("/g.txt", b"x", &[]).expect("create");
    fs.set_owner("/g.txt", Some(100), Some(200)).expect("chown both");

    fs.set_owner("/g.txt", None, Some(888)).expect("chgrp");
    let inode = fs.get_inode("/g.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 100, "uid preserved");
    assert_eq!(inode.metadata.gid, 888);
}

#[test]
fn chown_and_chmod_are_independent() {
    let mut fs = test_fs();
    fs.create_file("/i.txt", b"x", &[]).expect("create");

    fs.set_mode("/i.txt", 0o755).expect("chmod");
    fs.set_owner("/i.txt", Some(42), Some(42)).expect("chown");
    let inode = fs.get_inode("/i.txt").expect("inode");
    assert_eq!(inode.metadata.mode, 0o755, "mode preserved after chown");
    assert_eq!(inode.metadata.uid, 42);
    assert_eq!(inode.metadata.gid, 42);

    fs.set_mode("/i.txt", 0o600).expect("chmod 2");
    let inode = fs.get_inode("/i.txt").expect("inode");
    assert_eq!(inode.metadata.mode, 0o600);
    assert_eq!(inode.metadata.uid, 42, "uid preserved after chmod");
    assert_eq!(inode.metadata.gid, 42, "gid preserved after chmod");
}

#[test]
fn attributes_survive_multiple_roundtrips() {
    use crate::storage::volume_image;
    let path = std::env::temp_dir().join(format!(
        "corefs-attr-loop-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_file("/a.txt", b"data", &[]).expect("create");
    fs.set_owner("/a.txt", Some(500), Some(600)).expect("chown");
    fs.set_mode("/a.txt", 0o751).expect("chmod");
    fs.save_image_to_path(&path).expect("save 1");

    // Three mount-unmount cycles.
    for i in 0..3 {
        let mut loaded = CoreFsService::load_image_from_path(&path).expect("load");
        let inode = loaded.get_inode("/a.txt").expect("inode");
        assert_eq!(inode.metadata.uid, 500, "uid stable cycle {i}");
        assert_eq!(inode.metadata.gid, 600, "gid stable cycle {i}");
        assert_eq!(inode.metadata.mode, 0o751, "mode stable cycle {i}");
        loaded.save_image_to_path(&path).expect("save cycle");
    }

    let _ = std::fs::remove_file(path);
    let _ = volume_image::build_volume_image_bytes;
}

#[test]
fn delete_file_does_not_affect_other_timestamps() {
    let mut fs = test_fs();
    fs.create_file("/a.txt", b"a", &[]).expect("a");
    fs.create_file("/b.txt", b"b", &[]).expect("b");

    let before_a = fs.get_inode("/a.txt").expect("a").modified_at;
    sleep_ms(10);
    fs.delete_file("/b.txt", false).expect("delete b");

    let after_a = fs.get_inode("/a.txt").expect("a").modified_at;
    assert_eq!(before_a, after_a, "unrelated file's mtime must not change");
}

#[test]
fn owner_and_mode_persist_through_image_roundtrip() {
    use crate::storage::volume_image;
    let path = std::env::temp_dir().join(format!(
        "corefs-chown-roundtrip-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_file("/persist.txt", b"hello", &[]).expect("file");
    fs.set_owner("/persist.txt", Some(1234), Some(5678))
        .expect("chown");
    fs.set_mode("/persist.txt", 0o600).expect("chmod");
    fs.save_image_to_path(&path).expect("save");

    let loaded = CoreFsService::load_image_from_path(&path).expect("load");
    let inode = loaded.get_inode("/persist.txt").expect("inode");
    assert_eq!(inode.metadata.uid, 1234);
    assert_eq!(inode.metadata.gid, 5678);
    assert_eq!(inode.metadata.mode, 0o600);

    let _ = std::fs::remove_file(path);
    let _ = volume_image::build_volume_image_bytes; // keep import alive
}
