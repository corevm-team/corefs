// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::domain::inode::InodeKind;
use crate::domain::metadata::FileMetadata;
use crate::platform::Timestamp;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Per-path metadata captured at snapshot creation time.
///
/// This preserves the inode attributes (kind, size, timestamps, mode/uid/gid,
/// ACLs, tags, xattrs) as they were when the snapshot was taken, so historical
/// metadata can be inspected even after subsequent chmod/chown/write operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInode {
    pub kind: InodeKind,
    pub size: usize,
    pub created_at: Timestamp,
    pub modified_at: Timestamp,
    pub changed_at: Timestamp,
    pub metadata: FileMetadata,
    /// For symlinks: the target path captured at snapshot time.
    /// `None` for files and directories.
    pub symlink_target: Option<String>,
}

/// A point-in-time consistent snapshot of the filesystem.
///
/// Beyond recording which paths existed at snapshot time, the snapshot retains
/// the **uncompressed byte content** of every regular file in `file_data` and
/// the full per-path metadata in `inodes`.  This makes snapshot restoration
/// and historical metadata inspection self-contained and independent of
/// whether the active blocks have since been overwritten or compacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: u64,
    pub name: String,
    pub scope_root: String,
    pub created_at: Timestamp,
    /// All paths (files, directories, symlinks) that existed at snapshot time.
    pub paths: Vec<String>,
    /// Captured uncompressed content for every regular file at snapshot time,
    /// keyed by absolute path.  Directories and symlinks are not included.
    pub file_data: BTreeMap<String, Vec<u8>>,
    /// Per-path metadata snapshot (kind, size, timestamps, mode/uid/gid,
    /// ACLs, tags, xattrs, symlink targets) at snapshot creation time.
    pub inodes: BTreeMap<String, SnapshotInode>,
}
