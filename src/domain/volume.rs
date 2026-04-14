// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::config::CoreFsConfig;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[path = "volume_tests.rs"]
mod tests;
