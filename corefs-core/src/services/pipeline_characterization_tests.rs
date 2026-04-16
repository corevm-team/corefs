// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Characterization tests for the compression and encryption services.
//
// Gated on the features that make the services available:
//   compression tests  → feature = "compression"
//   encryption tests   → feature = "crypto" + feature = "std" (for OS-RNG encrypt)
//
// These tests must pass against the current implementation and continue to
// pass — unchanged — after the extent-based refactor.

// ---------------------------------------------------------------------------
// Compression characterization tests
// ---------------------------------------------------------------------------

#[cfg(feature = "compression")]
mod compression_char_tests {
    use crate::services::compression::CompressionService;

    fn prng_bytes(seed: u64, len: usize) -> alloc::vec::Vec<u8> {
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

    #[test]
    fn char_compress_roundtrip_repeated_content() {
        let svc = CompressionService;
        let data: alloc::vec::Vec<u8> = b"abcdefgh".iter().copied().cycle().take(10_000).collect();
        let compressed = svc.compress(&data).expect("compress");
        assert!(
            compressed.len() < data.len(),
            "repeated payload must compress smaller ({} >= {})",
            compressed.len(),
            data.len()
        );
        let restored = svc.decompress(&compressed).expect("decompress");
        assert_eq!(restored, data, "decompressed bytes must equal original");
    }

    #[test]
    fn char_compress_random_content_still_decompresses() {
        let svc = CompressionService;
        let data = prng_bytes(0xFEED_FACE, 64 * 1024);
        // Random data may or may not compress — we don't assert size.
        let compressed = svc.compress(&data).expect("compress random");
        let restored = svc.decompress(&compressed).expect("decompress random");
        assert_eq!(restored, data, "decompressed random bytes must equal original");
    }

    #[test]
    fn char_compress_empty_payload_roundtrips() {
        let svc = CompressionService;
        let empty: alloc::vec::Vec<u8> = alloc::vec![];
        let compressed = svc.compress(&empty).expect("compress empty");
        let restored = svc.decompress(&compressed).expect("decompress empty");
        assert_eq!(restored, empty);
    }

    #[test]
    fn char_compress_threshold_respected() {
        let svc = CompressionService;
        // Single byte — below threshold.
        assert!(
            !svc.should_compress(b"x"),
            "should_compress must return false for tiny payloads"
        );
        // 64+ bytes of repeated content — above threshold and worthwhile.
        let big: alloc::vec::Vec<u8> = b"A".iter().copied().cycle().take(256).collect();
        assert!(
            svc.should_compress(&big),
            "should_compress must return true for large payloads"
        );
    }
}

// ---------------------------------------------------------------------------
// Encryption characterization tests
// (require both crypto feature and std feature for OS-RNG variant)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "crypto", feature = "std"))]
mod encryption_char_tests {
    use crate::services::encryption::EncryptionService;

    fn new_svc() -> EncryptionService {
        let mut svc = EncryptionService::default();
        svc.derive_key_from(b"char-test-material");
        svc
    }

    #[test]
    fn char_encrypt_roundtrip_with_os_rng() {
        let svc = new_svc();
        let plaintext = b"secret message 1234";
        let ciphertext = svc.encrypt(plaintext).expect("encrypt");
        let restored = svc.decrypt(&ciphertext).expect("decrypt");
        assert_eq!(restored, plaintext);
    }

    #[test]
    fn char_encrypt_produces_different_ciphertext_each_call() {
        // ChaCha20-Poly1305 uses a fresh OS-RNG nonce on every encrypt call.
        let svc = new_svc();
        let plaintext = b"same plaintext";
        let ct1 = svc.encrypt(plaintext).expect("encrypt 1");
        let ct2 = svc.encrypt(plaintext).expect("encrypt 2");
        assert_ne!(ct1, ct2, "two encryptions of the same plaintext must produce different ciphertexts due to nonce randomness");
        // Both must decrypt back to the original.
        assert_eq!(svc.decrypt(&ct1).expect("decrypt 1"), plaintext);
        assert_eq!(svc.decrypt(&ct2).expect("decrypt 2"), plaintext);
    }

    #[test]
    fn char_decrypt_with_wrong_key_fails() {
        let svc = new_svc();
        let plaintext = b"protected content";
        let ciphertext = svc.encrypt(plaintext).expect("encrypt");

        let mut wrong_key_svc = EncryptionService::default();
        wrong_key_svc.derive_key_from(b"completely-different-key");
        assert!(
            wrong_key_svc.decrypt(&ciphertext).is_err(),
            "decryption with wrong key must fail"
        );
    }

    #[test]
    fn char_decrypt_truncated_payload_fails() {
        let svc = new_svc();
        let ciphertext = svc.encrypt(b"hello").expect("encrypt");
        // Truncate to less than the nonce length — must fail gracefully.
        let truncated = &ciphertext[..3];
        assert!(
            svc.decrypt(truncated).is_err(),
            "decrypting truncated payload must fail"
        );
    }

    #[test]
    fn char_encrypt_empty_payload_roundtrips() {
        let svc = new_svc();
        let ct = svc.encrypt(b"").expect("encrypt empty");
        let restored = svc.decrypt(&ct).expect("decrypt empty");
        assert!(restored.is_empty());
    }
}
