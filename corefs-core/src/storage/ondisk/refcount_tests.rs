// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn ext(start: u64, len: u32) -> Extent {
    Extent {
        logical_block: 0,
        length_blocks: len,
        physical_block: start,
    }
}

// ---------------------------------------------------------------------------
// RefCountTable
// ---------------------------------------------------------------------------

#[test]
fn fresh_table_starts_at_zero() {
    let t = RefCountTable::new(256);
    assert_eq!(t.capacity(), 256);
    for b in 0..256 {
        assert_eq!(t.get(b), 0);
    }
    assert_eq!(t.allocated(), 0);
    assert_eq!(t.shared(), 0);
}

#[test]
fn acquire_and_release_increment_and_decrement() {
    let mut t = RefCountTable::new(64);
    assert_eq!(t.acquire(10).unwrap(), 1);
    assert_eq!(t.acquire(10).unwrap(), 2);
    assert_eq!(t.acquire(10).unwrap(), 3);
    assert_eq!(t.release(10).unwrap(), 2);
    assert_eq!(t.release(10).unwrap(), 1);
    assert_eq!(t.release(10).unwrap(), 0);
}

#[test]
fn release_on_zero_returns_error() {
    let mut t = RefCountTable::new(16);
    let err = t.release(0).unwrap_err();
    assert!(format!("{err}").contains("refcount was zero"));
}

#[test]
fn out_of_range_index_is_error() {
    let mut t = RefCountTable::new(16);
    assert!(t.acquire(100).is_err());
    assert!(t.release(100).is_err());
    assert!(t.get_checked(100).is_err());
    // get() is lenient and returns 0.
    assert_eq!(t.get(100), 0);
}

#[test]
fn overflow_at_max_refcount_is_error() {
    let mut t = RefCountTable::new(4);
    for _ in 0..u16::MAX {
        t.acquire(0).unwrap();
    }
    let err = t.acquire(0).unwrap_err();
    assert!(format!("{err}").contains("overflow"));
    assert_eq!(t.get(0), u16::MAX);
}

#[test]
fn acquire_extent_rolls_back_on_mid_failure() {
    let mut t = RefCountTable::new(4);
    // Pre-fill block 2 to MAX so the 3-block acquire fails on i=2.
    for _ in 0..u16::MAX {
        t.acquire(2).unwrap();
    }
    let e = ext(0, 3);
    assert!(t.acquire_extent(e).is_err());
    // Blocks 0 and 1 must have rolled back to zero.
    assert_eq!(t.get(0), 0);
    assert_eq!(t.get(1), 0);
    // Block 2 stays at MAX (its value was already MAX before).
    assert_eq!(t.get(2), u16::MAX);
}

#[test]
fn release_extent_collects_freed_blocks() {
    let mut t = RefCountTable::new(16);
    let e = ext(4, 3);
    t.acquire_extent(e).unwrap();
    // Blocks 4,5,6 have refcount 1.  Releasing drops all to 0.
    let freed = t.release_extent(e).unwrap();
    assert_eq!(freed, vec![4, 5, 6]);
}

#[test]
fn release_extent_only_reports_blocks_hitting_zero() {
    let mut t = RefCountTable::new(16);
    t.acquire(3).unwrap();
    t.acquire(3).unwrap(); // refcount 2
    t.acquire(4).unwrap(); // refcount 1
    let e = ext(3, 2);
    let freed = t.release_extent(e).unwrap();
    assert_eq!(freed, vec![4]);
    // Block 3 still has refcount 1.
    assert_eq!(t.get(3), 1);
}

#[test]
fn allocated_and_shared_counters_are_accurate() {
    let mut t = RefCountTable::new(16);
    t.acquire(1).unwrap(); // 1
    t.acquire(2).unwrap(); // 1
    t.acquire(2).unwrap(); // 2 (shared)
    t.acquire(3).unwrap(); // 1
    t.acquire(3).unwrap(); // 2
    t.acquire(3).unwrap(); // 3
    assert_eq!(t.allocated(), 3);
    assert_eq!(t.shared(), 2); // blocks 2 and 3
}

// ---------------------------------------------------------------------------
// Encode / decode roundtrip
// ---------------------------------------------------------------------------

#[test]
fn encode_decode_empty_table() {
    let t = RefCountTable::new(256);
    let enc = t.encode();
    let dec = RefCountTable::decode(&enc, 256).unwrap();
    assert_eq!(dec, t);
}

#[test]
fn encode_decode_populated_table() {
    let mut t = RefCountTable::new(2500);
    for (b, count) in [(10u64, 1), (20, 3), (100, 42), (2499, 7)] {
        for _ in 0..count {
            t.acquire(b).unwrap();
        }
    }
    let enc = t.encode();
    // Multi-block region: 2500 / 2044 = 2 blocks.
    assert_eq!(
        enc.len() as u64,
        2 * crate::storage::ondisk::layout::BLOCK_SIZE
    );
    let dec = RefCountTable::decode(&enc, 2500).unwrap();
    assert_eq!(dec, t);
    assert_eq!(dec.get(100), 42);
    assert_eq!(dec.get(2499), 7);
}

#[test]
fn decode_detects_crc_corruption() {
    let t = RefCountTable::new(256);
    let mut enc = t.encode();
    enc[42] ^= 0x01;
    let err = RefCountTable::decode(&enc, 256).unwrap_err();
    assert!(format!("{err}").contains("CRC"));
}

#[test]
fn decode_rejects_wrong_buffer_length() {
    let enc = vec![0u8; 100];
    assert!(RefCountTable::decode(&enc, 256).is_err());
}

#[test]
fn blocks_needed_matches_expected_capacity() {
    assert_eq!(RefCountTable::blocks_needed(0), 0);
    assert_eq!(RefCountTable::blocks_needed(1), 1);
    assert_eq!(RefCountTable::blocks_needed(COUNTS_PER_BLOCK as u64), 1);
    assert_eq!(RefCountTable::blocks_needed(COUNTS_PER_BLOCK as u64 + 1), 2);
}

// ---------------------------------------------------------------------------
// BlockSharing
// ---------------------------------------------------------------------------

#[test]
fn register_fresh_sets_refcount_to_one() {
    let mut s = BlockSharing::new(RefCountTable::new(32));
    s.register_fresh(ext(5, 3)).unwrap();
    assert_eq!(s.table().get(5), 1);
    assert_eq!(s.table().get(6), 1);
    assert_eq!(s.table().get(7), 1);
}

#[test]
fn register_fresh_rejects_already_allocated() {
    let mut s = BlockSharing::new(RefCountTable::new(32));
    s.register_fresh(ext(5, 2)).unwrap();
    let err = s.register_fresh(ext(5, 2)).unwrap_err();
    assert!(format!("{err}").contains("already has refcount"));
}

#[test]
fn clone_extent_enables_sharing() {
    let mut s = BlockSharing::new(RefCountTable::new(32));
    s.register_fresh(ext(10, 4)).unwrap();
    s.clone_extent(ext(10, 4)).unwrap();
    assert_eq!(s.table().get(10), 2);
    assert_eq!(s.shared_blocks(), 4);
    assert_eq!(
        s.bytes_saved(),
        4 * crate::storage::ondisk::layout::BLOCK_SIZE
    );
}

#[test]
fn cow_write_on_sole_reference_is_in_place() {
    let mut s = BlockSharing::new(RefCountTable::new(16));
    s.register_fresh(ext(3, 2)).unwrap();
    let outcome = s.cow_write(ext(3, 2)).unwrap();
    assert_eq!(outcome, CowOutcome::InPlace);
    // Refcount is untouched.
    assert_eq!(s.table().get(3), 1);
}

#[test]
fn cow_write_on_shared_reference_decrements_and_requests_copy() {
    let mut s = BlockSharing::new(RefCountTable::new(16));
    s.register_fresh(ext(5, 2)).unwrap();
    s.clone_extent(ext(5, 2)).unwrap(); // both blocks at refcount 2
    let outcome = s.cow_write(ext(5, 2)).unwrap();
    match outcome {
        CowOutcome::MustCopy { freed } => {
            assert!(freed.is_empty(), "other inode still references the blocks");
        }
        other => panic!("unexpected outcome: {other:?}"),
    }
    // After cow_write the writer's reference is gone — refcount is back to 1.
    assert_eq!(s.table().get(5), 1);
    assert_eq!(s.table().get(6), 1);
}

#[test]
fn cow_write_release_chain_eventually_frees_blocks() {
    let mut s = BlockSharing::new(RefCountTable::new(16));
    s.register_fresh(ext(9, 1)).unwrap();
    s.clone_extent(ext(9, 1)).unwrap(); // refcount 2
    let _ = s.cow_write(ext(9, 1)).unwrap(); // refcount 1
    let freed = s.release(ext(9, 1)).unwrap();
    assert_eq!(freed, vec![9]);
    assert_eq!(s.table().get(9), 0);
}

#[test]
fn release_returns_only_blocks_that_hit_zero() {
    let mut s = BlockSharing::new(RefCountTable::new(16));
    s.register_fresh(ext(2, 3)).unwrap();
    s.clone_extent(ext(2, 1)).unwrap(); // block 2 now has refcount 2
    let freed = s.release(ext(2, 3)).unwrap();
    assert_eq!(freed, vec![3, 4]); // block 2 survived because it was shared
    assert_eq!(s.table().get(2), 1);
}

#[test]
fn into_table_returns_persistable_state() {
    let mut s = BlockSharing::new(RefCountTable::new(8));
    s.register_fresh(ext(0, 2)).unwrap();
    let t = s.into_table();
    assert_eq!(t.allocated(), 2);
}
