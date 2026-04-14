// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::CoreFsConfig;
use crate::platform::Timestamp;

#[test]
fn volume_descriptor_exposes_enabled_features() {
    // Uses the no_std-friendly `from_config_at` so the test runs both with and
    // without the `std` feature of `corefs-core`.
    let descriptor = VolumeDescriptor::from_config_at(&CoreFsConfig::default(), Timestamp::EPOCH);

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
