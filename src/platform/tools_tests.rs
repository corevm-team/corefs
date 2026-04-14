use super::*;

#[test]
fn tool_registry_uses_expected_command_names() {
    let registry = ToolRegistry::default();

    assert_eq!(registry.mkfs, "corefs mkfs");
    assert_eq!(registry.fsck, "corefs fsck");
    assert_eq!(registry.admin, "corefs admin");
    assert_eq!(registry.benchmark, "corefs benchmark");
}
