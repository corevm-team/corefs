// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Volume-Deskriptor.
//!
//! Beschreibt die Identität und Feature-Konfiguration eines einzelnen
//! CoreFS-Volumes. Der `created_at`-Zeitstempel liegt als [`Timestamp`] vor,
//! damit der Kern no_std-fähig bleibt; bestehende Bincode-Serialisate von
//! `SystemTime` sind byte-identisch weiterhin lesbar (siehe
//! [`crate::platform::Timestamp`]).

use crate::config::CoreFsConfig;
use crate::platform::Timestamp;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeDescriptor {
    pub name: String,
    pub block_size: usize,
    pub created_at: Timestamp,
    pub feature_flags: Vec<String>,
}

impl VolumeDescriptor {
    /// Konstruiert einen Volume-Deskriptor aus der Konfiguration und dem
    /// gegebenen Zeitstempel.
    ///
    /// no_std-fähig: der Aufrufer liefert die Zeit explizit.
    pub fn from_config_at(config: &CoreFsConfig, created_at: Timestamp) -> Self {
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
            created_at,
            feature_flags,
        }
    }

    /// Konstruiert einen Volume-Deskriptor mit der aktuellen Systemzeit.
    ///
    /// Bequemere Variante von [`VolumeDescriptor::from_config_at`] für
    /// std-basierte Umgebungen.
    #[cfg(feature = "std")]
    pub fn from_config(config: &CoreFsConfig) -> Self {
        Self::from_config_at(config, Timestamp::now())
    }
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
