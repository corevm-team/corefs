// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::bincode_compat;
use crate::domain::inode::InodeId;
use crate::platform::Timestamp;

const ZERO_MK: [u8; KEY_BYTES] = [0u8; KEY_BYTES];
const ONES_MK: [u8; KEY_BYTES] = [1u8; KEY_BYTES];
const VK: [u8; KEY_BYTES] = [
    0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
    0xcc, 0xdd, 0xee, 0xff, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0,
];
const SALT: [u8; SALT_BYTES] = [0x42; SALT_BYTES];
const UUID: [u8; 16] = [9; 16];
const NONCE_A: [u8; NONCE_BYTES] = [1; NONCE_BYTES];
const NONCE_B: [u8; NONCE_BYTES] = [2; NONCE_BYTES];

#[test]
fn derive_file_key_is_deterministic() {
    let ks = Keystore::new(VK, SALT, UUID);
    let k1 = ks.derive_file_key(InodeId(42));
    let k2 = ks.derive_file_key(InodeId(42));
    assert_eq!(k1, k2);
}

#[test]
fn derive_file_key_varies_per_inode() {
    let ks = Keystore::new(VK, SALT, UUID);
    let k1 = ks.derive_file_key(InodeId(1));
    let k2 = ks.derive_file_key(InodeId(2));
    assert_ne!(k1, k2);
}

#[test]
fn derive_file_key_varies_per_volume_key() {
    let k_a = Keystore::new(VK, SALT, UUID).derive_file_key(InodeId(7));
    let mut vk2 = VK;
    vk2[0] ^= 0xFF;
    let k_b = Keystore::new(vk2, SALT, UUID).derive_file_key(InodeId(7));
    assert_ne!(k_a, k_b);
}

#[test]
fn wrap_unwrap_roundtrip() {
    let ks = Keystore::new(VK, SALT, UUID);
    let wrapped = ks.wrap(&ZERO_MK, NONCE_A).unwrap();
    let unwrapped = Keystore::unwrap_volume_key(&ZERO_MK, &wrapped).unwrap();
    assert_eq!(unwrapped, VK);
}

#[test]
fn wrong_master_key_unwrap_fails() {
    let ks = Keystore::new(VK, SALT, UUID);
    let wrapped = ks.wrap(&ZERO_MK, NONCE_A).unwrap();
    let err = Keystore::unwrap_volume_key(&ONES_MK, &wrapped).unwrap_err();
    match err {
        CoreFsError::PolicyViolation(msg) => assert!(msg.contains("unwrap")),
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn rotation_preserves_volume_key_access() {
    let ks = Keystore::new(VK, SALT, UUID);
    let old_wrapped = ks.wrap(&ZERO_MK, NONCE_A).unwrap();
    let new_wrapped = Keystore::rotate_master(&ZERO_MK, &ONES_MK, &old_wrapped, NONCE_B).unwrap();
    // Alter Master geht nicht mehr auf new_wrapped:
    assert!(Keystore::unwrap_volume_key(&ZERO_MK, &new_wrapped).is_err());
    // Neuer Master liefert den gleichen Volume-Key:
    let vk2 = Keystore::unwrap_volume_key(&ONES_MK, &new_wrapped).unwrap();
    assert_eq!(vk2, VK);
}

#[test]
fn rotation_does_not_change_file_keys() {
    let ks = Keystore::new(VK, SALT, UUID);
    let pre = ks.derive_file_key(InodeId(5));
    let old_wrapped = ks.wrap(&ZERO_MK, NONCE_A).unwrap();
    let new_wrapped = Keystore::rotate_master(&ZERO_MK, &ONES_MK, &old_wrapped, NONCE_B).unwrap();
    let file = KeystoreFile {
        magic: KEYSTORE_MAGIC,
        version: KEYSTORE_VERSION,
        kdf: ks.kdf().clone(),
        wrapped_volume_key: new_wrapped,
        volume_uuid: UUID,
        created_at: Timestamp::EPOCH,
    };
    let ks2 = Keystore::import_file(&file, &ONES_MK).unwrap();
    let post = ks2.derive_file_key(InodeId(5));
    assert_eq!(pre, post);
}

#[test]
fn export_import_roundtrip() {
    let ks = Keystore::new(VK, SALT, UUID);
    let file = ks
        .export_file(&ZERO_MK, NONCE_A, Timestamp::from_secs(1000))
        .unwrap();
    assert_eq!(file.magic, KEYSTORE_MAGIC);
    assert_eq!(file.version, KEYSTORE_VERSION);
    let bytes = bincode_compat::serialize(&file).unwrap();
    let decoded: KeystoreFile = bincode_compat::deserialize(&bytes).unwrap();
    assert_eq!(decoded, file);
    let ks2 = Keystore::import_file(&decoded, &ZERO_MK).unwrap();
    assert_eq!(ks2.volume_key(), &VK);
    assert_eq!(ks2.volume_uuid(), &UUID);
}

#[test]
fn import_rejects_bad_magic() {
    let ks = Keystore::new(VK, SALT, UUID);
    let mut file = ks.export_file(&ZERO_MK, NONCE_A, Timestamp::EPOCH).unwrap();
    file.magic = 0xDEAD_BEEF;
    let err = Keystore::import_file(&file, &ZERO_MK).unwrap_err();
    match err {
        CoreFsError::InvalidInput(msg) => assert!(msg.contains("bad magic")),
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn import_rejects_bad_version() {
    let ks = Keystore::new(VK, SALT, UUID);
    let mut file = ks.export_file(&ZERO_MK, NONCE_A, Timestamp::EPOCH).unwrap();
    file.version = 999;
    let err = Keystore::import_file(&file, &ZERO_MK).unwrap_err();
    match err {
        CoreFsError::InvalidInput(msg) => assert!(msg.contains("unsupported version")),
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn unwrap_rejects_too_short_blob() {
    let err = Keystore::unwrap_volume_key(&ZERO_MK, &[0u8; 5]).unwrap_err();
    match err {
        CoreFsError::InvalidInput(msg) => assert!(msg.contains("too short")),
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn tampered_wrapped_blob_fails_unwrap() {
    let ks = Keystore::new(VK, SALT, UUID);
    let mut wrapped = ks.wrap(&ZERO_MK, NONCE_A).unwrap();
    // Flip ein Byte hinter der Nonce (im Ciphertext):
    let tamper_idx = NONCE_BYTES + 3;
    wrapped[tamper_idx] ^= 0x01;
    let err = Keystore::unwrap_volume_key(&ZERO_MK, &wrapped).unwrap_err();
    assert!(matches!(err, CoreFsError::PolicyViolation(_)));
}

#[test]
fn wire_format_magic_is_coreksfs_in_little_endian() {
    // Regressionstest: "COREFSKS" ASCII
    assert_eq!(KEYSTORE_MAGIC.to_le_bytes(), *b"COREFSKS");
}

#[test]
fn keystore_file_bincode_stable_shape() {
    let ks = Keystore::new(VK, SALT, UUID);
    let file = ks
        .export_file(&ZERO_MK, NONCE_A, Timestamp::from_secs(123))
        .unwrap();
    let bytes = bincode_compat::serialize(&file).unwrap();
    // Magic muss in den ersten 8 Bytes als LE-u64 erkennbar sein:
    let mut magic_bytes = [0u8; 8];
    magic_bytes.copy_from_slice(&bytes[..8]);
    assert_eq!(u64::from_le_bytes(magic_bytes), KEYSTORE_MAGIC);
}

#[test]
fn derive_file_key_varies_per_salt() {
    let mut salt2 = SALT;
    salt2[0] ^= 0xFF;
    let k1 = Keystore::new(VK, SALT, UUID).derive_file_key(InodeId(1));
    let k2 = Keystore::new(VK, salt2, UUID).derive_file_key(InodeId(1));
    assert_ne!(k1, k2);
}
