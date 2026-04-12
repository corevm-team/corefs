use crate::domain::inode::Inode;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct RecoveryService {
    tombstones: BTreeMap<String, Inode>,
}

impl RecoveryService {
    pub fn remember(&mut self, inode: Inode) {
        self.tombstones.insert(inode.path.clone(), inode);
    }

    pub fn recover(&mut self, path: &str) -> Option<Inode> {
        self.tombstones.remove(path)
    }

    pub fn recoverable_paths(&self) -> Vec<String> {
        self.tombstones.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::inode::{Inode, InodeId, InodeKind};
    use crate::domain::metadata::FileMetadata;

    #[test]
    fn remember_and_recover_round_trips_deleted_inode() {
        let inode = Inode::new(
            InodeId(2),
            InodeKind::File,
            "/lost.txt".to_string(),
            FileMetadata::default(),
        );
        let mut recovery = RecoveryService::default();
        recovery.remember(inode.clone());

        assert_eq!(recovery.recoverable_paths(), vec!["/lost.txt".to_string()]);
        assert_eq!(recovery.recover("/lost.txt"), Some(inode));
        assert!(recovery.recover("/lost.txt").is_none());
    }
}
