use crate::domain::inode::InodeId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub inode: InodeId,
    pub bytes: Vec<u8>,
    pub checksum: u64,
    pub device_block: u64,
    pub allocated_blocks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeExtentRecord {
    pub device_block: u64,
    pub allocated_blocks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationStrategy {
    BestFit,
    FirstFit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocatorPolicy {
    pub strategy: AllocationStrategy,
    pub split_threshold_blocks: u64,
    pub coalesce_on_release: bool,
    pub tail_trim_enabled: bool,
    pub background_compaction_enabled: bool,
    pub fragmentation_threshold_percent: u8,
}

impl Default for AllocatorPolicy {
    fn default() -> Self {
        Self {
            strategy: AllocationStrategy::BestFit,
            split_threshold_blocks: 1,
            coalesce_on_release: true,
            tail_trim_enabled: true,
            background_compaction_enabled: false,
            fragmentation_threshold_percent: 25,
        }
    }
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
    device_block: u64,
    allocated_blocks: u64,
}

type FreeExtent = FreeExtentRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupeStats {
    pub logical_blocks: usize,
    pub unique_blobs: usize,
    pub deduplicated_blocks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefragmentationReport {
    pub moved_entries: usize,
    pub reclaimed_gaps: usize,
    pub final_device_blocks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentationReport {
    pub free_extents: usize,
    pub total_free_blocks: u64,
    pub largest_free_extent: u64,
    pub fragmented_free_blocks: u64,
    pub fragmentation_percent: u8,
    pub needs_compaction: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationReport {
    pub before: FragmentationReport,
    pub after: FragmentationReport,
    pub defragmentation: Option<DefragmentationReport>,
}

#[derive(Debug)]
pub struct BlockStore {
    block_size: usize,
    next_device_block: u64,
    policy: AllocatorPolicy,
    free_extents: Vec<FreeExtent>,
    blocks: BTreeMap<InodeId, BlockEntry>,
    blobs: BTreeMap<u64, BlobRecord>,
}

impl BlockStore {
    pub fn with_block_size(block_size: usize) -> Self {
        Self::with_block_size_and_policy(block_size, AllocatorPolicy::default())
    }

    pub fn with_block_size_and_policy(block_size: usize, policy: AllocatorPolicy) -> Self {
        Self {
            block_size: block_size.max(1),
            next_device_block: 0,
            policy,
            free_extents: Vec::new(),
            blocks: BTreeMap::new(),
            blobs: BTreeMap::new(),
        }
    }

    pub fn write(&mut self, inode: InodeId, bytes: Vec<u8>) -> usize {
        let checksum = checksum(&bytes);
        let size = bytes.len();
        let required_blocks = required_blocks(size, self.block_size);
        let existing_allocation = self
            .blocks
            .get(&inode)
            .map(|entry| (entry.device_block, entry.allocated_blocks));
        self.release_inode(inode);

        let (device_block, allocated_blocks) = match existing_allocation {
            Some((device_block, allocated_blocks)) if allocated_blocks >= required_blocks => {
                if allocated_blocks > required_blocks {
                    self.insert_free_extent(FreeExtent {
                        device_block: device_block.saturating_add(required_blocks),
                        allocated_blocks: allocated_blocks - required_blocks,
                    });
                }
                (device_block, required_blocks)
            }
            Some((device_block, allocated_blocks)) => {
                self.insert_free_extent(FreeExtent {
                    device_block,
                    allocated_blocks,
                });
                self.allocate_extent(required_blocks.max(1))
            }
            _ => self.allocate_extent(required_blocks.max(1)),
        };

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
                device_block,
                allocated_blocks,
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
            device_block: entry.device_block,
            allocated_blocks: entry.allocated_blocks,
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
        store.ingest_records(records);
        store
    }

    pub fn from_records_with_block_size(records: Vec<BlockRecord>, block_size: usize) -> Self {
        let mut store = Self::with_block_size(block_size);
        store.ingest_records(records);
        store
    }

    pub fn from_records_with_allocator(
        records: Vec<BlockRecord>,
        block_size: usize,
        policy: AllocatorPolicy,
        free_extents: Vec<FreeExtentRecord>,
    ) -> Self {
        let mut store = Self::with_block_size_and_policy(block_size, policy);
        store.ingest_records(records);
        if store.adopt_free_extents(free_extents).is_err() {
            store.rebuild_free_extents();
        }
        store
    }

    pub fn allocator_policy(&self) -> &AllocatorPolicy {
        &self.policy
    }

    pub fn free_extents(&self) -> Vec<FreeExtentRecord> {
        self.free_extents.clone()
    }

    pub fn set_allocator_policy(&mut self, policy: AllocatorPolicy) {
        self.policy = policy;
        if self.policy.coalesce_on_release {
            self.normalize_free_extents();
        } else {
            self.free_extents.sort_by_key(|extent| extent.device_block);
            if self.policy.tail_trim_enabled {
                self.trim_free_tail();
            }
        }
    }

    fn ingest_records(&mut self, records: Vec<BlockRecord>) {
        for record in records {
            let next = record
                .device_block
                .saturating_add(record.allocated_blocks.max(1));
            self.next_device_block = self.next_device_block.max(next);

            let blob = self
                .blobs
                .entry(record.checksum)
                .or_insert_with(|| BlobRecord {
                    bytes: record.bytes.clone(),
                    checksum: record.checksum,
                    ref_count: 0,
                });
            blob.ref_count += 1;
            self.blocks.insert(
                record.inode,
                BlockEntry {
                    inode: record.inode,
                    blob_checksum: record.checksum,
                    size: record.bytes.len(),
                    device_block: record.device_block,
                    allocated_blocks: record.allocated_blocks.max(1),
                },
            );
        }
        self.rebuild_free_extents();
    }

    pub fn dedupe_stats(&self) -> DedupeStats {
        DedupeStats {
            logical_blocks: self.blocks.len(),
            unique_blobs: self.blobs.len(),
            deduplicated_blocks: self.blocks.len().saturating_sub(self.blobs.len()),
        }
    }

    pub fn fragmentation_report(&self) -> FragmentationReport {
        let total_free_blocks = self
            .free_extents
            .iter()
            .map(|extent| extent.allocated_blocks)
            .sum::<u64>();
        let largest_free_extent = self
            .free_extents
            .iter()
            .map(|extent| extent.allocated_blocks)
            .max()
            .unwrap_or(0);
        let fragmented_free_blocks = total_free_blocks.saturating_sub(largest_free_extent);
        let fragmentation_percent = if total_free_blocks == 0 {
            0
        } else {
            ((fragmented_free_blocks.saturating_mul(100)) / total_free_blocks).min(100) as u8
        };
        let threshold = self.policy.fragmentation_threshold_percent.min(100);

        FragmentationReport {
            free_extents: self.free_extents.len(),
            total_free_blocks,
            largest_free_extent,
            fragmented_free_blocks,
            fragmentation_percent,
            needs_compaction: self.policy.background_compaction_enabled
                && fragmentation_percent >= threshold
                && self.free_extents.len() > 1,
        }
    }

    pub fn defragment(&mut self) -> DefragmentationReport {
        let mut entries: Vec<_> = self
            .blocks
            .iter()
            .map(|(inode, entry)| (*inode, entry.clone()))
            .collect();
        entries.sort_by_key(|(_, entry)| entry.device_block);

        let original_free = self.free_extents.len();
        let mut cursor = 0u64;
        let mut moved_entries = 0usize;

        for (_, entry) in &mut entries {
            if entry.device_block != cursor {
                entry.device_block = cursor;
                moved_entries += 1;
            }
            cursor = cursor.saturating_add(entry.allocated_blocks);
        }

        self.blocks = entries
            .into_iter()
            .map(|(inode, entry)| (inode, entry))
            .collect();
        self.free_extents.clear();
        self.next_device_block = cursor;

        DefragmentationReport {
            moved_entries,
            reclaimed_gaps: original_free,
            final_device_blocks: cursor,
        }
    }

    pub fn optimize(&mut self) -> OptimizationReport {
        let before = self.fragmentation_report();
        let threshold = self.policy.fragmentation_threshold_percent.min(100);
        let should_compact = before.fragmentation_percent >= threshold && before.free_extents > 1;
        let defragmentation = if should_compact {
            Some(self.defragment())
        } else {
            None
        };
        let after = self.fragmentation_report();

        OptimizationReport {
            before,
            after,
            defragmentation,
        }
    }

    fn allocate_extent(&mut self, required_blocks: u64) -> (u64, u64) {
        let index = match self.policy.strategy {
            AllocationStrategy::BestFit => self
                .free_extents
                .iter()
                .enumerate()
                .filter(|(_, extent)| extent.allocated_blocks >= required_blocks)
                .min_by_key(|(_, extent)| extent.allocated_blocks)
                .map(|(index, _)| index),
            AllocationStrategy::FirstFit => self
                .free_extents
                .iter()
                .enumerate()
                .find(|(_, extent)| extent.allocated_blocks >= required_blocks)
                .map(|(index, _)| index),
        };

        if let Some(index) = index {
            let extent = self.free_extents.remove(index);
            let remainder = extent.allocated_blocks.saturating_sub(required_blocks);
            if remainder >= self.policy.split_threshold_blocks.max(1) {
                self.insert_free_extent(FreeExtent {
                    device_block: extent.device_block.saturating_add(required_blocks),
                    allocated_blocks: remainder,
                });
                return (extent.device_block, required_blocks);
            }
            return (extent.device_block, extent.allocated_blocks);
        }

        let device_block = self.next_device_block;
        self.next_device_block = self.next_device_block.saturating_add(required_blocks);
        (device_block, required_blocks)
    }

    fn insert_free_extent(&mut self, extent: FreeExtent) {
        if extent.allocated_blocks == 0 {
            return;
        }
        self.free_extents.push(extent);
        if self.policy.coalesce_on_release {
            self.normalize_free_extents();
        } else {
            self.free_extents
                .sort_by_key(|candidate| candidate.device_block);
            if self.policy.tail_trim_enabled {
                self.trim_free_tail();
            }
        }
    }

    fn normalize_free_extents(&mut self) {
        self.free_extents.sort_by_key(|extent| extent.device_block);
        let mut merged: Vec<FreeExtent> = Vec::with_capacity(self.free_extents.len());

        for extent in self.free_extents.drain(..) {
            if let Some(last) = merged.last_mut() {
                let last_end = last.device_block.saturating_add(last.allocated_blocks);
                if last_end >= extent.device_block {
                    let extent_end = extent.device_block.saturating_add(extent.allocated_blocks);
                    last.allocated_blocks = extent_end.saturating_sub(last.device_block);
                    continue;
                }
            }
            merged.push(extent);
        }

        self.free_extents = merged;
        if self.policy.tail_trim_enabled {
            self.trim_free_tail();
        }
    }

    fn trim_free_tail(&mut self) {
        while let Some(last) = self.free_extents.last().copied() {
            let last_end = last.device_block.saturating_add(last.allocated_blocks);
            if last_end != self.next_device_block {
                break;
            }
            self.next_device_block = last.device_block;
            self.free_extents.pop();
        }
    }

    fn rebuild_free_extents(&mut self) {
        let mut occupied: Vec<(u64, u64)> = self
            .blocks
            .values()
            .map(|entry| {
                (
                    entry.device_block,
                    entry.device_block.saturating_add(entry.allocated_blocks),
                )
            })
            .collect();
        occupied.sort_by_key(|(start, _)| *start);

        self.free_extents.clear();
        let mut cursor = 0u64;
        for (start, end) in occupied {
            if start > cursor {
                self.free_extents.push(FreeExtent {
                    device_block: cursor,
                    allocated_blocks: start - cursor,
                });
            }
            cursor = cursor.max(end);
        }
        self.next_device_block = self.next_device_block.max(cursor);
        if self.policy.tail_trim_enabled {
            self.trim_free_tail();
        }
    }

    fn adopt_free_extents(&mut self, free_extents: Vec<FreeExtentRecord>) -> Result<(), ()> {
        self.free_extents = free_extents
            .into_iter()
            .filter(|extent| extent.allocated_blocks > 0)
            .collect();
        if self.policy.coalesce_on_release {
            self.normalize_free_extents();
        } else {
            self.free_extents.sort_by_key(|extent| extent.device_block);
            if self.policy.tail_trim_enabled {
                self.trim_free_tail();
            }
        }
        self.validate_allocator_state()
    }

    fn validate_allocator_state(&mut self) -> Result<(), ()> {
        let mut occupied: Vec<(u64, u64)> = self
            .blocks
            .values()
            .map(|entry| {
                (
                    entry.device_block,
                    entry.device_block.saturating_add(entry.allocated_blocks),
                )
            })
            .collect();
        occupied.sort_by_key(|(start, _)| *start);

        let mut free = self.free_extents.clone();
        free.sort_by_key(|extent| extent.device_block);
        for pair in free.windows(2) {
            let left = pair[0];
            let right = pair[1];
            if left.device_block.saturating_add(left.allocated_blocks) > right.device_block {
                return Err(());
            }
        }

        for extent in &free {
            let free_start = extent.device_block;
            let free_end = extent.device_block.saturating_add(extent.allocated_blocks);
            for (occupied_start, occupied_end) in &occupied {
                if free_start < *occupied_end && *occupied_start < free_end {
                    return Err(());
                }
            }
        }

        let max_free_end = free
            .iter()
            .map(|extent| extent.device_block.saturating_add(extent.allocated_blocks))
            .max()
            .unwrap_or(0);
        let max_occupied_end = occupied.iter().map(|(_, end)| *end).max().unwrap_or(0);
        self.next_device_block = self
            .next_device_block
            .max(max_free_end.max(max_occupied_end));
        Ok(())
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

        self.insert_free_extent(FreeExtent {
            device_block: entry.device_block,
            allocated_blocks: entry.allocated_blocks,
        });
    }
}

impl Default for BlockStore {
    fn default() -> Self {
        Self::with_block_size(4096)
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, byte| {
        acc.wrapping_mul(16777619).wrapping_add(u64::from(*byte))
    })
}

fn required_blocks(size: usize, block_size: usize) -> u64 {
    if size == 0 {
        1
    } else {
        size.div_ceil(block_size) as u64
    }
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
        assert_eq!(store.read(inode).map(|record| record.device_block), Some(0));
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

    #[test]
    fn write_preserves_existing_allocation_when_size_fits() {
        let mut store = BlockStore::with_block_size(4);
        let inode = InodeId(11);

        store.write(inode, b"hello".to_vec());
        let first = store.read(inode).expect("record");
        store.write(inode, b"abcd".to_vec());
        let second = store.read(inode).expect("record");

        assert_eq!(first.device_block, second.device_block);
        assert_eq!(first.allocated_blocks, 2);
        assert_eq!(second.allocated_blocks, 1);
    }

    #[test]
    fn freed_extents_are_reused_for_new_writes() {
        let mut store = BlockStore::with_block_size(4);

        store.write(InodeId(1), b"abcd".to_vec());
        store.write(InodeId(2), b"efgh".to_vec());
        let removed = store.remove(InodeId(1)).expect("record");
        assert_eq!(removed.device_block, 0);

        store.write(InodeId(3), b"zzzz".to_vec());
        let reused = store.read(InodeId(3)).expect("record");

        assert_eq!(reused.device_block, 0);
        assert_eq!(reused.allocated_blocks, 1);
    }

    #[test]
    fn shrinking_write_releases_surplus_blocks_for_reuse() {
        let mut store = BlockStore::with_block_size(4);
        let inode = InodeId(1);

        store.write(inode, b"abcdefgh".to_vec());
        store.write(inode, b"abc".to_vec());
        store.write(InodeId(2), b"wxyz".to_vec());

        let first = store.read(inode).expect("record");
        let second = store.read(InodeId(2)).expect("record");
        assert_eq!(first.device_block, 0);
        assert_eq!(first.allocated_blocks, 1);
        assert_eq!(second.device_block, 1);
    }

    #[test]
    fn records_rebuild_free_extents_and_reuse_gaps() {
        let records = vec![
            BlockRecord {
                inode: InodeId(1),
                bytes: b"aa".to_vec(),
                checksum: checksum(b"aa"),
                device_block: 0,
                allocated_blocks: 1,
            },
            BlockRecord {
                inode: InodeId(2),
                bytes: b"bb".to_vec(),
                checksum: checksum(b"bb"),
                device_block: 2,
                allocated_blocks: 1,
            },
        ];
        let mut store = BlockStore::from_records_with_block_size(records, 4);

        store.write(InodeId(3), b"cc".to_vec());
        let inserted = store.read(InodeId(3)).expect("record");

        assert_eq!(inserted.device_block, 1);
    }

    #[test]
    fn allocator_metadata_round_trips_with_policy_and_free_extents() {
        let policy = AllocatorPolicy {
            strategy: AllocationStrategy::FirstFit,
            split_threshold_blocks: 2,
            coalesce_on_release: true,
            tail_trim_enabled: true,
            background_compaction_enabled: true,
            fragmentation_threshold_percent: 40,
        };
        let records = vec![BlockRecord {
            inode: InodeId(1),
            bytes: b"aa".to_vec(),
            checksum: checksum(b"aa"),
            device_block: 2,
            allocated_blocks: 1,
        }];
        let free_extents = vec![FreeExtentRecord {
            device_block: 0,
            allocated_blocks: 2,
        }];

        let store = BlockStore::from_records_with_allocator(
            records,
            4,
            policy.clone(),
            free_extents.clone(),
        );

        assert_eq!(store.allocator_policy(), &policy);
        assert_eq!(store.free_extents(), free_extents);
    }

    #[test]
    fn defragment_compacts_entries_and_clears_gaps() {
        let records = vec![
            BlockRecord {
                inode: InodeId(1),
                bytes: b"aa".to_vec(),
                checksum: checksum(b"aa"),
                device_block: 0,
                allocated_blocks: 1,
            },
            BlockRecord {
                inode: InodeId(2),
                bytes: b"bb".to_vec(),
                checksum: checksum(b"bb"),
                device_block: 3,
                allocated_blocks: 1,
            },
        ];
        let free_extents = vec![FreeExtentRecord {
            device_block: 1,
            allocated_blocks: 2,
        }];
        let mut store = BlockStore::from_records_with_allocator(
            records,
            4,
            AllocatorPolicy::default(),
            free_extents,
        );

        let report = store.defragment();

        assert_eq!(report.moved_entries, 1);
        assert_eq!(report.reclaimed_gaps, 1);
        assert_eq!(report.final_device_blocks, 2);
        assert!(store.free_extents().is_empty());
        assert_eq!(store.read(InodeId(2)).expect("record").device_block, 1);
    }

    #[test]
    fn fragmentation_report_detects_split_free_space() {
        let records = vec![
            BlockRecord {
                inode: InodeId(1),
                bytes: b"aa".to_vec(),
                checksum: checksum(b"aa"),
                device_block: 0,
                allocated_blocks: 1,
            },
            BlockRecord {
                inode: InodeId(2),
                bytes: b"bb".to_vec(),
                checksum: checksum(b"bb"),
                device_block: 3,
                allocated_blocks: 1,
            },
        ];
        let free_extents = vec![
            FreeExtentRecord {
                device_block: 1,
                allocated_blocks: 1,
            },
            FreeExtentRecord {
                device_block: 2,
                allocated_blocks: 1,
            },
        ];
        let policy = AllocatorPolicy {
            background_compaction_enabled: true,
            fragmentation_threshold_percent: 25,
            coalesce_on_release: false,
            ..AllocatorPolicy::default()
        };
        let store = BlockStore::from_records_with_allocator(records, 4, policy, free_extents);

        let report = store.fragmentation_report();

        assert_eq!(report.free_extents, 2);
        assert_eq!(report.total_free_blocks, 2);
        assert_eq!(report.largest_free_extent, 1);
        assert_eq!(report.fragmentation_percent, 50);
        assert!(report.needs_compaction);
    }

    #[test]
    fn optimize_compacts_when_policy_requires_it() {
        let records = vec![
            BlockRecord {
                inode: InodeId(1),
                bytes: b"aa".to_vec(),
                checksum: checksum(b"aa"),
                device_block: 0,
                allocated_blocks: 1,
            },
            BlockRecord {
                inode: InodeId(2),
                bytes: b"bb".to_vec(),
                checksum: checksum(b"bb"),
                device_block: 3,
                allocated_blocks: 1,
            },
        ];
        let free_extents = vec![
            FreeExtentRecord {
                device_block: 1,
                allocated_blocks: 1,
            },
            FreeExtentRecord {
                device_block: 2,
                allocated_blocks: 1,
            },
        ];
        let policy = AllocatorPolicy {
            background_compaction_enabled: true,
            fragmentation_threshold_percent: 25,
            coalesce_on_release: false,
            ..AllocatorPolicy::default()
        };
        let mut store = BlockStore::from_records_with_allocator(records, 4, policy, free_extents);

        let report = store.optimize();

        assert!(report.defragmentation.is_some());
        assert_eq!(report.before.fragmentation_percent, 50);
        assert_eq!(report.after.fragmentation_percent, 0);
        assert_eq!(store.read(InodeId(2)).expect("record").device_block, 1);
    }
}
