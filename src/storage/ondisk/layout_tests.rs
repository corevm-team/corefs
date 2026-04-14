// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn plan_rejects_too_small_volumes() {
    let err = LayoutGeometry::plan(LayoutParams::with_defaults(32)).unwrap_err();
    assert!(format!("{err}").contains("too small"));
}

#[test]
fn plan_rejects_zero_inode_count() {
    let params = LayoutParams {
        total_blocks: 1024,
        inode_count: 0,
        journal_blocks: 8,
    };
    assert!(LayoutGeometry::plan(params).is_err());
}

#[test]
fn plan_is_self_consistent_for_default_sizes() {
    let geom = LayoutGeometry::plan(LayoutParams::with_defaults(4096)).unwrap();
    assert_eq!(geom.total_blocks, 4096);
    assert_eq!(geom.block_bitmap_start, 2);
    assert!(geom.inode_table_start > geom.inode_bitmap_start);
    assert!(geom.journal_start >= geom.inode_table_start + geom.inode_table_blocks);
    assert!(geom.data_start >= geom.journal_start + geom.journal_blocks);
    assert_eq!(geom.data_start + geom.data_blocks, geom.total_blocks);
    assert_eq!(geom.secondary_superblock_block, 4095);
    assert_eq!(geom.tertiary_superblock_block, 2048);
}

#[test]
fn plan_rejects_if_data_area_vanishes() {
    let params = LayoutParams {
        total_blocks: 128,
        inode_count: 4096,
        journal_blocks: 64,
    };
    assert!(LayoutGeometry::plan(params).is_err());
}

#[test]
fn inode_record_location_maps_into_table() {
    let geom = LayoutGeometry::plan(LayoutParams::with_defaults(4096)).unwrap();
    let (block, offset) = geom.inode_record_location(0).unwrap();
    assert_eq!(block, geom.inode_table_start);
    assert_eq!(offset, 0);

    let (block, offset) = geom
        .inode_record_location(INODES_PER_BLOCK - 1)
        .unwrap();
    assert_eq!(block, geom.inode_table_start);
    assert_eq!(offset, (INODES_PER_BLOCK - 1) * INODE_SIZE);

    let (block, offset) = geom.inode_record_location(INODES_PER_BLOCK).unwrap();
    assert_eq!(block, geom.inode_table_start + 1);
    assert_eq!(offset, 0);
}

#[test]
fn inode_record_location_rejects_out_of_range() {
    let geom = LayoutGeometry::plan(LayoutParams::with_defaults(4096)).unwrap();
    assert!(geom.inode_record_location(geom.inode_count).is_err());
}

#[test]
fn bitmap_covers_entire_volume() {
    let geom = LayoutGeometry::plan(LayoutParams::with_defaults(100_000)).unwrap();
    assert!(geom.block_bitmap_blocks * BITS_PER_BITMAP_BLOCK >= geom.total_blocks);
}

#[test]
fn is_data_block_boundaries() {
    let geom = LayoutGeometry::plan(LayoutParams::with_defaults(4096)).unwrap();
    assert!(!geom.is_data_block(geom.data_start - 1));
    assert!(geom.is_data_block(geom.data_start));
    assert!(geom.is_data_block(geom.total_blocks - 1));
    assert!(!geom.is_data_block(geom.total_blocks));
}
