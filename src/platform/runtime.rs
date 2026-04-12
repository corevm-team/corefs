#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIntegrationBlueprint {
    pub kernel_module_name: String,
    pub mount_interface: String,
    pub admin_endpoint: String,
    pub compatibility_targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAdapterDescriptor {
    pub name: String,
    pub runtime: String,
    pub persistent_volume: bool,
}

pub trait MountAdapter {
    fn descriptor(&self) -> PlatformAdapterDescriptor;
}

impl Default for RuntimeIntegrationBlueprint {
    fn default() -> Self {
        Self {
            kernel_module_name: "corefs_runtime".to_string(),
            mount_interface: "corefs://mount".to_string(),
            admin_endpoint: "corefs://admin".to_string(),
            compatibility_targets: vec![
                "native-os".to_string(),
                "posix-adapter".to_string(),
                "fuse-adapter".to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
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
}
