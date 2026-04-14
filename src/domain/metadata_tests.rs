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
