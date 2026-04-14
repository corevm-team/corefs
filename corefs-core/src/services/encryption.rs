// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};

use crate::error::{CoreFsError, CoreFsResult};

/// Nonce length (12 bytes) prepended to every ciphertext.
const NONCE_LEN: usize = 12;

/// Minimum plaintext size below which encryption overhead outweighs benefits.
/// Payloads smaller than this are still encrypted when the caller insists, but
/// `should_encrypt` returns `false`.
const MIN_ENCRYPT_BYTES: usize = 1;

/// Provides ChaCha20-Poly1305 authenticated encryption for file content.
///
/// The service is initialised without a key.  Callers must either:
/// - `set_key(key)` with a 256-bit master key (e.g. supplied at mount time), or
/// - `derive_key_from(material)` to derive a key from arbitrary bytes (e.g. a
///   passphrase or volume name — suitable for testing and development, not for
///   production key management).
///
/// Ciphertext layout:  `nonce (12 B) || ciphertext || tag (16 B)`
#[derive(Debug)]
pub struct EncryptionService {
    key: Option<[u8; 32]>,
}

impl Default for EncryptionService {
    fn default() -> Self {
        Self { key: None }
    }
}

impl EncryptionService {
    /// Sets the master key directly.
    pub fn set_key(&mut self, key: [u8; 32]) {
        self.key = Some(key);
    }

    /// Derives a 256-bit key from arbitrary material using a simple hash.
    /// **Not suitable for production** — use a proper KDF (Argon2, HKDF) instead.
    pub fn derive_key_from(&mut self, material: &[u8]) {
        // Simple deterministic key derivation via repeated FNV-like folding.
        let mut key = [0u8; 32];
        let mut state = 0xcbf29ce484222325u64;
        for (i, &byte) in material.iter().cycle().take(256).enumerate() {
            state = state
                .wrapping_mul(0x100000001b3)
                .wrapping_add(u64::from(byte));
            key[i % 32] ^= (state >> ((i % 8) * 8)) as u8;
        }
        self.key = Some(key);
    }

    /// Returns `true` when a key has been set and the service is ready.
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// Returns `true` when `data` is large enough for encryption to be worthwhile.
    pub fn should_encrypt(&self, data: &[u8]) -> bool {
        data.len() >= MIN_ENCRYPT_BYTES && self.key.is_some()
    }

    /// Encrypts `plaintext` with ChaCha20-Poly1305.
    ///
    /// Returns `nonce (12 B) || ciphertext || tag (16 B)`.
    ///
    /// Fails if no key has been set.
    pub fn encrypt(&self, plaintext: &[u8]) -> CoreFsResult<Vec<u8>> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| CoreFsError::State("encryption key not set".to_string()))?;

        let cipher = ChaCha20Poly1305::new_from_slice(key).expect("key length is always 32 bytes");
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|e| CoreFsError::State(format!("encryption failed: {e}")))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypts data previously produced by `encrypt`.
    ///
    /// Expects `nonce (12 B) || ciphertext || tag (16 B)`.
    pub fn decrypt(&self, data: &[u8]) -> CoreFsResult<Vec<u8>> {
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| CoreFsError::State("encryption key not set".to_string()))?;

        if data.len() < NONCE_LEN {
            return Err(CoreFsError::State(
                "encrypted payload too short for nonce".to_string(),
            ));
        }

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
        let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);
        let cipher = ChaCha20Poly1305::new_from_slice(key).expect("key length is always 32 bytes");

        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| CoreFsError::State(format!("decryption failed: {e}")))
    }
}

#[cfg(test)]
#[path = "encryption_tests.rs"]
mod tests;
