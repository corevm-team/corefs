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
    p.push(format!("corefs-tools-fsck-{tag}-{pid}-{nano}.img"));
    p
}

#[test]
fn check_image_on_freshly_formatted_volume_is_clean() {
    let path = tmp_image_path("clean");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = check_image(&path).expect("check ok");
    assert!(
        report.is_clean,
        "fresh volume must be clean, got issues: {:#?}",
        report.issues
    );
    assert_eq!(report.count(SeverityKind::Error), 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn check_image_returns_image_path_in_report() {
    let path = tmp_image_path("path");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = check_image(&path).expect("check ok");
    assert_eq!(report.image_path, path.display().to_string());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn summary_indicates_no_errors_for_pristine_volume() {
    let path = tmp_image_path("summary");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");
    let report = check_image(&path).expect("check ok");

    // A pristine volume must not contain any Error-severity findings.
    // Whether warnings/info are present depends on the ODF default feature
    // gates, so accept both `fsck clean` and `fsck ok` as healthy summaries.
    let summary = report.summary();
    assert!(
        summary.starts_with("fsck clean") || summary.starts_with("fsck ok"),
        "expected healthy summary, got: {summary}"
    );
    assert_eq!(report.count(SeverityKind::Error), 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn render_text_lists_all_metric_lines() {
    let path = tmp_image_path("text");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");
    let report = check_image(&path).expect("check ok");

    let text = report.render_text();
    assert!(text.contains("inodes checked"));
    assert!(text.contains("extents checked"));
    assert!(text.contains("blocks referenced"));
    assert!(text.contains("clean"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn render_json_round_trips_via_serde() {
    let path = tmp_image_path("json");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");
    let report = check_image(&path).expect("check ok");

    let json = report.render_json();
    let parsed: FsckCheckReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.is_clean, report.is_clean);
    assert_eq!(parsed.inodes_checked, report.inodes_checked);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn check_image_fails_on_missing_path() {
    let path = tmp_image_path("missing");
    // Datei wurde absichtlich nicht angelegt.
    let err = check_image(&path).expect_err("missing path must fail");
    // FileImageDevice::open meldet das als CoreFsError → ToolsError::Core.
    assert!(matches!(err, ToolsError::Core(_)));
}

#[test]
fn severity_kind_serializes_as_lowercase() {
    let issue = FsckIssueReport {
        severity: SeverityKind::Error,
        code: "TEST.X".to_string(),
        message: "boom".to_string(),
    };
    let json = serde_json::to_string(&issue).unwrap();
    assert!(json.contains(r#""severity":"error""#));
}
