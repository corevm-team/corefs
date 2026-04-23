// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Service-freie Session-Abstraktion für ein CoreFS-Volume.
//!
//! [`OdfDeviceSession`] besitzt ein [`BlockDevice`] sowie einen
//! aktuell hydrierten [`PersistedState`]. Mutationen erfolgen direkt
//! gegen den `PersistedState` (keine `CoreFsService`-Schicht), damit
//! die Session in `no_std + alloc`-Kontexten (AnyOS-Userspace,
//! AnyOS-Kernel-Treiber) ohne die std-gebundene App-Schicht nutzbar
//! ist.
//!
//! Konsumenten, die eine high-level Service-API brauchen
//! (`create_file`, `write_file`, …), können die main-Crate-Wrapper
//! `OdfFileSession` und die ältere `OdfDeviceSession` aus dem
//! `corefs::storage::ondisk::session`-Modul des main `corefs` crate
//! verwenden, die zusätzlich `CoreFsService` einhängen.

use crate::config::CoreFsConfig;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::Timestamp;
use crate::storage::block_device::BlockDevice;
use crate::storage::persisted_state::PersistedState;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use super::layout::{BLOCK_SIZE, DEFAULT_INODE_COUNT, DEFAULT_JOURNAL_BLOCKS, MIN_VOLUME_BLOCKS};
use super::native::{
    IncrementalSaveReport, load_state_native, save_state_native, save_state_native_incremental,
};
use super::volume::{FormatOptions, format_device};

/// Minimale Volume-Kapazität in Bytes für ein neu formatiertes Volume.
pub const MIN_ODF_CAPACITY_BYTES: u64 = MIN_VOLUME_BLOCKS * BLOCK_SIZE;

/// Konfigurations-Optionen für [`OdfDeviceSession::format_new_at`].
#[derive(Debug, Clone)]
pub struct OdfSessionOptions {
    /// Gesamtkapazität in Bytes (nur beim Frisch-Formatieren relevant).
    pub capacity_bytes: u64,
    /// Volume-Label. Wird auf 32 Bytes gekürzt.
    pub label: String,
    /// Volume-UUID; ein All-Zero-Wert lässt den Konstruktor einen
    /// deterministischen Zeitstempel-basierten Pseudo-UUID vergeben.
    pub uuid: [u8; 16],
    /// Anzahl On-Disk-Inode-Slots.
    pub inode_count: u64,
    /// Größe des Journal-Bereichs in 4-KiB-Blöcken.
    pub journal_blocks: u64,
    /// Volume-Konfiguration, die in den Initial-`PersistedState` einfließt.
    pub config: CoreFsConfig,
}

impl OdfSessionOptions {
    /// Default 64 MiB-Volume mit Standard-Parametern.
    #[must_use]
    pub fn with_defaults() -> Self {
        use alloc::string::ToString;
        Self {
            capacity_bytes: 64 * 1024 * 1024,
            label: "corefs".to_string(),
            uuid: [0u8; 16],
            inode_count: DEFAULT_INODE_COUNT,
            journal_blocks: DEFAULT_JOURNAL_BLOCKS,
            config: CoreFsConfig::default(),
        }
    }
}

/// Report einer [`OdfDeviceSession::flush`]/[`OdfDeviceSession::mutate`]-Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushReport {
    /// Inkrementelle-Save-Statistik.
    pub incremental: IncrementalSaveReport,
}

/// Session, die ein eigenständiges [`BlockDevice`] besitzt und einen
/// hydrierten [`PersistedState`] verwaltet — service-frei.
pub struct OdfDeviceSession {
    device: Box<dyn BlockDevice>,
    state: PersistedState,
}

impl core::fmt::Debug for OdfDeviceSession {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OdfDeviceSession")
            .field("device_capacity", &self.device.capacity())
            .field("active_inodes", &self.state.active_inodes.len())
            .finish()
    }
}

impl OdfDeviceSession {
    /// Formatiert ein frisches ODF-Volume auf `device` und persistiert
    /// einen leeren `PersistedState` im NATIVE-Layout.
    ///
    /// Diese no_std-fähige Variante nimmt den Erstellungs-Timestamp
    /// explizit entgegen.
    pub fn format_new_at(
        mut device: Box<dyn BlockDevice>,
        options: &OdfSessionOptions,
        created_at: Timestamp,
    ) -> CoreFsResult<Self> {
        if options.capacity_bytes < MIN_ODF_CAPACITY_BYTES {
            return Err(CoreFsError::InvalidInput(format!(
                "ODF session: capacity {} below minimum {MIN_ODF_CAPACITY_BYTES}",
                options.capacity_bytes
            )));
        }
        let format_opts = FormatOptions {
            label: options.label.clone(),
            uuid: resolve_uuid(options.uuid, created_at),
            inode_count: options.inode_count,
            journal_blocks: options.journal_blocks,
        };
        format_device(device.as_mut(), &format_opts)?;
        let state = PersistedState::empty_at(options.config.clone(), created_at);
        save_state_native(device.as_mut(), &state)?;
        Ok(Self { device, state })
    }

    /// Bequemere Variante von [`OdfDeviceSession::format_new_at`] für
    /// std-basierte Umgebungen (Erstellungszeit aus
    /// [`std::time::SystemTime::now`]).
    #[cfg(feature = "std")]
    pub fn format_new(
        device: Box<dyn BlockDevice>,
        options: &OdfSessionOptions,
    ) -> CoreFsResult<Self> {
        Self::format_new_at(device, options, Timestamp::now())
    }

    /// Öffnet ein bestehendes ODF-Volume und hydriert seinen
    /// `PersistedState` aus dem NATIVE-Layout.
    pub fn open(device: Box<dyn BlockDevice>) -> CoreFsResult<Self> {
        let state = load_state_native(device.as_ref())?;
        Ok(Self { device, state })
    }

    /// Liefert eine Referenz auf den hydrierten Zustand.
    #[must_use]
    pub fn state(&self) -> &PersistedState {
        &self.state
    }

    /// Liefert eine `&mut`-Referenz auf den hydrierten Zustand.
    ///
    /// Aufrufer, die den Zustand verändern, sollten anschließend
    /// [`OdfDeviceSession::flush`] aufrufen, damit die Änderungen
    /// persistiert werden — alternativ direkt [`OdfDeviceSession::mutate`]
    /// nutzen, das beides in einem Aufruf erledigt.
    pub fn state_mut(&mut self) -> &mut PersistedState {
        &mut self.state
    }

    /// Liefert eine Referenz auf das zugrundeliegende BlockDevice.
    #[must_use]
    pub fn device(&self) -> &dyn BlockDevice {
        self.device.as_ref()
    }

    /// Schreibt den aktuellen `PersistedState` inkrementell zurück.
    pub fn flush(&mut self) -> CoreFsResult<FlushReport> {
        let incremental = save_state_native_incremental(self.device.as_mut(), &self.state)?;
        Ok(FlushReport { incremental })
    }

    /// Führt eine Mutation am `PersistedState` aus und flusht
    /// anschließend automatisch.
    ///
    /// Schlägt der Operation-Closure fehl, wird **nicht** geflusht — der
    /// Zustand bleibt im RAM unverändert (außer der Closure hat ihn
    /// bereits partiell modifiziert; das ist Aufrufer-Verantwortung).
    pub fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut PersistedState) -> CoreFsResult<T>,
    ) -> CoreFsResult<(T, FlushReport)> {
        let value = operation(&mut self.state)?;
        let report = self.flush()?;
        Ok((value, report))
    }

    /// Konsumiert die Session und gibt das Block-Device zurück.
    #[must_use]
    pub fn into_device(self) -> Box<dyn BlockDevice> {
        self.device
    }
}

fn resolve_uuid(requested: [u8; 16], created_at: Timestamp) -> [u8; 16] {
    if requested != [0u8; 16] {
        return requested;
    }
    // Deterministischer Pseudo-UUID aus dem Zeitstempel — vermeidet eine
    // separate `Rng`-Abhängigkeit für die Session und reicht als Identifier
    // für ein frisch formatiertes Volume.
    let secs = created_at.as_secs();
    let nanos = created_at.subsec_nanos() as u64;
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&secs.to_le_bytes());
    out[8..12].copy_from_slice(&(nanos as u32).to_le_bytes());
    out[12..16].copy_from_slice(&((secs ^ nanos) as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::block_device::MemoryDevice;

    fn make_dev(capacity: u64) -> Box<dyn BlockDevice> {
        Box::new(MemoryDevice::new(capacity, 4096).expect("memory device"))
    }

    #[test]
    fn format_new_at_yields_native_volume() {
        let opts = OdfSessionOptions {
            capacity_bytes: 4 * 1024 * 1024,
            ..OdfSessionOptions::with_defaults()
        };
        let session =
            OdfDeviceSession::format_new_at(make_dev(4 * 1024 * 1024), &opts, Timestamp::EPOCH)
                .expect("format ok");
        // Zustand ist leer
        assert!(session.state().active_inodes.is_empty());
        assert!(session.state().clean_unmount);
    }

    #[test]
    fn format_new_rejects_too_small_capacity() {
        let opts = OdfSessionOptions {
            capacity_bytes: 4096, // viel zu klein
            ..OdfSessionOptions::with_defaults()
        };
        let err = OdfDeviceSession::format_new_at(make_dev(4096), &opts, Timestamp::EPOCH)
            .expect_err("must reject");
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn open_round_trips_via_format_then_open() {
        let opts = OdfSessionOptions {
            capacity_bytes: 4 * 1024 * 1024,
            ..OdfSessionOptions::with_defaults()
        };
        let dev = make_dev(4 * 1024 * 1024);
        let session =
            OdfDeviceSession::format_new_at(dev, &opts, Timestamp::EPOCH).expect("format ok");
        let dev = session.into_device();
        let reopened = OdfDeviceSession::open(dev).expect("open ok");
        assert_eq!(reopened.state().volume.name, "corefs");
        assert!(reopened.state().clean_unmount);
    }

    #[test]
    fn mutate_persists_changes() {
        let opts = OdfSessionOptions {
            capacity_bytes: 4 * 1024 * 1024,
            ..OdfSessionOptions::with_defaults()
        };
        let dev = make_dev(4 * 1024 * 1024);
        let mut session =
            OdfDeviceSession::format_new_at(dev, &opts, Timestamp::EPOCH).expect("format ok");

        let ((), _flush) = session
            .mutate(|state| {
                state.next_snapshot_id = 42;
                Ok(())
            })
            .expect("mutate ok");
        assert_eq!(session.state().next_snapshot_id, 42);

        // Reopen und verifizieren
        let dev = session.into_device();
        let reopened = OdfDeviceSession::open(dev).expect("reopen ok");
        assert_eq!(reopened.state().next_snapshot_id, 42);
    }

    #[test]
    fn resolve_uuid_uses_requested_when_nonzero() {
        let req = [9u8; 16];
        assert_eq!(resolve_uuid(req, Timestamp::EPOCH), req);
    }

    #[test]
    fn resolve_uuid_derives_from_timestamp_when_zero() {
        let a = resolve_uuid([0u8; 16], Timestamp::from_secs(1_000));
        let b = resolve_uuid([0u8; 16], Timestamp::from_secs(2_000));
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 16]);
    }
}
