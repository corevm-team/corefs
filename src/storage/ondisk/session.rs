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
use super::volume::read_sb_with_fallbacks;
use super::volume::{FormatOptions, format_device};
use crate::app::PersistedState;
use crate::domain::inode::InodeId;
use crate::storage::block_store::{BlockRecord, ExtentRef};

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
    /// Cache of ODF extents to avoid re-writing unchanged content.
    odf_extents: std::collections::HashMap<InodeId, (u32, Vec<ExtentRef>)>,
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
            odf_extents: std::collections::HashMap::new(),
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
        let odf_extents = build_odf_extents_cache(&state);
        let mut service = CoreFsService::from_persisted_state(state);
        restore_bytes_from_odf_device(&device, &mut service)?;
        Ok(Self {
            image_path: path,
            device,
            service,
            odf_extents,
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
        let state = write_bytes_to_odf_device(
            &mut self.device,
            &self.service,
            state,
            &mut self.odf_extents,
        )?;
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
    /// Cache of InodeId → (content_crc, Vec<ExtentRef>) for blocks
    /// that have already been written to the ODF device.  Used to avoid
    /// re-writing unchanged file content on each flush.
    odf_extents: std::collections::HashMap<InodeId, (u32, Vec<ExtentRef>)>,
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
        Ok(Self {
            device,
            service,
            odf_extents: std::collections::HashMap::new(),
        })
    }

    /// Open an existing ODF volume from `device`, replaying any
    /// pending journal transactions.
    pub fn open(mut device: Box<dyn BlockDevice>) -> CoreFsResult<Self> {
        recover_pending_transactions(device.as_mut())?;
        let state = load_state_native(device.as_ref())?;
        let odf_extents = build_odf_extents_cache(&state);
        let mut service = CoreFsService::from_persisted_state(state);
        // Restore file bytes from ODF device into the compat device so
        // CoreFsService::read_file() works after open.
        restore_bytes_from_odf_device(device.as_ref(), &mut service)?;
        Ok(Self {
            device,
            service,
            odf_extents,
        })
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
        let state = write_bytes_to_odf_device(
            self.device.as_mut(),
            &self.service,
            state,
            &mut self.odf_extents,
        )?;
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

/// Public wrapper for `build_odf_extents_cache`.
pub fn build_odf_extents_cache_pub(
    state: &PersistedState,
) -> std::collections::HashMap<InodeId, (u32, Vec<ExtentRef>)> {
    build_odf_extents_cache(state)
}

/// Public wrapper for `restore_bytes_from_odf_device`.
pub fn restore_bytes_from_odf_device_pub(
    device: &dyn crate::storage::block_device::BlockDevice,
    service: &mut crate::app::CoreFsService,
) -> crate::error::CoreFsResult<()> {
    restore_bytes_from_odf_device(device, service)
}

/// Builds a cache of `InodeId → (content_crc, extents)` from a persisted state.
/// Used to detect unchanged files on subsequent flushes.
fn build_odf_extents_cache(
    state: &PersistedState,
) -> std::collections::HashMap<InodeId, (u32, Vec<ExtentRef>)> {
    let mut cache = std::collections::HashMap::new();
    for rec in &state.block_records {
        if !rec.extents.is_empty() {
            cache.insert(rec.inode, (rec.content_crc, rec.extents.clone()));
        }
    }
    cache
}

/// Reads file bytes from the ODF device's data blocks (via BlockRecord extents)
/// and writes them into the CoreFsService's compat device.  Called after
/// `from_persisted_state` so that `read_file()` works correctly.
fn restore_bytes_from_odf_device(
    device: &dyn crate::storage::block_device::BlockDevice,
    service: &mut crate::app::CoreFsService,
) -> crate::error::CoreFsResult<()> {
    let state = service.export_state();
    for rec in &state.block_records {
        if rec.extents.is_empty() || rec.logical_size == 0 {
            continue;
        }
        // Read bytes from the ODF device at the extent's physical block.
        let mut all_bytes = Vec::with_capacity(rec.logical_size as usize);
        for ext in &rec.extents {
            let byte_offset = ext.physical_block * BLOCK_SIZE;
            let read_len = u64::from(ext.length_blocks) * BLOCK_SIZE;
            if byte_offset + read_len <= device.capacity() {
                let buf = device.read_at(byte_offset, read_len)?;
                let want = (ext.logical_len as usize).min(buf.len());
                all_bytes.extend_from_slice(&buf[..want]);
            }
        }
        all_bytes.truncate(rec.logical_size as usize);
        if !all_bytes.is_empty() {
            service.blocks_write(rec.inode, all_bytes);
        }
    }
    Ok(())
}

/// Write file bytes from the compat device to the ODF device and return
/// an updated `block_records` list with proper ODF extents.
///
/// After Phase-A, `BlockStore::write(inode, bytes)` stores bytes in an
/// internal compat `MemoryDevice` and creates extents pointing to that
/// device's physical blocks.  Those block numbers are meaningless on the
/// ODF device.  This helper reads the bytes back from the compat device
/// and writes them to the ODF device's data region, then populates the
/// `BlockRecord.extents` with valid ODF block addresses so that
/// `save_state_native` can reuse them.
///
/// `odf_extents_cache` maps InodeId → (content_crc, extents) for blocks
/// already written to the ODF device.  Unchanged files reuse their cached
/// extents to avoid double-allocating blocks on each flush.
/// Public wrapper around `write_bytes_to_odf_device` for migration paths.
pub fn write_bytes_to_odf_device_pub(
    device: &mut dyn crate::storage::block_device::BlockDevice,
    service: &crate::app::CoreFsService,
    state: PersistedState,
    odf_extents_cache: &mut std::collections::HashMap<InodeId, (u32, Vec<ExtentRef>)>,
) -> crate::error::CoreFsResult<PersistedState> {
    write_bytes_to_odf_device(device, service, state, odf_extents_cache)
}

fn write_bytes_to_odf_device(
    device: &mut dyn crate::storage::block_device::BlockDevice,
    service: &crate::app::CoreFsService,
    mut state: PersistedState,
    odf_extents_cache: &mut std::collections::HashMap<InodeId, (u32, Vec<ExtentRef>)>,
) -> crate::error::CoreFsResult<PersistedState> {
    use super::bitmap::Bitmap;
    use super::checksum::Crc32c;
    use std::collections::HashMap;

    let block_bytes: HashMap<InodeId, Vec<u8>> = service.read_all_block_bytes();
    if block_bytes.is_empty() {
        // No file bytes → clear any stale extents from compat device and cache.
        for rec in &mut state.block_records {
            if !rec.extents.is_empty() {
                rec.extents.clear();
            }
        }
        odf_extents_cache.retain(|id, _| state.block_records.iter().any(|r| r.inode == *id));
        return Ok(state);
    }

    // Read the superblock to get the data region start.
    let sb = read_sb_with_fallbacks(device).map_err(|e| {
        crate::error::CoreFsError::State(format!("odf session: cannot read superblock: {e}"))
    })?;
    let geom = sb.geometry();
    // Find the next free data block after the current end of used blocks.
    // We lay blobs out sequentially starting from `geom.data_start`.
    // To avoid conflicts with inodes already saved, we start at the end
    // of the data region and work backwards … actually: read the block
    // bitmap to find free blocks.  For simplicity, scan from data_start.
    let bbm_bytes = device
        .read_at(
            geom.block_bitmap_start * BLOCK_SIZE,
            geom.block_bitmap_blocks * BLOCK_SIZE,
        )
        .unwrap_or_else(|_| vec![0u8; (geom.block_bitmap_blocks * BLOCK_SIZE) as usize]);
    let mut bbm = Bitmap::from_bytes(bbm_bytes, geom.total_blocks)
        .unwrap_or_else(|_| Bitmap::new(geom.total_blocks));

    // Find the first free data block.
    let mut next_data_block = geom.data_start;
    while next_data_block < geom.total_blocks {
        if !bbm.is_set(next_data_block).unwrap_or(true) {
            break;
        }
        next_data_block += 1;
    }

    let mut bytes_by_inode: HashMap<InodeId, Vec<u8>> = block_bytes;

    for rec in &mut state.block_records {
        let inode_bytes = bytes_by_inode.remove(&rec.inode).unwrap_or_default();
        if inode_bytes.is_empty() {
            // Clear stale compat extents.
            rec.extents.clear();
            rec.logical_size = 0;
            rec.content_crc = 0;
            continue;
        }

        let crc = Crc32c::hash(&inode_bytes);
        let size = inode_bytes.len() as u64;

        // Check if this inode's content is already on the ODF device with the same CRC.
        // If so, reuse the existing extents to avoid double-allocating blocks.
        if let Some((cached_crc, cached_extents)) = odf_extents_cache.get(&rec.inode) {
            if *cached_crc == crc && !cached_extents.is_empty() {
                // Content unchanged and already on ODF device — reuse extents.
                rec.extents = cached_extents.clone();
                rec.logical_size = size;
                rec.content_crc = crc;
                // Mark cached blocks as still used in the bitmap (they should already be).
                for ext in cached_extents {
                    for b in 0..u64::from(ext.length_blocks) {
                        let _ = bbm.set(ext.physical_block + b);
                    }
                }
                continue;
            }
        }

        let needed_blocks = size.div_ceil(BLOCK_SIZE);

        // Check if there's room.
        if next_data_block + needed_blocks > geom.total_blocks {
            // Not enough room; clear extents (file will appear empty).
            rec.extents.clear();
            continue;
        }

        // Find contiguous free blocks for this file.
        // Re-scan to skip any used blocks (attr blocks, etc.) that may be interspersed.
        let mut found_block = None;
        let mut scan = next_data_block;
        while scan + needed_blocks <= geom.total_blocks {
            // Check if all needed_blocks starting at `scan` are free.
            let all_free = (0..needed_blocks).all(|b| !bbm.is_set(scan + b).unwrap_or(true));
            if all_free {
                found_block = Some(scan);
                break;
            }
            scan += 1;
        }
        let phys_block = match found_block {
            Some(b) => b,
            None => {
                // Not enough room; clear extents (file will appear empty).
                rec.extents.clear();
                continue;
            }
        };
        let padded_len = (needed_blocks * BLOCK_SIZE) as usize;
        let mut buf = vec![0u8; padded_len];
        buf[..inode_bytes.len()].copy_from_slice(&inode_bytes);
        device.write_at(phys_block * BLOCK_SIZE, &buf)?;

        // Mark those blocks as used in the bitmap.
        for b in 0..needed_blocks {
            let _ = bbm.set(phys_block + b);
        }
        // Advance past the written blocks for the next iteration's initial guess.
        next_data_block = phys_block + needed_blocks;

        // Update the BlockRecord with the ODF extent.
        let extent = ExtentRef {
            logical_block: 0,
            logical_len: size as u32,
            physical_block: phys_block,
            length_blocks: needed_blocks as u32,
            physical_len: size as u32,
            content_crc: crc,
            flags: 0,
        };
        rec.extents = vec![extent.clone()];
        rec.logical_size = size;
        rec.content_crc = crc;
        // Update the cache.
        odf_extents_cache.insert(rec.inode, (crc, vec![extent]));
    }

    // Write the updated bitmap back to the device so that save_state_native_incremental
    // doesn't re-allocate the blocks we just wrote file data to.
    device.write_at(geom.block_bitmap_start * BLOCK_SIZE, bbm.as_bytes())?;

    Ok(state)
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
