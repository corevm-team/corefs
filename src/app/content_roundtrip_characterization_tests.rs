// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Characterization tests for CoreFsService content roundtrips.
//
// These tests exercise the full pipeline (compression + encryption +
// BlockStore + catalog) through the public CoreFsService API and pin the
// current observable behaviour.  They must pass against the current
// implementation and continue to pass — unchanged — after the
// extent-based refactor.  A pre-refactor green that turns red post-refactor
// signals a regression.

use super::*;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Default service: CoreFsConfig::default() has compression_enabled = true
/// and encryption_at_rest = true with a derived key from "corefs".
fn test_fs() -> CoreFsService {
    CoreFsService::format(CoreFsConfig::default())
}

/// Service without encryption — needed for dedup tests (different nonces
/// prevent identical encrypted ciphertexts from deduplicating).
fn test_fs_no_encryption() -> CoreFsService {
    let mut config = CoreFsConfig::default();
    config.security.encryption_at_rest = false;
    CoreFsService::format(config)
}

/// Deterministic pseudo-random bytes (Knuth MMIX LCG).
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
// Basic write / read roundtrips
// ---------------------------------------------------------------------------

#[test]
fn char_service_create_read_empty() {
    let mut fs = test_fs();
    fs.create_file("/empty.bin", b"", &[]).unwrap();
    assert_eq!(fs.read_file("/empty.bin").unwrap(), b"");
}

#[test]
fn char_service_create_read_1_byte() {
    let mut fs = test_fs();
    fs.create_file("/one.bin", b"\xAB", &[]).unwrap();
    assert_eq!(fs.read_file("/one.bin").unwrap(), b"\xAB");
}

#[test]
fn char_service_create_read_1_block() {
    let mut fs = test_fs();
    let payload = vec![0x5A_u8; 4096];
    fs.create_file("/block.bin", &payload, &[]).unwrap();
    assert_eq!(fs.read_file("/block.bin").unwrap(), payload);
}

#[test]
fn char_service_create_read_1_block_plus_one() {
    let mut fs = test_fs();
    let payload = vec![0x7F_u8; 4097];
    fs.create_file("/cross.bin", &payload, &[]).unwrap();
    assert_eq!(fs.read_file("/cross.bin").unwrap(), payload);
}

#[test]
fn char_service_create_read_4mb_random_seeded() {
    let mut fs = test_fs();
    let payload = prng_bytes(0x1234_ABCD, 4 * 1024 * 1024);
    fs.create_file("/big.bin", &payload, &[]).unwrap();
    assert_eq!(fs.read_file("/big.bin").unwrap(), payload);
}

// ---------------------------------------------------------------------------
// Write-file update semantics
// ---------------------------------------------------------------------------

#[test]
fn char_service_write_file_replaces_content() {
    let mut fs = test_fs();
    fs.create_file("/rw.txt", b"original content", &[]).unwrap();
    fs.write_file("/rw.txt", b"replaced content").unwrap();
    assert_eq!(fs.read_file("/rw.txt").unwrap(), b"replaced content");
}

#[test]
fn char_service_write_file_smaller_replaces_content() {
    let mut fs = test_fs();
    let big = prng_bytes(0xAABB, 8192);
    let small = b"tiny";
    fs.create_file("/shrink.bin", &big, &[]).unwrap();
    fs.write_file("/shrink.bin", small).unwrap();
    assert_eq!(fs.read_file("/shrink.bin").unwrap(), small);
}

// ---------------------------------------------------------------------------
// Extend (append)
// ---------------------------------------------------------------------------

#[test]
fn char_service_extend_file_appends_bytes() {
    let mut fs = test_fs_no_encryption();
    fs.create_file("/append.txt", b"hello", &[]).unwrap();
    fs.extend_file("/append.txt", b" world").unwrap();
    assert_eq!(fs.read_file("/append.txt").unwrap(), b"hello world");
}

#[test]
fn char_service_extend_empty_file_produces_content() {
    let mut fs = test_fs_no_encryption();
    fs.create_file("/ex.bin", b"", &[]).unwrap();
    fs.extend_file("/ex.bin", b"appended").unwrap();
    assert_eq!(fs.read_file("/ex.bin").unwrap(), b"appended");
}

// ---------------------------------------------------------------------------
// Deduplication
// ---------------------------------------------------------------------------

#[test]
fn char_service_dedup_two_identical_files_share_storage() {
    // Use a service without encryption so the stored bytes are identical
    // for identical plaintext (dedup works at the stored-bytes level).
    let mut fs = test_fs_no_encryption();
    let payload = prng_bytes(0xDEAD_CAFE, 8 * 1024);
    fs.create_file("/a.bin", &payload, &[]).unwrap();
    fs.create_file("/b.bin", &payload, &[]).unwrap();
    let stats = fs.cow_report();
    assert_eq!(
        stats.stats.shared_blobs, 1,
        "two files with identical content must share one blob"
    );
}

#[test]
fn char_service_modify_one_of_shared_breaks_sharing() {
    let mut fs = test_fs_no_encryption();
    let payload = prng_bytes(0xBEEF_FEED, 8 * 1024);
    fs.create_file("/x.bin", &payload, &[]).unwrap();
    fs.create_file("/y.bin", &payload, &[]).unwrap();
    // Overwrite /x.bin → CoW materialises separate blob.
    fs.write_file("/x.bin", b"different content").unwrap();
    let stats = fs.cow_report();
    assert_eq!(
        stats.stats.shared_blobs, 0,
        "after overwrite the files must not share a blob"
    );
    // /y.bin must still hold the original content.
    assert_eq!(fs.read_file("/y.bin").unwrap(), payload);
}

// ---------------------------------------------------------------------------
// Compression pipeline
// ---------------------------------------------------------------------------

#[test]
fn char_service_compress_roundtrip_compressible_payload() {
    // Default config has compression_enabled = true.
    let mut fs = test_fs();
    // Highly compressible payload (repeated bytes exceed 64-byte threshold).
    let payload: Vec<u8> = b"XY".iter().copied().cycle().take(2048).collect();
    fs.create_file("/comp.bin", &payload, &[]).unwrap();
    assert_eq!(
        fs.read_file("/comp.bin").unwrap(),
        payload,
        "compressed+encrypted content must round-trip byte-for-byte"
    );
}

#[test]
fn char_service_compress_roundtrip_incompressible_payload() {
    let mut fs = test_fs();
    // Random bytes — compression library may or may not shrink them.
    let payload = prng_bytes(0xC0FFEE_42, 4096);
    fs.create_file("/rand.bin", &payload, &[]).unwrap();
    assert_eq!(
        fs.read_file("/rand.bin").unwrap(),
        payload,
        "incompressible content must round-trip byte-for-byte"
    );
}

// ---------------------------------------------------------------------------
// Encryption pipeline
// ---------------------------------------------------------------------------

#[test]
fn char_service_encrypt_roundtrip() {
    // Default config has encryption_at_rest = true.
    let mut fs = test_fs();
    let payload = b"top secret data";
    fs.create_file("/secret.txt", payload, &[]).unwrap();
    assert_eq!(
        fs.read_file("/secret.txt").unwrap(),
        payload,
        "encrypted content must round-trip byte-for-byte"
    );
}

#[test]
fn char_service_compress_then_encrypt_pipeline() {
    // Default config: both compression and encryption enabled.
    let mut fs = test_fs();
    let payload: Vec<u8> = b"compress-then-encrypt-me"
        .iter()
        .copied()
        .cycle()
        .take(512)
        .collect();
    fs.create_file("/pipeline.bin", &payload, &[]).unwrap();
    assert_eq!(
        fs.read_file("/pipeline.bin").unwrap(),
        payload,
        "compress+encrypt pipeline must round-trip byte-for-byte"
    );
}

// ---------------------------------------------------------------------------
// In-memory state roundtrip (persisted_state → from_persisted_state)
// ---------------------------------------------------------------------------

#[test]
fn char_service_in_memory_state_roundtrip_preserves_all_files() {
    let mut fs = test_fs();
    let files: &[(&str, &[u8])] = &[
        ("/a.txt", b"alpha content"),
        ("/b.bin", &[0xDE, 0xAD, 0xBE, 0xEF]),
        ("/c.txt", b"gamma content"),
    ];
    for (path, content) in files {
        fs.create_file(path, content, &[]).unwrap();
    }
    let block_bytes = fs.read_all_block_bytes();
    let state = fs.persisted_state();
    let mut reloaded = CoreFsService::from_persisted_state(state);
    reloaded.restore_block_bytes(block_bytes);
    for (path, expected) in files {
        assert_eq!(
            reloaded.read_file(path).unwrap(),
            *expected,
            "file {path} must survive in-memory state roundtrip"
        );
    }
}

// ---------------------------------------------------------------------------
// Disk image roundtrip (save_image_to_path → load_image_from_path)
// ---------------------------------------------------------------------------

#[test]
fn char_service_save_then_load_image_preserves_all_files() {
    let path = std::env::temp_dir().join(format!(
        "corefs-char-save-{}-{}.img",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let mut fs = test_fs();
    let files: &[(&str, &[u8])] = &[
        ("/save-a.txt", b"saved alpha"),
        ("/save-b.bin", &[0x01, 0x02, 0x03]),
        ("/save-c.txt", b"saved gamma"),
    ];
    for (path, content) in files {
        fs.create_file(path, content, &[]).unwrap();
    }
    fs.save_image_to_path(&path).expect("save image");

    let loaded = CoreFsService::load_image_from_path(&path).expect("load image");
    for (p, expected) in files {
        assert_eq!(
            loaded.read_file(p).unwrap(),
            *expected,
            "file {p} must survive disk image roundtrip"
        );
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Integrity scrub
// ---------------------------------------------------------------------------

#[test]
fn char_service_integrity_scrub_passes_on_clean_image() {
    let mut fs = test_fs();
    fs.create_file("/a.txt", b"clean data", &[]).unwrap();
    fs.create_file("/b.bin", &prng_bytes(0x1111, 1024), &[])
        .unwrap();
    let report = fs.scrub();
    assert_eq!(
        report.invalid_blocks, 0,
        "scrub must report zero invalid blocks on a freshly written image"
    );
}

#[test]
fn char_service_integrity_scrub_detects_corrupted_checksum() {
    // Simulate a corrupted block by manipulating the persisted state:
    // keep bytes intact but break the content_crc so verify() returns false.
    let mut fs = test_fs();
    fs.create_file("/corrupt.bin", b"some content here", &[])
        .unwrap();

    let mut state = fs.persisted_state();
    // Corrupt the content_crc of the first block record.
    if let Some(rec) = state.block_records.first_mut() {
        rec.content_crc = rec.content_crc.wrapping_add(1);
        // Also corrupt the extent's CRC so the compat-device verify fails.
        if let Some(ext) = rec.extents.first_mut() {
            ext.content_crc = ext.content_crc.wrapping_add(1);
        }
    }
    let corrupted_fs = CoreFsService::from_persisted_state(state);
    let report = corrupted_fs.scrub();
    assert!(
        report.invalid_blocks > 0,
        "scrub must detect at least one invalid block after checksum corruption"
    );
}
