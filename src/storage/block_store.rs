// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::domain::inode::InodeId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

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
    pub heat_reallocation: Option<HeatReallocationReport>,
    pub defragmentation: Option<DefragmentationReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatReallocationReport {
    pub prioritized_inodes: usize,
    pub promoted_hot_inodes: usize,
    pub moved_entries: usize,
    pub final_device_blocks: u64,
}

/// Report returned by an explicit deduplication pass over all blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupePassReport {
    /// Total blobs inspected.
    pub blobs_scanned: usize,
    /// Total bytes inspected across all blobs.
    pub bytes_scanned: usize,
    /// Number of duplicate blobs consolidated (byte-identical content under
    /// different checksums, e.g. after a hash algorithm change or corruption).
    pub duplicates_consolidated: usize,
    /// Bytes reclaimed by consolidation.
    pub bytes_reclaimed: usize,
    /// Number of hash collisions detected (same checksum, different bytes).
    /// A nonzero value indicates a data-integrity risk.
    pub hash_collisions: usize,
    /// Reference-count mismatches: blobs whose ref_count disagrees with the
    /// number of `BlockEntry` records that reference them.
    pub ref_count_mismatches: usize,
}

/// Snapshot of copy-on-write sharing state across all blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CowStats {
    /// Blobs referenced by two or more `BlockEntry` records.
    pub shared_blobs: usize,
    /// Sum of `blob_size × ref_count` for all shared blobs (total logical bytes mapped
    /// by shared blobs, counting each referencing inode separately).
    pub shared_logical_bytes: usize,
    /// Physical bytes saved by sharing (`blob_size × (ref_count − 1)` per shared blob).
    pub bytes_saved_by_sharing: usize,
    /// Blobs owned exclusively by a single inode (ref_count == 1).
    pub exclusive_blobs: usize,
    /// Highest reference count seen across all blobs.
    pub max_ref_count: usize,
}

/// A freed device-block range, suitable for issuing TRIM/discard to the
/// underlying block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreedExtent {
    pub device_block: u64,
    pub block_count: u64,
}

#[derive(Debug)]
pub struct BlockStore {
    block_size: usize,
    next_device_block: u64,
    policy: AllocatorPolicy,
    free_extents: Vec<FreeExtent>,
    blocks: BTreeMap<InodeId, BlockEntry>,
    blobs: BTreeMap<u64, BlobRecord>,
    /// Extents freed since the last call to [`drain_freed_extents`].
    /// The caller (typically the volume session or FUSE mount) can drain
    /// this list and forward it as TRIM/discard commands to the device.
    pending_trims: Vec<FreedExtent>,
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
            pending_trims: Vec::new(),
        }
    }

    pub fn write(&mut self, inode: InodeId, bytes: Vec<u8>) -> usize {
        let checksum = checksum(&bytes);
        let size = bytes.len();
        let required_blocks = required_blocks(size, self.block_size);
        let existing = self.blocks.get(&inode).map(|entry| {
            (
                entry.device_block,
                entry.allocated_blocks,
                entry.blob_checksum,
            )
        });

        let (device_block, allocated_blocks) = match existing {
            Some((device_block, existing_blocks, old_checksum))
                if existing_blocks >= required_blocks =>
            {
                // Reuse the existing allocation in-place: release the blob reference without
                // freeing the extent back to the free list (the block address stays occupied).
                self.blocks.remove(&inode);
                if let Some(blob) = self.blobs.get_mut(&old_checksum) {
                    blob.ref_count = blob.ref_count.saturating_sub(1);
                    if blob.ref_count == 0 {
                        self.blobs.remove(&old_checksum);
                    }
                }
                if existing_blocks > required_blocks {
                    let tail_block = device_block.saturating_add(required_blocks);
                    let tail_count = existing_blocks - required_blocks;
                    self.pending_trims.push(FreedExtent {
                        device_block: tail_block,
                        block_count: tail_count,
                    });
                    self.insert_free_extent(FreeExtent {
                        device_block: tail_block,
                        allocated_blocks: tail_count,
                    });
                }
                (device_block, required_blocks)
            }
            _ => {
                // Old allocation is too small (or absent): release it (adds to free list) and
                // allocate a fresh extent of the required size.
                self.release_inode(inode);
                self.allocate_extent(required_blocks.max(1))
            }
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

    /// Appends `extra` bytes to the existing data for `inode`.
    ///
    /// When the inode's blob is exclusively owned (ref_count == 1) the bytes are
    /// extended in-place and the checksum is updated incrementally — O(extra.len())
    /// amortised, no full re-read required. When the blob is shared (ref_count > 1)
    /// a copy-on-write clone is performed before extending.
    ///
    /// If `inode` has no existing data this behaves identically to `write`.
    pub fn append_to_inode(&mut self, inode: InodeId, extra: &[u8]) -> usize {
        let entry = self.blocks.get(&inode);
        let Some(entry) = entry else {
            return self.write(inode, extra.to_vec());
        };

        let old_checksum = entry.blob_checksum;
        let old_device_block = entry.device_block;
        let old_allocated = entry.allocated_blocks;

        let Some(blob) = self.blobs.get_mut(&old_checksum) else {
            return self.write(inode, extra.to_vec());
        };

        if blob.ref_count == 1 {
            // Exclusively owned: extend in-place.
            // The hash is a polynomial fold so it is incrementally composable:
            // checksum(A ++ B) == checksum(B, seed=checksum(A)).
            let incremental_new = extra.iter().fold(blob.checksum, |acc, byte| {
                acc.wrapping_mul(16777619).wrapping_add(u64::from(*byte))
            });
            blob.bytes.extend_from_slice(extra);
            blob.checksum = incremental_new;
            let new_size = blob.bytes.len();

            // Re-key the blob HashMap (old checksum → new checksum).
            let updated_blob = self.blobs.remove(&old_checksum).expect("blob must exist");
            self.blobs.insert(incremental_new, updated_blob);

            let required = required_blocks(new_size, self.block_size);
            self.blocks.insert(
                inode,
                BlockEntry {
                    inode,
                    blob_checksum: incremental_new,
                    size: new_size,
                    device_block: old_device_block,
                    allocated_blocks: required.max(old_allocated),
                },
            );
            new_size
        } else {
            // Shared blob: copy-on-write before extending.
            //
            // Clone the content while the blob is still alive, then delegate to
            // write() which will atomically release the old BlockEntry (decrementing
            // the blob's ref_count by exactly one) and register the new blob.  We
            // must NOT decrement ref_count manually here — write() does it when it
            // removes the existing BlockEntry, and a double-decrement would zero the
            // ref_count and destroy the blob while other inodes still reference it.
            let mut new_bytes = blob.bytes.clone();
            new_bytes.extend_from_slice(extra);
            self.write(inode, new_bytes)
        }
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

    /// Drains and returns all device-block ranges freed since the last drain.
    ///
    /// The caller can forward these to [`crate::storage::block_device::BlockDevice::trim`] for DISCARD.
    pub fn drain_freed_extents(&mut self) -> Vec<FreedExtent> {
        std::mem::take(&mut self.pending_trims)
    }

    /// Returns the pending TRIM list without draining it.
    pub fn pending_trims(&self) -> &[FreedExtent] {
        &self.pending_trims
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

    /// Returns `true` when the blob for `inode` is referenced by more than one
    /// `BlockEntry`.  A subsequent write to either inode will copy-on-write.
    pub fn is_shared(&self, inode: InodeId) -> bool {
        self.blocks
            .get(&inode)
            .and_then(|entry| self.blobs.get(&entry.blob_checksum))
            .is_some_and(|blob| blob.ref_count > 1)
    }

    /// Returns the content-hash of the blob currently assigned to `inode`,
    /// or `None` if the inode has no allocated block.
    pub fn blob_checksum(&self, inode: InodeId) -> Option<u64> {
        self.blocks.get(&inode).map(|entry| entry.blob_checksum)
    }

    /// Creates a `BlockEntry` for `target` that shares the same blob as `source`
    /// (CoW clone semantics).  The shared blob's reference count is incremented so
    /// that neither inode's subsequent write will silently mutate the other's data —
    /// `append_to_inode` and `write` both honour the reference count and perform a
    /// copy-on-write materialisation when they detect a shared blob.
    ///
    /// The clone receives its own device extent so that the two inodes remain
    /// independently addressable at the physical layer even while sharing logical
    /// data.
    ///
    /// Returns `false` when `source` has no allocated block (nothing to clone).
    pub fn clone_for_inode(&mut self, source: InodeId, target: InodeId) -> bool {
        let Some(source_entry) = self.blocks.get(&source).cloned() else {
            return false;
        };
        let Some(blob) = self.blobs.get_mut(&source_entry.blob_checksum) else {
            return false;
        };
        blob.ref_count += 1;
        // Allocate a separate device extent for the clone — physical independence
        // even while the logical blob is shared.
        let required = required_blocks(source_entry.size, self.block_size);
        let (device_block, allocated_blocks) = self.allocate_extent(required.max(1));
        self.blocks.insert(
            target,
            BlockEntry {
                inode: target,
                blob_checksum: source_entry.blob_checksum,
                size: source_entry.size,
                device_block,
                allocated_blocks,
            },
        );
        true
    }

    /// Returns a point-in-time snapshot of copy-on-write sharing state.
    pub fn cow_stats(&self) -> CowStats {
        let mut shared_blobs = 0usize;
        let mut shared_logical_bytes = 0usize;
        let mut bytes_saved_by_sharing = 0usize;
        let mut exclusive_blobs = 0usize;
        let mut max_ref_count = 0usize;

        for blob in self.blobs.values() {
            max_ref_count = max_ref_count.max(blob.ref_count);
            if blob.ref_count > 1 {
                shared_blobs += 1;
                shared_logical_bytes = shared_logical_bytes
                    .saturating_add(blob.bytes.len().saturating_mul(blob.ref_count));
                bytes_saved_by_sharing = bytes_saved_by_sharing
                    .saturating_add(blob.bytes.len().saturating_mul(blob.ref_count - 1));
            } else {
                exclusive_blobs += 1;
            }
        }

        CowStats {
            shared_blobs,
            shared_logical_bytes,
            bytes_saved_by_sharing,
            exclusive_blobs,
            max_ref_count,
        }
    }

    /// Runs an explicit deduplication and consistency pass over all blocks.
    ///
    /// 1. Verifies reference counts: each blob's `ref_count` should equal the
    ///    number of `BlockEntry` records referencing its checksum.
    /// 2. Detects hash collisions: blobs whose checksum maps to bytes that do
    ///    **not** match `checksum(bytes)`.
    /// 3. Finds byte-identical blobs stored under different checksums (e.g. after
    ///    corruption or hash-algorithm changes) and consolidates them.
    pub fn dedup_pass(&mut self) -> DedupePassReport {
        let blobs_scanned = self.blobs.len();
        let bytes_scanned: usize = self.blobs.values().map(|b| b.bytes.len()).sum();

        // --- Phase 1: ref-count audit ---
        let mut expected_refs: HashMap<u64, usize> = HashMap::new();
        for entry in self.blocks.values() {
            *expected_refs.entry(entry.blob_checksum).or_insert(0) += 1;
        }
        let mut ref_count_mismatches = 0usize;
        for (cs, blob) in &mut self.blobs {
            let expected = expected_refs.get(cs).copied().unwrap_or(0);
            if blob.ref_count != expected {
                blob.ref_count = expected;
                ref_count_mismatches += 1;
            }
        }
        // Remove orphaned blobs (ref_count == 0 with no entries).
        self.blobs.retain(|_, blob| blob.ref_count > 0);

        // --- Phase 2: hash collision detection ---
        let mut hash_collisions = 0usize;
        let checksums: Vec<u64> = self.blobs.keys().copied().collect();
        for cs in &checksums {
            if let Some(blob) = self.blobs.get(cs) {
                if checksum(&blob.bytes) != blob.checksum {
                    hash_collisions += 1;
                }
            }
        }

        // --- Phase 3: byte-identical consolidation ---
        // Group blobs by actual byte content and merge duplicates.
        let mut content_map: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut remap: HashMap<u64, u64> = HashMap::new(); // old_checksum → canonical_checksum
        let blob_checksums: Vec<u64> = self.blobs.keys().copied().collect();
        for cs in &blob_checksums {
            let Some(blob) = self.blobs.get(cs) else {
                continue;
            };
            if let Some(&canonical) = content_map.get(&blob.bytes) {
                if canonical != *cs {
                    remap.insert(*cs, canonical);
                }
            } else {
                content_map.insert(blob.bytes.clone(), *cs);
            }
        }

        let duplicates_consolidated = remap.len();
        let mut bytes_reclaimed = 0usize;

        // Re-point entries and move ref_counts.
        for (old_cs, canonical_cs) in &remap {
            let old_refs = self.blobs.get(old_cs).map(|b| b.ref_count).unwrap_or(0);
            let old_size = self.blobs.get(old_cs).map(|b| b.bytes.len()).unwrap_or(0);
            bytes_reclaimed += old_size;
            if let Some(canonical) = self.blobs.get_mut(canonical_cs) {
                canonical.ref_count += old_refs;
            }
            self.blobs.remove(old_cs);
            // Update all BlockEntries that pointed to old_cs.
            for entry in self.blocks.values_mut() {
                if entry.blob_checksum == *old_cs {
                    entry.blob_checksum = *canonical_cs;
                }
            }
        }

        DedupePassReport {
            blobs_scanned,
            bytes_scanned,
            duplicates_consolidated,
            bytes_reclaimed,
            hash_collisions,
            ref_count_mismatches,
        }
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
        self.optimize_with_priorities(&[])
    }

    pub fn optimize_with_priorities(&mut self, prioritized: &[InodeId]) -> OptimizationReport {
        let before = self.fragmentation_report();
        let threshold = self.policy.fragmentation_threshold_percent.min(100);
        let should_compact = before.fragmentation_percent >= threshold && before.free_extents > 1;
        let heat_reallocation = if !prioritized.is_empty()
            && (should_compact || self.has_misplaced_priorities(prioritized))
        {
            Some(self.reallocate_prioritized_extents(prioritized))
        } else {
            None
        };
        let after_heat = self.fragmentation_report();
        let defragmentation = if heat_reallocation.is_none()
            && after_heat.fragmentation_percent >= threshold
            && after_heat.free_extents > 1
        {
            Some(self.defragment())
        } else {
            None
        };
        let after = self.fragmentation_report();

        OptimizationReport {
            before,
            after,
            heat_reallocation,
            defragmentation,
        }
    }

    fn has_misplaced_priorities(&self, prioritized: &[InodeId]) -> bool {
        let mut entries: Vec<_> = self.blocks.values().collect();
        entries.sort_by_key(|entry| entry.device_block);
        let prioritized: Vec<_> = prioritized
            .iter()
            .copied()
            .filter(|inode| self.blocks.contains_key(inode))
            .collect();
        if prioritized.is_empty() {
            return false;
        }

        entries
            .iter()
            .take(prioritized.len())
            .map(|entry| entry.inode)
            .ne(prioritized)
    }

    pub fn reallocate_prioritized_extents(
        &mut self,
        prioritized: &[InodeId],
    ) -> HeatReallocationReport {
        let priority_map: HashMap<InodeId, usize> = prioritized
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, inode)| self.blocks.contains_key(inode))
            .map(|(index, inode)| (inode, index))
            .collect();
        let prioritized_inodes = priority_map.len();

        let mut entries: Vec<_> = self
            .blocks
            .iter()
            .map(|(inode, entry)| (*inode, entry.clone()))
            .collect();
        entries.sort_by(|left, right| {
            let left_rank = priority_map.get(&left.0).copied().unwrap_or(usize::MAX);
            let right_rank = priority_map.get(&right.0).copied().unwrap_or(usize::MAX);
            left_rank
                .cmp(&right_rank)
                .then_with(|| left.1.device_block.cmp(&right.1.device_block))
        });

        let mut cursor = 0u64;
        let mut moved_entries = 0usize;
        let mut promoted_hot_inodes = 0usize;

        for (inode, entry) in &mut entries {
            let original = entry.device_block;
            if entry.device_block != cursor {
                entry.device_block = cursor;
                moved_entries += 1;
                if priority_map.contains_key(inode) && cursor < original {
                    promoted_hot_inodes += 1;
                }
            }
            cursor = cursor.saturating_add(entry.allocated_blocks);
        }

        self.blocks = entries.into_iter().collect();
        self.free_extents.clear();
        self.next_device_block = cursor;

        HeatReallocationReport {
            prioritized_inodes,
            promoted_hot_inodes,
            moved_entries,
            final_device_blocks: cursor,
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

        // Record the freed extent for TRIM/discard.
        self.pending_trims.push(FreedExtent {
            device_block: entry.device_block,
            block_count: entry.allocated_blocks,
        });

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
#[path = "block_store_tests.rs"]
mod tests;
