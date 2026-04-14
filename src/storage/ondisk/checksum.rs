// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! CRC32C (Castagnoli) — the checksum primitive of the CoreFS On-Disk Format.
//!
//! A software table-based implementation is used so no additional
//! dependency is required.  Polynomial: `0x1EDC6F41` reflected form
//! `0x82F63B78`.

/// Precomputed lookup table for CRC32C (Castagnoli).
pub struct Crc32c;

const CRC32C_POLY: u32 = 0x82F6_3B78;

static TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
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
        table[i as usize] = c;
        i += 1;
    }
    table
}

impl Crc32c {
    /// Compute the CRC32C over `data` starting from `seed`.  A fresh
    /// checksum is obtained by passing `!0u32` as the initial seed and
    /// XORing the result with `!0u32` again.
    pub fn update(seed: u32, data: &[u8]) -> u32 {
        let mut crc = seed;
        for &byte in data {
            let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
            crc = (crc >> 8) ^ TABLE[idx];
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
