// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Persistence roundtrip tests — write a position-dependent pattern,
// read it back, verify it byte-for-byte.  Designed to catch silent
// data loss like the pre-Phase-3d compat-device truncation at 64 MiB:
//
//   * A position-dependent pattern (8-byte LE offset at every
//     8-byte boundary) makes the first corrupt byte pinpoint the
//     exact file offset where something went wrong.
//   * Coverage spans the historical truncation boundary (64 MiB)
//     and enough beyond it to make sure the growth path itself does
//     not corrupt later bytes.
//   * Each size is exercised four ways:
//       a) in-session write + read
//       b) write + save_image_to_path + load + read
//          (catches persist-time truncation in split_blocks,
//          build_image, or the save/load roundtrip)
//       c) extend-append + reload + read
//          (catches the P2 extent-append fast path)
//       d) multi-file interleave + reload + read
//          (catches cross-file layout confusion)
//
// If any byte is wrong the panic message includes the offset so the
// bug can be located immediately.

use super::*;
use crate::storage::volume_image;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Position-dependent pattern.
///
/// Byte `i` of the output is `((i / 8) ^ seed).to_le_bytes()[i % 8]`.
/// This means every 8-byte chunk carries its own chunk index XORed
/// with `seed`, so:
///   * any truncation is visible immediately (trailing chunk wrong),
///   * any offset shift (e.g. bytes from inode A appearing at inode
///     B's offset) is caught because the chunk number mismatches,
///   * different files in the same test can use different seeds to
///     prove cross-file isolation.
fn pattern(size: usize, seed: u64) -> Vec<u8> {
    let mut out = vec![0u8; size];
    let chunks = size / 8;
    for c in 0..chunks {
        let v = (c as u64) ^ seed;
        out[c * 8..c * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    // Tail: position-and-seed dependent too, so short payloads still
    // differ from each other.
    for i in (chunks * 8)..size {
        out[i] = (i as u8).wrapping_add((seed & 0xFF) as u8);
    }
    out
}

/// Verify `actual` matches `pattern(size, seed)` and, if not, panic
/// with the exact offset of the first mismatch.
#[track_caller]
fn assert_pattern(actual: &[u8], size: usize, seed: u64, label: &str) {
    if actual.len() != size {
        panic!(
            "{label}: length mismatch — expected {size} bytes, got {} ({} short/extra)",
            actual.len(),
            (actual.len() as i64) - (size as i64)
        );
    }
    let expected = pattern(size, seed);
    if actual != expected {
        // Find first mismatch.
        let diff = actual
            .iter()
            .zip(expected.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(actual.len());
        let preview_start = diff.saturating_sub(8);
        let preview_end = (diff + 16).min(size);
        panic!(
            "{label}: first byte mismatch at offset {diff} (of {size}): \
             got {:?}, expected {:?}",
            &actual[preview_start..preview_end],
            &expected[preview_start..preview_end],
        );
    }
}

fn test_fs() -> CoreFsService {
    // These tests focus on catching truncation / byte-shifting bugs
    // across reload boundaries.  The *existing*
    // `content_roundtrip_characterization_tests` already exercises the
    // compress + encrypt pipeline at ≤ 4 MiB payloads.  Doing 100 MiB
    // through ChaCha20-Poly1305 + LZ4 in a debug build takes minutes
    // per test and dominates `cargo test` wall time without adding
    // coverage.  We disable both here to keep the suite responsive.
    let mut cfg = CoreFsConfig::default();
    cfg.security.encryption_at_rest = false;
    cfg.performance.compression_enabled = false;
    // Versioning clones the whole payload on every create/write; keep
    // it off too so the test exercises only the block pipeline.
    cfg.versioning.keep_latest = 0;
    cfg.versioning.auto_prune = false;
    cfg.versioning.max_version_bytes = None;
    CoreFsService::format(cfg)
}

fn save_and_reload(fs: &CoreFsService) -> CoreFsService {
    use std::env::temp_dir;
    let tmp = temp_dir().join(format!(
        "corefs-persistence-roundtrip-{}-{}.img",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs.save_image_to_path(&tmp).expect("save");
    let reloaded = CoreFsService::load_image_from_path(&tmp).expect("reload");
    let _ = std::fs::remove_file(&tmp);
    reloaded
}

// Size menu.
//
// Below 64 MiB: pre-Phase-3d safe zone.  `64MiB` / `64MiB+1` straddle
// the historical compat-device truncation boundary and are the
// smallest sizes that would have caught that bug.  `100MiB` confirms
// the grow path keeps working past a single doubling step.
//
// Larger sizes (200+ MiB) would also be valid but debug-mode builds
// with compression+encryption take several minutes per size, which
// makes `cargo test` too heavy for normal development.  The on-disk
// growth past 100 MiB is covered indirectly by `save_reload` on
// 100 MiB + earlier smaller files in the same volume.
const SIZES: &[(usize, &str)] = &[
    (0, "0"),
    (1, "1B"),
    (4095, "4095B"),
    (4096, "4KiB"),
    (4097, "4KiB+1"),
    (1024 * 1024, "1MiB"),
    (63 * 1024 * 1024 - 1, "63MiB-1"),
    (64 * 1024 * 1024, "64MiB"),
    (64 * 1024 * 1024 + 1, "64MiB+1"),
    (100 * 1024 * 1024, "100MiB"),
];

// ---------------------------------------------------------------------------
// In-session write + read (same-process, no persist)
// ---------------------------------------------------------------------------

#[test]
fn persistence_in_session_all_sizes() {
    let mut fs = test_fs();
    for (size, label) in SIZES {
        let seed = (*size as u64) ^ 0xC0FE_BABE_C0FE_BABE;
        let path = format!("/in-session-{label}");
        let data = pattern(*size, seed);
        fs.create_file(&path, &data, &[]).expect("create");
        let back = fs.read_file(&path).expect("read");
        assert_pattern(&back, *size, seed, &format!("in-session {label}"));
    }
}

// ---------------------------------------------------------------------------
// Persist via save_image / load_image and verify across reload
// ---------------------------------------------------------------------------

#[test]
fn persistence_save_reload_all_sizes() {
    let mut fs = test_fs();
    let mut seeds: Vec<(String, usize, u64)> = Vec::new();
    for (size, label) in SIZES {
        let seed = (*size as u64).wrapping_mul(0x1234_5678_9ABC_DEF0);
        let path = format!("/reload-{label}");
        let data = pattern(*size, seed);
        fs.create_file(&path, &data, &[]).expect("create");
        seeds.push((path, *size, seed));
    }

    let reloaded = save_and_reload(&fs);
    for (path, size, seed) in &seeds {
        let back = reloaded.read_file(path).expect("read after reload");
        assert_pattern(&back, *size, *seed, &format!("reload {path}"));
    }
}

// ---------------------------------------------------------------------------
// Extend (append) path — exercises append_to_inode + reload
// ---------------------------------------------------------------------------

#[test]
fn persistence_extend_then_reload() {
    // Build a file in multiple extend chunks, then reload and verify.
    // Chunks chosen so the total crosses the 64 MiB compat-device
    // boundary: 60 MiB seed + 8 MiB extend = 68 MiB total.
    let mut fs = test_fs();
    let head_size = 60 * 1024 * 1024;
    let tail_size = 8 * 1024 * 1024;
    let total = head_size + tail_size;
    let seed = 0xFACE_D00D_FACE_D00D;

    let full = pattern(total, seed);
    fs.create_file("/extended.bin", &full[..head_size], &[])
        .expect("create");
    fs.extend_file("/extended.bin", &full[head_size..])
        .expect("extend");

    // In-session sanity.
    let back = fs.read_file("/extended.bin").expect("read");
    assert_pattern(&back, total, seed, "extend in-session");

    // Reload and verify.
    let reloaded = save_and_reload(&fs);
    let back = reloaded.read_file("/extended.bin").expect("read reload");
    assert_pattern(&back, total, seed, "extend reload");
}

// ---------------------------------------------------------------------------
// Multiple files with different seeds — cross-file isolation
// ---------------------------------------------------------------------------

#[test]
fn persistence_multi_file_interleave_then_reload() {
    // Write four files with distinct seeds.  If cross-file layout
    // ever confuses an inode's bytes with its neighbour's, the
    // position pattern will flag it immediately.
    let mut fs = test_fs();
    let files = [
        ("/a.bin", 70 * 1024 * 1024, 0x1111_1111_1111_1111u64),
        ("/b.bin", 5 * 1024 * 1024, 0x2222_2222_2222_2222u64),
        ("/c.bin", 1 * 1024 * 1024, 0x3333_3333_3333_3333u64),
        ("/d.bin", 4 * 1024, 0x4444_4444_4444_4444u64),
    ];
    for (p, s, seed) in &files {
        let data = pattern(*s, *seed);
        fs.create_file(p, &data, &[]).expect("create");
    }

    let reloaded = save_and_reload(&fs);
    for (p, s, seed) in &files {
        let back = reloaded.read_file(p).expect("read reload");
        assert_pattern(&back, *s, *seed, &format!("multi-file {p}"));
    }
}

// ---------------------------------------------------------------------------
// Write-then-overwrite — the replacement must be complete, and the
// reload must return the replacement, not the old content.
// ---------------------------------------------------------------------------

#[test]
fn persistence_overwrite_larger_then_reload() {
    let mut fs = test_fs();
    let small = pattern(4096, 0xAAAA);
    fs.create_file("/rw.bin", &small, &[]).expect("create");

    // Overwrite with a much bigger payload that crosses the old
    // 64 MiB boundary.
    let big_size = 70 * 1024 * 1024;
    let big_seed = 0xBBBB_BBBB_BBBB_BBBBu64;
    let big = pattern(big_size, big_seed);
    fs.write_file("/rw.bin", &big).expect("overwrite");

    let reloaded = save_and_reload(&fs);
    let back = reloaded.read_file("/rw.bin").expect("read reload");
    assert_pattern(&back, big_size, big_seed, "overwrite reload");
}

// ---------------------------------------------------------------------------
// Direct low-level device roundtrip — writes a volume image to a
// MemoryDevice and loads it back, verifying the on-disk format
// itself preserves bytes for large files.
// ---------------------------------------------------------------------------

#[test]
fn persistence_device_volume_roundtrip_large_file() {
    use crate::storage::block_device::MemoryDevice;

    let mut fs = test_fs();
    let size = 96 * 1024 * 1024;
    let seed = 0xDEAD_BEEF_CAFE_BABE;
    let data = pattern(size, seed);
    fs.create_file("/big.bin", &data, &[]).expect("create");

    let state = fs.persisted_state();
    let block_bytes = fs.read_all_block_bytes();

    let mut dev = MemoryDevice::new(256 * 1024 * 1024, 4096).expect("device");
    volume_image::save_to_device_with_bytes(&mut dev, &state, &block_bytes)
        .expect("save_to_device");
    let (loaded_state, loaded_bytes) =
        volume_image::load_from_device_with_bytes(&dev).expect("load_from_device");

    let mut reloaded = CoreFsService::from_persisted_state(loaded_state);
    reloaded.restore_block_bytes(loaded_bytes);
    let back = reloaded.read_file("/big.bin").expect("read");
    assert_pattern(&back, size, seed, "device roundtrip 96MiB");
}
