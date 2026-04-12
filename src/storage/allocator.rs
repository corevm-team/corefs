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

    pub fn with_next_inode(next_inode: u64) -> Self {
        Self {
            next_inode,
            recycled: VecDeque::new(),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
