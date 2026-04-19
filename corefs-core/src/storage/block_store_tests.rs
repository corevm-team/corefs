// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::block_device::MemoryDevice;
use alloc::vec;

// Helper: build a BlockRecord with the new fields, using a single simple extent
// with a given device_block and allocated_blocks (for backward-compat tests).
fn make_record(inode: InodeId, payload: &[u8], device_block: u64, allocated_blocks: u64) -> BlockRecord {
    let crc = if payload.is_empty() { 0 } else { crate::storage::ondisk::checksum::Crc32c::hash(payload) };
    BlockRecord {
        inode,
        logical_size: payload.len() as u64,
        extents: if payload.is_empty() {
            alloc::vec![]
        } else {
            alloc::vec![ExtentRef {
                logical_block: 0,
                logical_len: payload.len() as u32,
                physical_block: device_block,
                length_blocks: allocated_blocks as u32,
                physical_len: payload.len() as u32,
                content_crc: crc,
                flags: 0,
            }]
        },
        content_crc: crc,
        flags: 0,
    }
}

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
        make_record(InodeId(1), b"aa", 0, 1),
        make_record(InodeId(2), b"bb", 2, 1),
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
    let records = vec![make_record(InodeId(1), b"aa", 2, 1)];
    let free_extents = vec![FreeExtentRecord {
        device_block: 0,
        allocated_blocks: 2,
    }];

    let store =
        BlockStore::from_records_with_allocator(records, 4, policy.clone(), free_extents.clone());

    assert_eq!(store.allocator_policy(), &policy);
    assert_eq!(store.free_extents(), free_extents);
}

#[test]
fn defragment_compacts_entries_and_clears_gaps() {
    let records = vec![
        make_record(InodeId(1), b"aa", 0, 1),
        make_record(InodeId(2), b"bb", 3, 1),
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
        make_record(InodeId(1), b"aa", 0, 1),
        make_record(InodeId(2), b"bb", 3, 1),
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
        make_record(InodeId(1), b"aa", 0, 1),
        make_record(InodeId(2), b"bb", 3, 1),
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

    assert!(report.heat_reallocation.is_none());
    assert!(report.defragmentation.is_some());
    assert_eq!(report.before.fragmentation_percent, 50);
    assert_eq!(report.after.fragmentation_percent, 0);
    assert_eq!(store.read(InodeId(2)).expect("record").device_block, 1);
}

#[test]
fn prioritize_reallocation_promotes_hot_inodes_to_front() {
    let records = vec![
        make_record(InodeId(1), b"aa", 2, 1),
        make_record(InodeId(2), b"bb", 0, 1),
        make_record(InodeId(3), b"cc", 4, 1),
    ];
    let free_extents = vec![
        FreeExtentRecord {
            device_block: 1,
            allocated_blocks: 1,
        },
        FreeExtentRecord {
            device_block: 3,
            allocated_blocks: 1,
        },
    ];
    let mut store = BlockStore::from_records_with_allocator(
        records,
        4,
        AllocatorPolicy::default(),
        free_extents,
    );

    let report = store.reallocate_prioritized_extents(&[InodeId(3), InodeId(1)]);

    assert_eq!(report.prioritized_inodes, 2);
    assert_eq!(report.promoted_hot_inodes, 2);
    assert_eq!(store.read(InodeId(3)).expect("record").device_block, 0);
    assert_eq!(store.read(InodeId(1)).expect("record").device_block, 1);
}

// ── CoW / clone_for_inode / cow_stats ──────────────────────────────────────

#[test]
fn is_shared_detects_multiple_refs() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"shared".to_vec());

    assert!(!store.is_shared(InodeId(1)), "single inode: not shared");
    assert!(!store.is_shared(InodeId(99)), "missing inode: not shared");

    // A second inode writing the same bytes creates a shared blob.
    store.write(InodeId(2), b"shared".to_vec());
    assert!(store.is_shared(InodeId(1)));
    assert!(store.is_shared(InodeId(2)));
}

#[test]
fn blob_checksum_is_consistent_before_and_after_clone() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"hello".to_vec());
    let cs = store.blob_checksum(InodeId(1)).expect("checksum");

    store.clone_for_inode(InodeId(1), InodeId(2));

    // Both inodes report the same checksum while sharing the blob.
    assert_eq!(store.blob_checksum(InodeId(1)), Some(cs));
    assert_eq!(store.blob_checksum(InodeId(2)), Some(cs));
}

#[test]
fn clone_for_inode_increments_ref_count_and_marks_shared() {
    let mut store = BlockStore::default();
    store.write(InodeId(10), b"data".to_vec());
    assert!(!store.is_shared(InodeId(10)));

    let cloned = store.clone_for_inode(InodeId(10), InodeId(20));
    assert!(cloned, "clone should succeed");
    assert!(store.is_shared(InodeId(10)), "source becomes shared");
    assert!(store.is_shared(InodeId(20)), "clone is also shared");
    assert_eq!(
        store.read(InodeId(10)).map(|r| r.bytes.clone()),
        store.read(InodeId(20)).map(|r| r.bytes.clone()),
        "both inodes read the same data"
    );
}

#[test]
fn clone_for_inode_returns_false_for_missing_source() {
    let mut store = BlockStore::default();
    assert!(!store.clone_for_inode(InodeId(99), InodeId(1)));
}

#[test]
fn write_after_clone_materialises_independent_copy() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"original".to_vec());
    store.clone_for_inode(InodeId(1), InodeId(2));

    // Overwrite source — clone must still read original data.
    store.write(InodeId(1), b"modified".to_vec());

    assert_eq!(
        store.read(InodeId(1)).expect("source").bytes,
        b"modified".to_vec()
    );
    assert_eq!(
        store.read(InodeId(2)).expect("clone").bytes,
        b"original".to_vec(),
        "clone must be independent after source write"
    );
    assert!(
        !store.is_shared(InodeId(1)),
        "source is now exclusively owned"
    );
    assert!(
        !store.is_shared(InodeId(2)),
        "clone is now exclusively owned"
    );
}

#[test]
fn append_after_clone_triggers_cow_materialisation() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"base".to_vec());
    store.clone_for_inode(InodeId(1), InodeId(2));

    // Append to source: shared blob must be cloned first.
    store.append_to_inode(InodeId(1), b"_ext");

    assert_eq!(store.read(InodeId(1)).expect("src").bytes, b"base_ext");
    assert_eq!(
        store.read(InodeId(2)).expect("clone").bytes,
        b"base",
        "clone must remain unmodified"
    );
}

#[test]
fn cow_stats_reports_sharing_and_savings() {
    let mut store = BlockStore::default();
    // Two inodes sharing the same blob (identical content).
    store.write(InodeId(1), b"abc".to_vec());
    store.write(InodeId(2), b"abc".to_vec());
    // One inode with unique content.
    store.write(InodeId(3), b"xyz".to_vec());

    let stats = store.cow_stats();

    assert_eq!(stats.shared_blobs, 1, "one shared blob");
    assert_eq!(stats.exclusive_blobs, 1, "one exclusive blob");
    assert_eq!(stats.shared_logical_bytes, 6, "3 bytes × ref_count 2");
    assert_eq!(stats.bytes_saved_by_sharing, 3, "3 bytes saved");
    assert_eq!(stats.max_ref_count, 2);
}

#[test]
fn cow_stats_all_exclusive_when_no_sharing() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"a".to_vec());
    store.write(InodeId(2), b"b".to_vec());

    let stats = store.cow_stats();

    assert_eq!(stats.shared_blobs, 0);
    assert_eq!(stats.bytes_saved_by_sharing, 0);
    assert_eq!(stats.exclusive_blobs, 2);
}

#[test]
fn cow_stats_empty_store_is_zero() {
    let store = BlockStore::default();
    let stats = store.cow_stats();
    assert_eq!(stats.shared_blobs, 0);
    assert_eq!(stats.exclusive_blobs, 0);
    assert_eq!(stats.max_ref_count, 0);
}

// -----------------------------------------------------------------------
// TRIM / freed extent tracking
// -----------------------------------------------------------------------

#[test]
fn remove_records_freed_extent_for_trim() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"hello".to_vec());
    let record = store.read(InodeId(1)).unwrap();
    let expected_block = record.device_block;
    let expected_count = record.allocated_blocks;

    assert!(store.pending_trims().is_empty());

    store.remove(InodeId(1));

    let trims = store.drain_freed_extents();
    assert_eq!(trims.len(), 1);
    assert_eq!(trims[0].device_block, expected_block);
    assert_eq!(trims[0].block_count, expected_count);
}

#[test]
fn write_shrink_records_tail_freed_extent() {
    let mut store = BlockStore::default();
    // Write a large payload that needs multiple blocks
    store.write(InodeId(1), vec![0xAA; 16384]);
    store.drain_freed_extents(); // clear

    // Overwrite with a smaller payload
    store.write(InodeId(1), vec![0xBB; 1]);

    let trims = store.drain_freed_extents();
    // Should have freed the tail blocks
    assert!(!trims.is_empty());
}

#[test]
fn device_append_coalesces_into_tail_block_without_rewriting_file() {
    let mut device = MemoryDevice::new(1024 * 1024, 4096).expect("device");
    let mut store = BlockStore::default();
    let inode = InodeId(42);

    store.write_device(&mut device, inode, b"hello").expect("write");
    store.append_device(&mut device, inode, b" world").expect("append");

    let rec = store.record(inode).expect("record");
    assert_eq!(rec.logical_size, 11);
    assert_eq!(rec.extents.len(), 1, "small append should reuse tail room in the last block");
    assert_eq!(rec.extents[0].logical_len, 11);
    assert_eq!(rec.extents[0].physical_len, 11);

    let mut buf = [0u8; 11];
    let n = store.read_bytes(&device, inode, 0, &mut buf).expect("read");
    assert_eq!(n, 11);
    assert_eq!(&buf, b"hello world");
    assert!(store.verify_device(&device, inode));
}

#[test]
fn device_append_allocates_new_extent_after_tail_block_is_full() {
    let mut device = MemoryDevice::new(1024 * 1024, 4096).expect("device");
    let mut store = BlockStore::default();
    let inode = InodeId(44);

    store.write_device(&mut device, inode, &alloc::vec![0xAA; 4090]).expect("write");
    store.append_device(&mut device, inode, &alloc::vec![0xBB; 16]).expect("append");

    let rec = store.record(inode).expect("record");
    assert_eq!(rec.logical_size, 4106);
    assert_eq!(rec.extents.len(), 2);
    assert_eq!(rec.extents[0].logical_len, 4096);
    assert_eq!(rec.extents[1].logical_len, 10);

    let mut buf = alloc::vec![0u8; 4106];
    let n = store.read_bytes(&device, inode, 0, &mut buf).expect("read");
    assert_eq!(n, 4106);
    assert_eq!(&buf[..4090], &alloc::vec![0xAA; 4090]);
    assert_eq!(&buf[4090..], &alloc::vec![0xBB; 16]);
    assert!(store.verify_device(&device, inode));
}

#[test]
fn device_ranged_read_crosses_extent_boundaries_without_full_materialisation() {
    let mut device = MemoryDevice::new(1024 * 1024, 4096).expect("device");
    let mut store = BlockStore::default();
    let inode = InodeId(43);

    store.write_device(&mut device, inode, b"abcd").expect("write");
    store.append_device(&mut device, inode, b"efgh").expect("append");
    store.append_device(&mut device, inode, b"ijkl").expect("append");

    let mut buf = [0u8; 6];
    let n = store.read_bytes(&device, inode, 3, &mut buf).expect("read");
    assert_eq!(n, 6);
    assert_eq!(&buf, b"defghi");
}

#[test]
fn drain_freed_extents_clears_pending() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"hello".to_vec());
    store.remove(InodeId(1));

    let first = store.drain_freed_extents();
    assert!(!first.is_empty());

    let second = store.drain_freed_extents();
    assert!(second.is_empty());
}

#[test]
fn multiple_removes_accumulate_trims() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"aaa".to_vec());
    store.write(InodeId(2), b"bbb".to_vec());
    store.write(InodeId(3), b"ccc".to_vec());

    store.remove(InodeId(1));
    store.remove(InodeId(3));

    let trims = store.drain_freed_extents();
    assert_eq!(trims.len(), 2);
}

#[test]
fn pending_trims_does_not_drain() {
    let mut store = BlockStore::default();
    store.write(InodeId(1), b"hello".to_vec());
    store.remove(InodeId(1));

    assert!(!store.pending_trims().is_empty());
    assert!(!store.pending_trims().is_empty()); // Still there

    let drained = store.drain_freed_extents();
    assert!(!drained.is_empty());
    assert!(store.pending_trims().is_empty()); // Now empty
}
