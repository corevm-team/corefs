// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

fn test_service() -> EncryptionService {
    let mut svc = EncryptionService::default();
    svc.set_key([42u8; 32]);
    svc
}

#[test]
fn encrypt_decrypt_roundtrip() {
    let svc = test_service();
    let plaintext = b"hello encrypted world!";
    let encrypted = svc.encrypt(plaintext).expect("encrypt");
    assert_ne!(encrypted, plaintext, "ciphertext must differ from plaintext");
    assert!(
        encrypted.len() > plaintext.len(),
        "ciphertext must be larger (nonce + tag overhead)"
    );
    let decrypted = svc.decrypt(&encrypted).expect("decrypt");
    assert_eq!(decrypted, plaintext);
}

#[test]
fn decrypt_with_wrong_key_fails() {
    let svc = test_service();
    let encrypted = svc.encrypt(b"secret").expect("encrypt");

    let mut wrong = EncryptionService::default();
    wrong.set_key([99u8; 32]);
    assert!(wrong.decrypt(&encrypted).is_err());
}

#[test]
fn encrypt_without_key_fails() {
    let svc = EncryptionService::default();
    assert!(!svc.has_key());
    assert!(svc.encrypt(b"data").is_err());
}

#[test]
fn derive_key_from_produces_deterministic_key() {
    let mut a = EncryptionService::default();
    a.derive_key_from(b"passphrase");
    let mut b = EncryptionService::default();
    b.derive_key_from(b"passphrase");
    assert_eq!(a.key, b.key, "same material must produce same key");

    let mut c = EncryptionService::default();
    c.derive_key_from(b"different");
    assert_ne!(a.key, c.key, "different material must produce different key");
}

#[test]
fn each_encryption_produces_unique_ciphertext() {
    let svc = test_service();
    let a = svc.encrypt(b"same").expect("encrypt");
    let b = svc.encrypt(b"same").expect("encrypt");
    assert_ne!(a, b, "random nonce must produce different ciphertext each time");
    // Both must still decrypt to the same plaintext.
    assert_eq!(svc.decrypt(&a).unwrap(), svc.decrypt(&b).unwrap());
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let svc = test_service();
    let mut encrypted = svc.encrypt(b"integrity check").expect("encrypt");
    // Flip a byte in the ciphertext portion (after the 12-byte nonce).
    if encrypted.len() > 13 {
        encrypted[13] ^= 0xff;
    }
    assert!(
        svc.decrypt(&encrypted).is_err(),
        "tampered data must fail authentication"
    );
}
