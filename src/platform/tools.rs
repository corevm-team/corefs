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
#[path = "tools_tests.rs"]
mod tests;
