use crate::config::CoreFsConfig;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDescriptor {
    pub name: String,
    pub block_size: usize,
    pub created_at: SystemTime,
    pub feature_flags: Vec<String>,
}

impl VolumeDescriptor {
    pub fn from_config(config: &CoreFsConfig) -> Self {
        let mut feature_flags = Vec::new();

        if config.performance.journaling_enabled {
            feature_flags.push("journaling".to_string());
        }
        if config.performance.copy_on_write {
            feature_flags.push("copy_on_write".to_string());
        }
        if config.performance.compression_enabled {
            feature_flags.push("compression".to_string());
        }
        if config.security.encryption_at_rest {
            feature_flags.push("encryption".to_string());
        }
        if config.security.acl_enabled {
            feature_flags.push("acl".to_string());
        }
        if config.versioning.expose_time_travel {
            feature_flags.push("time_travel".to_string());
        }

        Self {
            name: config.volume_name.clone(),
            block_size: config.block_size,
            created_at: SystemTime::now(),
            feature_flags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CoreFsConfig;

    #[test]
    fn volume_descriptor_exposes_enabled_features() {
        let descriptor = VolumeDescriptor::from_config(&CoreFsConfig::default());

        assert_eq!(descriptor.name, "corefs");
        assert_eq!(descriptor.block_size, 4096);
        assert!(
            descriptor
                .feature_flags
                .iter()
                .any(|flag| flag == "journaling")
        );
        assert!(
            descriptor
                .feature_flags
                .iter()
                .any(|flag| flag == "copy_on_write")
        );
        assert!(
            descriptor
                .feature_flags
                .iter()
                .any(|flag| flag == "compression")
        );
        assert!(
            descriptor
                .feature_flags
                .iter()
                .any(|flag| flag == "encryption")
        );
        assert!(descriptor.feature_flags.iter().any(|flag| flag == "acl"));
        assert!(
            descriptor
                .feature_flags
                .iter()
                .any(|flag| flag == "time_travel")
        );
    }
}
