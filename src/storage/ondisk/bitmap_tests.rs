// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::ondisk::layout::{BITS_PER_BITMAP_BLOCK, BLOCK_SIZE};

#[test]
fn fresh_bitmap_is_empty() {
    let b = Bitmap::new(4096);
    assert_eq!(b.popcount(), 0);
    assert_eq!(b.block_count(), 1);
    for i in [0u64, 1, 100, 4095] {
        assert!(!b.is_set(i).unwrap());
    }
}

#[test]
fn set_and_clear_flip_bits() {
    let mut b = Bitmap::new(64);
    assert!(b.set(7).unwrap());
    assert!(!b.set(7).unwrap()); // second set reports no change
    assert!(b.is_set(7).unwrap());
    assert!(b.clear(7).unwrap());
    assert!(!b.clear(7).unwrap());
    assert!(!b.is_set(7).unwrap());
}

#[test]
fn allocate_first_walks_sequentially() {
    let mut b = Bitmap::new(32);
    for i in 0..32 {
        assert_eq!(b.allocate_first(0), Some(i as u64));
    }
    assert_eq!(b.allocate_first(0), None);
}

#[test]
fn allocate_first_respects_start_hint() {
    let mut b = Bitmap::new(64);
    assert_eq!(b.allocate_first(10), Some(10));
    assert_eq!(b.allocate_first(0), Some(0));
}

#[test]
fn allocate_first_skips_full_bytes_efficiently() {
    let mut b = Bitmap::new(1024);
    for i in 0..512 {
        b.set(i).unwrap();
    }
    assert_eq!(b.allocate_first(0), Some(512));
    assert_eq!(b.popcount(), 513);
}

#[test]
fn roundtrip_through_bytes() {
    let mut b = Bitmap::new(16_000);
    b.set(0).unwrap();
    b.set(500).unwrap();
    b.set(15_999).unwrap();
    let bytes = b.as_bytes().to_vec();
    assert_eq!(bytes.len() as u64, BLOCK_SIZE);
    let restored = Bitmap::from_bytes(bytes, 16_000).unwrap();
    assert!(restored.is_set(0).unwrap());
    assert!(restored.is_set(500).unwrap());
    assert!(restored.is_set(15_999).unwrap());
    assert_eq!(restored.popcount(), 3);
}

#[test]
fn out_of_range_indices_error() {
    let mut b = Bitmap::new(128);
    assert!(b.is_set(128).is_err());
    assert!(b.set(200).is_err());
    assert!(b.clear(u64::MAX).is_err());
}

#[test]
fn large_bitmap_spans_multiple_blocks() {
    let cap = BITS_PER_BITMAP_BLOCK * 3 + 17;
    let b = Bitmap::new(cap);
    assert_eq!(b.block_count(), 4);
    assert!(b.as_bytes().len() as u64 % BLOCK_SIZE == 0);
}

#[test]
fn from_bytes_rejects_unaligned_buffer() {
    assert!(Bitmap::from_bytes(vec![0u8; 1000], 8000).is_err());
}

#[test]
fn from_bytes_rejects_undersized_buffer() {
    let small = vec![0u8; BLOCK_SIZE as usize];
    assert!(Bitmap::from_bytes(small, 100_000).is_err());
}
