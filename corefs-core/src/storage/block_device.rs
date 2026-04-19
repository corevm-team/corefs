// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutraler [`BlockDevice`]-Trait.
//!
//! Beschreibt eine sektor-adressierbare Speicherressource. Implementierungen
//! stellen die Anbindung an konkrete Backends bereit:
//!
//! - `MemoryDevice` (in dieser Crate, no_std + alloc) — In-Memory-Referenz.
//! - `FileImageDevice` (im main `corefs` crate, std-only) — Datei-basiert.
//! - `RawBlockDevice` (im main `corefs` crate, Linux-only) — `/dev/sdX`.
//!
//! Konsumenten (`storage::ondisk`, `services`, `app`) arbeiten ausschließlich
//! gegen das Trait-Interface und sind dadurch backend-agnostisch.
//!
//! ## Alignment-Vorgaben
//!
//! Alle `read_at`/`write_at`/`trim`-Aufrufe **müssen** byte-aligned auf
//! die Sektorgröße sein. Implementierungen weisen Verstöße mit
//! [`CoreFsError::InvalidInput`] zurück. Schreibversuche auf read-only
//! Devices werden mit [`CoreFsError::PolicyViolation`] zurückgewiesen.

use crate::error::{CoreFsError, CoreFsResult};
use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

// ---------------------------------------------------------------------------
// Sector size constants
// ---------------------------------------------------------------------------

/// Klassische Hard-Disk-Sektorgröße (512 Bytes).
pub const SECTOR_SIZE_512: u32 = 512;

/// Advanced-Format-/NVMe-/Flash-Sektorgröße (4096 Bytes).
pub const SECTOR_SIZE_4K: u32 = 4096;

// ---------------------------------------------------------------------------
// DeviceGeometry — physical parameters of the underlying device
// ---------------------------------------------------------------------------

/// Beschreibt die physikalische Geometrie eines Block-Devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGeometry {
    /// Sektorgröße in Bytes (kleinste adressierbare Einheit).
    pub sector_size: u32,
    /// Anzahl Sektoren auf dem Device.
    pub sector_count: u64,
    /// Gesamtkapazität in Bytes (`sector_size * sector_count`).
    pub capacity_bytes: u64,
    /// `true`, wenn das Device read-only ist (z. B. write-protected USB-Stick).
    pub read_only: bool,
}

// ---------------------------------------------------------------------------
// BlockDevice trait — the core abstraction
// ---------------------------------------------------------------------------

/// Abstraktion über eine sektor-adressierbare Speicherressource.
///
/// Alle Offsets und Längen sind in **Bytes** und müssen aligned zur
/// Sektorgröße sein. Implementierungen geben
/// [`CoreFsError::InvalidInput`] für Misalignments und
/// [`CoreFsError::State`] für I/O-Fehler zurück.
pub trait BlockDevice: fmt::Debug + Send {
    /// Liefert die physikalische Geometrie des Devices.
    fn geometry(&self) -> &DeviceGeometry;

    /// Liest `length` Bytes ab Byte-Offset `offset`.
    ///
    /// `offset` und `length` müssen Vielfache der Sektorgröße sein.
    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>>;

    /// Schreibt `data` ab Byte-Offset `offset`.
    ///
    /// `offset` muss ein Vielfaches der Sektorgröße sein.
    /// `data.len()` muss ein Vielfaches der Sektorgröße sein.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()>;

    /// Flusht buffered Writes auf den Persistenz-Layer.
    fn sync(&mut self) -> CoreFsResult<()>;

    /// Teilt dem Device mit, dass der Byte-Bereich `[offset, offset + length)`
    /// nicht mehr genutzt wird und discardet werden darf (TRIM / UNMAP).
    ///
    /// Devices ohne TRIM-Support liefern silent `Ok(())`.
    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()>;

    /// `true`, wenn das Device TRIM/Discard unterstützt.
    fn supports_trim(&self) -> bool;

    /// Gesamtkapazität in Bytes.
    fn capacity(&self) -> u64 {
        self.geometry().capacity_bytes
    }

    /// Sektorgröße in Bytes.
    fn sector_size(&self) -> u32 {
        self.geometry().sector_size
    }

    /// `true`, wenn das Device read-only ist.
    fn is_read_only(&self) -> bool {
        self.geometry().read_only
    }

    /// Vergrößert oder verkleinert das Device auf `new_capacity` Bytes.
    ///
    /// `new_capacity` muss ein Vielfaches der Sektorgröße sein.  Devices
    /// mit fester Größe (z. B. ein echtes Block-Device oder
    /// [`MemoryDevice`]) liefern
    /// [`CoreFsError::InvalidInput`].  Devices mit wachsender Kapazität
    /// (z. B. `FileImageDevice`) überschreiben diese Default-Implementierung.
    ///
    /// Eingeführt mit Phase 1c (P3), damit der FUSE-Persist-Pfad ein
    /// file-backed Image bei wachsendem Volume in-place vergrößern kann,
    /// ohne dabei die Trait-Objekt-Abstraktion zu verlieren.
    fn resize(&mut self, new_capacity: u64) -> CoreFsResult<()> {
        let _ = new_capacity;
        Err(CoreFsError::InvalidInput(
            "BlockDevice::resize is not supported by this device".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Alignment helpers (pub(crate)+pub for backend reuse)
// ---------------------------------------------------------------------------

/// Validiert, dass `offset` und `length` aligned zur Sektorgröße sind.
///
/// Wird sowohl von [`MemoryDevice`] als auch von std-seitigen Backends
/// genutzt.
pub fn check_alignment(offset: u64, length: u64, sector_size: u32) -> CoreFsResult<()> {
    let ss = u64::from(sector_size);
    if offset % ss != 0 {
        return Err(CoreFsError::InvalidInput(format!(
            "offset {offset} is not aligned to sector size {sector_size}"
        )));
    }
    if length % ss != 0 {
        return Err(CoreFsError::InvalidInput(format!(
            "length {length} is not aligned to sector size {sector_size}"
        )));
    }
    Ok(())
}

/// Validiert, dass `[offset, offset + length)` innerhalb der Device-Kapazität liegt.
pub fn check_bounds(offset: u64, length: u64, capacity: u64) -> CoreFsResult<()> {
    let end = offset.checked_add(length).ok_or_else(|| {
        CoreFsError::InvalidInput(format!("offset {offset} + length {length} overflows u64"))
    })?;
    if end > capacity {
        return Err(CoreFsError::InvalidInput(format!(
            "access past end of device: offset {offset} + length {length} = {end} > capacity {capacity}"
        )));
    }
    Ok(())
}

/// Lehnt Schreibversuche auf read-only Devices ab.
pub fn check_write_permission(read_only: bool) -> CoreFsResult<()> {
    if read_only {
        return Err(CoreFsError::PolicyViolation(
            "device is read-only".to_string(),
        ));
    }
    Ok(())
}

// ===========================================================================
// MemoryDevice — in-memory block device for testing
// ===========================================================================

/// Ein [`BlockDevice`], das vollständig im Speicher gehalten wird.
///
/// Geeignet für Unit-Tests und als Referenzimplementierung. Trim wird
/// als Zero-Fill simuliert; getrimmte Ranges werden zur späteren
/// Inspektion gesammelt ([`MemoryDevice::trimmed_ranges`]).
#[derive(Debug, Clone)]
pub struct MemoryDevice {
    data: Vec<u8>,
    geometry: DeviceGeometry,
    trim_supported: bool,
    trimmed_ranges: Vec<(u64, u64)>,
}

impl MemoryDevice {
    /// Konstruiert ein neues, mit Nullen gefülltes In-Memory-Device.
    pub fn new(capacity_bytes: u64, sector_size: u32) -> CoreFsResult<Self> {
        if capacity_bytes == 0 {
            return Err(CoreFsError::InvalidInput(
                "capacity must be greater than zero".to_string(),
            ));
        }
        if sector_size == 0 || (sector_size & (sector_size - 1)) != 0 {
            return Err(CoreFsError::InvalidInput(format!(
                "sector size {sector_size} must be a power of two"
            )));
        }
        if capacity_bytes % u64::from(sector_size) != 0 {
            return Err(CoreFsError::InvalidInput(format!(
                "capacity {capacity_bytes} must be a multiple of sector size {sector_size}"
            )));
        }
        Ok(Self {
            data: vec![0u8; capacity_bytes as usize],
            geometry: DeviceGeometry {
                sector_size,
                sector_count: capacity_bytes / u64::from(sector_size),
                capacity_bytes,
                read_only: false,
            },
            trim_supported: true,
            trimmed_ranges: Vec::new(),
        })
    }

    /// Konstruiert ein In-Memory-Device aus existierenden Bytes.
    pub fn from_bytes(data: Vec<u8>, sector_size: u32) -> CoreFsResult<Self> {
        let capacity_bytes = data.len() as u64;
        if capacity_bytes == 0 {
            return Err(CoreFsError::InvalidInput(
                "capacity must be greater than zero".to_string(),
            ));
        }
        if capacity_bytes % u64::from(sector_size) != 0 {
            return Err(CoreFsError::InvalidInput(format!(
                "data length {} must be a multiple of sector size {sector_size}",
                data.len()
            )));
        }
        Ok(Self {
            data,
            geometry: DeviceGeometry {
                sector_size,
                sector_count: capacity_bytes / u64::from(sector_size),
                capacity_bytes,
                read_only: false,
            },
            trim_supported: true,
            trimmed_ranges: Vec::new(),
        })
    }

    /// Aktiviert oder deaktiviert TRIM-Support (für Tests von Trim-Codepfaden).
    pub fn set_trim_supported(&mut self, supported: bool) {
        self.trim_supported = supported;
    }

    /// Markiert das Device als read-only (für Tests von Write-Protection).
    pub fn set_read_only(&mut self, read_only: bool) {
        self.geometry.read_only = read_only;
    }

    /// Snapshot des darunterliegenden Daten-Buffers.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Liste aller seit Erzeugung (oder letztem Clear) getrimmten Ranges.
    pub fn trimmed_ranges(&self) -> &[(u64, u64)] {
        &self.trimmed_ranges
    }

    /// Leert die getrimmte-Ranges-Liste.
    pub fn clear_trimmed_ranges(&mut self) {
        self.trimmed_ranges.clear();
    }
}

impl BlockDevice for MemoryDevice {
    fn geometry(&self) -> &DeviceGeometry {
        &self.geometry
    }

    /// Grow (or shrink) the in-memory backing `Vec` so the device can
    /// accept writes past its previous capacity.  Required by the
    /// BlockStore compat-device path — without this the store silently
    /// dropped every byte past the initial 64 MiB allocation, making
    /// large-file workloads lose data.
    fn resize(&mut self, new_capacity: u64) -> CoreFsResult<()> {
        let ss = u64::from(self.geometry.sector_size);
        if new_capacity % ss != 0 {
            return Err(CoreFsError::InvalidInput(format!(
                "new capacity {new_capacity} must be a multiple of sector size {}",
                self.geometry.sector_size
            )));
        }
        let new_len = new_capacity as usize;
        self.data.resize(new_len, 0);
        self.geometry.capacity_bytes = new_capacity;
        self.geometry.sector_count = new_capacity / ss;
        Ok(())
    }

    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        check_alignment(offset, length, self.geometry.sector_size)?;
        check_bounds(offset, length, self.geometry.capacity_bytes)?;
        if length == 0 {
            return Ok(Vec::new());
        }
        let start = offset as usize;
        let end = start + length as usize;
        Ok(self.data[start..end].to_vec())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        check_write_permission(self.geometry.read_only)?;
        let length = data.len() as u64;
        check_alignment(offset, length, self.geometry.sector_size)?;
        check_bounds(offset, length, self.geometry.capacity_bytes)?;
        if data.is_empty() {
            return Ok(());
        }
        let start = offset as usize;
        let end = start + data.len();
        self.data[start..end].copy_from_slice(data);
        Ok(())
    }

    fn sync(&mut self) -> CoreFsResult<()> {
        // No-op for in-memory device.
        Ok(())
    }

    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
        if !self.trim_supported {
            return Ok(());
        }
        check_alignment(offset, length, self.geometry.sector_size)?;
        check_bounds(offset, length, self.geometry.capacity_bytes)?;
        if length == 0 {
            return Ok(());
        }
        // Zero-fill den getrimmten Bereich (simuliert Device-Discard).
        let start = offset as usize;
        let end = start + length as usize;
        self.data[start..end].fill(0);
        self.trimmed_ranges.push((offset, length));
        Ok(())
    }

    fn supports_trim(&self) -> bool {
        self.trim_supported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(cap: u64, ss: u32) -> MemoryDevice {
        MemoryDevice::new(cap, ss).unwrap()
    }

    #[test]
    fn new_rejects_zero_capacity() {
        assert!(MemoryDevice::new(0, 512).is_err());
    }

    #[test]
    fn new_rejects_non_pow2_sector_size() {
        assert!(MemoryDevice::new(4096, 600).is_err());
    }

    #[test]
    fn new_rejects_capacity_not_multiple_of_sector() {
        assert!(MemoryDevice::new(4097, 4096).is_err());
    }

    #[test]
    fn geometry_is_consistent() {
        let d = dev(8192, 512);
        let g = d.geometry();
        assert_eq!(g.sector_size, 512);
        assert_eq!(g.sector_count, 16);
        assert_eq!(g.capacity_bytes, 8192);
        assert!(!g.read_only);
    }

    #[test]
    fn write_then_read_round_trips() {
        let mut d = dev(4096, 512);
        d.write_at(512, &[7u8; 512]).unwrap();
        let read = d.read_at(512, 512).unwrap();
        assert_eq!(read, vec![7u8; 512]);
    }

    #[test]
    fn read_misaligned_offset_rejected() {
        let d = dev(4096, 512);
        let err = d.read_at(100, 512).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn read_past_end_rejected() {
        let d = dev(4096, 512);
        let err = d.read_at(4096, 512).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn read_only_blocks_writes() {
        let mut d = dev(4096, 512);
        d.set_read_only(true);
        let err = d.write_at(0, &[0u8; 512]).unwrap_err();
        assert!(matches!(err, CoreFsError::PolicyViolation(_)));
    }

    #[test]
    fn trim_zero_fills_and_records_range() {
        let mut d = dev(4096, 512);
        d.write_at(0, &[1u8; 512]).unwrap();
        d.trim(0, 512).unwrap();
        assert_eq!(d.read_at(0, 512).unwrap(), vec![0u8; 512]);
        assert_eq!(d.trimmed_ranges(), &[(0, 512)]);
    }

    #[test]
    fn trim_silent_when_unsupported() {
        let mut d = dev(4096, 512);
        d.set_trim_supported(false);
        d.write_at(0, &[1u8; 512]).unwrap();
        d.trim(0, 512).unwrap();
        // Data preserved, no range recorded.
        assert_eq!(d.read_at(0, 512).unwrap(), vec![1u8; 512]);
        assert!(d.trimmed_ranges().is_empty());
    }

    #[test]
    fn from_bytes_constructs_device() {
        let buf = vec![42u8; 1024];
        let d = MemoryDevice::from_bytes(buf, 512).unwrap();
        assert_eq!(d.capacity(), 1024);
        assert_eq!(d.read_at(512, 512).unwrap(), vec![42u8; 512]);
    }

    #[test]
    fn check_alignment_validates_both_dims() {
        assert!(check_alignment(0, 0, 512).is_ok());
        assert!(check_alignment(512, 1024, 512).is_ok());
        assert!(check_alignment(100, 512, 512).is_err());
        assert!(check_alignment(0, 100, 512).is_err());
    }

    #[test]
    fn check_bounds_detects_overflow() {
        assert!(check_bounds(0, 100, 100).is_ok());
        assert!(check_bounds(50, 50, 100).is_ok());
        assert!(check_bounds(50, 51, 100).is_err());
        assert!(check_bounds(u64::MAX, 1, u64::MAX).is_err());
    }
}
