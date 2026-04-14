// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::{Report, ToolsError};
use crate::mkfs::{FormatImageOptions, format_image};
use std::path::PathBuf;

fn tmp_image_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-tools-dump-{tag}-{pid}-{nano}.img"));
    p
}

#[test]
fn superblock_decodes_after_format() {
    let path = tmp_image_path("after_format");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = superblock(&path).expect("dump ok");
    assert!(report.magic_ok, "fresh volume must have valid magic");
    assert_eq!(report.block_size, 4096);
    assert_eq!(report.total_blocks, 4 * 1024 * 1024 / 4096);
    // mkfs::format_image now produces a NATIVE-mode volume by default. The
    // initial format bumps the generation once (1), and the subsequent
    // save_state_native bumps it again (2).
    assert!(report.generation >= 2, "got generation {}", report.generation);
    assert_eq!(report.layout_mode, "native");
    assert_eq!(report.uuid_hex.len(), 32);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn superblock_label_round_trips() {
    let path = tmp_image_path("label");
    let opts = FormatImageOptions {
        label: "my-volume".to_string(),
        ..Default::default()
    };
    format_image(&path, 4 * 1024 * 1024, &opts).expect("format ok");

    let report = superblock(&path).expect("dump ok");
    assert_eq!(report.label, "my-volume");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn summary_mentions_layout_mode_and_generation() {
    let path = tmp_image_path("summary");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = superblock(&path).expect("dump ok");
    let summary = report.summary();
    assert!(summary.contains("superblock"));
    assert!(summary.contains(&format!("gen {}", report.generation)));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn render_text_includes_uuid_and_features() {
    let path = tmp_image_path("text");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = superblock(&path).expect("dump ok");
    let text = report.render_text();
    assert!(text.contains("uuid"));
    assert!(text.contains("feature_compat"));
    assert!(text.contains("feature_incompat"));
    assert!(text.contains(&report.uuid_hex));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn render_json_round_trips_via_serde() {
    let path = tmp_image_path("json");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = superblock(&path).expect("dump ok");
    let json = report.render_json();
    let parsed: SuperblockReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.magic, report.magic);
    assert_eq!(parsed.uuid_hex, report.uuid_hex);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn superblock_fails_on_missing_path() {
    let path = tmp_image_path("missing");
    let err = superblock(&path).expect_err("missing path must fail");
    assert!(matches!(err, ToolsError::Core(_)));
}

// =====================================================================
// dump::inode tests
// =====================================================================

#[test]
fn inode_dump_returns_system_payload_for_slot_zero() {
    // Slot 0 is the system payload slot (reserved by mkfs).
    let path = tmp_image_path("inode_zero");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = inode(&path, 0).expect("inode 0 dump ok");
    assert_eq!(report.slot, 0);
    // On a freshly formatted volume, slot 0 holds the system payload.
    assert_eq!(report.kind, "SystemPayload");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn inode_dump_fails_for_unallocated_slot() {
    let path = tmp_image_path("inode_unalloc");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    // Slot 100 is well within range but not allocated on a fresh volume.
    let err = inode(&path, 100).expect_err("unallocated slot must fail");
    assert!(matches!(err, ToolsError::Core(_)));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn inode_dump_fails_for_out_of_range_slot() {
    let path = tmp_image_path("inode_oor");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let err = inode(&path, u64::MAX).expect_err("out-of-range slot must fail");
    assert!(matches!(err, ToolsError::Core(_)));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn inode_dump_summary_mentions_slot_and_kind() {
    let path = tmp_image_path("inode_sum");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = inode(&path, 0).expect("dump ok");
    let summary = report.summary();
    assert!(summary.contains("inode slot 0"));
    assert!(summary.contains(&report.kind));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn inode_dump_render_text_lists_metric_lines() {
    let path = tmp_image_path("inode_text");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = inode(&path, 0).expect("dump ok");
    let text = report.render_text();
    assert!(text.contains("inode dump"));
    assert!(text.contains("slot"));
    assert!(text.contains("kind"));
    assert!(text.contains("data crc"));
    assert!(text.contains("extents"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn inode_dump_render_json_round_trips_via_serde() {
    let path = tmp_image_path("inode_json");
    format_image(&path, 4 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");

    let report = inode(&path, 0).expect("dump ok");
    let json = report.render_json();
    let parsed: InodeDumpReport = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed.slot, report.slot);
    assert_eq!(parsed.kind, report.kind);
    assert_eq!(parsed.data_crc, report.data_crc);

    let _ = std::fs::remove_file(&path);
}
