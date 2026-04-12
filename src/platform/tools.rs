#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRegistry {
    pub mkfs: String,
    pub fsck: String,
    pub admin: String,
    pub benchmark: String,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            mkfs: "corefs mkfs".to_string(),
            fsck: "corefs fsck".to_string(),
            admin: "corefs admin".to_string(),
            benchmark: "corefs benchmark".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_registry_uses_expected_command_names() {
        let registry = ToolRegistry::default();

        assert_eq!(registry.mkfs, "corefs mkfs");
        assert_eq!(registry.fsck, "corefs fsck");
        assert_eq!(registry.admin, "corefs admin");
        assert_eq!(registry.benchmark, "corefs benchmark");
    }
}
