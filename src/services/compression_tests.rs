// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn compress_decompress_roundtrip() {
    let service = CompressionService;
    let data = b"hello corefs compression! ".repeat(100);
    let compressed = service.compress(&data).expect("compress");
    assert!(
        compressed.len() < data.len(),
        "repeated payload should compress smaller"
    );
    let restored = service.decompress(&compressed).expect("decompress");
    assert_eq!(restored, data);
}

#[test]
fn small_payload_is_still_handled_correctly() {
    let service = CompressionService;
    let data = b"tiny";
    assert!(!service.should_compress(data));
    // Even below threshold we can still compress/decompress if caller insists.
    let compressed = service.compress(data).expect("compress");
    let restored = service.decompress(&compressed).expect("decompress");
    assert_eq!(restored, data);
}
