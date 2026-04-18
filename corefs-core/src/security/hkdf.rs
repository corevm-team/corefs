// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! HKDF-SHA256 (RFC 5869) — `no_std + alloc`.
//!
//! Zwei Funktionen: [`extract`] → PRK, [`expand`] → OKM beliebiger Länge.
//! Für die Keystore-Workloads reicht [`derive`] als Bequemlichkeit.

use super::sha256::{DIGEST_BYTES, hmac_sha256, hmac_sha256_multi};
use alloc::vec::Vec;

/// HKDF-Extract: PRK = HMAC-SHA256(salt, ikm).
///
/// `salt` darf leer sein — in diesem Fall wird intern ein Null-Salt
/// der Digest-Größe verwendet (RFC 5869 § 2.2).
pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; DIGEST_BYTES] {
    if salt.is_empty() {
        let zero = [0u8; DIGEST_BYTES];
        hmac_sha256(&zero, ikm)
    } else {
        hmac_sha256(salt, ikm)
    }
}

/// HKDF-Expand: OKM ← HMAC-basierte Iteration bis `out_len`.
///
/// Begrenzt auf `255 * DIGEST_BYTES` Ausgabebytes (RFC 5869 § 2.3).
///
/// # Panics
/// Wenn `out_len > 255 * DIGEST_BYTES` oder `prk.len() < DIGEST_BYTES`.
pub fn expand(prk: &[u8; DIGEST_BYTES], info: &[u8], out_len: usize) -> Vec<u8> {
    assert!(out_len <= 255 * DIGEST_BYTES, "HKDF out_len too large");

    let n = out_len.div_ceil(DIGEST_BYTES);
    let mut t_prev: [u8; DIGEST_BYTES] = [0u8; DIGEST_BYTES];
    let mut out = Vec::with_capacity(n * DIGEST_BYTES);
    for i in 1..=n {
        let counter = [i as u8];
        let t = if i == 1 {
            hmac_sha256_multi(prk, &[info, &counter])
        } else {
            hmac_sha256_multi(prk, &[&t_prev, info, &counter])
        };
        t_prev = t;
        out.extend_from_slice(&t);
    }
    out.truncate(out_len);
    out
}

/// Convenience-Wrapper: Extract + Expand in einem Aufruf.
pub fn derive(salt: &[u8], ikm: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let prk = extract(salt, ikm);
    expand(&prk, info, out_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// RFC 5869 — Appendix A, Test Case 1 (SHA-256).
    #[test]
    fn rfc5869_test_case_1() {
        let ikm = vec![0x0b; 22];
        let salt: [u8; 13] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info: [u8; 10] = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];

        let prk = extract(&salt, &ikm);
        let expected_prk: [u8; 32] = [
            0x07, 0x77, 0x09, 0x36, 0x2c, 0x2e, 0x32, 0xdf, 0x0d, 0xdc, 0x3f, 0x0d, 0xc4, 0x7b,
            0xba, 0x63, 0x90, 0xb6, 0xc7, 0x3b, 0xb5, 0x0f, 0x9c, 0x31, 0x22, 0xec, 0x84, 0x4a,
            0xd7, 0xc2, 0xb3, 0xe5,
        ];
        assert_eq!(prk, expected_prk);

        let okm = expand(&prk, &info, 42);
        let expected_okm: [u8; 42] = [
            0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36,
            0x2f, 0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56,
            0xec, 0xc4, 0xc5, 0xbf, 0x34, 0x00, 0x72, 0x08, 0xd5, 0xb8, 0x87, 0x18, 0x58, 0x65,
        ];
        assert_eq!(&okm[..], &expected_okm[..]);
    }

    /// RFC 5869 — Appendix A, Test Case 3 (empty salt, empty info).
    #[test]
    fn rfc5869_test_case_3() {
        let ikm = vec![0x0b; 22];
        let prk = extract(&[], &ikm);
        let expected_prk: [u8; 32] = [
            0x19, 0xef, 0x24, 0xa3, 0x2c, 0x71, 0x7b, 0x16, 0x7f, 0x33, 0xa9, 0x1d, 0x6f, 0x64,
            0x8b, 0xdf, 0x96, 0x59, 0x67, 0x76, 0xaf, 0xdb, 0x63, 0x77, 0xac, 0x43, 0x4c, 0x1c,
            0x29, 0x3c, 0xcb, 0x04,
        ];
        assert_eq!(prk, expected_prk);

        let okm = expand(&prk, &[], 42);
        let expected_okm: [u8; 42] = [
            0x8d, 0xa4, 0xe7, 0x75, 0xa5, 0x63, 0xc1, 0x8f, 0x71, 0x5f, 0x80, 0x2a, 0x06, 0x3c,
            0x5a, 0x31, 0xb8, 0xa1, 0x1f, 0x5c, 0x5e, 0xe1, 0x87, 0x9e, 0xc3, 0x45, 0x4e, 0x5f,
            0x3c, 0x73, 0x8d, 0x2d, 0x9d, 0x20, 0x13, 0x95, 0xfa, 0xa4, 0xb6, 0x1a, 0x96, 0xc8,
        ];
        assert_eq!(&okm[..], &expected_okm[..]);
    }

    #[test]
    fn derive_matches_extract_expand() {
        let salt = b"salt";
        let ikm = b"ikm-data";
        let info = b"info";
        let a = derive(salt, ikm, info, 64);
        let prk = extract(salt, ikm);
        let b = expand(&prk, info, 64);
        assert_eq!(a, b);
    }

    #[test]
    #[should_panic(expected = "HKDF out_len too large")]
    fn expand_rejects_oversize() {
        let prk = [0u8; DIGEST_BYTES];
        let _ = expand(&prk, b"info", 255 * DIGEST_BYTES + 1);
    }
}
