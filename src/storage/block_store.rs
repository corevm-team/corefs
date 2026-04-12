use crate::domain::inode::InodeId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub inode: InodeId,
    pub bytes: Vec<u8>,
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobRecord {
    bytes: Vec<u8>,
    checksum: u64,
    ref_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockEntry {
    inode: InodeId,
    blob_checksum: u64,
    size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupeStats {
    pub logical_blocks: usize,
    pub unique_blobs: usize,
    pub deduplicated_blocks: usize,
}

#[derive(Debug, Default)]
pub struct BlockStore {
    blocks: BTreeMap<InodeId, BlockEntry>,
    blobs: BTreeMap<u64, BlobRecord>,
}

impl BlockStore {
    pub fn write(&mut self, inode: InodeId, bytes: Vec<u8>) -> usize {
        let checksum = checksum(&bytes);
        let size = bytes.len();
        self.release_inode(inode);

        let blob = self.blobs.entry(checksum).or_insert_with(|| BlobRecord {
            bytes,
            checksum,
            ref_count: 0,
        });
        blob.ref_count += 1;
        self.blocks.insert(
            inode,
            BlockEntry {
                inode,
                blob_checksum: checksum,
                size,
            },
        );
        size
    }

    pub fn read(&self, inode: InodeId) -> Option<BlockRecord> {
        let entry = self.blocks.get(&inode)?;
        let blob = self.blobs.get(&entry.blob_checksum)?;
        Some(BlockRecord {
            inode: entry.inode,
            bytes: blob.bytes.clone(),
            checksum: blob.checksum,
        })
    }

    pub fn contains(&self, inode: InodeId) -> bool {
        self.blocks.contains_key(&inode)
    }

    pub fn remove(&mut self, inode: InodeId) -> Option<BlockRecord> {
        let record = self.read(inode)?;
        self.release_inode(inode);
        Some(record)
    }

    pub fn verify(&self, inode: InodeId) -> bool {
        self.read(inode)
            .map(|record| record.checksum == checksum(&record.bytes))
            .unwrap_or(false)
    }

    pub fn records(&self) -> Vec<BlockRecord> {
        self.blocks
            .keys()
            .filter_map(|inode| self.read(*inode))
            .collect()
    }

    pub fn from_records(records: Vec<BlockRecord>) -> Self {
        let mut store = Self::default();
        for record in records {
            store.write(record.inode, record.bytes);
        }
        store
    }

    pub fn dedupe_stats(&self) -> DedupeStats {
        DedupeStats {
            logical_blocks: self.blocks.len(),
            unique_blobs: self.blobs.len(),
            deduplicated_blocks: self.blocks.len().saturating_sub(self.blobs.len()),
        }
    }

    fn release_inode(&mut self, inode: InodeId) {
        let Some(entry) = self.blocks.remove(&inode) else {
            return;
        };

        let remove_blob = if let Some(blob) = self.blobs.get_mut(&entry.blob_checksum) {
            blob.ref_count = blob.ref_count.saturating_sub(1);
            blob.ref_count == 0
        } else {
            false
        };

        if remove_blob {
            self.blobs.remove(&entry.blob_checksum);
        }
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
    fn dedupe_reuses_identical_payloads() {
        let mut store = BlockStore::default();
        store.write(InodeId(1), b"same".to_vec());
        store.write(InodeId(2), b"same".to_vec());
        store.write(InodeId(3), b"other".to_vec());

        let stats = store.dedupe_stats();
        assert_eq!(stats.logical_blocks, 3);
        assert_eq!(stats.unique_blobs, 2);
        assert_eq!(stats.deduplicated_blocks, 1);

        store.remove(InodeId(1));
        let stats = store.dedupe_stats();
        assert_eq!(stats.logical_blocks, 2);
        assert_eq!(stats.unique_blobs, 2);

        store.remove(InodeId(2));
        let stats = store.dedupe_stats();
        assert_eq!(stats.logical_blocks, 1);
        assert_eq!(stats.unique_blobs, 1);
    }

    #[test]
    fn checksum_changes_with_payload() {
        assert_ne!(checksum(b"a"), checksum(b"b"));
        assert_eq!(checksum(b"same"), checksum(b"same"));
    }
}
