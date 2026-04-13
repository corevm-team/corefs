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
    /// POSIX `birth time` — set on creation, never updated afterwards.
    pub created_at: SystemTime,
    /// POSIX `mtime` — updated when file **content** changes
    /// (write, truncate).  Not updated by chown/chmod/rename.
    pub modified_at: SystemTime,
    /// POSIX `ctime` — updated on any inode change: content changes,
    /// metadata changes (chown/chmod), or rename.
    pub changed_at: SystemTime,
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
            changed_at: now,
            metadata,
        }
    }

    /// Marks the inode as having its content modified (updates both
    /// `modified_at` and `changed_at`).
    pub fn touch_modified(&mut self) {
        let now = SystemTime::now();
        self.modified_at = now;
        self.changed_at = now;
    }

    /// Marks the inode as having its status changed (metadata-only update).
    /// Updates `changed_at` but leaves `modified_at` untouched.
    pub fn touch_changed(&mut self) {
        self.changed_at = SystemTime::now();
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
