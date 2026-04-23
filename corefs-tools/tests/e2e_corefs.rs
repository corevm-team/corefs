// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! End-to-end host test for the CoreFS tool pipeline.
//!
//! Exercises the round-trip that a typical admin session performs:
//! mkfs → fsck → scrub → cleanup. No FUSE, no kernel — just the plain
//! host tool APIs against a file-image block device.
//!
//! This is deliberately conservative: it does not mutate via a running
//! `OdfFileSession` (that would pull in platform-coupled paths), it just
//! verifies that the integrity pipeline holds on a freshly formatted,
//! empty NATIVE-mode volume.

use std::path::PathBuf;

use corefs_tools::fsck;
use corefs_tools::mkfs::{self, FormatImageOptions, LayoutMode};
use corefs_tools::scrub::{self, ScrubMode};

fn tmp_image_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-e2e-{tag}-{pid}-{nano}.img"));
    p
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn e2e_mkfs_fsck_scrub_roundtrip() {
    let path = tmp_image_path("mfs");
    let capacity: u64 = 16 * 1024 * 1024;

    // 1. mkfs NATIVE layout.
    let opts = FormatImageOptions {
        label: "e2e".to_string(),
        layout_mode: LayoutMode::Native,
        ..Default::default()
    };
    let mkfs_report = mkfs::format_image(&path, capacity, &opts).expect("mkfs succeeds");
    assert_eq!(mkfs_report.label, "e2e");
    assert!(mkfs_report.total_blocks > 0);

    // 2. fsck — freshly formatted volume must be clean.
    let fsck_report = fsck::check_image(&path).expect("fsck returns a report");
    assert!(
        fsck_report.is_clean,
        "freshly formatted volume should be clean: {:?}",
        fsck_report
    );

    // 3. scrub full mode — clean pass expected.
    let scrub_report = scrub::scrub_image(&path, ScrubMode::Full).expect("scrub runs");
    assert!(
        scrub_report.is_clean,
        "scrub on a clean image must be clean: {:?}",
        scrub_report
    );

    // 4. second fsck pass after scrub — still clean.
    let fsck_after = fsck::check_image(&path).expect("fsck post-scrub");
    assert!(fsck_after.is_clean, "post-scrub fsck: {:?}", fsck_after);

    cleanup(&path);
}

#[test]
fn e2e_mkfs_rejects_zero_capacity() {
    let path = tmp_image_path("zero");
    let opts = FormatImageOptions::default();
    let err = mkfs::format_image(&path, 0, &opts).expect_err("zero capacity must fail");
    let msg = format!("{err}");
    assert!(msg.to_ascii_lowercase().contains("capacity"), "msg: {msg}");
    cleanup(&path);
}

#[test]
fn e2e_scrub_readonly_mode_is_clean_on_fresh_image() {
    let path = tmp_image_path("ro");
    let capacity: u64 = 8 * 1024 * 1024;
    let opts = FormatImageOptions {
        layout_mode: LayoutMode::Native,
        ..Default::default()
    };
    mkfs::format_image(&path, capacity, &opts).unwrap();

    let report = scrub::scrub_image(&path, ScrubMode::ReadOnly).unwrap();
    assert!(
        report.is_clean,
        "read-only scrub on fresh image should be clean: {:?}",
        report
    );
    cleanup(&path);
}
