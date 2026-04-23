// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str, extension: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "corefs-cli-{name}-{}-{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos(),
            extension
        ))
        .display()
        .to_string()
}

#[test]
fn cli_without_command_returns_ok() {
    let result = run(vec!["corefs".to_string()]);
    assert!(result.is_ok());
}

#[test]
fn cli_supports_successful_commands() {
    let fsck_image_path = temp_path("fsck", "img");
    let repair_image_path = temp_path("fsck-repair", "img");
    let diagnose_mountpoint = temp_path("diagnose-mount", "dir");
    let fs = bootstrap_demo_fs().expect("bootstrap should succeed");
    fs.save_image_to_path(&fsck_image_path)
        .expect("image should be saved");
    fs.save_image_to_path(&repair_image_path)
        .expect("repair image should be saved");
    fs::create_dir_all(&diagnose_mountpoint).expect("mountpoint should be created");
    let mut repair_bytes = fs::read(&repair_image_path).expect("repair image should exist");
    let primary_offset =
        u64::from_le_bytes(repair_bytes[24..32].try_into().expect("fixed")) as usize;
    repair_bytes[primary_offset] ^= 0xFF;
    fs::write(&repair_image_path, repair_bytes).expect("corrupted repair image should save");

    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut successful = vec![
        vec!["corefs".to_string(), "mkfs".to_string()],
        vec!["corefs".to_string(), "status".to_string()],
        vec!["corefs".to_string(), "ls".to_string()],
        vec![
            "corefs".to_string(),
            "snapshot".to_string(),
            "nightly".to_string(),
        ],
        vec!["corefs".to_string(), "scrub".to_string()],
        vec!["corefs".to_string(), "defrag".to_string()],
        vec!["corefs".to_string(), "optimize".to_string()],
        vec![
            "corefs".to_string(),
            "write".to_string(),
            "/etc/corefs.conf".to_string(),
            "updated".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "read".to_string(),
            "/etc/corefs.conf".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "save-image".to_string(),
            temp_path("save", "img"),
        ],
        vec![
            "corefs".to_string(),
            "mkfs-image".to_string(),
            temp_path("mkfs-image", "img"),
            "--demo".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "fsck-image".to_string(),
            fsck_image_path.clone(),
        ],
        vec![
            "corefs".to_string(),
            "fsck-image".to_string(),
            repair_image_path.clone(),
            "--repair".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "defrag-image".to_string(),
            fsck_image_path.clone(),
        ],
        vec![
            "corefs".to_string(),
            "optimize-image".to_string(),
            fsck_image_path.clone(),
        ],
        vec!["corefs".to_string(), "benchmark".to_string()],
        vec![
            "corefs".to_string(),
            "benchmark".to_string(),
            "--profile".to_string(),
            "small-files".to_string(),
            "--files".to_string(),
            "16".to_string(),
            "--payload".to_string(),
            "128".to_string(),
            "--snapshots".to_string(),
            "2".to_string(),
            "--saves".to_string(),
            "2".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "benchmark-log".to_string(),
            std::env::temp_dir()
                .join("corefs-cli-benchmark.md")
                .display()
                .to_string(),
            "--profile".to_string(),
            "persist-heavy".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "delete".to_string(),
            "/var/readme.txt".to_string(),
        ],
        vec![
            "corefs".to_string(),
            "delete".to_string(),
            "/var/readme.txt".to_string(),
            "--secure".to_string(),
        ],
    ];

    #[cfg(target_os = "linux")]
    {
        successful.push(vec![
            "corefs".to_string(),
            "diagnose-mount".to_string(),
            fsck_image_path.clone(),
            diagnose_mountpoint.clone(),
        ]);
        successful.push(vec![
            "corefs".to_string(),
            "diagnose-mount".to_string(),
            temp_path("diagnose-create", "img"),
            diagnose_mountpoint.clone(),
            "--create".to_string(),
            "--threads".to_string(),
            "4".to_string(),
        ]);
    }

    for args in successful {
        assert!(run(args).is_ok());
    }

    let _ = fs::remove_file(fsck_image_path);
    let _ = fs::remove_file(repair_image_path);
    let _ = fs::remove_dir_all(diagnose_mountpoint);
}

#[test]
fn cli_returns_errors_for_invalid_commands_and_missing_arguments() {
    let invalid = run(vec!["corefs".to_string(), "nope".to_string()]);
    assert!(matches!(invalid, Err(CoreFsError::InvalidCommand(_))));

    let delete = run(vec!["corefs".to_string(), "delete".to_string()]);
    assert!(matches!(delete, Err(CoreFsError::InvalidCommand(_))));

    let restore = run(vec!["corefs".to_string(), "restore".to_string()]);
    assert!(matches!(restore, Err(CoreFsError::InvalidCommand(_))));

    let write_path = run(vec!["corefs".to_string(), "write".to_string()]);
    assert!(matches!(write_path, Err(CoreFsError::InvalidCommand(_))));

    let write_payload = run(vec![
        "corefs".to_string(),
        "write".to_string(),
        "/etc/corefs.conf".to_string(),
    ]);
    assert!(matches!(write_payload, Err(CoreFsError::InvalidCommand(_))));

    let read = run(vec!["corefs".to_string(), "read".to_string()]);
    assert!(matches!(read, Err(CoreFsError::InvalidCommand(_))));

    let save_image = run(vec!["corefs".to_string(), "save-image".to_string()]);
    assert!(matches!(save_image, Err(CoreFsError::InvalidCommand(_))));

    let mkfs_image = run(vec!["corefs".to_string(), "mkfs-image".to_string()]);
    assert!(matches!(mkfs_image, Err(CoreFsError::InvalidCommand(_))));

    let load_image = run(vec!["corefs".to_string(), "load-image".to_string()]);
    assert!(matches!(load_image, Err(CoreFsError::InvalidCommand(_))));

    let fsck_image = run(vec!["corefs".to_string(), "fsck-image".to_string()]);
    assert!(matches!(fsck_image, Err(CoreFsError::InvalidCommand(_))));

    let defrag_image = run(vec!["corefs".to_string(), "defrag-image".to_string()]);
    assert!(matches!(defrag_image, Err(CoreFsError::InvalidCommand(_))));

    let mount_image = run(vec!["corefs".to_string(), "mount-image".to_string()]);
    assert!(matches!(mount_image, Err(CoreFsError::InvalidCommand(_))));

    let unmount_image_win = run(vec!["corefs".to_string(), "unmount-image-win".to_string()]);
    assert!(matches!(
        unmount_image_win,
        Err(CoreFsError::InvalidCommand(_))
    ));

    let diagnose_mount = run(vec!["corefs".to_string(), "diagnose-mount".to_string()]);
    assert!(matches!(
        diagnose_mount,
        Err(CoreFsError::InvalidCommand(_))
    ));

    let benchmark_log = run(vec!["corefs".to_string(), "benchmark-log".to_string()]);
    assert!(matches!(benchmark_log, Err(CoreFsError::InvalidCommand(_))));

    let benchmark_profile = run(vec![
        "corefs".to_string(),
        "benchmark".to_string(),
        "--profile".to_string(),
    ]);
    assert!(matches!(
        benchmark_profile,
        Err(CoreFsError::InvalidCommand(_))
    ));

    let benchmark_value = run(vec![
        "corefs".to_string(),
        "benchmark".to_string(),
        "--files".to_string(),
        "abc".to_string(),
    ]);
    assert!(matches!(benchmark_value, Err(CoreFsError::InvalidInput(_))));
}

#[test]
fn bootstrap_demo_fs_creates_expected_layout() {
    let fs = bootstrap_demo_fs().expect("bootstrap should succeed");
    let paths = fs.list_paths();

    assert!(paths.iter().any(|path| path == "/etc"));
    assert!(paths.iter().any(|path| path == "/var"));
    assert!(paths.iter().any(|path| path == "/etc/corefs.conf"));
    assert!(paths.iter().any(|path| path == "/var/readme.txt"));
    assert!(paths.iter().any(|path| path == "/etc/corefs-current"));
}

#[test]
fn benchmark_config_parser_accepts_overrides() {
    let args = vec![
        "--profile".to_string(),
        "snapshot-heavy".to_string(),
        "--files".to_string(),
        "10".to_string(),
        "--payload".to_string(),
        "512".to_string(),
        "--snapshots".to_string(),
        "3".to_string(),
        "--saves".to_string(),
        "2".to_string(),
    ];

    let config = benchmark_config_from_args(&args).expect("config should parse");

    assert_eq!(config.profile, BenchmarkProfile::SnapshotHeavy);
    assert_eq!(config.file_count, 10);
    assert_eq!(config.payload_size, 512);
    assert_eq!(config.snapshot_count, 3);
    assert_eq!(config.persist_runs, 2);
}

#[test]
fn cli_odf_mkfs_inspect_and_fsck_roundtrip() {
    let odf_path = temp_path("odf", "odf");
    // Format a fresh 8 MiB ODF volume via the CLI.
    run(vec![
        "corefs".to_string(),
        "mkfs-odf".to_string(),
        odf_path.clone(),
        "--size".to_string(),
        (8 * 1024 * 1024).to_string(),
    ])
    .expect("mkfs-odf should succeed");
    // inspect-odf should read it back without error.
    run(vec![
        "corefs".to_string(),
        "inspect-odf".to_string(),
        odf_path.clone(),
    ])
    .expect("inspect-odf should succeed");
    // fsck-odf should report zero errors on a fresh volume.
    run(vec![
        "corefs".to_string(),
        "fsck-odf".to_string(),
        odf_path.clone(),
    ])
    .expect("fsck-odf should succeed on a clean volume");
    let _ = std::fs::remove_file(&odf_path);
}

#[test]
fn cli_odf_session_demo_runs_end_to_end() {
    let image = temp_path("session-demo", "odf");
    run(vec![
        "corefs".to_string(),
        "odf-session-demo".to_string(),
        image.clone(),
        "--size".to_string(),
        (16 * 1024 * 1024).to_string(),
    ])
    .expect("odf-session-demo should succeed");
    // Follow-up: fsck-odf should report the result as clean.
    run(vec![
        "corefs".to_string(),
        "fsck-odf".to_string(),
        image.clone(),
    ])
    .expect("fsck-odf should be clean after session demo");
    let _ = std::fs::remove_file(&image);
}

#[test]
fn cli_migrate_to_odf_produces_fsck_clean_volume() {
    let src_img = temp_path("src", "img");
    let dst_odf = temp_path("dst", "odf");
    // Build a legacy volume_image first.
    let fs = bootstrap_demo_fs().expect("bootstrap");
    fs.save_image_to_path(&src_img).expect("save legacy image");

    run(vec![
        "corefs".to_string(),
        "migrate-to-odf".to_string(),
        src_img.clone(),
        dst_odf.clone(),
        "--size".to_string(),
        (16 * 1024 * 1024).to_string(),
    ])
    .expect("migrate-to-odf should succeed");

    run(vec![
        "corefs".to_string(),
        "fsck-odf".to_string(),
        dst_odf.clone(),
    ])
    .expect("fsck-odf should be clean after migration");

    let _ = std::fs::remove_file(&src_img);
    let _ = std::fs::remove_file(&dst_odf);
}
