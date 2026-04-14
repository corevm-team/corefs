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
    p.push(format!("corefs-tools-defrag-{tag}-{pid}-{nano}.img"));
    p
}

#[test]
fn defrag_on_pristine_volume_is_noop() {
    let path = tmp_image_path("noop");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = defrag_image(&path).expect("defrag ok");
    assert_eq!(report.moved_entries, 0);
    assert_eq!(report.reclaimed_gaps, 0);
    assert!(report.summary().contains("nothing to do"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn defrag_render_text_lists_metric_lines() {
    let path = tmp_image_path("text");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = defrag_image(&path).expect("defrag ok");
    let text = report.render_text();
    assert!(text.contains("moved entries"));
    assert!(text.contains("reclaimed gaps"));
    assert!(text.contains("final device blocks"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn defrag_render_json_round_trips_via_serde() {
    let path = tmp_image_path("json");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = defrag_image(&path).expect("defrag ok");
    let json = report.render_json();
    let parsed: DefragImageReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.moved_entries, report.moved_entries);
    assert_eq!(parsed.reclaimed_gaps, report.reclaimed_gaps);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn defrag_fails_on_missing_path() {
    let path = tmp_image_path("missing");
    let err = defrag_image(&path).expect_err("missing path must fail");
    assert!(matches!(err, ToolsError::Core(_)));
}

#[test]
fn defrag_idempotent_when_run_twice() {
    let path = tmp_image_path("idempotent");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let r1 = defrag_image(&path).expect("first ok");
    let r2 = defrag_image(&path).expect("second ok");
    assert_eq!(r1.moved_entries, 0);
    assert_eq!(r2.moved_entries, 0);

    let _ = std::fs::remove_file(&path);
}
