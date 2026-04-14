// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn g(
    start: u64,
    len: u64,
    bitmap: u64,
    inode_start: u64,
    inode_count: u64,
) -> BlockGroupDescriptor {
    BlockGroupDescriptor {
        data_start: start,
        data_blocks: len,
        bitmap_block: bitmap,
        inode_range_start: inode_start,
        inode_range_count: inode_count,
        free_blocks: len as u32,
        bitmap_crc: 0,
    }
}

#[test]
fn empty_table_roundtrip() {
    let t = BlockGroupTable::new(vec![]).unwrap();
    let enc = t.encode().unwrap();
    let dec = BlockGroupTable::decode(&enc).unwrap();
    assert_eq!(dec.groups.len(), 0);
}

#[test]
fn single_group_roundtrip() {
    let t = BlockGroupTable::new(vec![g(100, 1000, 99, 0, 256)]).unwrap();
    let enc = t.encode().unwrap();
    let dec = BlockGroupTable::decode(&enc).unwrap();
    assert_eq!(dec, t);
}

#[test]
fn multi_group_roundtrip_preserves_order() {
    let t = BlockGroupTable::new(vec![
        g(100, 1000, 99, 0, 256),
        g(1100, 2000, 1099, 256, 256),
        g(3100, 500, 3099, 512, 256),
    ])
    .unwrap();
    let enc = t.encode().unwrap();
    let dec = BlockGroupTable::decode(&enc).unwrap();
    assert_eq!(dec, t);
}

#[test]
fn overlapping_groups_rejected() {
    let result = BlockGroupTable::new(vec![
        g(100, 1000, 99, 0, 256),
        g(800, 500, 799, 256, 256), // overlaps with first
    ]);
    assert!(result.is_err());
}

#[test]
fn too_many_groups_rejected() {
    let groups: Vec<_> = (0..MAX_GROUPS_PER_TABLE + 1)
        .map(|i| {
            g(
                (i as u64 + 1) * 2000,
                999,
                (i as u64 + 1) * 2000 - 1,
                i as u64 * 256,
                256,
            )
        })
        .collect();
    assert!(BlockGroupTable::new(groups).is_err());
}

#[test]
fn checksum_detects_corruption() {
    let t = BlockGroupTable::new(vec![g(100, 1000, 99, 0, 256)]).unwrap();
    let mut enc = t.encode().unwrap();
    enc[20] ^= 0x40;
    assert!(BlockGroupTable::decode(&enc).is_err());
}

#[test]
fn bad_magic_rejected() {
    let t = BlockGroupTable::new(vec![]).unwrap();
    let mut enc = t.encode().unwrap();
    enc[0..4].copy_from_slice(&0u32.to_le_bytes());
    let mut zeroed = enc.clone();
    zeroed[CRC_OFFSET..CRC_OFFSET + 4].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(&zeroed);
    enc[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&csum.to_le_bytes());
    let err = BlockGroupTable::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("magic"));
}

#[test]
fn group_for_inode_locates_correct_group() {
    let t = BlockGroupTable::new(vec![
        g(100, 1000, 99, 0, 256),
        g(1100, 1000, 1099, 256, 256),
        g(2100, 1000, 2099, 512, 256),
    ])
    .unwrap();
    assert_eq!(t.group_for_inode(0), Some(0));
    assert_eq!(t.group_for_inode(255), Some(0));
    assert_eq!(t.group_for_inode(256), Some(1));
    assert_eq!(t.group_for_inode(700), Some(2));
    assert_eq!(t.group_for_inode(800), None);
}

#[test]
fn group_for_block_locates_correct_group() {
    let t = BlockGroupTable::new(vec![
        g(100, 1000, 99, 0, 256),
        g(1100, 1000, 1099, 256, 256),
    ])
    .unwrap();
    assert_eq!(t.group_for_block(100), Some(0));
    assert_eq!(t.group_for_block(1099), Some(0));
    assert_eq!(t.group_for_block(1100), Some(1));
    assert_eq!(t.group_for_block(50), None);
    assert_eq!(t.group_for_block(99_999), None);
}

#[test]
fn capacity_constants_match_block_size() {
    // Header (16) + N descriptors (48) + CRC (4) <= BLOCK_SIZE (4096).
    let max_bytes = 16 + MAX_GROUPS_PER_TABLE * DESCRIPTOR_BYTES + 4;
    assert!(max_bytes <= crate::storage::ondisk::layout::BLOCK_SIZE as usize);
    // And one more group would overflow.
    let too_many = 16 + (MAX_GROUPS_PER_TABLE + 1) * DESCRIPTOR_BYTES + 4;
    assert!(too_many > crate::storage::ondisk::layout::BLOCK_SIZE as usize);
}
