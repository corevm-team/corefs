// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

fn sample() -> OnDiskInode {
    OnDiskInode {
        version: 1,
        kind: OnDiskKind::File,
        mode: 0o100644,
        uid: 1000,
        gid: 1000,
        link_count: 1,
        flags: 0,
        size_bytes: 12_345,
        blocks_allocated: 4,
        created_at: 1_700_000_000,
        modified_at: 1_700_000_100,
        changed_at: 1_700_000_200,
        accessed_at: 1_700_000_300,
        generation: 42,
        extents: vec![
            Extent {
                logical_block: 0,
                length_blocks: 2,
                physical_block: 1000,
            },
            Extent {
                logical_block: 2,
                length_blocks: 2,
                physical_block: 2000,
            },
        ],
    }
}

#[test]
fn roundtrip_preserves_fields() {
    let inode = sample();
    let enc = inode.encode().unwrap();
    assert_eq!(enc.len(), INODE_RECORD_SIZE);
    let dec = OnDiskInode::decode(&enc).unwrap();
    assert_eq!(dec, inode);
}

#[test]
fn unused_slot_roundtrips() {
    let u = OnDiskInode::unused();
    let enc = u.encode().unwrap();
    let dec = OnDiskInode::decode(&enc).unwrap();
    assert_eq!(dec, u);
    assert_eq!(dec.kind, OnDiskKind::Unused);
}

#[test]
fn checksum_detects_corruption() {
    let enc = sample().encode().unwrap();
    let mut corrupted = enc;
    corrupted[10] ^= 0x20;
    let err = OnDiskInode::decode(&corrupted).unwrap_err();
    assert!(format!("{err}").contains("checksum"));
}

#[test]
fn reject_overflowing_extent_list() {
    let mut inode = sample();
    inode.extents = (0..9)
        .map(|i| Extent {
            logical_block: i,
            length_blocks: 1,
            physical_block: 100 + i as u64,
        })
        .collect();
    assert!(inode.encode().is_err());
}

#[test]
fn unknown_kind_rejected() {
    let mut enc = sample().encode().unwrap();
    // Overwrite kind with an unknown value and recompute checksum.
    enc[2] = 99;
    enc[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4].fill(0);
    let csum = super::super::checksum::Crc32c::hash(&enc);
    enc[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&csum.to_le_bytes());
    let err = OnDiskInode::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("kind"));
}

#[test]
fn rejects_wrong_buffer_length() {
    assert!(OnDiskInode::decode(&[0u8; 128]).is_err());
}

#[test]
fn rejects_extent_count_field_overflow() {
    let mut enc = sample().encode().unwrap();
    enc[80..84].copy_from_slice(&(9u32).to_le_bytes());
    enc[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4].fill(0);
    let csum = super::super::checksum::Crc32c::hash(&enc);
    enc[INODE_CHECKSUM_OFFSET..INODE_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&csum.to_le_bytes());
    assert!(OnDiskInode::decode(&enc).is_err());
}
