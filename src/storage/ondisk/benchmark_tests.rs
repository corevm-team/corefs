// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn default_config_runs_cleanly() {
    let cfg = OdfBenchConfig::default();
    let result = run_odf_bench(cfg).unwrap();
    assert_eq!(result.files_populated, cfg.file_count);
    // Every phase must complete (Duration::is_zero allowed but not negative).
    let _ = result.format
        + result.blob_save
        + result.blob_load
        + result.native_save
        + result.native_load;
}

#[test]
fn small_config_finishes_quickly() {
    let cfg = OdfBenchConfig {
        volume_blocks: 512,
        inode_count: 128,
        file_count: 4,
        payload_size: 128,
    };
    let result = run_odf_bench(cfg).unwrap();
    assert_eq!(result.files_populated, 4);
    assert!(result.format.as_millis() < 1000);
}

#[test]
fn zero_files_still_succeeds() {
    let cfg = OdfBenchConfig {
        volume_blocks: 512,
        inode_count: 64,
        file_count: 0,
        payload_size: 0,
    };
    let result = run_odf_bench(cfg).unwrap();
    assert_eq!(result.files_populated, 0);
}
