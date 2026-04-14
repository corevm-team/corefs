// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn empty_roundtrip() {
    let blk = AttrBlock::new(Vec::new());
    let enc = blk.encode().unwrap();
    let dec = AttrBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
}

#[test]
fn small_payload_roundtrip() {
    let payload = b"hello, attr block!".to_vec();
    let blk = AttrBlock {
        next_attr_block: 99,
        payload: payload.clone(),
    };
    let enc = blk.encode().unwrap();
    let dec = AttrBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
}

#[test]
fn max_payload_roundtrip() {
    let blk = AttrBlock::new(vec![0xA5u8; ATTR_BLOCK_CAPACITY]);
    let enc = blk.encode().unwrap();
    let dec = AttrBlock::decode(&enc).unwrap();
    assert_eq!(dec.payload.len(), ATTR_BLOCK_CAPACITY);
}

#[test]
fn oversize_rejected() {
    let blk = AttrBlock::new(vec![0u8; ATTR_BLOCK_CAPACITY + 1]);
    assert!(blk.encode().is_err());
}

#[test]
fn corruption_caught() {
    let blk = AttrBlock::new(b"xyz".to_vec());
    let mut enc = blk.encode().unwrap();
    enc[40] ^= 0x01;
    assert!(AttrBlock::decode(&enc).is_err());
}

#[test]
fn bad_magic_rejected() {
    let blk = AttrBlock::new(Vec::new());
    let mut enc = blk.encode().unwrap();
    enc[0..4].copy_from_slice(&0u32.to_le_bytes());
    let mut zeroed = enc.clone();
    zeroed[4092..4096].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(&zeroed);
    enc[4092..4096].copy_from_slice(&csum.to_le_bytes());
    let err = AttrBlock::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("magic"));
}
