use crate::config::StorageTier;
use crate::domain::acl::AclEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentClass {
    Text,
    Binary,
    Image,
    SourceCode,
    Archive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub tags: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub content_class: ContentClass,
    pub storage_tier: StorageTier,
    pub acl: Vec<AclEntry>,
    pub encrypted: bool,
    pub compressed: bool,
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
    }
}
