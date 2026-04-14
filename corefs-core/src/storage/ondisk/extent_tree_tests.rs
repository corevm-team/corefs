// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::storage::block_device::MemoryDevice;
use crate::storage::ondisk::layout::BLOCK_SIZE;

fn make_device(blocks: u64) -> MemoryDevice {
    MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap()
}

fn sample_extents(n: usize) -> Vec<Extent> {
    (0..n)
        .map(|i| Extent {
            logical_block: (i * 4) as u32,
            length_blocks: 4,
            physical_block: 1000 + (i as u64) * 4,
        })
        .collect()
}

#[test]
fn empty_index_block_roundtrips() {
    let blk = ExtentIndexBlock::empty();
    let enc = blk.encode().unwrap();
    assert_eq!(enc.len(), BLOCK_SIZE as usize);
    let dec = ExtentIndexBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
}

#[test]
fn full_index_block_roundtrips() {
    let blk = ExtentIndexBlock {
        next_index_block: 42,
        extents: sample_extents(EXTENTS_PER_INDEX_BLOCK),
    };
    let enc = blk.encode().unwrap();
    let dec = ExtentIndexBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
}

#[test]
fn overflow_is_rejected_on_encode() {
    let blk = ExtentIndexBlock {
        next_index_block: 0,
        extents: sample_extents(EXTENTS_PER_INDEX_BLOCK + 1),
    };
    assert!(blk.encode().is_err());
}

#[test]
fn checksum_detects_corruption() {
    let blk = ExtentIndexBlock {
        next_index_block: 5,
        extents: sample_extents(10),
    };
    let mut enc = blk.encode().unwrap();
    enc[20] ^= 0x01;
    assert!(ExtentIndexBlock::decode(&enc).is_err());
}

#[test]
fn bad_magic_is_rejected() {
    let blk = ExtentIndexBlock::empty();
    let mut enc = blk.encode().unwrap();
    enc[0..4].copy_from_slice(&0u32.to_le_bytes());
    // Recompute CRC with the new magic so only the magic check fires.
    let mut zeroed = enc.clone();
    zeroed[4092..4096].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(&zeroed);
    enc[4092..4096].copy_from_slice(&csum.to_le_bytes());
    let err = ExtentIndexBlock::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("magic"));
}

#[test]
fn single_block_chain_roundtrips_through_device() {
    let mut dev = make_device(64);
    let extents = sample_extents(10);
    let used = ExtentChain::write_chain(&mut dev, &extents, &[10]).unwrap();
    assert_eq!(used, vec![10]);
    let read = ExtentChain::read_chain(&dev, 10).unwrap();
    assert_eq!(read, extents);
}

#[test]
fn multi_block_chain_stitches_in_order() {
    let mut dev = make_device(64);
    // 3 blocks worth of extents — 508.5, so 509-ish? Use a round count.
    let count = EXTENTS_PER_INDEX_BLOCK * 2 + 7;
    let extents = sample_extents(count);
    let reserve = [10u64, 20, 30];
    let used = ExtentChain::write_chain(&mut dev, &extents, &reserve).unwrap();
    assert_eq!(used, vec![10, 20, 30]);
    let read = ExtentChain::read_chain(&dev, 10).unwrap();
    assert_eq!(read, extents);
}

#[test]
fn empty_root_means_empty_chain() {
    let dev = make_device(64);
    let read = ExtentChain::read_chain(&dev, 0).unwrap();
    assert!(read.is_empty());
}

#[test]
fn write_chain_rejects_insufficient_reserve() {
    let mut dev = make_device(64);
    let extents = sample_extents(EXTENTS_PER_INDEX_BLOCK + 1);
    assert!(ExtentChain::write_chain(&mut dev, &extents, &[10]).is_err());
}

#[test]
fn read_chain_detects_loop() {
    let mut dev = make_device(64);
    // Write a block whose next_index_block points back to itself.
    let looped = ExtentIndexBlock {
        next_index_block: 10,
        extents: sample_extents(3),
    };
    let bytes = looped.encode().unwrap();
    dev.write_at(10 * BLOCK_SIZE, &bytes).unwrap();
    let err = ExtentChain::read_chain(&dev, 10).unwrap_err();
    assert!(format!("{err}").contains("loop"));
}

#[test]
fn index_blocks_needed_helper() {
    assert_eq!(ExtentChain::index_blocks_needed(0), 0);
    assert_eq!(ExtentChain::index_blocks_needed(1), 1);
    assert_eq!(ExtentChain::index_blocks_needed(EXTENTS_PER_INDEX_BLOCK), 1);
    assert_eq!(
        ExtentChain::index_blocks_needed(EXTENTS_PER_INDEX_BLOCK + 1),
        2
    );
    assert_eq!(
        ExtentChain::index_blocks_needed(EXTENTS_PER_INDEX_BLOCK * 3),
        3
    );
}
