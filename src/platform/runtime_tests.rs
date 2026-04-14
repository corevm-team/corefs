// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn runtime_blueprint_defaults_to_platform_neutral_targets() {
    let blueprint = RuntimeIntegrationBlueprint::default();

    assert_eq!(blueprint.kernel_module_name, "corefs_runtime");
    assert_eq!(blueprint.mount_interface, "corefs://mount");
    assert_eq!(blueprint.admin_endpoint, "corefs://admin");
    assert!(
        blueprint
            .compatibility_targets
            .iter()
            .any(|target| target == "native-os")
    );
    assert!(
        blueprint
            .compatibility_targets
            .iter()
            .any(|target| target == "posix-adapter")
    );
    assert!(
        blueprint
            .compatibility_targets
            .iter()
            .any(|target| target == "fuse-adapter")
    );
}

#[test]
fn platform_adapter_descriptor_captures_runtime_contract() {
    let descriptor = PlatformAdapterDescriptor {
        name: "linux-fuse".to_string(),
        runtime: "userspace".to_string(),
        persistent_volume: true,
    };

    assert_eq!(descriptor.name, "linux-fuse");
    assert_eq!(descriptor.runtime, "userspace");
    assert!(descriptor.persistent_volume);
}
