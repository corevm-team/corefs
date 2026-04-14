// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::StorageTier;
use crate::domain::metadata::{ContentClass, FileMetadata};
use std::time::SystemTime;

fn inode(id: u64, kind: InodeKind, path: &str, size: usize) -> Inode {
    let now = SystemTime::now();
    Inode {
        id: crate::domain::inode::InodeId(id),
        kind,
        path: path.to_string(),
        size,
        created_at: now,
        modified_at: now,
        changed_at: now,
        metadata: FileMetadata::default(),
    }
}

#[test]
fn quota_report_counts_files_and_bytes() {
    let service = QuotaService;
    let report = service.report(
        &QuotaPolicy {
            max_files: Some(4),
            max_bytes: Some(1024),
        },
        &[
            inode(1, InodeKind::Directory, "/data", 0),
            inode(2, InodeKind::File, "/data/a", 100),
            inode(3, InodeKind::Symlink, "/data/b", 8),
        ],
    );

    assert_eq!(report.used_files, 2);
    assert_eq!(report.used_bytes, 108);
}

#[test]
fn quota_enforcement_rejects_excess() {
    let service = QuotaService;
    let result = service.enforce_delta(
        &QuotaPolicy {
            max_files: Some(1),
            max_bytes: Some(16),
        },
        &[inode(1, InodeKind::File, "/a", 10)],
        1,
        8,
    );

    assert!(matches!(result, Err(CoreFsError::PolicyViolation(_))));
}
