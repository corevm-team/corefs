use crate::config::StorageTier;
use crate::domain::acl::AclEntry;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentClass {
    Text,
    Binary,
    Image,
    SourceCode,
    Archive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadata {
    pub tags: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub content_class: ContentClass,
    pub storage_tier: StorageTier,
    pub acl: Vec<AclEntry>,
    pub encrypted: bool,
    pub compressed: bool,
    /// POSIX owner user ID.
    pub uid: u32,
    /// POSIX owner group ID.
    pub gid: u32,
    /// POSIX mode bits (permissions only — type is encoded in `InodeKind`).
    /// Typical values: files `0o644`, directories `0o755`, symlinks `0o777`.
    pub mode: u32,
}

impl Default for FileMetadata {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            attributes: Vec::new(),
            content_class: ContentClass::Unknown,
            storage_tier: StorageTier::Warm,
            acl: Vec::new(),
            encrypted: false,
            compressed: false,
            uid: 0,
            gid: 0,
            mode: 0o644,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metadata_is_safe_and_empty() {
        let metadata = FileMetadata::default();

        assert!(metadata.tags.is_empty());
        assert!(metadata.attributes.is_empty());
        assert_eq!(metadata.content_class, ContentClass::Unknown);
        assert_eq!(metadata.storage_tier, StorageTier::Warm);
        assert!(metadata.acl.is_empty());
        assert!(!metadata.encrypted);
        assert!(!metadata.compressed);
        assert_eq!(metadata.uid, 0);
        assert_eq!(metadata.gid, 0);
        assert_eq!(metadata.mode, 0o644);
    }
}
