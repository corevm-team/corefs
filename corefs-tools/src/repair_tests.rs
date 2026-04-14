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
    p.push(format!("corefs-tools-repair-{tag}-{pid}-{nano}.img"));
    p
}

#[test]
fn repair_on_pristine_volume_is_noop() {
    let path = tmp_image_path("noop");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = repair_image(&path).expect("repair ok");
    // Pristine volume has no Error/Warning issues → repair is a no-op.
    assert_eq!(report.fixed.len(), 0);
    assert_eq!(report.unfixable.len(), 0);
    assert_eq!(report.ops_committed, 0);
    assert!(report.fully_repaired);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn repair_summary_for_noop_volume() {
    let path = tmp_image_path("noop_sum");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = repair_image(&path).expect("repair ok");
    assert!(report.summary().contains("nothing to do"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn repair_render_text_lists_metric_lines() {
    let path = tmp_image_path("text");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = repair_image(&path).expect("repair ok");
    let text = report.render_text();
    assert!(text.contains("ops committed"));
    assert!(text.contains("fixed"));
    assert!(text.contains("unfixable"));
    assert!(text.contains("fully repaired"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn repair_render_json_round_trips_via_serde() {
    let path = tmp_image_path("json");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = repair_image(&path).expect("repair ok");
    let json = report.render_json();
    let parsed: RepairImageReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.fully_repaired, report.fully_repaired);
    assert_eq!(parsed.ops_committed, report.ops_committed);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn repair_fails_on_missing_path() {
    let path = tmp_image_path("missing");
    let err = repair_image(&path).expect_err("missing path must fail");
    assert!(matches!(err, ToolsError::Core(_)));
}

#[test]
fn repair_idempotent_on_clean_volume() {
    let path = tmp_image_path("idempotent");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    // First pass: no-op.
    let r1 = repair_image(&path).expect("first pass");
    // Second pass: still no-op.
    let r2 = repair_image(&path).expect("second pass");

    assert_eq!(r1.ops_committed, 0);
    assert_eq!(r2.ops_committed, 0);
    assert!(r1.fully_repaired && r2.fully_repaired);

    let _ = std::fs::remove_file(&path);
}
