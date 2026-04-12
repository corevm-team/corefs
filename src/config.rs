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
pub struct CoreFsConfig {
    pub volume_name: String,
    pub block_size: usize,
    pub inode_table_capacity: usize,
    pub default_tier: StorageTier,
    pub versioning: VersioningPolicy,
    pub security: SecurityPolicy,
    pub performance: PerformancePolicy,
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
        }
    }
}

#[cfg(test)]
mod tests {
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
    }
}
