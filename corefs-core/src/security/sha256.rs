// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Pure-Rust SHA-256 — `no_std`, keine externen Abhängigkeiten.
//!
//! Referenz: FIPS PUB 180-4. Diese Implementierung ist optimiert für
//! Klarheit und Auditierbarkeit, nicht für maximale Performance. Für
//! Key-Derivation-Workloads (HKDF, keystore) ist sie mehr als ausreichend.

use alloc::vec::Vec;

/// SHA-256 Digest-Größe in Bytes.
pub const DIGEST_BYTES: usize = 32;
/// SHA-256 Blockgröße in Bytes.
pub const BLOCK_BYTES: usize = 64;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Inkrementeller SHA-256-Hasher.
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buffer: [u8; BLOCK_BYTES],
    buffered: usize,
    len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Neuer, leerer Hasher.
    pub const fn new() -> Self {
        Self {
            h: H_INIT,
            buffer: [0; BLOCK_BYTES],
            buffered: 0,
            len: 0,
        }
    }

    /// Bindet weitere Bytes ein.
    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = core::cmp::min(BLOCK_BYTES - self.buffered, data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == BLOCK_BYTES {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
        while data.len() >= BLOCK_BYTES {
            let mut block = [0u8; BLOCK_BYTES];
            block.copy_from_slice(&data[..BLOCK_BYTES]);
            self.compress(&block);
            data = &data[BLOCK_BYTES..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    /// Finalisiert und liefert den 32-Byte-Digest.
    pub fn finalize(mut self) -> [u8; DIGEST_BYTES] {
        let bit_len = self.len.wrapping_mul(8);
        // Padding: 0x80, dann Nullen, sodass Länge ≡ 56 (mod 64), dann 8-Byte BE-Länge.
        let mut pad = [0u8; BLOCK_BYTES * 2];
        pad[0] = 0x80;
        let pad_len = if self.buffered < 56 {
            56 - self.buffered
        } else {
            56 + BLOCK_BYTES - self.buffered
        };
        let pad_slice = &pad[..pad_len];
        self.update(pad_slice);
        let len_be = bit_len.to_be_bytes();
        self.update(&len_be);
        debug_assert_eq!(self.buffered, 0);

        let mut out = [0u8; DIGEST_BYTES];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; BLOCK_BYTES]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = self.h[0];
        let mut b = self.h[1];
        let mut c = self.h[2];
        let mut d = self.h[3];
        let mut e = self.h[4];
        let mut f = self.h[5];
        let mut g = self.h[6];
        let mut h = self.h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }
}

/// Ein-Schuss SHA-256 über einen Slice.
pub fn sha256(data: &[u8]) -> [u8; DIGEST_BYTES] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// HMAC-SHA256 mit Schlüssel `key` über `data`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; DIGEST_BYTES] {
    // Vorverarbeitung des Keys auf Blocklänge.
    let mut k_block = [0u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        let d = sha256(key);
        k_block[..DIGEST_BYTES].copy_from_slice(&d);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; BLOCK_BYTES];
    let mut opad = [0u8; BLOCK_BYTES];
    for i in 0..BLOCK_BYTES {
        ipad[i] = k_block[i] ^ 0x36;
        opad[i] = k_block[i] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(data);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    outer.finalize()
}

/// Multi-chunk HMAC (für HKDF-Expand).
pub fn hmac_sha256_multi(key: &[u8], chunks: &[&[u8]]) -> [u8; DIGEST_BYTES] {
    // Flach konkatenieren; für unsere Workloads sind die Chunks winzig.
    let mut buf = Vec::new();
    for c in chunks {
        buf.extend_from_slice(c);
    }
    hmac_sha256(key, &buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn empty_string_digest() {
        // NIST test vector: SHA-256("") = e3b0c442...
        let d = sha256(b"");
        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(d, expected);
    }

    #[test]
    fn abc_digest() {
        // NIST test vector: SHA-256("abc") = ba7816bf...
        let d = sha256(b"abc");
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(d, expected);
    }

    #[test]
    fn long_message_matches_incremental() {
        let msg = vec![0xAAu8; 1024];
        let d1 = sha256(&msg);
        let mut h = Sha256::new();
        for chunk in msg.chunks(37) {
            h.update(chunk);
        }
        let d2 = h.finalize();
        assert_eq!(d1, d2);
    }

    #[test]
    fn hmac_rfc4231_test_case_1() {
        // HMAC-SHA256 test vector from RFC 4231 (Test Case 1):
        // key = 0x0b * 20, data = "Hi There"
        let key = [0x0b; 20];
        let data = b"Hi There";
        let mac = hmac_sha256(&key, data);
        let expected: [u8; 32] = [
            0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
            0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
            0x2e, 0x32, 0xcf, 0xf7,
        ];
        assert_eq!(mac, expected);
    }
}
