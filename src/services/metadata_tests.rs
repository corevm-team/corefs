use super::*;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::error::CoreFsError;

#[test]
fn metadata_service_updates_tags_and_attributes() {
    let mut inode = Inode::new(
        InodeId(1),
        InodeKind::File,
        "/doc.txt".to_string(),
        FileMetadata::default(),
    );

    MetadataService::add_tag(&mut inode, "docs");
    MetadataService::add_tag(&mut inode, "docs");
    MetadataService::set_attribute(&mut inode, "owner", "team-a");
    MetadataService::set_attribute(&mut inode, "owner", "team-b");
    MetadataService::set_storage_tier(&mut inode, StorageTier::Hot);
    MetadataService::remove_tag(&mut inode, "missing");

    assert_eq!(inode.metadata.tags, vec!["docs".to_string()]);
    assert_eq!(
        inode.metadata.attributes,
        vec![("owner".to_string(), "team-b".to_string())]
    );
    assert_eq!(inode.metadata.storage_tier, StorageTier::Hot);
}

#[test]
fn pointer_attributes_resolve_dynamically() {
    let mut inode = Inode::new(
        InodeId(2),
        InodeKind::File,
        "/meta.txt".to_string(),
        FileMetadata::default(),
    );

    MetadataService::set_content_pointer(&mut inode, "summary", "/doc.txt", "summary-4");
    let resolved = MetadataService::resolve_attributes(&inode, |path| {
        if path == "/doc.txt" {
            Ok(b"abcdef".to_vec())
        } else {
            Err(CoreFsError::NotFound("missing".to_string()))
        }
    });

    assert_eq!(
        resolved,
        vec![("summary".to_string(), "abcd...".to_string())]
    );
    assert!(MetadataService::is_pointer_value(
        &inode.metadata.attributes[0].1
    ));
}
