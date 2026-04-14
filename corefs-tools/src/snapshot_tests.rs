// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::mkfs::{FormatImageOptions, format_image};
use crate::{Report, ToolsError};
use std::path::PathBuf;

fn tmp_image_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-tools-snapshot-{tag}-{pid}-{nano}.img"));
    p
}

fn fresh_image(tag: &str) -> PathBuf {
    let path = tmp_image_path(tag);
    format_image(&path, 16 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");
    path
}

#[test]
fn list_on_empty_volume_returns_no_snapshots() {
    let path = fresh_image("list_empty");
    let report = list(&path).expect("list ok");
    assert!(report.snapshots.is_empty());
    assert!(report.summary().contains("none"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn create_then_list_returns_one_snapshot() {
    let path = fresh_image("create_then_list");

    let created = create(
        &path,
        &CreateOptions {
            name: "v1".to_string(),
            scope_root: None,
        },
    )
    .expect("create ok");
    assert_eq!(created.name, "v1");
    assert_eq!(created.scope_root, "/");

    let listed = list(&path).expect("list ok");
    assert_eq!(listed.snapshots.len(), 1);
    assert_eq!(listed.snapshots[0].id, created.id);
    assert_eq!(listed.snapshots[0].name, "v1");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn create_with_scope_root_records_scope() {
    let path = fresh_image("scoped");
    let created = create(
        &path,
        &CreateOptions {
            name: "etc-only".to_string(),
            scope_root: Some("/etc".to_string()),
        },
    )
    .expect("create ok");
    assert_eq!(created.scope_root, "/etc");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn delete_removes_snapshot() {
    let path = fresh_image("delete");
    let created = create(
        &path,
        &CreateOptions {
            name: "tmp".to_string(),
            scope_root: None,
        },
    )
    .expect("create ok");

    let deleted = delete(&path, created.id).expect("delete ok");
    assert_eq!(deleted.id, created.id);

    let listed = list(&path).expect("list ok");
    assert!(listed.snapshots.is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn delete_unknown_snapshot_id_fails() {
    let path = fresh_image("delete_unknown");
    let err = delete(&path, 999_999).expect_err("unknown id must fail");
    assert!(matches!(err, ToolsError::Core(_)));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn restore_returns_report_for_existing_snapshot() {
    let path = fresh_image("restore");
    let created = create(
        &path,
        &CreateOptions {
            name: "baseline".to_string(),
            scope_root: None,
        },
    )
    .expect("create ok");

    let restored = restore(&path, created.id).expect("restore ok");
    assert_eq!(restored.snapshot_id, created.id);
    assert_eq!(restored.snapshot_name, "baseline");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn restore_unknown_snapshot_id_fails() {
    let path = fresh_image("restore_unknown");
    let err = restore(&path, 12345).expect_err("unknown id must fail");
    assert!(matches!(err, ToolsError::Core(_)));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn list_summary_reflects_count() {
    let path = fresh_image("list_count");
    let report_empty = list(&path).expect("list empty");
    assert!(report_empty.summary().contains("none"));

    create(
        &path,
        &CreateOptions {
            name: "a".to_string(),
            scope_root: None,
        },
    )
    .expect("create a");
    create(
        &path,
        &CreateOptions {
            name: "b".to_string(),
            scope_root: None,
        },
    )
    .expect("create b");

    let report_two = list(&path).expect("list two");
    assert_eq!(report_two.snapshots.len(), 2);
    assert!(report_two.summary().contains("snapshots: 2"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn list_render_text_lists_each_snapshot() {
    let path = fresh_image("list_text");
    create(
        &path,
        &CreateOptions {
            name: "alpha".to_string(),
            scope_root: None,
        },
    )
    .expect("create alpha");
    let report = list(&path).expect("list ok");
    let text = report.render_text();
    assert!(text.contains("alpha"));
    assert!(text.contains("snapshots in"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn create_render_json_round_trips_via_serde() {
    let path = fresh_image("create_json");
    let created = create(
        &path,
        &CreateOptions {
            name: "j".to_string(),
            scope_root: None,
        },
    )
    .expect("create ok");
    let json = created.render_json();
    let parsed: SnapshotCreateReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.id, created.id);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn list_after_delete_is_empty() {
    let path = fresh_image("delete_then_list");
    let s1 = create(
        &path,
        &CreateOptions {
            name: "x".to_string(),
            scope_root: None,
        },
    )
    .expect("create ok");
    delete(&path, s1.id).expect("delete ok");
    let listed = list(&path).expect("list ok");
    assert!(listed.snapshots.is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn list_fails_on_missing_path() {
    let path = tmp_image_path("missing");
    let err = list(&path).expect_err("missing path must fail");
    assert!(matches!(err, ToolsError::Core(_)));
}
