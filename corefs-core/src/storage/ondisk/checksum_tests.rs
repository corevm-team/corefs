// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::vec;

use super::Crc32c;

#[test]
fn empty_input_yields_zero() {
    assert_eq!(Crc32c::hash(&[]), 0);
}

#[test]
fn known_vectors_match_castagnoli_reference() {
    // Reference vectors from RFC 3720 / iSCSI CRC32C (Castagnoli):
    //   CRC32C("123456789") == 0xE3069283
    assert_eq!(Crc32c::hash(b"123456789"), 0xE306_9283);
    //   CRC32C of 32 zero bytes == 0x8A9136AA
    assert_eq!(Crc32c::hash(&[0u8; 32]), 0x8A91_36AA);
    //   CRC32C of 32 bytes of 0xFF == 0x62A8AB43
    assert_eq!(Crc32c::hash(&[0xFFu8; 32]), 0x62A8_AB43);
}

#[test]
fn different_inputs_produce_different_hashes() {
    let a = Crc32c::hash(b"corefs-volume-a");
    let b = Crc32c::hash(b"corefs-volume-b");
    assert_ne!(a, b);
}

#[test]
fn incremental_update_matches_oneshot() {
    let buf = b"The quick brown fox jumps over the lazy dog";
    let oneshot = Crc32c::hash(buf);
    let mut seed = !0u32;
    for chunk in buf.chunks(7) {
        seed = Crc32c::update(seed, chunk);
    }
    assert_eq!(!seed, oneshot);
}

#[test]
fn single_bit_flip_changes_hash() {
    let a = vec![0u8; 4096];
    let mut b = a.clone();
    b[2048] = 1;
    assert_ne!(Crc32c::hash(&a), Crc32c::hash(&b));
}
