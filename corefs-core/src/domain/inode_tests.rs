// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::platform::Timestamp;
use alloc::string::ToString;

#[test]
fn new_inode_starts_with_zero_size_and_timestamps() {
    let now = Timestamp::from_secs(1_700_000_000);
    let inode = Inode::new_at(
        InodeId(7),
        InodeKind::File,
        "/data.txt".to_string(),
        FileMetadata::default(),
        now,
    );

    assert_eq!(inode.id, InodeId(7));
    assert_eq!(inode.kind, InodeKind::File);
    assert_eq!(inode.path, "/data.txt");
    assert_eq!(inode.size, 0);
    assert_eq!(inode.created_at, now);
    assert_eq!(inode.modified_at, now);
    assert_eq!(inode.changed_at, now);
}

#[test]
fn touch_modified_at_updates_mtime_and_ctime_but_preserves_crtime() {
    let t0 = Timestamp::from_secs(1_700_000_000);
    let t1 = Timestamp::from_secs(1_700_000_100);
    let mut inode = Inode::new_at(
        InodeId(1),
        InodeKind::File,
        "/a".to_string(),
        FileMetadata::default(),
        t0,
    );
    inode.touch_modified_at(t1);
    assert_eq!(inode.created_at, t0, "crtime must not change");
    assert_eq!(inode.modified_at, t1);
    assert_eq!(inode.changed_at, t1);
}

#[test]
fn touch_changed_at_updates_only_ctime() {
    let t0 = Timestamp::from_secs(1_700_000_000);
    let t1 = Timestamp::from_secs(1_700_000_100);
    let mut inode = Inode::new_at(
        InodeId(1),
        InodeKind::File,
        "/a".to_string(),
        FileMetadata::default(),
        t0,
    );
    inode.touch_changed_at(t1);
    assert_eq!(inode.created_at, t0);
    assert_eq!(inode.modified_at, t0, "mtime must not change");
    assert_eq!(inode.changed_at, t1);
}

#[test]
fn new_inode_seeds_accessed_at_equal_to_modified_at() {
    let now = Timestamp::from_secs(1_700_000_000);
    let inode = Inode::new_at(
        InodeId(1),
        InodeKind::File,
        "/a".to_string(),
        FileMetadata::default(),
        now,
    );
    assert_eq!(inode.accessed_at, inode.modified_at);
    assert_eq!(inode.accessed_at, now);
}

#[test]
fn touch_accessed_at_updates_only_atime() {
    let t0 = Timestamp::from_secs(1_700_000_000);
    let t1 = Timestamp::from_secs(1_700_000_500);
    let mut inode = Inode::new_at(
        InodeId(1),
        InodeKind::File,
        "/a".to_string(),
        FileMetadata::default(),
        t0,
    );
    inode.touch_accessed_at(t1);
    assert_eq!(inode.accessed_at, t1);
    assert_eq!(inode.modified_at, t0, "mtime must not change on atime touch");
    assert_eq!(inode.changed_at, t0, "ctime must not change on atime touch");
    assert_eq!(inode.created_at, t0);
}
