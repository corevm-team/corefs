use super::*;
use std::time::SystemTime;

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
    assert_eq!(snapshot.scope_root, "/");
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
fn list_directory_and_rename_support_nested_paths() {
    let mut fs = test_fs();
    fs.create_directory("/srv").expect("srv");
    fs.create_directory("/srv/corefs").expect("corefs");
    fs.create_file("/srv/corefs/a.txt", b"alpha", &[])
        .expect("file");

    let entries = fs.list_directory("/srv/corefs").expect("directory listing");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "a.txt");

    fs.rename_path("/srv/corefs", "/srv/platform")
        .expect("rename");
    assert!(fs.get_inode("/srv/platform").is_some());
    assert!(fs.get_inode("/srv/platform/a.txt").is_some());
    assert_eq!(
        fs.read_file("/srv/platform/a.txt").expect("renamed file"),
        b"alpha".to_vec()
    );
}

#[test]
fn remove_directory_requires_empty_directory() {
    let mut fs = test_fs();
    fs.create_directory("/data").expect("data");
    fs.create_file("/data/file.txt", b"payload", &[])
        .expect("file");
    assert!(matches!(
        fs.remove_directory("/data"),
        Err(CoreFsError::PolicyViolation(_))
    ));

    fs.delete_file("/data/file.txt", false).expect("delete");
    fs.remove_directory("/data").expect("remove");
    assert!(fs.get_inode("/data").is_none());
}

#[test]
fn subtree_snapshots_metadata_and_version_selectors_work() {
    let mut fs = test_fs();
    fs.create_directory("/projects").expect("projects");
    fs.create_directory("/projects/a").expect("project a");
    fs.create_file("/projects/a/readme.txt", b"v1", &["docs".to_string()])
        .expect("file");
    let first = fs
        .list_versions_for_path("/projects/a/readme.txt")
        .expect("versions")[0]
        .clone();

    std::thread::sleep(std::time::Duration::from_millis(2));
    fs.write_file("/projects/a/readme.txt", b"v2")
        .expect("write");
    fs.add_tag("/projects/a/readme.txt", "important")
        .expect("tag add");
    fs.set_attribute("/projects/a/readme.txt", "owner", "team-a")
        .expect("attr");
    fs.set_storage_tier("/projects/a/readme.txt", StorageTier::Hot)
        .expect("tier");

    let snapshot = fs
        .create_snapshot_for_subtree("project-a", "/projects")
        .expect("snapshot");
    assert_eq!(snapshot.scope_root, "/projects");
    assert!(
        snapshot
            .paths
            .iter()
            .all(|path| path.starts_with("/projects"))
    );

    let metadata = fs
        .metadata_for_path("/projects/a/readme.txt")
        .expect("metadata");
    assert!(metadata.tags.iter().any(|tag| tag == "important"));
    let attrs = metadata
        .attributes
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(attrs.get("owner").map(String::as_str), Some("team-a"));
    assert_eq!(
        attrs.get("semantic.byte_size").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        attrs.get("semantic.language").map(String::as_str),
        Some("text")
    );
    assert_eq!(attrs.get("semantic.lines").map(String::as_str), Some("1"));
    assert_eq!(attrs.get("semantic.words").map(String::as_str), Some("1"));
    assert_eq!(
        attrs.get("semantic.summary").map(String::as_str),
        Some("v2")
    );
    assert_eq!(
        attrs.get("semantic.pointer.summary").map(String::as_str),
        Some("v2")
    );
    assert_eq!(
        attrs.get("semantic.pointer.fulltext").map(String::as_str),
        Some("v2")
    );
    assert_eq!(metadata.storage_tier, StorageTier::Hot);
    assert_eq!(
        fs.find_by_tag("important"),
        vec!["/projects/a/readme.txt".to_string()]
    );
    assert_eq!(
        fs.read_version_selector("/projects/a/readme.txt@latest")
            .expect("latest"),
        b"v2".to_vec()
    );
    assert_eq!(
        fs.read_version_selector(&format!("/projects/a/readme.txt@v{}", first.version_id))
            .expect("by id"),
        b"v1".to_vec()
    );
    assert_eq!(
        fs.read_version_selector("/projects/a/readme.txt@2099-01-01-00-00-00")
            .expect("future resolves latest before timestamp"),
        b"v2".to_vec()
    );
    assert_eq!(
        fs.find_by_content_term("team-a"),
        vec!["/projects/a/readme.txt".to_string()]
    );
    assert_eq!(
        fs.find_by_content_term("v2"),
        vec!["/projects/a/readme.txt".to_string()]
    );
}

#[test]
fn quotas_are_enforced_for_file_creation_and_growth() {
    let mut config = CoreFsConfig::default();
    config.quotas.max_files = Some(1);
    config.quotas.max_bytes = Some(4);
    let mut fs = CoreFsService::format(config);

    fs.create_file("/a.txt", b"1234", &[]).expect("first file");
    let report = fs.quota_report();
    assert_eq!(report.used_files, 1);
    assert_eq!(report.used_bytes, 4);

    assert!(matches!(
        fs.create_file("/b.txt", b"1", &[]),
        Err(CoreFsError::PolicyViolation(_))
    ));
    assert!(matches!(
        fs.write_file("/a.txt", b"12345"),
        Err(CoreFsError::PolicyViolation(_))
    ));
}

#[test]
fn state_can_be_saved_and_loaded_again() {
    let path = std::env::temp_dir().join(format!(
        "corefs-service-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));

    let mut fs = test_fs();
    fs.create_directory("/etc").expect("dir");
    fs.create_file("/etc/config.txt", b"cfg", &["config".to_string()])
        .expect("file");
    fs.create_snapshot("baseline");
    fs.mark_synced("/etc/config.txt", "node-a").expect("sync");
    fs.delete_file("/etc/config.txt", false).expect("delete");
    fs.save_to_path(&path).expect("save should succeed");

    let loaded = CoreFsService::load_from_path(&path).expect("load should succeed");

    assert_eq!(loaded.volume_name(), "corefs");
    assert!(loaded.list_paths().iter().any(|path| path == "/etc"));
    assert_eq!(
        loaded.recoverable_paths(),
        vec!["/etc/config.txt".to_string()]
    );
    assert_eq!(loaded.snapshot_names(), vec!["baseline".to_string()]);
    assert_eq!(loaded.synced_paths(), 1);

    let _ = std::fs::remove_file(path);
}

#[test]
fn loading_invalid_state_returns_error() {
    let path = std::env::temp_dir().join(format!(
        "corefs-invalid-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));
    std::fs::write(&path, b"not-json").expect("test file should be written");

    let result = CoreFsService::load_from_path(&path);
    assert!(matches!(result, Err(CoreFsError::State(_))));

    let _ = std::fs::remove_file(path);
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
