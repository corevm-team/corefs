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
