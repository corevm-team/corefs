use crate::domain::inode::InodeId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRecord {
    pub inode: InodeId,
    pub bytes: Vec<u8>,
    pub checksum: u64,
}

#[derive(Debug, Default)]
pub struct BlockStore {
    blocks: BTreeMap<InodeId, BlockRecord>,
}

impl BlockStore {
    pub fn write(&mut self, inode: InodeId, bytes: Vec<u8>) -> usize {
        let checksum = checksum(&bytes);
        let size = bytes.len();
        self.blocks.insert(
            inode,
            BlockRecord {
                inode,
                bytes,
                checksum,
            },
        );
        size
    }

    pub fn read(&self, inode: InodeId) -> Option<&BlockRecord> {
        self.blocks.get(&inode)
    }

    pub fn contains(&self, inode: InodeId) -> bool {
        self.blocks.contains_key(&inode)
    }

    pub fn remove(&mut self, inode: InodeId) -> Option<BlockRecord> {
        self.blocks.remove(&inode)
    }

    pub fn verify(&self, inode: InodeId) -> bool {
        self.blocks
            .get(&inode)
            .map(|record| record.checksum == checksum(&record.bytes))
            .unwrap_or(false)
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, byte| {
        acc.wrapping_mul(16777619).wrapping_add(u64::from(*byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_verify_and_remove_manage_blocks() {
        let mut store = BlockStore::default();
        let inode = InodeId(11);

        assert_eq!(store.write(inode, b"hello".to_vec()), 5);
        assert!(store.contains(inode));
        assert_eq!(
            store.read(inode).map(|record| record.bytes.clone()),
            Some(b"hello".to_vec())
        );
        assert!(store.verify(inode));
        assert!(store.remove(inode).is_some());
        assert!(!store.contains(inode));
        assert!(!store.verify(inode));
    }

    #[test]
    fn checksum_changes_with_payload() {
        assert_ne!(checksum(b"a"), checksum(b"b"));
        assert_eq!(checksum(b"same"), checksum(b"same"));
    }
}
