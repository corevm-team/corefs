// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen fuer das Keystore-Management.
//!
//! Wrapper um [`corefs_core::security::keystore`]:
//!
//! - [`init`]   — neuen Keystore anlegen (generiert Volume-Key zufaellig,
//!                wrapped ihn unter einem Master-Key aus `master_key_path`)
//! - [`rotate`] — Master-Key rotieren (altes + neues Master-Key-File)
//! - [`verify`] — Keystore-Datei pruefen (Magic, Version, Unwrap ok)
//!
//! Master-Keys werden aktuell als 32-Byte-Rohdateien erwartet (z. B.
//! via `head -c 32 /dev/urandom > master.bin`). Keystore-Dateien sind
//! bincode-serialisierte [`corefs_core::security::keystore::KeystoreFile`].

use crate::error::{ToolsError, ToolsResult};
use crate::report::{Report, to_pretty_json};
use corefs_core::bincode_compat;
use corefs_core::error::CoreFsError;
use corefs_core::platform::Timestamp;
use corefs_core::security::keystore::{
    KEY_BYTES, KEYSTORE_MAGIC, KEYSTORE_VERSION, Keystore, KeystoreFile, NONCE_BYTES, SALT_BYTES,
};
use rand_bytes::fill_random;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// rand_bytes — kleines Modul ohne externe Abhängigkeit, nutzt /dev/urandom
// (Linux) bzw. /dev/random als Fallback.
// ---------------------------------------------------------------------------

mod rand_bytes {
    use std::fs::File;
    use std::io::Read;

    pub fn fill_random(buf: &mut [u8]) -> std::io::Result<()> {
        let mut f = File::open("/dev/urandom")?;
        f.read_exact(buf)?;
        Ok(())
    }
}

fn te(msg: String) -> ToolsError {
    ToolsError::Core(CoreFsError::State(msg))
}

fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| Timestamp::from_secs_nanos(d.as_secs(), d.subsec_nanos()))
        .unwrap_or(Timestamp::EPOCH)
}

fn read_master_key(path: &Path) -> ToolsResult<[u8; KEY_BYTES]> {
    let meta = fs::metadata(path).map_err(|e| {
        te(format!("master-key {}: {e}", path.display()))
    })?;
    if meta.len() != KEY_BYTES as u64 {
        return Err(ToolsError::InvalidArgument(format!(
            "master-key {}: expected exactly {KEY_BYTES} bytes, got {}",
            path.display(),
            meta.len()
        )));
    }
    let mut f = File::open(path)
        .map_err(|e| te(format!("open master-key {}: {e}", path.display())))?;
    let mut key = [0u8; KEY_BYTES];
    f.read_exact(&mut key)
        .map_err(|e| te(format!("read master-key {}: {e}", path.display())))?;
    Ok(key)
}

fn parse_uuid_hex(s: &str) -> ToolsResult<[u8; 16]> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    if clean.len() != 32 {
        return Err(ToolsError::InvalidArgument(format!(
            "volume-uuid: expected 32 hex chars (got {})",
            clean.len()
        )));
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).map_err(|e| {
            ToolsError::InvalidArgument(format!("volume-uuid: not hex ({e})"))
        })?;
    }
    Ok(out)
}

fn write_keystore_file(path: &Path, file: &KeystoreFile) -> ToolsResult<()> {
    let bytes = bincode_compat::serialize(file)
        .map_err(|e| te(format!("serialize keystore: {e}")))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| te(format!("open keystore {}: {e}", path.display())))?;
    f.write_all(&bytes)
        .map_err(|e| te(format!("write keystore: {e}")))?;
    Ok(())
}

fn read_keystore_file(path: &Path) -> ToolsResult<KeystoreFile> {
    let bytes = std::fs::read(path)
        .map_err(|e| te(format!("read keystore {}: {e}", path.display())))?;
    let file: KeystoreFile = bincode_compat::deserialize(&bytes).map_err(|e| {
        te(format!("deserialize keystore {}: {e}", path.display()))
    })?;
    Ok(file)
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

/// Strukturierter Report fuer [`init`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystoreInitReport {
    /// Pfad der erzeugten Keystore-Datei.
    pub keystore_path: String,
    /// Volume-UUID (hex).
    pub volume_uuid: String,
    /// Keystore-Version.
    pub version: u16,
    /// Bytes der geschriebenen Datei.
    pub bytes_written: u64,
}

impl Report for KeystoreInitReport {
    fn summary(&self) -> String {
        format!(
            "keystore init {} (uuid={}, {} bytes)",
            self.keystore_path, self.volume_uuid, self.bytes_written
        )
    }
    fn render_text(&self) -> String {
        format!(
            "keystore initialised\n────────────────\npath          : {}\nvolume uuid   : {}\nversion       : {}\nbytes written : {}\n",
            self.keystore_path, self.volume_uuid, self.version, self.bytes_written
        )
    }
    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Erzeugt einen neuen Keystore: zufaelliger Volume-Key + Salt + Nonce,
/// gewrappt unter dem Master-Key aus `master_key`.
pub fn init(
    keystore_path: &Path,
    master_key: &Path,
    volume_uuid_hex: &str,
) -> ToolsResult<KeystoreInitReport> {
    let master = read_master_key(master_key)?;
    let uuid = parse_uuid_hex(volume_uuid_hex)?;

    let mut vk = [0u8; KEY_BYTES];
    fill_random(&mut vk).map_err(|e| te(format!("rand volume-key: {e}")))?;
    let mut salt = [0u8; SALT_BYTES];
    fill_random(&mut salt).map_err(|e| te(format!("rand salt: {e}")))?;
    let mut nonce = [0u8; NONCE_BYTES];
    fill_random(&mut nonce).map_err(|e| te(format!("rand nonce: {e}")))?;

    let ks = Keystore::new(vk, salt, uuid);
    let file = ks
        .export_file(&master, nonce, now())
        .map_err(ToolsError::from)?;
    write_keystore_file(keystore_path, &file)?;
    let bytes_written = fs::metadata(keystore_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let uuid_str = uuid
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    Ok(KeystoreInitReport {
        keystore_path: keystore_path.display().to_string(),
        volume_uuid: uuid_str,
        version: file.version,
        bytes_written,
    })
}

// ---------------------------------------------------------------------------
// rotate
// ---------------------------------------------------------------------------

/// Strukturierter Report fuer [`rotate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystoreRotateReport {
    /// Pfad der rotierten Keystore-Datei.
    pub keystore_path: String,
    /// Volume-UUID (hex).
    pub volume_uuid: String,
    /// Neue Wrap-Nonce, hex (zur Audit-Spur).
    pub new_wrap_nonce_hex: String,
}

impl Report for KeystoreRotateReport {
    fn summary(&self) -> String {
        format!("keystore rotated {} (uuid={})", self.keystore_path, self.volume_uuid)
    }
    fn render_text(&self) -> String {
        format!(
            "keystore rotated\n────────────────\npath          : {}\nvolume uuid   : {}\nwrap nonce    : {}\n",
            self.keystore_path, self.volume_uuid, self.new_wrap_nonce_hex
        )
    }
    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Rotiert den Master-Key: liest `keystore_path` mit `old_master`, wrapped
/// den Volume-Key neu unter `new_master` und schreibt die Datei zurueck.
pub fn rotate(
    keystore_path: &Path,
    old_master: &Path,
    new_master: &Path,
) -> ToolsResult<KeystoreRotateReport> {
    let old_mk = read_master_key(old_master)?;
    let new_mk = read_master_key(new_master)?;
    let mut file = read_keystore_file(keystore_path)?;

    let mut nonce = [0u8; NONCE_BYTES];
    fill_random(&mut nonce).map_err(|e| te(format!("rand nonce: {e}")))?;

    let new_wrapped = Keystore::rotate_master(&old_mk, &new_mk, &file.wrapped_volume_key, nonce)
        .map_err(ToolsError::from)?;
    file.wrapped_volume_key = new_wrapped;
    write_keystore_file(keystore_path, &file)?;

    let uuid_str = file
        .volume_uuid
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    let nonce_str = nonce.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    Ok(KeystoreRotateReport {
        keystore_path: keystore_path.display().to_string(),
        volume_uuid: uuid_str,
        new_wrap_nonce_hex: nonce_str,
    })
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// Strukturierter Report fuer [`verify`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystoreVerifyReport {
    /// Pfad der geprueften Keystore-Datei.
    pub keystore_path: String,
    /// Magic-Wert, stimmt mit dem Erwartungswert ueberein?
    pub magic_ok: bool,
    /// Version (aktuell 1 erwartet).
    pub version: u16,
    /// Version passt zur gebauten Tool-Version?
    pub version_ok: bool,
    /// Volume-UUID (hex).
    pub volume_uuid: String,
    /// Unwrap mit angegebenem Master-Key hat funktioniert.
    pub unwrap_ok: bool,
    /// Diagnose-String, falls ein Check fehlschlug.
    pub diagnosis: String,
}

impl Report for KeystoreVerifyReport {
    fn summary(&self) -> String {
        if self.magic_ok && self.version_ok && self.unwrap_ok {
            format!("keystore ok: {}", self.keystore_path)
        } else {
            format!("keystore FAILED: {} — {}", self.keystore_path, self.diagnosis)
        }
    }
    fn render_text(&self) -> String {
        format!(
            "keystore verify\n────────────────\npath        : {}\nmagic ok    : {}\nversion     : {} ({})\nuuid        : {}\nunwrap ok   : {}\ndiagnosis   : {}\n",
            self.keystore_path,
            self.magic_ok,
            self.version,
            if self.version_ok { "ok" } else { "UNSUPPORTED" },
            self.volume_uuid,
            self.unwrap_ok,
            self.diagnosis
        )
    }
    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Prueft die Integritaet einer Keystore-Datei und versucht ein Probe-Unwrap
/// mit dem angegebenen Master-Key.
pub fn verify(keystore_path: &Path, master_key: &Path) -> ToolsResult<KeystoreVerifyReport> {
    let file = read_keystore_file(keystore_path)?;
    let master = read_master_key(master_key)?;

    let magic_ok = file.magic == KEYSTORE_MAGIC;
    let version_ok = file.version == KEYSTORE_VERSION;

    let unwrap_result = if magic_ok && version_ok {
        Keystore::import_file(&file, &master)
    } else {
        Err(CoreFsError::InvalidInput(
            "magic/version precondition failed".to_string(),
        ))
    };

    let (unwrap_ok, diagnosis) = match unwrap_result {
        Ok(_) => (true, String::from("ok")),
        Err(e) => (false, format!("{e}")),
    };

    let uuid_str = file
        .volume_uuid
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    Ok(KeystoreVerifyReport {
        keystore_path: keystore_path.display().to_string(),
        magic_ok,
        version: file.version,
        version_ok,
        volume_uuid: uuid_str,
        unwrap_ok,
        diagnosis,
    })
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
