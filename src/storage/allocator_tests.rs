// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn allocator_reuses_released_inodes() {
    let mut allocator = InodeAllocator::default();
    let first = allocator.allocate();
    let second = allocator.allocate();
    allocator.release(first);
    let recycled = allocator.allocate();

    assert_eq!(first, InodeId(1));
    assert_eq!(second, InodeId(2));
    assert_eq!(recycled, InodeId(1));
}

#[test]
fn allocator_can_reserve_specific_inode_ids() {
    let mut allocator = InodeAllocator::default();
    allocator.allocate_specific(InodeId(7));

    assert_eq!(allocator.allocate(), InodeId(8));

    allocator.release(InodeId(3));
    allocator.allocate_specific(InodeId(3));
    assert_eq!(allocator.allocate(), InodeId(9));
}
