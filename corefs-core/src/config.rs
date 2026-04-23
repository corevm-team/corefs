// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::string::{String, ToString};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageTier {
    Hot,
    Warm,
    Cold,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersioningPolicy {
    pub keep_latest: usize,
    pub auto_prune: bool,
    pub expose_time_travel: bool,
    /// Maximum total bytes that all stored versions may occupy.
    /// When this limit is exceeded after a write, the oldest versions across
    /// all paths are pruned until the total drops back below the threshold.
    /// `None` disables byte-budget pruning (count-based `keep_latest` still applies).
    pub max_version_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityPolicy {
    pub encryption_at_rest: bool,
    pub acl_enabled: bool,
    pub secure_delete_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformancePolicy {
    pub journaling_enabled: bool,
    pub copy_on_write: bool,
    pub compression_enabled: bool,
    pub deduplication_enabled: bool,
    pub trim_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaPolicy {
    pub max_files: Option<usize>,
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreFsConfig {
    pub volume_name: String,
    pub block_size: usize,
    pub inode_table_capacity: usize,
    pub default_tier: StorageTier,
    pub versioning: VersioningPolicy,
    pub security: SecurityPolicy,
    pub performance: PerformancePolicy,
    pub quotas: QuotaPolicy,
}

impl Default for CoreFsConfig {
    fn default() -> Self {
        Self {
            volume_name: "corefs".to_string(),
            block_size: 4096,
            inode_table_capacity: 1_000_000,
            default_tier: StorageTier::Warm,
            versioning: VersioningPolicy {
                keep_latest: 16,
                auto_prune: true,
                expose_time_travel: true,
                max_version_bytes: Some(64 * 1024 * 1024), // 64 MiB default budget
            },
            security: SecurityPolicy {
                encryption_at_rest: true,
                acl_enabled: true,
                secure_delete_supported: true,
            },
            performance: PerformancePolicy {
                journaling_enabled: true,
                copy_on_write: true,
                compression_enabled: true,
                deduplication_enabled: false,
                trim_enabled: true,
            },
            quotas: QuotaPolicy {
                max_files: None,
                max_bytes: None,
            },
        }
    }
}

impl CoreFsConfig {
    /// Throughput-oriented profile for filesystem benchmark and appliance use.
    ///
    /// This keeps the format and semantics platform-neutral: Windows, Linux and
    /// anyOS can all request the same profile and get the same CoreFS policy.
    pub fn performance_profile() -> Self {
        let mut config = Self::default();
        config.default_tier = StorageTier::Hot;
        config.versioning.keep_latest = 0;
        config.versioning.auto_prune = false;
        config.versioning.expose_time_travel = false;
        config.versioning.max_version_bytes = None;
        config.security.encryption_at_rest = false;
        config.performance.copy_on_write = false;
        config.performance.compression_enabled = false;
        config.performance.deduplication_enabled = false;
        config
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
