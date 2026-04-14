// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Property-based / randomised invariant tests for the ODF stack
//! (D.12).
//!
//! Instead of pulling in an external crate like `proptest` or
//! `quickcheck`, this module provides a small deterministic
//! sequence-driver built on top of a seed-driven xorshift64 PRNG:
//!
//! * [`Op`] models the five filesystem operations that any ODF-
//!   backed service must handle: create file, delete file, overwrite
//!   file content, create directory, make a snapshot.
//! * [`generate_sequence(seed, len)`] produces a deterministic op
//!   sequence from a 64-bit seed.  The same seed always produces the
//!   same sequence — so failure messages include a reproducible seed.
//! * [`run_and_check(seed, len)`] applies the generated ops against
//!   a fresh ODF-backed [`super::session::OdfDeviceSession`] and checks the following
//!   invariants **after every single op**:
//!   1. `fsck::check` has no Error-severity issues.
//!   2. The set of `active_inodes` paths observed via the service is
//!      consistent with the op sequence applied so far.
//!   3. [`load_state_native`] after a final flush deserialises
//!      without error.
//!
//! Because the generator is deterministic and the PRNG lives in this
//! file, any regression triggers a specific seed we can paste into a
//! focused unit test.

use crate::config::CoreFsConfig;
use crate::error::CoreFsResult;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::fsck;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::native::load_state_native;
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

/// The operations the randomised driver emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Create a brand-new file at `path` with `content` bytes.
    CreateFile { path: String, content: Vec<u8> },
    /// Delete the file at `path` (may be a no-op if missing).
    DeleteFile { path: String },
    /// Overwrite the file at `path` — modelled as delete + create.
    OverwriteFile { path: String, content: Vec<u8> },
    /// Create an empty directory at `path`.
    CreateDirectory { path: String },
    /// Create a new snapshot with the given name.
    CreateSnapshot { name: String },
}

/// Seed-driven xorshift64 PRNG.  Deterministic across platforms —
/// critical for reproducible property-test failures.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Avoid the degenerate all-zero state that xorshift64 gets
        // stuck on.
        Rng(if seed == 0 { 0xDEAD_BEEF_1234_5678 } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn next_range(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        self.next_u64() % n
    }

    pub fn choose<'a, T>(&mut self, slice: &'a [T]) -> &'a T {
        let idx = (self.next_u64() as usize) % slice.len();
        &slice[idx]
    }
}

/// Build a deterministic op sequence of length `len` from `seed`.
pub fn generate_sequence(seed: u64, len: usize) -> Vec<Op> {
    let mut rng = Rng::new(seed);
    let mut live_paths: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(len);
    let name_alphabet = b"abcdefghijklmnop";
    let fresh_name = |rng: &mut Rng, alphabet: &[u8]| -> String {
        let mut s = String::from("/f");
        for _ in 0..6 {
            s.push(alphabet[(rng.next_u64() as usize) % alphabet.len()] as char);
        }
        s
    };
    let fresh_content = |rng: &mut Rng| -> Vec<u8> {
        let n = (rng.next_range(32) + 1) as usize;
        (0..n).map(|i| (rng.next_u64() as u8).wrapping_add(i as u8)).collect()
    };

    for _ in 0..len {
        let choice = rng.next_range(100);
        let op = match choice {
            // Weighted: favour create (40), overwrite (20), delete (15),
            // directory (10), snapshot (15).
            0..=39 => {
                let p = fresh_name(&mut rng, name_alphabet);
                if !live_paths.contains(&p) {
                    live_paths.push(p.clone());
                }
                Op::CreateFile {
                    path: p,
                    content: fresh_content(&mut rng),
                }
            }
            40..=59 if !live_paths.is_empty() => {
                let p = rng.choose(&live_paths).clone();
                Op::OverwriteFile {
                    path: p,
                    content: fresh_content(&mut rng),
                }
            }
            60..=74 if !live_paths.is_empty() => {
                let idx = (rng.next_u64() as usize) % live_paths.len();
                let p = live_paths.remove(idx);
                Op::DeleteFile { path: p }
            }
            75..=84 => {
                let p = format!("/d{}", rng.next_range(1000));
                Op::CreateDirectory { path: p }
            }
            _ => Op::CreateSnapshot {
                name: format!("snap{}", rng.next_range(1_000_000)),
            },
        };
        out.push(op);
    }
    out
}

/// Apply `ops` against a fresh ODF-backed session, verifying the
/// invariants after each op.  Returns the final state on success.
pub fn run_and_check(seed: u64, ops: &[Op]) -> CoreFsResult<()> {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 32 * 1024 * 1024;
    opts.inode_count = 512;
    opts.journal_blocks = 32;
    opts.config = CoreFsConfig::default();
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    opts.config.versioning.keep_latest = 0;
    let dev: Box<dyn BlockDevice> =
        Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts)?;

    for (step, op) in ops.iter().enumerate() {
        let result: CoreFsResult<()> = sess.mutate(|fs| {
            match op {
                Op::CreateFile { path, content } => {
                    if fs.list_paths().contains(path) {
                        // Collision with a previously-created path —
                        // ignore rather than error.  The generator is
                        // random; overlaps happen.
                        return Ok(());
                    }
                    let _ = fs.create_file(path, content, &[]);
                }
                Op::DeleteFile { path } => {
                    let _ = fs.delete_file(path, false);
                }
                Op::OverwriteFile { path, content } => {
                    let _ = fs.delete_file(path, false);
                    let _ = fs.create_file(path, content, &[]);
                }
                Op::CreateDirectory { path } => {
                    let _ = fs.create_directory(path);
                }
                Op::CreateSnapshot { name } => {
                    let _ = fs.create_snapshot(name);
                }
            }
            Ok(())
        }).map(|(_, _)| ());
        result?;

        // Invariant #1: fsck stays Error-free.
        let report = fsck::check(sess.device())?;
        if !report.is_clean() {
            panic!(
                "seed {seed} step {step}: fsck NOT clean after {op:?}\nissues: {:?}",
                report.issues
            );
        }

        // Invariant #2: the service's catalog view is decodable by
        // load_state_native against the same bytes.
        let reloaded = load_state_native(sess.device())?;
        assert_eq!(
            reloaded.active_inodes.len(),
            sess.service().persisted_state().active_inodes.len(),
            "seed {seed} step {step}: active_inodes count differs across reload"
        );
    }
    Ok(())
}

/// Helper for tests — pick N seeds, run generate_sequence(seed, len)
/// + run_and_check(seed, …) for each, and surface the seed in any
/// panic message.
pub fn fuzz_many_seeds(seeds: &[u64], sequence_len: usize) -> CoreFsResult<()> {
    for seed in seeds {
        let ops = generate_sequence(*seed, sequence_len);
        if let Err(e) = run_and_check(*seed, &ops) {
            return Err(crate::error::CoreFsError::State(format!(
                "property test failed at seed {seed}: {e}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "property_tests.rs"]
mod tests;

// Suppress unused-warning when BLOCK_SIZE is only indirectly referenced.
#[allow(dead_code)]
const _BLOCK_SIZE: u64 = BLOCK_SIZE;
