// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::CoreFsConfig;

#[test]
fn wal_operations_apply_to_service() {
    let mut service = CoreFsService::format(CoreFsConfig::default());
    let wal = VolumeWal {
        transaction_id: 1,
        label: "rw-writeback".to_string(),
        created_at: SystemTime::now(),
        operations: vec![
            WalOperation::CreateDirectory {
                path: "/data".to_string(),
                inode: InodeId(1),
            },
            WalOperation::CreateFile {
                path: "/data/hello.txt".to_string(),
                inode: InodeId(2),
            },
            WalOperation::PatchExtent {
                inode: InodeId(2),
                device_block: 0,
                block_offset: 0,
                inode_offset: 0,
                bytes: b"hello".to_vec(),
                final_len: 5,
            },
            WalOperation::RenamePath {
                from: "/data/hello.txt".to_string(),
                to: "/data/world.txt".to_string(),
            },
        ],
    };

    for operation in &wal.operations {
        apply_operation(&mut service, operation).expect("operation should apply");
    }

    assert_eq!(
        service
            .read_file("/data/world.txt")
            .expect("file should exist"),
        b"hello".to_vec()
    );
}

#[test]
fn wal_patch_and_truncate_apply_as_deltas() {
    let mut service = CoreFsService::format(CoreFsConfig::default());
    service
        .create_file("/delta.txt", b"abcdefgh", &[])
        .expect("file");
    let inode = service.inode_for_path("/delta.txt").expect("inode");

    apply_operation(
        &mut service,
        &WalOperation::PatchExtent {
            inode,
            device_block: 0,
            block_offset: 2,
            inode_offset: 2,
            bytes: b"XYZ".to_vec(),
            final_len: 8,
        },
    )
    .expect("patch should apply");
    apply_operation(
        &mut service,
        &WalOperation::TruncateInode { inode, size: 6 },
    )
    .expect("truncate should apply");

    assert_eq!(
        service.read_file("/delta.txt").expect("file should exist"),
        b"abXYZf".to_vec()
    );
}

#[test]
fn wal_patch_extent_uses_device_block_mapping_when_available() {
    let mut service = CoreFsService::format(CoreFsConfig {
        block_size: 4,
        ..CoreFsConfig::default()
    });
    service
        .create_file("/extent.txt", b"abcdefgh", &[])
        .expect("file");
    let inode = service.inode_for_path("/extent.txt").expect("inode");

    apply_operation(
        &mut service,
        &WalOperation::PatchExtent {
            inode,
            device_block: 1,
            block_offset: 1,
            inode_offset: 5,
            bytes: b"ZZ".to_vec(),
            final_len: 8,
        },
    )
    .expect("patch should apply");

    assert_eq!(
        service.read_file("/extent.txt").expect("file should exist"),
        b"abcdeZZh".to_vec()
    );
}
