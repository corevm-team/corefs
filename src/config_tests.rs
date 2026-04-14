// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn default_config_has_expected_enterprise_defaults() {
    let config = CoreFsConfig::default();

    assert_eq!(config.volume_name, "corefs");
    assert_eq!(config.block_size, 4096);
    assert_eq!(config.inode_table_capacity, 1_000_000);
    assert_eq!(config.default_tier, StorageTier::Warm);
    assert!(config.versioning.auto_prune);
    assert!(config.versioning.expose_time_travel);
    assert!(config.security.encryption_at_rest);
    assert!(config.security.acl_enabled);
    assert!(config.security.secure_delete_supported);
    assert!(config.performance.journaling_enabled);
    assert!(config.performance.copy_on_write);
    assert!(config.performance.compression_enabled);
    assert!(!config.performance.deduplication_enabled);
    assert!(config.performance.trim_enabled);
    assert_eq!(config.quotas.max_files, None);
    assert_eq!(config.quotas.max_bytes, None);
}
