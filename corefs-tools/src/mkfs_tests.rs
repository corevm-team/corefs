// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::Report;
use std::path::PathBuf;

/// Liefert einen eindeutigen temporären Pfad pro Test-Aufruf.
fn tmp_image_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-tools-mkfs-{tag}-{pid}-{nano}.img"));
    p
}

#[test]
fn format_image_creates_file_and_returns_report() {
    let path = tmp_image_path("basic");
    let cap = 4 * 1024 * 1024; // 4 MiB
    let opts = FormatImageOptions::default();

    let report = format_image(&path, cap, &opts).expect("format ok");

    assert!(path.exists(), "image file must exist");
    assert_eq!(report.capacity_bytes, cap);
    assert!(report.total_blocks > 0);
    assert!(report.inode_count > 0);
    assert!(report.generation >= 1);
    assert_eq!(report.uuid_hex.len(), 32, "uuid is 16 bytes hex-encoded");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn format_image_rejects_zero_capacity() {
    let path = tmp_image_path("zero");
    let err = format_image(&path, 0, &FormatImageOptions::default())
        .expect_err("zero capacity must fail");
    assert!(matches!(err, ToolsError::InvalidArgument(_)));
    assert!(!path.exists(), "no file should have been created");
}

#[test]
fn format_image_rejects_capacity_not_aligned_to_sector() {
    let path = tmp_image_path("misaligned");
    let opts = FormatImageOptions {
        sector_size: 4096,
        ..Default::default()
    };
    // 4 MiB + 1 byte — not a multiple of 4096.
    let err = format_image(&path, 4 * 1024 * 1024 + 1, &opts).expect_err("misaligned must fail");
    // Inner Core error, but surfaces via ToolsError::Core.
    assert!(matches!(err, ToolsError::Core(_)));
}

#[test]
fn format_image_rejects_existing_file() {
    let path = tmp_image_path("exists");
    let cap = 4 * 1024 * 1024;
    let _first = format_image(&path, cap, &FormatImageOptions::default()).expect("first ok");
    // Second call must fail because the file already exists (create_new semantic).
    let err = format_image(&path, cap, &FormatImageOptions::default())
        .expect_err("existing file must reject");
    assert!(matches!(err, ToolsError::Core(_)));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn format_image_label_is_preserved_in_report() {
    let path = tmp_image_path("label");
    let opts = FormatImageOptions {
        label: "test-volume".to_string(),
        ..Default::default()
    };
    let report = format_image(&path, 4 * 1024 * 1024, &opts).expect("format ok");
    assert_eq!(report.label, "test-volume");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn report_summary_mentions_image_and_size() {
    let path = tmp_image_path("summary");
    let report = format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default())
        .expect("format ok");

    let summary = report.summary();
    assert!(summary.contains("formatted"));
    assert!(summary.contains("MiB"));
    assert!(summary.contains("blocks"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn report_render_text_is_multiline() {
    let path = tmp_image_path("text");
    let report = format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default())
        .expect("format ok");

    let text = report.render_text();
    assert!(text.contains("mkfs report"));
    assert!(text.contains("image path"));
    assert!(text.contains("uuid"));
    assert!(text.lines().count() >= 5);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn report_render_json_is_valid_json() {
    let path = tmp_image_path("json");
    let report = format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default())
        .expect("format ok");

    let json = report.render_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed["capacity_bytes"], 4 * 1024 * 1024);
    assert!(parsed["uuid_hex"].is_string());
    let _ = std::fs::remove_file(&path);
}
