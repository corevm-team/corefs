// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::Report;
use std::path::PathBuf;

fn tmp_path(tag: &str, ext: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    p.push(format!("corefs-keys-{tag}-{pid}-{nano}.{ext}"));
    p
}

fn write_master_key(path: &PathBuf, bytes: [u8; 32]) {
    std::fs::write(path, bytes).expect("write master key");
}

#[test]
fn init_creates_valid_keystore() {
    let mk = tmp_path("init", "mk");
    write_master_key(&mk, [1u8; 32]);
    let ks = tmp_path("init", "kst");

    let report = init(&ks, &mk, "000102030405060708090a0b0c0d0e0f").expect("init");
    assert_eq!(report.version, KEYSTORE_VERSION);
    assert!(report.bytes_written > 0);
    assert!(ks.exists());

    // Datei lesbar?
    let file = read_keystore_file(&ks).expect("read ok");
    assert_eq!(file.magic, KEYSTORE_MAGIC);
    let _ = std::fs::remove_file(&mk);
    let _ = std::fs::remove_file(&ks);
}

#[test]
fn verify_accepts_correct_master_key() {
    let mk = tmp_path("verify-ok", "mk");
    write_master_key(&mk, [7u8; 32]);
    let ks = tmp_path("verify-ok", "kst");

    init(&ks, &mk, "aabbccddeeff00112233445566778899").expect("init");
    let v = verify(&ks, &mk).expect("verify");
    assert!(v.magic_ok);
    assert!(v.version_ok);
    assert!(v.unwrap_ok);
    assert!(v.summary().contains("ok"));

    let _ = std::fs::remove_file(&mk);
    let _ = std::fs::remove_file(&ks);
}

#[test]
fn verify_rejects_wrong_master_key() {
    let mk = tmp_path("verify-bad", "mk");
    let mk_other = tmp_path("verify-bad-other", "mk");
    write_master_key(&mk, [7u8; 32]);
    write_master_key(&mk_other, [8u8; 32]);
    let ks = tmp_path("verify-bad", "kst");

    init(&ks, &mk, "aabbccddeeff00112233445566778899").expect("init");
    let v = verify(&ks, &mk_other).expect("verify");
    assert!(v.magic_ok);
    assert!(v.version_ok);
    assert!(!v.unwrap_ok);
    assert!(v.summary().contains("FAILED"));

    let _ = std::fs::remove_file(&mk);
    let _ = std::fs::remove_file(&mk_other);
    let _ = std::fs::remove_file(&ks);
}

#[test]
fn rotate_changes_wrap_but_preserves_access() {
    let mk_old = tmp_path("rot-old", "mk");
    let mk_new = tmp_path("rot-new", "mk");
    write_master_key(&mk_old, [10u8; 32]);
    write_master_key(&mk_new, [20u8; 32]);
    let ks = tmp_path("rot", "kst");

    init(&ks, &mk_old, "00000000000000000000000000000001").expect("init");
    rotate(&ks, &mk_old, &mk_new).expect("rotate");

    // Alte master-key verifikation schlaegt jetzt fehl
    let v_old = verify(&ks, &mk_old).expect("verify");
    assert!(!v_old.unwrap_ok);
    // Neue master-key verifikation klappt
    let v_new = verify(&ks, &mk_new).expect("verify");
    assert!(v_new.unwrap_ok);

    let _ = std::fs::remove_file(&mk_old);
    let _ = std::fs::remove_file(&mk_new);
    let _ = std::fs::remove_file(&ks);
}

#[test]
fn init_rejects_bad_uuid_length() {
    let mk = tmp_path("badu", "mk");
    write_master_key(&mk, [0u8; 32]);
    let ks = tmp_path("badu", "kst");
    let err = init(&ks, &mk, "short").unwrap_err();
    assert!(matches!(err, ToolsError::InvalidArgument(_)));
    let _ = std::fs::remove_file(&mk);
}

#[test]
fn read_master_key_rejects_wrong_size() {
    let mk = tmp_path("wrongsz", "mk");
    std::fs::write(&mk, [0u8; 16]).unwrap(); // only 16 bytes, not 32
    let err = read_master_key(&mk).unwrap_err();
    assert!(matches!(err, ToolsError::InvalidArgument(_)));
    let _ = std::fs::remove_file(&mk);
}

#[test]
fn verify_returns_full_metadata() {
    let mk = tmp_path("meta", "mk");
    write_master_key(&mk, [3u8; 32]);
    let ks = tmp_path("meta", "kst");
    init(&ks, &mk, "0102030405060708090a0b0c0d0e0f10").expect("init");
    let v = verify(&ks, &mk).expect("verify");
    assert_eq!(v.volume_uuid, "0102030405060708090a0b0c0d0e0f10");
    assert_eq!(v.version, KEYSTORE_VERSION);
    let json = v.render_json();
    assert!(json.contains("\"magic_ok\""));
    let _ = std::fs::remove_file(&mk);
    let _ = std::fs::remove_file(&ks);
}

#[test]
fn double_init_overwrites() {
    let mk = tmp_path("double", "mk");
    write_master_key(&mk, [4u8; 32]);
    let ks = tmp_path("double", "kst");

    init(&ks, &mk, "0102030405060708090a0b0c0d0e0f10").expect("init1");
    let first = std::fs::read(&ks).unwrap();
    init(&ks, &mk, "0102030405060708090a0b0c0d0e0f10").expect("init2");
    let second = std::fs::read(&ks).unwrap();
    // Volume-Key wird zufaellig generiert -> Inhalt sollte unterschiedlich sein
    assert_ne!(first, second);

    let _ = std::fs::remove_file(&mk);
    let _ = std::fs::remove_file(&ks);
}
