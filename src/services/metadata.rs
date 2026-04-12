use crate::config::StorageTier;
use crate::domain::inode::Inode;

#[derive(Debug, Default)]
pub struct MetadataService;

impl MetadataService {
    pub fn set_attribute(inode: &mut Inode, key: &str, value: &str) {
        if let Some((_, existing)) = inode
            .metadata
            .attributes
            .iter_mut()
            .find(|(existing_key, _)| existing_key == key)
        {
            *existing = value.to_string();
        } else {
            inode
                .metadata
                .attributes
                .push((key.to_string(), value.to_string()));
        }
    }

    pub fn add_tag(inode: &mut Inode, tag: &str) {
        if !inode.metadata.tags.iter().any(|existing| existing == tag) {
            inode.metadata.tags.push(tag.to_string());
            inode.metadata.tags.sort();
        }
    }

    pub fn remove_tag(inode: &mut Inode, tag: &str) {
        inode.metadata.tags.retain(|existing| existing != tag);
    }

    pub fn set_storage_tier(inode: &mut Inode, tier: StorageTier) {
        inode.metadata.storage_tier = tier;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::inode::{Inode, InodeId, InodeKind};
    use crate::domain::metadata::FileMetadata;

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
}
