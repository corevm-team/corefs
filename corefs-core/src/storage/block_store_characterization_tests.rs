// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Characterization tests for BlockStore.
//
// These tests document and pin the OBSERVABLE BEHAVIOUR of the extent-based
// BlockStore implementation.  Updated for Phase A refactor where BlockRecord
// no longer has a `bytes` field — bytes live on the internal MemoryDevice.

use super::*;
use alloc::vec;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Deterministic pseudo-random bytes from a 64-bit LCG (Knuth MMIX).
fn prng_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Write / read roundtrips
// ---------------------------------------------------------------------------

#[test]
fn char_write_empty_reads_back_empty() {
    let mut store = BlockStore::default();
    let inode = InodeId(1);
    store.write(inode, vec![]);
    let rec = store.read(inode).expect("record exists after empty write");
    assert!(rec.bytes.is_empty());
}

#[test]
fn char_write_one_byte_reads_back_one_byte() {
    let mut store = BlockStore::default();
    let inode = InodeId(2);
    store.write(inode, vec![0x5A]);
    let rec = store.read(inode).expect("record exists");
    assert_eq!(rec.bytes, vec![0x5A]);
    assert!(store.verify(inode));
}

#[test]
fn char_write_exactly_one_block_reads_back_identical() {
    let block_size = 4096;
    let mut store = BlockStore::with_block_size(block_size);
    let inode = InodeId(3);
    let payload: Vec<u8> = (0..block_size).map(|i| (i & 0xFF) as u8).collect();
    store.write(inode, payload.clone());
    let rec = store.read(inode).expect("record exists");
    assert_eq!(rec.bytes, payload);
    assert_eq!(rec.allocated_blocks, 1);
    assert!(store.verify(inode));
}

#[test]
fn char_write_one_block_plus_one_byte_allocates_two_blocks() {
    let block_size = 4096;
    let mut store = BlockStore::with_block_size(block_size);
    let inode = InodeId(4);
    let payload: Vec<u8> = vec![0xAB; block_size + 1];
    store.write(inode, payload.clone());
    let rec = store.read(inode).expect("record exists");
    assert_eq!(rec.allocated_blocks, 2);
    assert_eq!(rec.bytes, payload);
}

#[test]
fn char_write_16kib_multi_block_roundtrip() {
    let mut store = BlockStore::default();
    let inode = InodeId(5);
    let payload = prng_bytes(0xDEAD_BEEF, 16 * 1024);
    store.write(inode, payload.clone());
    let rec = store.read(inode).expect("record exists");
    assert_eq!(rec.bytes, payload);
    assert!(store.verify(inode));
}

// ---------------------------------------------------------------------------
// Allocation reuse / relocation
// ---------------------------------------------------------------------------

#[test]
fn char_overwrite_in_place_when_size_fits_preserves_device_block() {
    let mut store = BlockStore::with_block_size(4);
    let inode = InodeId(6);
    // First write: 5 bytes → 2 blocks allocated.
    store.write(inode, b"hello".to_vec());
    let first = store.read(inode).expect("first record");
    // Second write: 4 bytes ≤ allocated 2 blocks → reuses the extent.
    store.write(inode, b"abcd".to_vec());
    let second = store.read(inode).expect("second record");
    assert_eq!(
        first.device_block, second.device_block,
        "device_block must not change on in-place update"
    );
    assert_eq!(second.bytes, b"abcd");
}

#[test]
fn char_overwrite_larger_moves_allocation() {
    let mut store = BlockStore::with_block_size(4);
    let inode = InodeId(7);
    // First write: 1 block.
    store.write(inode, b"ab".to_vec());
    // Second write: 2 blocks → old extent released, new extent at next free position.
    store.write(inode, b"abcde".to_vec());
    let second = store.read(inode).expect("second record");
    assert_eq!(second.bytes, b"abcde");
    // Byte content must be correct.
    assert!(store.verify(inode));
}

#[test]
fn char_shrinking_write_releases_tail_as_free_extent_and_trim() {
    let mut store = BlockStore::with_block_size(4);
    let inode = InodeId(8);
    store.write(inode, b"abcdefgh".to_vec()); // 2 blocks
    store.drain_freed_extents(); // clear startup trims
    store.write(inode, b"ab".to_vec()); // 1 block — tail freed
    let freed = store.drain_freed_extents();
    assert!(
        !freed.is_empty(),
        "shrink must emit a TRIM extent for the released tail"
    );
}

// ---------------------------------------------------------------------------
// Append
// ---------------------------------------------------------------------------

#[test]
fn char_append_to_exclusive_blob_extends_in_place() {
    let mut store = BlockStore::default();
    let inode = InodeId(9);
    store.write(inode, b"hello".to_vec());
    assert!(!store.is_shared(inode));
    store.append_to_inode(inode, b" world");
    let rec = store.read(inode).expect("record");
    assert_eq!(rec.bytes, b"hello world");
    assert!(store.verify(inode));
}

#[test]
fn char_append_to_shared_blob_materializes_cow() {
    let mut store = BlockStore::default();
    let src = InodeId(10);
    let dst = InodeId(11);
    store.write(src, b"original".to_vec());
    store.clone_for_inode(src, dst);
    assert!(store.is_shared(src));
    store.append_to_inode(src, b"-extra");
    let src_rec = store.read(src).expect("src record");
    let dst_rec = store.read(dst).expect("dst record");
    assert_eq!(src_rec.bytes, b"original-extra");
    assert_eq!(
        dst_rec.bytes, b"original",
        "clone must not be affected by append to source"
    );
}

// ---------------------------------------------------------------------------
// Deduplication
// ---------------------------------------------------------------------------

#[test]
fn char_dedup_identical_bytes_share_storage() {
    let mut store = BlockStore::default();
    store.write(InodeId(12), b"dedup-payload".to_vec());
    store.write(InodeId(13), b"dedup-payload".to_vec());
    let stats = store.dedupe_stats();
    assert_eq!(stats.logical_blocks, 2);
    assert_eq!(
        stats.unique_blobs, 1,
        "identical payloads must share one blob"
    );
    assert_eq!(stats.deduplicated_blocks, 1);
}

#[test]
fn char_dedup_modifying_one_does_not_affect_other() {
    let mut store = BlockStore::default();
    let a = InodeId(14);
    let b = InodeId(15);
    store.write(a, b"same".to_vec());
    store.write(b, b"same".to_vec());
    // Overwrite inode a — must not disturb inode b.
    store.write(a, b"different".to_vec());
    let rec_b = store.read(b).expect("inode b");
    assert_eq!(
        rec_b.bytes, b"same",
        "modifying a must not affect b's bytes"
    );
}

// ---------------------------------------------------------------------------
// Clone / CoW
// ---------------------------------------------------------------------------

#[test]
fn char_clone_then_write_source_preserves_target() {
    let mut store = BlockStore::default();
    let src = InodeId(16);
    let tgt = InodeId(17);
    store.write(src, b"shared data".to_vec());
    store.clone_for_inode(src, tgt);
    store.write(src, b"modified data".to_vec());
    let tgt_rec = store.read(tgt).expect("target record");
    assert_eq!(
        tgt_rec.bytes, b"shared data",
        "clone must be unaffected by source write"
    );
}

// ---------------------------------------------------------------------------
// Remove / TRIM
// ---------------------------------------------------------------------------

#[test]
fn char_remove_returns_record_and_frees_extent() {
    let mut store = BlockStore::default();
    let inode = InodeId(18);
    store.write(inode, b"to be removed".to_vec());
    store.drain_freed_extents(); // clear startup entries
    let removed = store.remove(inode).expect("remove returns the record");
    assert_eq!(removed.bytes, b"to be removed");
    assert!(!store.contains(inode));
    let trims = store.drain_freed_extents();
    assert!(!trims.is_empty(), "remove must emit a TRIM extent");
}

// ---------------------------------------------------------------------------
// Records roundtrip
// ---------------------------------------------------------------------------

#[test]
fn char_from_records_roundtrips_byte_content() {
    // Phase A: BlockRecord is metadata-only. Extent metadata (logical_size,
    // content_crc, extents) must survive a from_records roundtrip.
    // Bytes live on the device and are NOT transferred via BlockRecord.
    let mut store = BlockStore::with_block_size(4096);
    let inode = InodeId(19);
    let payload = prng_bytes(0x1234_5678, 8192);
    store.write(inode, payload.clone());
    let records = store.records();
    assert_eq!(records.len(), 1);
    // Rebuild from the serialised records: extent metadata survives.
    let store2 = BlockStore::from_records(records);
    let rec_meta = store2.record(inode).expect("record after rebuild");
    // Logical size and CRC are preserved.
    assert_eq!(rec_meta.logical_size, payload.len() as u64);
    let expected_crc = crate::storage::ondisk::checksum::Crc32c::hash(&payload);
    assert_eq!(rec_meta.content_crc, expected_crc);
}

// ---------------------------------------------------------------------------
// Verify (checksum integrity)
// ---------------------------------------------------------------------------

#[test]
fn char_random_payload_verifies_true() {
    let mut store = BlockStore::default();
    let inode = InodeId(20);
    let payload = prng_bytes(0xCAFE_BABE, 1024);
    store.write(inode, payload);
    assert!(
        store.verify(inode),
        "freshly written payload must verify clean"
    );
}

#[test]
fn char_corrupted_data_block_detected_by_verify() {
    let mut store = BlockStore::default();
    let inode = InodeId(21);
    store.write(inode, b"verify me".to_vec());
    assert!(store.verify(inode));

    // Simulate a corruption: build a BlockRecord with wrong content_crc
    // and reconstruct the store from it.
    let rec = store.record(inode).unwrap().clone();
    let bad_rec = BlockRecord {
        inode: rec.inode,
        logical_size: rec.logical_size,
        extents: rec.extents.clone(),
        // Deliberately wrong CRC — verify must detect this.
        content_crc: rec.content_crc.wrapping_add(1),
        flags: rec.flags,
    };
    // Reconstruct a store with the corrupted record.
    let store2 = BlockStore::from_records(vec![bad_rec]);
    assert!(
        !store2.verify(inode),
        "mismatched checksum must be detected by verify"
    );
}
