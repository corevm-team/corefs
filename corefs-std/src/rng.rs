// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Std-basierte Zufallsquelle.
//!
//! [`StdRng`] verwendet einen `xorshift64`-PRNG, gekeimt aus
//! `std::time::SystemTime` und `std::process::id()`. Geeignet für
//! Test-Setup, Inode-/Snapshot-IDs und nicht-sicherheitsrelevante
//! Zufallswerte. Für kryptografische Zwecke (Nonces, Schlüssel) muss
//! ein dedizierter CSPRNG-Adapter verwendet werden.

use corefs_core::platform::Rng;
use std::time::{SystemTime, UNIX_EPOCH};

/// Nicht-kryptografische, aber deterministisch reseedbare Zufallsquelle
/// auf `xorshift64`-Basis.
///
/// **Nicht für kryptografische Zwecke**. Für CSPRNG-Anwendungen (Nonces,
/// Schlüssel-Ableitung) ist ein OS-RNG-Adapter zu verwenden.
#[derive(Debug, Clone)]
pub struct StdRng {
    state: u64,
}

impl StdRng {
    /// Erzeugt eine neue [`StdRng`] mit einem Seed aus `SystemTime` ⊕ `pid`.
    ///
    /// Reseedet bei jedem Aufruf — wiederholte Konstruktion liefert
    /// unterschiedliche Streams.
    #[must_use]
    pub fn from_entropy() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEAD_BEEF);
        let pid = std::process::id() as u64;
        // Avoid a zero-state which would lock xorshift64 at zero.
        let seed = (nanos ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15)).max(1);
        Self::from_seed(seed)
    }

    /// Erzeugt eine [`StdRng`] mit explizitem Seed.
    ///
    /// Ein Seed von 0 wird zu 1 ersetzt, da xorshift64 sich aus dem
    /// Zustand 0 nicht mehr lösen kann.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u64_internal(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

impl Default for StdRng {
    fn default() -> Self {
        Self::from_entropy()
    }
}

impl Rng for StdRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let bytes = self.next_u64_internal().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.next_u64_internal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_seed_is_deterministic() {
        let mut a = StdRng::from_seed(42);
        let mut b = StdRng::from_seed(42);
        assert_eq!(a.next_u64(), b.next_u64());
        assert_eq!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = StdRng::from_seed(1);
        let mut b = StdRng::from_seed(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn fill_bytes_advances_state() {
        let mut rng = StdRng::from_seed(100);
        let mut buf = [0u8; 16];
        rng.fill_bytes(&mut buf);
        let nonzero = buf.iter().any(|&b| b != 0);
        assert!(nonzero, "filled buffer must not be all-zero");
    }

    #[test]
    fn fill_bytes_partial_chunk_works() {
        // 13 bytes — last chunk is 5 bytes, exercises the partial-chunk path.
        let mut rng = StdRng::from_seed(7);
        let mut buf = [0u8; 13];
        rng.fill_bytes(&mut buf);
    }

    #[test]
    fn from_entropy_succeeds() {
        // Sanity: two consecutive `from_entropy` calls almost certainly
        // produce different streams (probability of collision negligible).
        let mut a = StdRng::from_entropy();
        let mut b = StdRng::from_entropy();
        let _ = a.next_u64();
        let _ = b.next_u64();
    }

    #[test]
    fn zero_seed_is_replaced() {
        let mut rng = StdRng::from_seed(0);
        let v = rng.next_u64();
        // If seed weren't replaced, xorshift64 would return 0 forever.
        assert_ne!(v, 0);
    }

    #[test]
    fn implements_rng_via_trait_object() {
        let mut rng: Box<dyn Rng> = Box::new(StdRng::from_seed(99));
        let mut buf = [0u8; 4];
        rng.fill_bytes(&mut buf);
    }
}
