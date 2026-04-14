// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

fn descriptor(start: u64, blocks: u64, inode_start: u64, inode_count: u64) -> BlockGroupDescriptor {
    BlockGroupDescriptor {
        data_start: start,
        data_blocks: blocks,
        bitmap_block: start - 1,
        inode_range_start: inode_start,
        inode_range_count: inode_count,
        free_blocks: blocks as u32,
        bitmap_crc: 0,
    }
}

fn three_group_allocator() -> MultiGroupAllocator {
    let groups = vec![
        descriptor(100, 64, 0, 32),
        descriptor(200, 64, 32, 32),
        descriptor(300, 64, 64, 32),
    ];
    let table = BlockGroupTable::new(groups).unwrap();
    let bitmaps = (0..3).map(|_| Bitmap::new(64)).collect();
    let inode_bitmap = Bitmap::new(128);
    MultiGroupAllocator::new(table, bitmaps, inode_bitmap, 0).unwrap()
}

#[test]
fn rejects_mismatched_bitmap_count() {
    let table = BlockGroupTable::new(vec![descriptor(100, 64, 0, 32)]).unwrap();
    let bitmaps = vec![]; // empty — mismatch
    let inode_bitmap = Bitmap::new(64);
    assert!(MultiGroupAllocator::new(table, bitmaps, inode_bitmap, 0).is_err());
}

#[test]
fn allocate_near_lands_in_home_group() {
    let mut alloc = three_group_allocator();
    // Inode slot 5 → group 0 (inode_range_start=0, count=32)
    let e0 = alloc.allocate_near(4, 5).unwrap();
    assert!(e0.physical_block >= 100 && e0.physical_block < 164);
    // Inode slot 50 → group 1
    let e1 = alloc.allocate_near(4, 50).unwrap();
    assert!(e1.physical_block >= 200 && e1.physical_block < 264);
    // Inode slot 80 → group 2
    let e2 = alloc.allocate_near(4, 80).unwrap();
    assert!(e2.physical_block >= 300 && e2.physical_block < 364);
}

#[test]
fn allocate_near_falls_back_to_other_groups_when_home_full() {
    let mut alloc = three_group_allocator();
    // Fill group 0 entirely.
    for _ in 0..16 {
        alloc.allocate_near(4, 0).unwrap();
    }
    // Next allocation requesting group 0 should spill to group 1 or 2.
    let extra = alloc.allocate_near(4, 0).unwrap();
    assert!(extra.physical_block >= 200, "expected spill, got {extra:?}");
}

#[test]
fn free_extent_returns_blocks_to_correct_group() {
    let mut alloc = three_group_allocator();
    let e = alloc.allocate_near(4, 50).unwrap();
    let free_before = alloc.free_data_blocks_in(1).unwrap();
    alloc.free_extent(e).unwrap();
    let free_after = alloc.free_data_blocks_in(1).unwrap();
    assert_eq!(free_after, free_before + 4);
}

#[test]
fn free_extent_rejects_block_outside_any_group() {
    let mut alloc = three_group_allocator();
    let bad = Extent {
        logical_block: 0,
        length_blocks: 1,
        physical_block: 9999,
    };
    assert!(alloc.free_extent(bad).is_err());
}

#[test]
fn allocate_inode_walks_global_bitmap() {
    let mut alloc = three_group_allocator();
    let i0 = alloc.allocate_inode().unwrap();
    let i1 = alloc.allocate_inode().unwrap();
    assert_eq!(i1, i0 + 1);
}

#[test]
fn refresh_descriptors_updates_free_count_and_crc() {
    let mut alloc = three_group_allocator();
    alloc.allocate_near(8, 5).unwrap();
    alloc.refresh_descriptors();
    let table = alloc.table();
    assert_eq!(table.groups[0].free_blocks, 56);
    assert_ne!(table.groups[0].bitmap_crc, 0);
    // Untouched group keeps a fully-free bitmap.
    assert_eq!(table.groups[1].free_blocks, 64);
}

#[test]
fn total_free_data_blocks_aggregates_across_groups() {
    let mut alloc = three_group_allocator();
    alloc.allocate_near(8, 5).unwrap();
    alloc.allocate_near(16, 50).unwrap();
    assert_eq!(alloc.total_free_data_blocks(), 64 * 3 - 8 - 16);
}

#[test]
fn allocate_zero_blocks_rejected() {
    let mut alloc = three_group_allocator();
    assert!(alloc.allocate_near(0, 0).is_err());
}

#[test]
fn into_parts_returns_persistable_components() {
    let alloc = three_group_allocator();
    let (table, bitmaps, ibm) = alloc.into_parts();
    assert_eq!(bitmaps.len(), 3);
    assert_eq!(table.groups.len(), 3);
    assert_eq!(ibm.capacity(), 128);
}
