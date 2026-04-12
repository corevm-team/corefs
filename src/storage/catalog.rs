use crate::domain::inode::{Inode, InodeId};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct Catalog {
    entries: BTreeMap<String, Inode>,
    deleted: BTreeMap<String, Inode>,
}

impl Catalog {
    pub fn insert(&mut self, inode: Inode) {
        self.entries.insert(inode.path.clone(), inode);
    }

    pub fn get(&self, path: &str) -> Option<&Inode> {
        self.entries.get(path)
    }

    pub fn get_mut(&mut self, path: &str) -> Option<&mut Inode> {
        self.entries.get_mut(path)
    }

    pub fn remove(&mut self, path: &str) -> Option<Inode> {
        self.entries.remove(path)
    }

    pub fn move_to_deleted(&mut self, inode: Inode) {
        self.deleted.insert(inode.path.clone(), inode);
    }

    pub fn restore_deleted(&mut self, path: &str) -> Option<Inode> {
        self.deleted.remove(path)
    }

    pub fn deleted_contains(&self, path: &str) -> bool {
        self.deleted.contains_key(path)
    }

    pub fn list_paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn list_deleted_paths(&self) -> Vec<String> {
        self.deleted.keys().cloned().collect()
    }

    pub fn inode_by_id(&self, inode_id: InodeId) -> Option<&Inode> {
        self.entries.values().find(|inode| inode.id == inode_id)
    }

    pub fn active_entries(&self) -> Vec<Inode> {
        self.entries.values().cloned().collect()
    }

    pub fn deleted_entries(&self) -> Vec<Inode> {
        self.deleted.values().cloned().collect()
    }

    pub fn from_parts(active: Vec<Inode>, deleted: Vec<Inode>) -> Self {
        let entries = active
            .into_iter()
            .map(|inode| (inode.path.clone(), inode))
            .collect();
        let deleted = deleted
            .into_iter()
            .map(|inode| (inode.path.clone(), inode))
            .collect();
        Self { entries, deleted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::inode::{Inode, InodeId, InodeKind};
    use crate::domain::metadata::FileMetadata;

    #[test]
    fn catalog_tracks_active_and_deleted_entries() {
        let inode = Inode::new(
            InodeId(1),
            InodeKind::File,
            "/data.txt".to_string(),
            FileMetadata::default(),
        );
        let mut catalog = Catalog::default();
        catalog.insert(inode.clone());

        assert!(catalog.get("/data.txt").is_some());
        assert_eq!(
            catalog
                .inode_by_id(InodeId(1))
                .map(|item| item.path.as_str()),
            Some("/data.txt")
        );
        assert_eq!(catalog.list_paths(), vec!["/data.txt".to_string()]);

        let removed = catalog.remove("/data.txt").expect("inode should exist");
        catalog.move_to_deleted(removed.clone());
        assert_eq!(catalog.list_deleted_paths(), vec!["/data.txt".to_string()]);
        assert_eq!(catalog.restore_deleted("/data.txt"), Some(removed));
        assert!(catalog.get_mut("/data.txt").is_none());
        assert!(catalog.inode_by_id(InodeId(99)).is_none());
    }
}
