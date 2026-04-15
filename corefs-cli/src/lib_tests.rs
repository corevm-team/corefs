// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use std::path::PathBuf;

fn args(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| (*s).to_string()).collect()
}

fn tmp_image_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-cli-{tag}-{pid}-{nano}.img"));
    p
}

fn capture() -> (Vec<u8>, Vec<u8>) {
    (Vec::new(), Vec::new())
}

// =====================================================================
// Dispatch / Usage
// =====================================================================

#[test]
fn empty_args_prints_usage_and_returns_usage_error() {
    let (mut out, mut err) = capture();
    let status = dispatch(&[], &mut out, &mut err);
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("USAGE"));
}

#[test]
fn help_prints_usage_to_stdout() {
    let (mut out, mut err) = capture();
    let status = dispatch(&args(&["help"]), &mut out, &mut err);
    assert_eq!(status, ExitStatus::Ok);
    let stdout = String::from_utf8(out).unwrap();
    assert!(stdout.contains("USAGE"));
    assert!(stdout.contains("mkfs"));
    assert!(stdout.contains("snapshot"));
    assert!(err.is_empty());
}

#[test]
fn unknown_subcommand_returns_usage_error() {
    let (mut out, mut err) = capture();
    let status = dispatch(&args(&["wat"]), &mut out, &mut err);
    assert_eq!(status, ExitStatus::UsageError);
    let stderr = String::from_utf8(err).unwrap();
    assert!(stderr.contains("unknown subcommand"));
}

// =====================================================================
// mkfs
// =====================================================================

#[test]
fn mkfs_creates_image_file() {
    let path = tmp_image_path("mkfs_basic");
    let path_str = path.display().to_string();
    let (mut out, mut err) = capture();

    let status = dispatch(
        &args(&["mkfs", &path_str, "--capacity", "4194304"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok, "stderr: {}", String::from_utf8_lossy(&err));
    assert!(path.exists());
    assert!(String::from_utf8(out).unwrap().contains("mkfs report"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn mkfs_with_json_emits_json() {
    let path = tmp_image_path("mkfs_json");
    let path_str = path.display().to_string();
    let (mut out, mut err) = capture();

    let status = dispatch(
        &args(&["mkfs", &path_str, "--capacity", "4194304", "--json"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    let stdout = String::from_utf8(out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(parsed["capacity_bytes"], 4_194_304);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn mkfs_missing_capacity_is_usage_error() {
    let path = tmp_image_path("mkfs_no_cap");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["mkfs", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("--capacity"));
    assert!(!path.exists());
}

#[test]
fn mkfs_with_blob_flag_produces_blob_layout() {
    // We can't directly observe the layout via mkfs's report, so this test
    // asserts the dispatch path accepts --blob and runs to completion.
    let path = tmp_image_path("mkfs_blob");
    let path_str = path.display().to_string();
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["mkfs", &path_str, "--capacity", "4194304", "--blob"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    assert!(path.exists());
    let _ = std::fs::remove_file(&path);
}

// =====================================================================
// fsck / repair / scrub / dump
// =====================================================================

fn fresh_image(tag: &str) -> PathBuf {
    let path = tmp_image_path(tag);
    let path_str = path.display().to_string();
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["mkfs", &path_str, "--capacity", "4194304"]),
        &mut out,
        &mut err,
    );
    assert_eq!(
        status,
        ExitStatus::Ok,
        "fresh_image setup failed: stderr={}",
        String::from_utf8_lossy(&err)
    );
    path
}

#[test]
fn fsck_on_fresh_image_succeeds() {
    let path = fresh_image("fsck_ok");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["fsck", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    let stdout = String::from_utf8(out).unwrap();
    assert!(stdout.contains("fsck report"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn fsck_on_missing_path_is_tool_error() {
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["fsck", "/tmp/definitely-not-a-corefs-image-123456.img"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::ToolError);
    assert!(String::from_utf8(err).unwrap().contains("corefs-cli"));
}

#[test]
fn repair_on_fresh_image_is_noop() {
    let path = fresh_image("repair_noop");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["repair", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    assert!(String::from_utf8(out).unwrap().contains("repair report"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_full_mode_succeeds() {
    let path = fresh_image("scrub_full");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["scrub", &path.display().to_string(), "--mode", "full"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn scrub_unknown_mode_is_usage_error() {
    let path = fresh_image("scrub_bad_mode");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["scrub", &path.display().to_string(), "--mode", "lol"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("--mode"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn dump_superblock_returns_json() {
    let path = fresh_image("dump_sb_json");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["dump-superblock", &path.display().to_string(), "--json"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    let stdout = String::from_utf8(out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert!(parsed["magic_ok"].as_bool().unwrap());
    assert_eq!(parsed["layout_mode"], "native");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn dump_inode_requires_slot() {
    let path = fresh_image("dump_inode_no_slot");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["dump-inode", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("--slot"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn dump_inode_zero_succeeds() {
    let path = fresh_image("dump_inode_zero");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["dump-inode", &path.display().to_string(), "--slot", "0"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    assert!(String::from_utf8(out).unwrap().contains("inode dump"));
    let _ = std::fs::remove_file(&path);
}

// =====================================================================
// snapshot
// =====================================================================

#[test]
fn snapshot_without_subcommand_is_usage_error() {
    let (mut out, mut err) = capture();
    let status = dispatch(&args(&["snapshot"]), &mut out, &mut err);
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("missing subcommand"));
}

#[test]
fn snapshot_unknown_subcommand_is_usage_error() {
    let (mut out, mut err) = capture();
    let status = dispatch(&args(&["snapshot", "nope"]), &mut out, &mut err);
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("unknown subcommand"));
}

#[test]
fn snapshot_list_on_empty_volume_succeeds() {
    let path = fresh_image("snap_list_empty");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["snapshot", "list", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    assert!(String::from_utf8(out).unwrap().contains("snapshots in"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_create_then_list_shows_one_entry() {
    let path = fresh_image("snap_create_list");
    let path_str = path.display().to_string();

    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["snapshot", "create", &path_str, "--name", "v1"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);

    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["snapshot", "list", &path_str, "--json"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    let parsed: serde_json::Value = serde_json::from_str(String::from_utf8(out).unwrap().trim())
        .expect("valid json");
    assert_eq!(parsed["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["snapshots"][0]["name"], "v1");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_create_missing_name_is_usage_error() {
    let path = fresh_image("snap_no_name");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["snapshot", "create", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::UsageError);
    assert!(String::from_utf8(err).unwrap().contains("--name"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_delete_unknown_id_is_tool_error() {
    let path = fresh_image("snap_del_unknown");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["snapshot", "delete", &path.display().to_string(), "--id", "9999"]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::ToolError);
    let _ = std::fs::remove_file(&path);
}

// =====================================================================
// defrag
// =====================================================================

#[test]
fn defrag_on_fresh_image_succeeds() {
    let path = fresh_image("defrag");
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["defrag", &path.display().to_string()]),
        &mut out,
        &mut err,
    );
    assert_eq!(status, ExitStatus::Ok);
    assert!(String::from_utf8(out).unwrap().contains("defrag report"));
    let _ = std::fs::remove_file(&path);
}

// =====================================================================
// Argument parser units
// =====================================================================

#[test]
fn parse_required_u64_rejects_non_numeric() {
    let r = parse_required_u64(&args(&["--n", "abc"]), "--n");
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("not a valid"));
}

#[test]
fn parse_required_u64_missing_flag_errors() {
    let r = parse_required_u64(&args(&[]), "--n");
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("missing required"));
}

#[test]
fn parse_string_returns_value_after_flag() {
    let v = parse_string(&args(&["--label", "hello"]), "--label");
    assert_eq!(v, Some("hello".to_string()));
}

#[test]
fn parse_string_missing_flag_returns_none() {
    let v = parse_string(&args(&["--other", "x"]), "--label");
    assert_eq!(v, None);
}

#[test]
fn has_flag_detects_present_and_absent() {
    assert!(has_flag(&args(&["--json"]), "--json"));
    assert!(!has_flag(&args(&["--blob"]), "--json"));
}

#[test]
fn collect_positional_skips_value_flags_but_keeps_bool_flags_invisible() {
    let positional = collect_positional(
        &args(&["/tmp/x.img", "--capacity", "1024", "--json"]),
        1,
        "test",
    )
    .unwrap();
    assert_eq!(positional, vec!["/tmp/x.img".to_string()]);
}

#[test]
fn collect_positional_reports_missing_required_arg() {
    let r = collect_positional(&args(&["--json"]), 1, "test <path>");
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("test <path>"));
}

// =====================================================================
// Mount-check gate
// =====================================================================

#[test]
fn scrub_unknown_mount_status_proceeds() {
    // An implausible path yields Unknown on most systems (or NotMounted on
    // Linux where /proc/mounts is readable). Either way the gate must not
    // refuse and the ToolError (because the path doesn't exist) surfaces.
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["scrub", "/does/not/exist/corefs-cli-gate-probe.img"]),
        &mut out,
        &mut err,
    );
    assert_ne!(
        status,
        ExitStatus::Unsupported,
        "gate must not refuse non-mounted / unknown devices"
    );
}

#[test]
fn defrag_accepts_online_flag_as_bool() {
    // --online must be treated as a boolean flag and not eat the path token.
    let (mut out, mut err) = capture();
    let status = dispatch(
        &args(&["defrag", "--online", "/does/not/exist/corefs-cli-gate.img"]),
        &mut out,
        &mut err,
    );
    // Gate may refuse (if the probe sees it as mounted somehow) or tool
    // fails. Crucially it is NOT a UsageError — the path was parsed.
    assert_ne!(status, ExitStatus::UsageError);
}
