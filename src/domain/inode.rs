use crate::domain::metadata::FileMetadata;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InodeKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inode {
    pub id: InodeId,
    pub kind: InodeKind,
    pub path: String,
    pub size: usize,
    pub created_at: SystemTime,
    pub modified_at: SystemTime,
    pub metadata: FileMetadata,
}

impl Inode {
    pub fn new(id: InodeId, kind: InodeKind, path: String, metadata: FileMetadata) -> Self {
        let now = SystemTime::now();
        Self {
            id,
            kind,
            path,
            size: 0,
            created_at: now,
            modified_at: now,
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_inode_starts_with_zero_size_and_timestamps() {
        let inode = Inode::new(
            InodeId(7),
            InodeKind::File,
            "/data.txt".to_string(),
            FileMetadata::default(),
        );

        assert_eq!(inode.id, InodeId(7));
        assert_eq!(inode.kind, InodeKind::File);
        assert_eq!(inode.path, "/data.txt");
        assert_eq!(inode.size, 0);
        assert!(inode.modified_at >= inode.created_at);
    }
}
