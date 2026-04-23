// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! CRC32C (Castagnoli) — the checksum primitive of the CoreFS On-Disk Format.
//!
//! A software table-based implementation is used so no additional
//! dependency is required.  Polynomial: `0x1EDC6F41` reflected form
//! `0x82F63B78`.

/// Precomputed lookup tables for CRC32C (Castagnoli).
pub struct Crc32c;

const CRC32C_POLY: u32 = 0x82F6_3B78;

// Slicing-by-8 tables.  TABLES[0] is the classic byte-at-a-time table
// (`TABLES[0][b] = crc32c({b})`); TABLES[k] (k ≥ 1) is the classic
// table with `k` extra bytes of zero-advance applied per lookup,
// allowing 8-byte-wide input consumption per iteration.
//
// Slicing-by-8 typically delivers 3–4× the throughput of
// byte-at-a-time on any target that has a fast multiply-free integer
// ALU — i.e. everywhere.  It is a pure-Rust no-std-friendly win and
// therefore the fallback of choice; hardware CRC32C via SSE4.2
// intrinsics would be a second, target-gated win on top.
static TABLES: [[u32; 256]; 8] = build_slicing_by_8_tables();

const fn build_slicing_by_8_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    // Classic CRC32C table.
    let mut i = 0u32;
    while i < 256 {
        let mut c = i;
        let mut k = 0;
        while k < 8 {
            c = if (c & 1) != 0 {
                (c >> 1) ^ CRC32C_POLY
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][i as usize] = c;
        i += 1;
    }
    // Higher slices: TABLES[n][b] = advance(TABLES[n-1][b]) by one byte.
    let mut slice = 1;
    while slice < 8 {
        let mut b = 0usize;
        while b < 256 {
            let prev = t[slice - 1][b];
            t[slice][b] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            b += 1;
        }
        slice += 1;
    }
    t
}

impl Crc32c {
    /// Compute the CRC32C over `data` starting from `seed`.  A fresh
    /// checksum is obtained by passing `!0u32` as the initial seed and
    /// XORing the result with `!0u32` again.
    ///
    /// Consumes 8 bytes per iteration via slicing-by-8; ~3–4× the
    /// throughput of a pure byte-at-a-time loop at equal cache cost.
    pub fn update(seed: u32, data: &[u8]) -> u32 {
        let mut crc = seed;
        let mut chunks = data.chunks_exact(8);
        for chunk in chunks.by_ref() {
            // Unrolled slicing-by-8.  `crc` XOR-combined with the low
            // four bytes, then each byte of that XOR and each of the
            // next four source bytes indexes one of the eight tables.
            let lo = crc ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            crc = TABLES[7][(lo & 0xFF) as usize]
                ^ TABLES[6][((lo >> 8) & 0xFF) as usize]
                ^ TABLES[5][((lo >> 16) & 0xFF) as usize]
                ^ TABLES[4][((lo >> 24) & 0xFF) as usize]
                ^ TABLES[3][chunk[4] as usize]
                ^ TABLES[2][chunk[5] as usize]
                ^ TABLES[1][chunk[6] as usize]
                ^ TABLES[0][chunk[7] as usize];
        }
        for &byte in chunks.remainder() {
            let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
            crc = (crc >> 8) ^ TABLES[0][idx];
        }
        crc
    }

    /// One-shot checksum computation.
    pub fn hash(data: &[u8]) -> u32 {
        !Self::update(!0u32, data)
    }
}

#[cfg(test)]
#[path = "checksum_tests.rs"]
mod tests;
