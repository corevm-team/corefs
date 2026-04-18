// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Produktives Key-Management für CoreFS-Volumes.
//!
//! Ein [`Keystore`] kapselt:
//! - einen **Master-Key** (256 Bit, kommt von außen — Passphrase-KDF,
//!   TPM, smartcard, …),
//! - einen **Volume-Key** (256 Bit, wird AEAD-wrapped unter dem Master
//!   persistiert),
//! - **Per-File-Keys**, die deterministisch aus dem Volume-Key und der
//!   `InodeId` via HKDF-SHA256 abgeleitet werden.
//!
//! Rotation des Master-Keys ändert **nur** das Wrapping des Volume-Keys;
//! der Volume-Key selbst bleibt stabil — Per-File-Keys ändern sich
//! dadurch nicht und alle bestehenden Dateien bleiben entschlüsselbar.
//!
//! Persistente Form ist [`KeystoreFile`], bincode-legacy-serialisiert,
//! mit Magic `"COREFSKS"` und Versionsfeld.

use super::hkdf;
use crate::domain::inode::InodeId;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::Timestamp;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use serde::{Deserialize, Serialize};

/// Magic für die Keystore-Datei: ASCII `"COREFSKS"` als LE-u64.
pub const KEYSTORE_MAGIC: u64 = u64::from_le_bytes(*b"COREFSKS");

/// Aktuelle Wire-Version.
pub const KEYSTORE_VERSION: u16 = 1;

/// Länge eines Schlüssels (256 Bit).
pub const KEY_BYTES: usize = 32;
/// AEAD-Nonce-Länge.
pub const NONCE_BYTES: usize = 12;
/// Salt-Länge für HKDF-Input (keystore-weit).
pub const SALT_BYTES: usize = 32;

/// KDF-Konfiguration (aktuell nur HKDF-SHA256 mit festem Info-Tag).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfConfig {
    /// Algorithmus-Tag, aktuell immer `"HKDF-SHA256"`.
    pub algorithm: alloc::string::String,
    /// Per-Keystore Salt für HKDF-Extract.
    pub salt: [u8; SALT_BYTES],
    /// Info-Domain-Separator für File-Key-Ableitung.
    pub file_info: alloc::string::String,
}

impl KdfConfig {
    /// Default-Konfiguration mit angegebenem Salt.
    pub fn with_salt(salt: [u8; SALT_BYTES]) -> Self {
        Self {
            algorithm: "HKDF-SHA256".to_string(),
            salt,
            file_info: "corefs-per-file-key-v1".to_string(),
        }
    }
}

/// In-Memory-Keystore.
#[derive(Clone)]
pub struct Keystore {
    volume_key: [u8; KEY_BYTES],
    kdf: KdfConfig,
    volume_uuid: [u8; 16],
}

impl Keystore {
    /// Erzeugt einen neuen Keystore mit frisch gesetztem Volume-Key,
    /// Salt und Volume-UUID.
    ///
    /// Der Master-Key wird hier noch **nicht** benötigt — er wird erst
    /// beim [`Keystore::export_file`] (wrap) und [`Keystore::import_file`]
    /// (unwrap) gebraucht.
    pub fn new(
        volume_key: [u8; KEY_BYTES],
        salt: [u8; SALT_BYTES],
        volume_uuid: [u8; 16],
    ) -> Self {
        Self {
            volume_key,
            kdf: KdfConfig::with_salt(salt),
            volume_uuid,
        }
    }

    /// Volume-Key im Klartext (für interne Service-Integration).
    pub fn volume_key(&self) -> &[u8; KEY_BYTES] {
        &self.volume_key
    }

    /// Volume-UUID (16 Byte).
    pub fn volume_uuid(&self) -> &[u8; 16] {
        &self.volume_uuid
    }

    /// KDF-Konfiguration.
    pub fn kdf(&self) -> &KdfConfig {
        &self.kdf
    }

    /// Leitet einen deterministischen Per-File-Schlüssel ab.
    ///
    /// Ableitungsfunktion: `HKDF-SHA256(salt, volume_key, info || inode_id_be)`.
    /// Gleicher `inode_id`-Wert liefert den gleichen Key — aber ohne den
    /// Volume-Key ist er nicht rekonstruierbar.
    pub fn derive_file_key(&self, inode: InodeId) -> [u8; KEY_BYTES] {
        let mut info = Vec::with_capacity(self.kdf.file_info.len() + 8);
        info.extend_from_slice(self.kdf.file_info.as_bytes());
        info.extend_from_slice(&inode.0.to_be_bytes());
        let out = hkdf::derive(&self.kdf.salt, &self.volume_key, &info, KEY_BYTES);
        let mut key = [0u8; KEY_BYTES];
        key.copy_from_slice(&out);
        key
    }

    /// Wrapped den Volume-Key unter `master_key` via ChaCha20-Poly1305.
    ///
    /// Der Aufrufer liefert eine 12-Byte-Nonce (z. B. aus einem CSPRNG).
    /// Das Ergebnis trägt `nonce || ciphertext || tag`.
    pub fn wrap(&self, master_key: &[u8; KEY_BYTES], nonce: [u8; NONCE_BYTES]) -> CoreFsResult<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new_from_slice(master_key)
            .map_err(|_| CoreFsError::State("keystore: invalid master key length".to_string()))?;
        let nonce_obj = Nonce::from_slice(&nonce);
        let ct = cipher
            .encrypt(nonce_obj, self.volume_key.as_slice())
            .map_err(|e| CoreFsError::State(format!("keystore: wrap failed: {e}")))?;
        let mut out = Vec::with_capacity(NONCE_BYTES + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Unwrap eines gewrappten Volume-Keys mit `master_key`.
    pub fn unwrap_volume_key(
        master_key: &[u8; KEY_BYTES],
        wrapped: &[u8],
    ) -> CoreFsResult<[u8; KEY_BYTES]> {
        if wrapped.len() < NONCE_BYTES + 16 {
            return Err(CoreFsError::InvalidInput(
                "keystore: wrapped blob too short".to_string(),
            ));
        }
        let (nonce_bytes, ct) = wrapped.split_at(NONCE_BYTES);
        let cipher = ChaCha20Poly1305::new_from_slice(master_key)
            .map_err(|_| CoreFsError::State("keystore: invalid master key length".to_string()))?;
        let pt = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ct)
            .map_err(|_| {
                CoreFsError::PolicyViolation(
                    "keystore: unwrap failed (wrong master key or tampered blob)".to_string(),
                )
            })?;
        if pt.len() != KEY_BYTES {
            return Err(CoreFsError::State(format!(
                "keystore: unwrapped key length {} (expected {KEY_BYTES})",
                pt.len()
            )));
        }
        let mut out = [0u8; KEY_BYTES];
        out.copy_from_slice(&pt);
        Ok(out)
    }

    /// Rotiert den Master-Key: gibt einen neuen wrapped-Blob unter
    /// `new_master` zurück; der alte Blob wird dabei mit `old_master`
    /// validiert.
    ///
    /// Semantik: der Volume-Key bleibt unverändert — bestehende
    /// verschlüsselte Daten bleiben weiterhin mit den bisherigen
    /// Per-File-Keys lesbar.
    pub fn rotate_master(
        old_master: &[u8; KEY_BYTES],
        new_master: &[u8; KEY_BYTES],
        old_wrapped: &[u8],
        new_nonce: [u8; NONCE_BYTES],
    ) -> CoreFsResult<Vec<u8>> {
        let volume_key = Self::unwrap_volume_key(old_master, old_wrapped)?;
        let cipher = ChaCha20Poly1305::new_from_slice(new_master)
            .map_err(|_| CoreFsError::State("keystore: invalid new master key".to_string()))?;
        let ct = cipher
            .encrypt(Nonce::from_slice(&new_nonce), volume_key.as_slice())
            .map_err(|e| CoreFsError::State(format!("keystore: rewrap failed: {e}")))?;
        let mut out = Vec::with_capacity(NONCE_BYTES + ct.len());
        out.extend_from_slice(&new_nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Exportiert eine persistente Keystore-Datei (wrapped volume key +
    /// metadata, ohne den Volume-Key im Klartext).
    pub fn export_file(
        &self,
        master_key: &[u8; KEY_BYTES],
        wrap_nonce: [u8; NONCE_BYTES],
        created_at: Timestamp,
    ) -> CoreFsResult<KeystoreFile> {
        let wrapped = self.wrap(master_key, wrap_nonce)?;
        Ok(KeystoreFile {
            magic: KEYSTORE_MAGIC,
            version: KEYSTORE_VERSION,
            kdf: self.kdf.clone(),
            wrapped_volume_key: wrapped,
            volume_uuid: self.volume_uuid,
            created_at,
        })
    }

    /// Importiert einen Keystore aus einer persistierten Datei.
    pub fn import_file(
        file: &KeystoreFile,
        master_key: &[u8; KEY_BYTES],
    ) -> CoreFsResult<Self> {
        if file.magic != KEYSTORE_MAGIC {
            return Err(CoreFsError::InvalidInput(format!(
                "keystore: bad magic 0x{:016x} (expected 0x{:016x})",
                file.magic, KEYSTORE_MAGIC
            )));
        }
        if file.version != KEYSTORE_VERSION {
            return Err(CoreFsError::InvalidInput(format!(
                "keystore: unsupported version {} (expected {})",
                file.version, KEYSTORE_VERSION
            )));
        }
        let volume_key = Self::unwrap_volume_key(master_key, &file.wrapped_volume_key)?;
        Ok(Self {
            volume_key,
            kdf: file.kdf.clone(),
            volume_uuid: file.volume_uuid,
        })
    }
}

impl core::fmt::Debug for Keystore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Keystore")
            .field("volume_uuid", &self.volume_uuid)
            .field("kdf", &self.kdf)
            .field("volume_key", &"<redacted>")
            .finish()
    }
}

/// Persistente Keystore-Datei.
///
/// Trägt **nicht** den Volume-Key im Klartext — nur AEAD-wrapped unter
/// dem aktuellen Master-Key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeystoreFile {
    /// Magic ("COREFSKS" als LE-u64).
    pub magic: u64,
    /// Wire-Version (aktuell 1).
    pub version: u16,
    /// KDF-Konfiguration.
    pub kdf: KdfConfig,
    /// AEAD-gewrappter Volume-Key: `nonce || ciphertext || tag`.
    pub wrapped_volume_key: Vec<u8>,
    /// Volume-UUID (zum Abgleich mit dem Volume-Image).
    pub volume_uuid: [u8; 16],
    /// Erstellungszeit.
    pub created_at: Timestamp,
}

#[cfg(test)]
#[path = "keystore_tests.rs"]
mod tests;
