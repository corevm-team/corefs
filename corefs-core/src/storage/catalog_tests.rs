// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::platform::Timestamp;
use alloc::string::ToString;
use alloc::vec;

#[test]
fn catalog_tracks_active_and_deleted_entries() {
    let inode = Inode::new_at(
        InodeId(1),
        InodeKind::File,
        "/data.txt".to_string(),
        FileMetadata::default(),
        Timestamp::EPOCH,
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
