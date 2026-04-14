// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

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
