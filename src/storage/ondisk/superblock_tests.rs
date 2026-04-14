// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::ondisk::layout::{
    BLOCK_SIZE, LayoutGeometry, LayoutParams, ODF_MAGIC, ODF_VERSION_MAJOR,
};

fn sample_geom() -> LayoutGeometry {
    LayoutGeometry::plan(LayoutParams::with_defaults(4096)).unwrap()
}

#[test]
fn roundtrip_preserves_all_fields() {
    let sb = Superblock::for_new_volume(&sample_geom(), [0xABu8; 16], "test-vol", 1_700_000_000);
    let bytes = sb.encode_block();
    assert_eq!(bytes.len(), BLOCK_SIZE as usize);
    let decoded = Superblock::decode_block(&bytes).unwrap();
    assert_eq!(decoded, sb);
    assert_eq!(decoded.magic, ODF_MAGIC);
    assert_eq!(decoded.version_major, ODF_VERSION_MAJOR);
    assert_eq!(&decoded.label[..8], b"test-vol");
}

#[test]
fn checksum_detects_single_bit_corruption() {
    let sb = Superblock::for_new_volume(&sample_geom(), [0u8; 16], "vol", 0);
    let mut bytes = sb.encode_block();
    bytes[42] ^= 0x01;
    let err = Superblock::decode_block(&bytes).unwrap_err();
    assert!(format!("{err}").contains("checksum"));
}

#[test]
fn bad_magic_is_rejected() {
    let sb = Superblock::for_new_volume(&sample_geom(), [0u8; 16], "vol", 0);
    let mut bytes = sb.encode_block();
    bytes[0..8].copy_from_slice(b"BADBADBD");
    // Recompute checksum over corrupted block so only magic is wrong.
    let mut corrupted = bytes.clone();
    corrupted[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4].fill(0);
    let expected = super::super::checksum::Crc32c::hash(&corrupted);
    bytes[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&expected.to_le_bytes());
    let err = Superblock::decode_block(&bytes).unwrap_err();
    assert!(format!("{err}").contains("magic"));
}

#[test]
fn unknown_incompat_feature_is_rejected() {
    let mut sb = Superblock::for_new_volume(&sample_geom(), [0u8; 16], "vol", 0);
    sb.feature_incompat |= 1u64 << 63;
    let bytes = sb.encode_block();
    let err = Superblock::decode_block(&bytes).unwrap_err();
    assert!(format!("{err}").contains("incompat"));
}

#[test]
fn geometry_roundtrips_through_superblock() {
    let geom = sample_geom();
    let sb = Superblock::for_new_volume(&geom, [0u8; 16], "vol", 0);
    let recovered = sb.geometry();
    assert_eq!(recovered.total_blocks, geom.total_blocks);
    assert_eq!(recovered.data_start, geom.data_start);
    assert_eq!(recovered.inode_table_start, geom.inode_table_start);
    assert_eq!(recovered.inode_count, geom.inode_count);
}

#[test]
fn wrong_block_size_is_rejected() {
    let short = vec![0u8; 1024];
    assert!(Superblock::decode_block(&short).is_err());
}

#[test]
fn initial_state_is_clean_and_gen_is_one() {
    let sb = Superblock::for_new_volume(&sample_geom(), [0u8; 16], "vol", 0);
    assert_eq!(sb.state, STATE_CLEAN);
    assert_eq!(sb.generation, 1);
    assert_eq!(sb.mount_count, 0);
}
