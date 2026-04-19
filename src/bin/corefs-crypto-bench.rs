// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Micro-bench that isolates the cost of compression + encryption on
//! the write path by running the same `write_file` with three config
//! profiles (full / no-encrypt / raw) and diffing the walltimes.
//!
//! Invoked out-of-band — not part of `cargo test`.  Run with:
//!   cargo run --release --bin corefs-crypto-bench -- [payload_mib]
//!
//! Output (space-separated lines):
//!   profile        write_ms   read_ms   stored_bytes

use corefs::app::CoreFsService;
use corefs::config::CoreFsConfig;
use std::time::Instant;

fn run_profile(label: &str, mut cfg: CoreFsConfig, payload: &[u8]) {
    // Disable versioning for this benchmark — versioning clones the
    // full payload per write and skews the pure compress+encrypt
    // measurement we're after here.
    cfg.versioning.keep_latest = 0;
    cfg.versioning.auto_prune = false;
    cfg.versioning.max_version_bytes = None;

    let mut svc = CoreFsService::format(cfg);
    svc.create_file("/bench.bin", b"", &[]).expect("create");

    let t0 = Instant::now();
    svc.write_file("/bench.bin", payload).expect("write");
    let write_ms = t0.elapsed().as_millis();

    // Second write of same-size content — same code path but on an
    // already-allocated extent (blocks.write reuses the existing
    // allocation).  Lets us see whether allocation is the main cost.
    let t_rw = Instant::now();
    svc.write_file("/bench.bin", payload).expect("rewrite");
    let rewrite_ms = t_rw.elapsed().as_millis();

    let t1 = Instant::now();
    let back = svc.read_file("/bench.bin").expect("read");
    let read_ms = t1.elapsed().as_millis();
    assert_eq!(back.len(), payload.len());

    // Approximate "stored bytes" = whatever the block store holds
    // after compression/encryption.  Expose via an internal probe
    // (the full value is logged through the journal in any case).
    println!(
        "{:<15} write_ms={:>4}  rewrite_ms={:>4}  read_ms={:>4}  stored_kib={:>6}",
        label,
        write_ms,
        rewrite_ms,
        read_ms,
        svc.persisted_state()
            .block_records
            .iter()
            .find(|r| r.inode.0 == 1)
            .map(|r| r.logical_size as usize / 1024)
            .unwrap_or(0)
    );
}

fn main() {
    let payload_mib: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    // Use pseudo-random content so LZ4 compression cannot shortcut
    // the pipeline — zeros compress to almost nothing and hide the
    // encryption cost behind the compressed size.
    let mut payload = vec![0u8; payload_mib * 1024 * 1024];
    let mut state: u64 = 0x12345678_9ABCDEF0;
    for b in payload.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (state >> 33) as u8;
    }

    println!("== corefs-crypto-bench: write + read {} MiB ==", payload_mib);

    // Full: compression + encryption (default config).
    let cfg_full = CoreFsConfig::default();
    run_profile("default", cfg_full, &payload);

    // Compress only — no crypto.
    let mut cfg_no_crypt = CoreFsConfig::default();
    cfg_no_crypt.security.encryption_at_rest = false;
    run_profile("no-encrypt", cfg_no_crypt, &payload);

    // Raw: no compression, no crypto.
    let mut cfg_raw = CoreFsConfig::default();
    cfg_raw.security.encryption_at_rest = false;
    cfg_raw.performance.compression_enabled = false;
    run_profile("raw", cfg_raw, &payload);

    // Compressed, no encryption — isolates compression cost alone
    // (the "default" profile adds the encryption cost on top).
    let mut cfg_crypt = CoreFsConfig::default();
    cfg_crypt.performance.compression_enabled = false;
    run_profile("encrypt-only", cfg_crypt, &payload);
}
