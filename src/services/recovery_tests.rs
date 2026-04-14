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

#[test]
fn forget_removes_tombstone_without_returning_inode() {
    let inode = Inode::new(
        InodeId(3),
        InodeKind::File,
        "/gone.txt".to_string(),
        FileMetadata::default(),
    );
    let mut recovery = RecoveryService::default();
    recovery.remember(inode);
    recovery.forget("/gone.txt");
    assert!(recovery.recoverable_paths().is_empty());
}
