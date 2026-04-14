// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::domain::inode::InodeId;
use std::collections::VecDeque;

#[derive(Debug, Default)]
pub struct InodeAllocator {
    next_inode: u64,
    recycled: VecDeque<InodeId>,
}

impl InodeAllocator {
    pub fn allocate(&mut self) -> InodeId {
        if let Some(id) = self.recycled.pop_front() {
            id
        } else {
            self.next_inode += 1;
            InodeId(self.next_inode)
        }
    }

    pub fn release(&mut self, inode: InodeId) {
        self.recycled.push_back(inode);
    }

    pub fn allocate_specific(&mut self, inode: InodeId) {
        if let Some(index) = self
            .recycled
            .iter()
            .position(|candidate| *candidate == inode)
        {
            self.recycled.remove(index);
        }
        if inode.0 > self.next_inode {
            self.next_inode = inode.0;
        }
    }

    pub fn with_next_inode(next_inode: u64) -> Self {
        Self {
            next_inode,
            recycled: VecDeque::new(),
        }
    }
}

#[cfg(test)]
#[path = "allocator_tests.rs"]
mod tests;
