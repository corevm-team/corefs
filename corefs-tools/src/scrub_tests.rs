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
    p.push(format!("corefs-tools-scrub-{tag}-{pid}-{nano}.img"));
    p
}

#[test]
fn scrub_full_on_pristine_volume_is_clean() {
    let path = tmp_image_path("full_clean");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = scrub_image(&path, ScrubMode::Full).expect("scrub ok");
    assert!(
        report.is_clean,
        "fresh volume must scrub clean, got: {report:#?}"
    );
    assert!(report.data_corruptions.is_empty());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_structural_only_skips_data_crc() {
    let path = tmp_image_path("structural");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = scrub_image(&path, ScrubMode::StructuralOnly).expect("scrub ok");
    // Structural-only mode never touches data extents.
    assert_eq!(report.extents_verified, 0);
    assert_eq!(report.blocks_verified, 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_read_only_does_not_commit_repair_ops() {
    let path = tmp_image_path("readonly");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = scrub_image(&path, ScrubMode::ReadOnly).expect("scrub ok");
    assert_eq!(
        report.repair_ops_committed, 0,
        "ReadOnly mode must never commit repair ops"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_summary_distinguishes_modes() {
    let path = tmp_image_path("summary");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = scrub_image(&path, ScrubMode::Full).expect("scrub ok");
    let summary = report.summary();
    assert!(
        summary.starts_with("scrub clean") || summary.starts_with("scrub FAIL"),
        "got: {summary}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_text_render_lists_metric_lines() {
    let path = tmp_image_path("text");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = scrub_image(&path, ScrubMode::Full).expect("scrub ok");
    let text = report.render_text();
    assert!(text.contains("extents verified"));
    assert!(text.contains("blocks verified"));
    assert!(text.contains("repair ops committed"));
    assert!(text.contains("data corruptions"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_render_json_round_trips_via_serde() {
    let path = tmp_image_path("json");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = scrub_image(&path, ScrubMode::Full).expect("scrub ok");
    let json = report.render_json();
    let parsed: ScrubImageReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.is_clean, report.is_clean);
    assert_eq!(parsed.extents_verified, report.extents_verified);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_fails_on_missing_path() {
    let path = tmp_image_path("missing");
    let err = scrub_image(&path, ScrubMode::Full).expect_err("missing path must fail");
    assert!(matches!(err, ToolsError::Core(_)));
}

#[test]
fn scrub_mode_serializes_as_capitalized_variant() {
    let report_full = ScrubImageReport {
        image_path: "x".into(),
        mode: ScrubMode::Full,
        extents_verified: 0,
        blocks_verified: 0,
        data_corruptions: vec![],
        residual_issues: vec![],
        repair_ops_committed: 0,
        fsck_issues_before: 0,
        is_clean: true,
    };
    let json = serde_json::to_string(&report_full).unwrap();
    assert!(json.contains(r#""mode":"Full""#));
}
