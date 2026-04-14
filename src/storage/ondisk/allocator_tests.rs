// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::ondisk::bitmap::Bitmap;
use crate::storage::ondisk::layout::{LayoutGeometry, LayoutParams};

fn mk_geom() -> LayoutGeometry {
    LayoutGeometry::plan(LayoutParams::with_defaults(4096)).unwrap()
}

fn make_allocator(strategy: AllocationStrategy) -> OndiskAllocator {
    let geom = mk_geom();
    let mut bbm = Bitmap::new(geom.total_blocks);
    // Reserve control blocks as the volume driver would.
    for b in [0u64, 1, geom.tertiary_superblock_block, geom.secondary_superblock_block] {
        bbm.set(b).unwrap();
    }
    for i in 0..geom.block_bitmap_blocks {
        bbm.set(geom.block_bitmap_start + i).unwrap();
    }
    for i in 0..geom.inode_bitmap_blocks {
        bbm.set(geom.inode_bitmap_start + i).unwrap();
    }
    for i in 0..geom.inode_table_blocks {
        bbm.set(geom.inode_table_start + i).unwrap();
    }
    for i in 0..geom.journal_blocks {
        bbm.set(geom.journal_start + i).unwrap();
    }
    let ibm = Bitmap::new(geom.inode_count);
    OndiskAllocator::new(&geom, bbm, ibm, strategy, 10)
}

#[test]
fn first_fit_returns_first_free_run() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    let geom = mk_geom();
    let ext = alloc.allocate_contiguous(4).unwrap();
    assert_eq!(ext.physical_block, geom.data_start);
    assert_eq!(ext.length_blocks, 4);
}

#[test]
fn sequential_allocations_advance_the_hint() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    let geom = mk_geom();
    let e1 = alloc.allocate_contiguous(3).unwrap();
    let e2 = alloc.allocate_contiguous(5).unwrap();
    assert_eq!(e1.physical_block, geom.data_start);
    assert_eq!(e2.physical_block, geom.data_start + 3);
}

#[test]
fn free_returns_blocks_to_pool() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    let ext = alloc.allocate_contiguous(8).unwrap();
    let free_before = alloc.free_data_blocks();
    alloc.free_extent(ext).unwrap();
    assert_eq!(alloc.free_data_blocks(), free_before + 8);
    let reused = alloc.allocate_contiguous(4).unwrap();
    assert_eq!(reused.physical_block, ext.physical_block);
}

#[test]
fn best_fit_chooses_smallest_suitable_gap() {
    let mut alloc = make_allocator(AllocationStrategy::BestFit);
    let geom = mk_geom();
    let a = alloc.allocate_contiguous(10).unwrap();
    let _b = alloc.allocate_contiguous(4).unwrap();
    let c = alloc.allocate_contiguous(20).unwrap();
    // Free a (size 10) and c (size 20), then request 11 — best fit = c.
    alloc.free_extent(a).unwrap();
    alloc.free_extent(c).unwrap();
    let picked = alloc.allocate_contiguous(11).unwrap();
    assert_eq!(picked.physical_block, c.physical_block);
    let _ = geom;
}

#[test]
fn allocate_any_spreads_across_fragments() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    // Create a fragmented pattern: allocate, free, allocate, free ...
    let a = alloc.allocate_contiguous(3).unwrap();
    let _b = alloc.allocate_contiguous(2).unwrap(); // hole placeholder
    let c = alloc.allocate_contiguous(4).unwrap();
    let _d = alloc.allocate_contiguous(1).unwrap();
    alloc.free_extent(a).unwrap();
    alloc.free_extent(c).unwrap();
    // Now the free map has two islands sized 3 and 4 (+ large tail).
    let exts = alloc.allocate_any(6).unwrap();
    let total: u64 = exts.iter().map(|e| u64::from(e.length_blocks)).sum();
    assert_eq!(total, 6);
    assert!(!exts.is_empty());
}

#[test]
fn inode_allocation_respects_reserved_floor() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    let i0 = alloc.allocate_inode().unwrap();
    let i1 = alloc.allocate_inode().unwrap();
    assert!(i0 >= 10);
    assert_eq!(i1, i0 + 1);
}

#[test]
fn reserve_inode_anchors_system_slots() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    alloc.reserve_inode(0).unwrap();
    alloc.reserve_inode(1).unwrap();
    let user = alloc.allocate_inode().unwrap();
    assert!(user >= 10);
}

#[test]
fn free_reserved_inode_is_rejected() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    alloc.reserve_inode(0).unwrap();
    assert!(alloc.free_inode(0).is_err());
}

#[test]
fn contiguous_request_beyond_capacity_fails() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    let huge = alloc.free_data_blocks() + 1;
    assert!(alloc.allocate_contiguous(huge).is_err());
}

#[test]
fn allocating_zero_blocks_is_rejected() {
    let mut alloc = make_allocator(AllocationStrategy::FirstFit);
    assert!(alloc.allocate_contiguous(0).is_err());
}
