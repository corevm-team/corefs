#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIntegrationBlueprint {
    pub kernel_module_name: String,
    pub mount_interface: String,
    pub admin_endpoint: String,
    pub compatibility_targets: Vec<String>,
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
}
