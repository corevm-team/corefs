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
#[path = "runtime_tests.rs"]
mod tests;
