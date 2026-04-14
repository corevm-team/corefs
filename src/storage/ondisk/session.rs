// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! High-level session wrappers that couple a CoreFS service instance
//! to an ODF-backed volume (file or block device).
//!
//! Analogue of [`crate::storage::volume_session::VolumeSession`] /
//! [`crate::storage::volume_session::DeviceVolumeSession`] but persisting
//! through the [`super::native`] layout instead of the legacy
//! [`crate::storage::volume_image`] segment-frame format.
//!
//! ## Responsibilities
//!
//! * **Lifecycle** — `format_new` / `open` / `open_or_format` with a
//!   fixed capacity parameter for the fresh case.
//! * **Flush** — every flush goes through
//!   [`save_state_native_incremental`], so after the initial save only
//!   the changed inodes end up being written.
//! * **Mutate** — combines a mutation closure with an immediate flush.
//!
//! ## Crash recovery
//!
//! `open` (and `open_or_format` on an existing volume) calls
//! [`super::journaled::recover_pending_transactions`] before reading the
//! state, so any half-finished save from a previous mount is replayed
//! transparently before the session exposes data.

use std::path::{Path, PathBuf};

use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::{BlockDevice, FileImageDevice};

use super::journaled::recover_pending_transactions;
use super::layout::{BLOCK_SIZE, DEFAULT_INODE_COUNT, DEFAULT_JOURNAL_BLOCKS};
use super::native::{
    IncrementalSaveReport, load_state_native, save_state_native, save_state_native_incremental,
};
use super::volume::{FormatOptions, format_device};

/// Minimum capacity in bytes for a freshly-formatted ODF volume.
pub const MIN_ODF_CAPACITY_BYTES: u64 = super::layout::MIN_VOLUME_BLOCKS * BLOCK_SIZE;

/// User-facing options for [`OdfFileSession::format_new`].
#[derive(Debug, Clone)]
pub struct OdfSessionOptions {
    /// Total capacity in bytes (only honoured when formatting fresh).
    pub capacity_bytes: u64,
    /// Volume label.  Truncated to 32 bytes.
    pub label: String,
    /// Volume UUID; set to all-zero for a time-based pseudo-UUID.
    pub uuid: [u8; 16],
    /// Number of on-disk inode slots.
    pub inode_count: u64,
    /// Journal region size in 4 KiB blocks.
    pub journal_blocks: u64,
    /// CoreFS-level configuration applied to the fresh service.
    pub config: CoreFsConfig,
}

impl OdfSessionOptions {
    /// Default 64 MiB image with standard parameters.
    pub fn with_defaults() -> Self {
        Self {
            capacity_bytes: 64 * 1024 * 1024,
            label: "corefs".into(),
            uuid: [0u8; 16],
            inode_count: DEFAULT_INODE_COUNT,
            journal_blocks: DEFAULT_JOURNAL_BLOCKS,
            config: CoreFsConfig::default(),
        }
    }
}

/// Report returned from [`OdfFileSession::flush`] / ::mutate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushReport {
    pub incremental: IncrementalSaveReport,
}

// ---------------------------------------------------------------------------
// OdfFileSession — file-backed ODF volume
// ---------------------------------------------------------------------------

/// Session that owns a file-backed ODF volume plus a CoreFS service
/// hydrated from it.
#[derive(Debug)]
pub struct OdfFileSession {
    image_path: PathBuf,
    device: FileImageDevice,
    service: CoreFsService,
}

impl OdfFileSession {
    /// Create a fresh image file, format it in native mode, and seed
    /// the service from the provided [`CoreFsConfig`].
    pub fn format_new(path: impl AsRef<Path>, options: &OdfSessionOptions) -> CoreFsResult<Self> {
        let capacity = options.capacity_bytes;
        if capacity < MIN_ODF_CAPACITY_BYTES {
            return Err(CoreFsError::InvalidInput(format!(
                "ODF session: capacity {capacity} below minimum {MIN_ODF_CAPACITY_BYTES}"
            )));
        }
        let path = path.as_ref().to_path_buf();
        let mut device = FileImageDevice::create(&path, capacity, 4096)?;
        let format_opts = FormatOptions {
            label: options.label.clone(),
            uuid: resolve_uuid(options.uuid),
            inode_count: options.inode_count,
            journal_blocks: options.journal_blocks,
        };
        format_device(&mut device, &format_opts)?;
        let service = CoreFsService::format(options.config.clone());
        // Initial full save so the volume is in LAYOUT_MODE_NATIVE.
        let state = service.persisted_state();
        save_state_native(&mut device, &state)?;
        Ok(Self {
            image_path: path,
            device,
            service,
        })
    }

    /// Open an existing image file, replay any pending journal
    /// transactions, and hydrate the service from the on-disk state.
    pub fn open(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(CoreFsError::NotFound(format!(
                "ODF session: image not found: {}",
                path.display()
            )));
        }
        let mut device = FileImageDevice::open(&path, false)?;
        recover_pending_transactions(&mut device)?;
        let state = load_state_native(&device)?;
        let service = CoreFsService::from_persisted_state(state);
        Ok(Self {
            image_path: path,
            device,
            service,
        })
    }

    /// Open an existing volume or format a new one with `options`.
    pub fn open_or_format(
        path: impl AsRef<Path>,
        options: &OdfSessionOptions,
    ) -> CoreFsResult<Self> {
        let p = path.as_ref();
        if p.exists() {
            Self::open(p)
        } else {
            Self::format_new(p, options)
        }
    }

    pub fn image_path(&self) -> &Path {
        &self.image_path
    }

    pub fn service(&self) -> &CoreFsService {
        &self.service
    }

    pub fn service_mut(&mut self) -> &mut CoreFsService {
        &mut self.service
    }

    /// Read-only handle onto the underlying device (useful for fsck/
    /// inspect tooling that wants structural access while a session
    /// is open).
    pub fn device(&self) -> &FileImageDevice {
        &self.device
    }

    /// Incrementally flush the current service state.  Only changed
    /// inodes + the ancillary slot + the superblock are rewritten.
    pub fn flush(&mut self) -> CoreFsResult<FlushReport> {
        let state = self.service.persisted_state();
        let incremental = save_state_native_incremental(&mut self.device, &state)?;
        Ok(FlushReport { incremental })
    }

    /// Run `operation` against the service, then flush.
    pub fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut CoreFsService) -> CoreFsResult<T>,
    ) -> CoreFsResult<(T, FlushReport)> {
        let value = operation(&mut self.service)?;
        let report = self.flush()?;
        Ok((value, report))
    }
}

// ---------------------------------------------------------------------------
// OdfDeviceSession — block-device-backed ODF volume
// ---------------------------------------------------------------------------

/// Session backed by an owned [`crate::storage::block_device::BlockDevice`] trait object (file image,
/// `/dev/sdX`, in-memory, etc.) instead of a file path.
pub struct OdfDeviceSession {
    device: Box<dyn BlockDevice>,
    service: CoreFsService,
}

impl std::fmt::Debug for OdfDeviceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OdfDeviceSession")
            .field("device_capacity", &self.device.capacity())
            .finish()
    }
}

impl OdfDeviceSession {
    /// Format a fresh ODF volume on `device`.
    pub fn format_new(
        mut device: Box<dyn BlockDevice>,
        options: &OdfSessionOptions,
    ) -> CoreFsResult<Self> {
        let format_opts = FormatOptions {
            label: options.label.clone(),
            uuid: resolve_uuid(options.uuid),
            inode_count: options.inode_count,
            journal_blocks: options.journal_blocks,
        };
        format_device(device.as_mut(), &format_opts)?;
        let service = CoreFsService::format(options.config.clone());
        let state = service.persisted_state();
        save_state_native(device.as_mut(), &state)?;
        Ok(Self { device, service })
    }

    /// Open an existing ODF volume from `device`, replaying any
    /// pending journal transactions.
    pub fn open(mut device: Box<dyn BlockDevice>) -> CoreFsResult<Self> {
        recover_pending_transactions(device.as_mut())?;
        let state = load_state_native(device.as_ref())?;
        let service = CoreFsService::from_persisted_state(state);
        Ok(Self { device, service })
    }

    pub fn service(&self) -> &CoreFsService {
        &self.service
    }

    pub fn service_mut(&mut self) -> &mut CoreFsService {
        &mut self.service
    }

    pub fn device(&self) -> &dyn BlockDevice {
        self.device.as_ref()
    }

    /// Incrementally flush.
    pub fn flush(&mut self) -> CoreFsResult<FlushReport> {
        let state = self.service.persisted_state();
        let incremental = save_state_native_incremental(self.device.as_mut(), &state)?;
        Ok(FlushReport { incremental })
    }

    pub fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut CoreFsService) -> CoreFsResult<T>,
    ) -> CoreFsResult<(T, FlushReport)> {
        let value = operation(&mut self.service)?;
        let report = self.flush()?;
        Ok((value, report))
    }

    /// Consume the session and return the underlying device.
    pub fn into_device(self) -> Box<dyn BlockDevice> {
        self.device
    }
}

fn resolve_uuid(requested: [u8; 16]) -> [u8; 16] {
    if requested != [0u8; 16] {
        return requested;
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let lo = nanos as u64;
    let hi = (nanos >> 64) as u64;
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&lo.to_le_bytes());
    out[8..16].copy_from_slice(&hi.to_le_bytes());
    out
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
