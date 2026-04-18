// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::Report;
use crate::mkfs::{FormatImageOptions, format_image};
use crate::snapshot::{CreateOptions, create as snapshot_create};
use std::path::PathBuf;

fn tmp_image_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-tools-backup-{tag}-{pid}-{nano}.img"));
    p
}

fn fresh_image(tag: &str) -> PathBuf {
    let path = tmp_image_path(tag);
    format_image(&path, 16 * 1024 * 1024, &FormatImageOptions::default()).expect("format ok");
    path
}

#[test]
fn dump_full_roundtrip_report_shape() {
    let src = fresh_image("dump-full");
    let mut out = src.clone();
    out.set_extension("bkp");

    let report = dump(&src, Some(&out), None).expect("dump ok");
    assert!(!report.incremental);
    assert!(report.bytes_written > 0);
    assert!(out.exists());
    assert!(report.summary().contains("full"));

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn restore_on_freshly_formatted_volume_applies_entries() {
    let src = fresh_image("restore-src");
    let mut out = src.clone();
    out.set_extension("bkp");

    // Snapshot anlegen, damit es etwas zu transportieren gibt.
    snapshot_create(
        &src,
        &CreateOptions {
            name: "v1".to_string(),
            scope_root: None,
        },
    )
    .expect("snapshot create");

    let dumped = dump(&src, Some(&out), None).expect("dump ok");
    assert!(dumped.snapshot_records >= 1);

    let tgt = fresh_image("restore-tgt");
    let r = restore(&tgt, Some(&out)).expect("restore ok");
    assert_eq!(r.entries_read, dumped.entries_written);
    assert!(r.snapshots_applied >= 1);
    assert!(r.summary().contains("entries"));

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&tgt);
}

#[test]
fn incremental_dump_with_unknown_base_fails() {
    let src = fresh_image("incr-bad-base");
    let mut out = src.clone();
    out.set_extension("bkp");

    let err = dump(&src, Some(&out), Some(9999)).unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("9999") || msg.contains("not found"));

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn dump_json_render_contains_bytes_written_field() {
    let src = fresh_image("dump-json");
    let mut out = src.clone();
    out.set_extension("bkp");

    let report = dump(&src, Some(&out), None).expect("dump ok");
    let json = report.render_json();
    assert!(json.contains("bytes_written"));
    assert!(json.contains("\"incremental\""));

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn restore_from_missing_input_errors() {
    let tgt = fresh_image("restore-missing");
    let err = restore(&tgt, Some(std::path::Path::new("/nope/corefs/does/not/exist.bkp")))
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("open input") || msg.contains("No such file"));
    let _ = std::fs::remove_file(&tgt);
}
