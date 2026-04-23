// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Std-seitige Backends für [`crate::storage::block_device::BlockDevice`].
//!
//! Die plattformneutralen Definitionen ([`BlockDevice`], [`DeviceGeometry`],
//! [`MemoryDevice`], Sektor-Konstanten, Alignment-Helfer) leben jetzt in
//! [`corefs_core::storage::block_device`] und werden hier transparent
//! re-exportiert. Nur die std-/Linux-spezifischen Backends ([`FileImageDevice`]
//! und [`raw::RawBlockDevice`]) sowie die Verifikations-Helfer
//! ([`sanity_check_writable`], [`verify_device_capacity`]) leben weiterhin
//! in dieser Datei.

use crate::error::{CoreFsError, CoreFsResult};
use std::path::{Path, PathBuf};

// Re-exports der no_std-Definitionen aus corefs-core. So bleiben alle
// `use crate::storage::block_device::{BlockDevice, DeviceGeometry, …}`-Pfade
// in den Konsumenten unverändert lauffähig.
pub use corefs_core::storage::block_device::{
    BlockDevice, DeviceGeometry, MemoryDevice, SECTOR_SIZE_4K, SECTOR_SIZE_512,
    check_alignment, check_bounds, check_write_permission,
};

// ---------------------------------------------------------------------------
// Fake-stick detection helpers
// ---------------------------------------------------------------------------

/// Result of a writability verification run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceVerificationReport {
    /// Total device capacity as reported by the device.
    pub advertised_bytes: u64,
    /// Byte offsets that were probed.
    pub probed_offsets: Vec<u64>,
    /// Byte offsets where the write-and-read-back check failed.
    pub failed_offsets: Vec<u64>,
    /// Highest byte offset that was successfully written and verified.
    pub highest_verified_offset: u64,
    /// Estimated actually usable capacity (bytes) based on the last
    /// successfully verified offset.  May underestimate on partial failure.
    pub estimated_usable_bytes: u64,
}

impl DeviceVerificationReport {
    pub fn is_honest(&self) -> bool {
        self.failed_offsets.is_empty()
    }

    pub fn fake_ratio_percent(&self) -> u8 {
        if self.advertised_bytes == 0 {
            return 100;
        }
        let usable_ratio =
            (self.estimated_usable_bytes as f64 / self.advertised_bytes as f64).clamp(0.0, 1.0);
        ((1.0 - usable_ratio) * 100.0).round() as u8
    }
}

/// Generates a unique 4-KiB test pattern for a given offset.
/// The pattern is the little-endian offset repeated, XORed with a rolling
/// counter.  Different offsets therefore produce different patterns.
fn generate_test_pattern(offset: u64, length: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(length);
    let seed = offset.wrapping_mul(0x9E3779B97F4A7C15);
    for i in 0..length {
        let byte = (seed.wrapping_add(i as u64).wrapping_mul(0x100000001B3))
            .wrapping_shr((i % 8 * 8) as u32) as u8;
        buf.push(byte);
    }
    buf
}

/// Performs a quick sampled write-and-read-back check at several offsets
/// distributed across the device.  Used to detect fake/counterfeit flash
/// devices that advertise more capacity than they actually have.
///
/// `skip_below`: offsets below this byte position are not probed (useful
/// to preserve a freshly written volume image at the start of the device).
///
/// On success, zero-fills the probed sectors to leave the device in a
/// known state.  On failure, returns a report listing which offsets could
/// not be written.
pub fn sanity_check_writable(
    device: &mut dyn BlockDevice,
    skip_below: u64,
) -> CoreFsResult<DeviceVerificationReport> {
    let capacity = device.capacity();
    let sector_size = device.sector_size() as u64;
    let pattern_len = (4096u64.max(sector_size) as usize).min(65536);
    let pattern_len = (pattern_len as u64 / sector_size * sector_size) as usize;

    // Place probes at 10%, 25%, 50%, 75%, 90%, 99% of capacity (sector-aligned).
    let probe_ratios = [10, 25, 50, 75, 90, 99];
    let mut offsets: Vec<u64> = probe_ratios
        .iter()
        .map(|pct| {
            let raw = (capacity / 100) * *pct;
            let aligned = (raw / sector_size) * sector_size;
            aligned.max(skip_below.div_ceil(sector_size) * sector_size)
        })
        .filter(|o| *o + pattern_len as u64 <= capacity)
        .collect();
    offsets.sort();
    offsets.dedup();

    let mut failed = Vec::new();
    let mut highest_verified = 0u64;

    for &offset in &offsets {
        let pattern = generate_test_pattern(offset, pattern_len);

        if device.write_at(offset, &pattern).is_err() {
            failed.push(offset);
            continue;
        }
        if device.sync().is_err() {
            failed.push(offset);
            continue;
        }

        match device.read_at(offset, pattern_len as u64) {
            Ok(read_back) => {
                if read_back != pattern {
                    failed.push(offset);
                } else {
                    highest_verified = highest_verified.max(offset + pattern_len as u64);
                    // Zero-fill the probe region to leave no stale test data.
                    let zeros = vec![0u8; pattern_len];
                    let _ = device.write_at(offset, &zeros);
                }
            }
            Err(_) => failed.push(offset),
        }
    }

    let _ = device.sync();

    let estimated_usable = if failed.is_empty() {
        capacity
    } else {
        highest_verified
    };

    Ok(DeviceVerificationReport {
        advertised_bytes: capacity,
        probed_offsets: offsets,
        failed_offsets: failed,
        highest_verified_offset: highest_verified,
        estimated_usable_bytes: estimated_usable,
    })
}

/// Progressive destructive capacity test: writes unique patterns in chunks
/// across the entire device and reads each back to verify.  Detects fake
/// sticks by finding the actual writable capacity.
///
/// **Destroys all data on the device.**  Intended for use on a freshly
/// formatted or empty device, or via an explicit `--destructive` flag.
///
/// `chunk_size`: bytes per probe chunk (rounded up to sector size).
/// `chunk_count`: number of chunks to probe, evenly spaced.  Use 100 for
/// a fast rough scan, 1000+ for a thorough scan.
pub fn verify_device_capacity(
    device: &mut dyn BlockDevice,
    chunk_size: u64,
    chunk_count: u64,
) -> CoreFsResult<DeviceVerificationReport> {
    check_write_permission(device.is_read_only())?;

    let capacity = device.capacity();
    let sector_size = device.sector_size() as u64;
    let chunk_size = chunk_size.div_ceil(sector_size) * sector_size;
    let chunk_size = chunk_size.max(sector_size);

    if chunk_count == 0 {
        return Err(CoreFsError::InvalidInput(
            "chunk_count must be greater than zero".to_string(),
        ));
    }

    // Space probes evenly.  First probe at offset 0, last at capacity - chunk_size.
    let max_offset = capacity.saturating_sub(chunk_size);
    let mut offsets: Vec<u64> = if chunk_count == 1 {
        vec![0]
    } else {
        (0..chunk_count)
            .map(|i| {
                let raw = max_offset * i / (chunk_count - 1);
                (raw / sector_size) * sector_size
            })
            .collect()
    };
    offsets.sort();
    offsets.dedup();

    let mut failed = Vec::new();
    let mut highest_verified = 0u64;

    for &offset in &offsets {
        let pattern = generate_test_pattern(offset, chunk_size as usize);

        if device.write_at(offset, &pattern).is_err() {
            failed.push(offset);
            continue;
        }
        if device.sync().is_err() {
            failed.push(offset);
            continue;
        }

        match device.read_at(offset, chunk_size) {
            Ok(read_back) => {
                if read_back != pattern {
                    failed.push(offset);
                } else {
                    highest_verified = highest_verified.max(offset + chunk_size);
                }
            }
            Err(_) => failed.push(offset),
        }
    }

    let _ = device.sync();

    let estimated_usable = if failed.is_empty() {
        capacity
    } else {
        highest_verified
    };

    Ok(DeviceVerificationReport {
        advertised_bytes: capacity,
        probed_offsets: offsets,
        failed_offsets: failed,
        highest_verified_offset: highest_verified,
        estimated_usable_bytes: estimated_usable,
    })
}

// ---------------------------------------------------------------------------
// FileImageDevice — file-backed .img storage
// ---------------------------------------------------------------------------

/// A [`crate::storage::block_device::BlockDevice`] backed by a regular file (`.img`).
///
/// The file is memory-mapped conceptually: reads and writes go through
/// `std::fs::File` with `seek` + `read_exact` / `write_all`.
/// This replaces the monolithic `fs::read` / `fs::write` in
/// `volume_image.rs` with a sector-aligned, random-access API.
#[derive(Debug)]
pub struct FileImageDevice {
    path: PathBuf,
    file: std::fs::File,
    geometry: DeviceGeometry,
    metadata_dirty: bool,
    write_through: bool,
}

impl FileImageDevice {
    /// Opens an existing image file.
    pub fn open(path: impl AsRef<Path>, read_only: bool) -> CoreFsResult<Self> {
        Self::open_with_options(path, read_only, false)
    }

    /// Opens an existing image file and optionally requests write-through I/O.
    ///
    /// Write-through mode is useful for mounted image files: individual
    /// sector-aligned writes become durable at the device boundary, avoiding a
    /// handle-wide flush of the whole image on every fsync.  Metadata changes
    /// such as resize still force a full metadata sync.
    pub fn open_with_write_through(
        path: impl AsRef<Path>,
        read_only: bool,
        write_through: bool,
    ) -> CoreFsResult<Self> {
        Self::open_with_options(path, read_only, write_through)
    }

    fn open_with_options(
        path: impl AsRef<Path>,
        read_only: bool,
        write_through: bool,
    ) -> CoreFsResult<Self> {
        let path = path.as_ref();
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(!read_only);
        apply_write_through(&mut options, write_through && !read_only);
        let file = options.open(path).map_err(|e| {
            CoreFsError::State(format!("failed to open image file {}: {e}", path.display()))
        })?;

        let metadata = file.metadata().map_err(|e| {
            CoreFsError::State(format!("failed to stat image file {}: {e}", path.display()))
        })?;

        let capacity_bytes = metadata.len();
        let sector_size = SECTOR_SIZE_4K;

        Ok(Self {
            path: path.to_path_buf(),
            file,
            geometry: DeviceGeometry {
                sector_size,
                sector_count: capacity_bytes / u64::from(sector_size),
                capacity_bytes,
                read_only,
            },
            metadata_dirty: false,
            write_through: write_through && !read_only,
        })
    }

    /// Creates a new image file with the given capacity.
    ///
    /// The file is zero-filled (sparse if the OS supports it).
    pub fn create(
        path: impl AsRef<Path>,
        capacity_bytes: u64,
        sector_size: u32,
    ) -> CoreFsResult<Self> {
        let path = path.as_ref();
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

        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        let file = options.open(path).map_err(|e| {
            CoreFsError::State(format!(
                "failed to create image file {}: {e}",
                path.display()
            ))
        })?;

        file.set_len(capacity_bytes).map_err(|e| {
            CoreFsError::State(format!(
                "failed to set image file size to {capacity_bytes}: {e}",
            ))
        })?;

        Ok(Self {
            path: path.to_path_buf(),
            file,
            geometry: DeviceGeometry {
                sector_size,
                sector_count: capacity_bytes / u64::from(sector_size),
                capacity_bytes,
                read_only: false,
            },
            metadata_dirty: true,
            write_through: false,
        })
    }

    /// Returns the file path of the backing image.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resizes the image to the new capacity.
    ///
    /// Shrinking is only permitted down to the current usage high-water mark
    /// tracked by the caller; this method does not validate data content.
    pub fn resize(&mut self, new_capacity: u64) -> CoreFsResult<()> {
        check_write_permission(self.geometry.read_only)?;
        let ss = u64::from(self.geometry.sector_size);
        if new_capacity % ss != 0 {
            return Err(CoreFsError::InvalidInput(format!(
                "new capacity {new_capacity} must be a multiple of sector size {}",
                self.geometry.sector_size
            )));
        }
        self.file
            .set_len(new_capacity)
            .map_err(|e| CoreFsError::State(format!("failed to resize image file: {e}")))?;
        self.geometry.capacity_bytes = new_capacity;
        self.geometry.sector_count = new_capacity / ss;
        self.metadata_dirty = true;
        Ok(())
    }
}

impl BlockDevice for FileImageDevice {
    fn geometry(&self) -> &DeviceGeometry {
        &self.geometry
    }

    fn resize(&mut self, new_capacity: u64) -> CoreFsResult<()> {
        // Delegates to the inherent `FileImageDevice::resize` (see above)
        // so callers that hold a `&mut dyn BlockDevice` can grow a
        // file-backed device through the trait without downcasting.
        Self::resize(self, new_capacity)
    }

    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        check_alignment(offset, length, self.geometry.sector_size)?;
        check_bounds(offset, length, self.geometry.capacity_bytes)?;
        if length == 0 {
            return Ok(Vec::new());
        }

        use std::io::{Read, Seek, SeekFrom};
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| CoreFsError::State(format!("seek failed at offset {offset}: {e}")))?;
        let mut buf = vec![0u8; length as usize];
        file.read_exact(&mut buf).map_err(|e| {
            CoreFsError::State(format!(
                "read failed at offset {offset}, length {length}: {e}"
            ))
        })?;
        Ok(buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        check_write_permission(self.geometry.read_only)?;
        let length = data.len() as u64;
        check_alignment(offset, length, self.geometry.sector_size)?;
        check_bounds(offset, length, self.geometry.capacity_bytes)?;
        if data.is_empty() {
            return Ok(());
        }

        use std::io::{Seek, SeekFrom, Write};
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| CoreFsError::State(format!("seek failed at offset {offset}: {e}")))?;
        file.write_all(data).map_err(|e| {
            CoreFsError::State(format!(
                "write failed at offset {offset}, length {}: {e}",
                data.len()
            ))
        })?;
        Ok(())
    }

    fn sync(&mut self) -> CoreFsResult<()> {
        if self.write_through && !self.metadata_dirty {
            return Ok(());
        }
        let result = if self.metadata_dirty {
            self.file.sync_all()
        } else {
            self.file.sync_data()
        };
        result.map_err(|e| {
            CoreFsError::State(format!("sync failed on {}: {e}", self.path.display()))
        })?;
        self.metadata_dirty = false;
        Ok(())
    }

    fn trim(&mut self, _offset: u64, _length: u64) -> CoreFsResult<()> {
        // File-backed images do not support TRIM — no-op.
        Ok(())
    }

    fn supports_trim(&self) -> bool {
        false
    }
}

fn apply_write_through(options: &mut std::fs::OpenOptions, enabled: bool) {
    if !enabled {
        return;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        options.custom_flags(FILE_FLAG_WRITE_THROUGH);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_SYNC);
    }
}

// ===========================================================================
// RawBlockDevice — Linux raw block device (/dev/sdX)
// ===========================================================================

#[cfg(target_os = "linux")]
pub mod raw {
    use super::*;
    use std::os::unix::fs::FileTypeExt;

    // Linux ioctl request codes for block devices (not exported by libc crate)
    const BLKGETSIZE64: libc::c_ulong = 0x80081272;
    const BLKDISCARD: libc::c_ulong = 0x1277;

    /// Safety information gathered about a block device before formatting.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DeviceInfo {
        /// Canonical device path (e.g. `/dev/sdb1`).
        pub path: PathBuf,
        /// Logical sector size reported by the kernel.
        pub logical_sector_size: u32,
        /// Physical sector size reported by the kernel.
        pub physical_sector_size: u32,
        /// Total device capacity in bytes.
        pub capacity_bytes: u64,
        /// `true` if the kernel reports the device as read-only.
        pub read_only: bool,
        /// `true` if this looks like a whole-disk device (no partition number).
        pub is_whole_disk: bool,
        /// `true` if the device appears to be currently mounted.
        pub is_mounted: bool,
    }

    impl DeviceInfo {
        /// Returns `true` if it is safe to format this device
        /// (not mounted, not whole-disk, not read-only).
        pub fn is_safe_to_format(&self) -> bool {
            !self.is_mounted && !self.is_whole_disk && !self.read_only
        }

        /// Returns a human-readable list of problems that prevent formatting.
        pub fn format_blockers(&self) -> Vec<String> {
            let mut blockers = Vec::new();
            if self.is_mounted {
                blockers.push(format!("{} is currently mounted", self.path.display()));
            }
            if self.is_whole_disk {
                blockers.push(format!(
                    "{} appears to be a whole-disk device without a partition table entry — \
                     formatting would destroy the entire disk",
                    self.path.display()
                ));
            }
            if self.read_only {
                blockers.push(format!(
                    "{} is read-only (write-protected?)",
                    self.path.display()
                ));
            }
            blockers
        }
    }

    /// Checks whether the current process has the permissions needed
    /// to open a block device for read-write access.
    ///
    /// Returns `Ok(())` if access is likely possible, or a descriptive
    /// error explaining what is missing (not root, no capability, no
    /// write permission on the device node).
    pub fn check_device_permissions(path: impl AsRef<Path>) -> CoreFsResult<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(CoreFsError::NotFound(format!(
                "device not found: {}",
                path.display()
            )));
        }

        // Check if running as root (euid == 0).
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            return Ok(());
        }

        // Not root — check if the device node is writable by the current user.
        let result = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path);

        match result {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                Err(CoreFsError::PolicyViolation(format!(
                    "permission denied: {} — block device access requires root \
                     or write permission on the device node.\n\
                     Try: sudo corefs <command> {}",
                    path.display(),
                    path.display()
                )))
            }
            Err(e) => Err(CoreFsError::State(format!(
                "cannot open {}: {e}",
                path.display()
            ))),
        }
    }

    /// Probes a block device and returns safety-relevant metadata.
    ///
    /// This does **not** open the device for writing — it only reads
    /// sysfs attributes and `/proc/mounts`.
    pub fn probe_device(path: impl AsRef<Path>) -> CoreFsResult<DeviceInfo> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(CoreFsError::NotFound(format!(
                "device not found: {}",
                path.display()
            )));
        }

        let metadata = std::fs::metadata(path).map_err(|e| {
            CoreFsError::State(format!("failed to stat device {}: {e}", path.display()))
        })?;

        if !metadata.file_type().is_block_device() {
            return Err(CoreFsError::InvalidInput(format!(
                "{} is not a block device",
                path.display()
            )));
        }

        let capacity_bytes = query_device_size(path)?;
        let logical_sector_size = query_sector_size(path, "logical_block_size");
        let physical_sector_size = query_sector_size(path, "physical_block_size");
        let read_only = query_read_only(path);
        let is_whole_disk = detect_whole_disk(path);
        let is_mounted = detect_mounted(path);

        Ok(DeviceInfo {
            path: path.to_path_buf(),
            logical_sector_size,
            physical_sector_size,
            capacity_bytes,
            read_only,
            is_whole_disk,
            is_mounted,
        })
    }

    // -----------------------------------------------------------------------
    // Sysfs / procfs helpers
    // -----------------------------------------------------------------------

    fn device_name(path: &Path) -> Option<String> {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    fn query_device_size(path: &Path) -> CoreFsResult<u64> {
        // ioctl BLKGETSIZE64
        use std::os::unix::io::AsRawFd;
        let file = std::fs::File::open(path).map_err(|e| {
            CoreFsError::State(format!(
                "failed to open device {} for size query: {e}",
                path.display()
            ))
        })?;
        let fd = file.as_raw_fd();
        let mut size: u64 = 0;
        // SAFETY: BLKGETSIZE64 writes a u64; fd is a valid block device fd.
        let ret = unsafe { libc::ioctl(fd, BLKGETSIZE64, &mut size as *mut u64) };
        if ret < 0 {
            return Err(CoreFsError::State(format!(
                "BLKGETSIZE64 ioctl failed on {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(size)
    }

    fn query_sector_size(path: &Path, attr: &str) -> u32 {
        let name = match device_name(path) {
            Some(n) => n,
            None => return SECTOR_SIZE_512,
        };
        // Strip partition number for sysfs lookup (e.g. sdb1 -> sdb)
        let disk_name = name.trim_end_matches(|c: char| c.is_ascii_digit());
        let sysfs = format!("/sys/block/{disk_name}/queue/{attr}");
        std::fs::read_to_string(&sysfs)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(SECTOR_SIZE_512)
    }

    fn query_read_only(path: &Path) -> bool {
        let name = match device_name(path) {
            Some(n) => n,
            None => return false,
        };
        let disk_name = name.trim_end_matches(|c: char| c.is_ascii_digit());
        let sysfs = format!("/sys/block/{disk_name}/ro");
        std::fs::read_to_string(&sysfs)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    fn detect_whole_disk(path: &Path) -> bool {
        let name = match device_name(path) {
            Some(n) => n,
            None => return false,
        };
        // A whole-disk device has no trailing digit after the base name,
        // or is an NVMe namespace without a 'p' partition suffix.
        if name.starts_with("nvme") {
            // nvme0n1 = whole disk, nvme0n1p1 = partition
            !name.contains('p')
                || name.ends_with(&name[name.rfind('n').map(|i| i + 1).unwrap_or(0)..])
        } else {
            // sd*, vd*, hd*, xvd*: whole disk has no trailing digits
            !name.ends_with(|c: char| c.is_ascii_digit())
        }
    }

    fn detect_mounted(path: &Path) -> bool {
        let canonical = std::fs::canonicalize(path)
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()));
        let device_str = canonical
            .as_deref()
            .unwrap_or_else(|| path.to_str().unwrap_or(""));
        if device_str.is_empty() {
            return false;
        }
        std::fs::read_to_string("/proc/mounts")
            .ok()
            .map(|mounts| {
                mounts.lines().any(|line| {
                    line.split_whitespace()
                        .next()
                        .is_some_and(|dev| dev == device_str)
                })
            })
            .unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // RawBlockDevice
    // -----------------------------------------------------------------------

    /// A [`crate::storage::block_device::BlockDevice`] backed by a raw Linux block device (`/dev/sdX1`).
    ///
    /// Uses `O_RDWR | O_SYNC` for writes to ensure persistence ordering.
    /// Sector-alignment is enforced at the trait boundary.
    #[derive(Debug)]
    pub struct RawBlockDevice {
        path: PathBuf,
        file: std::fs::File,
        geometry: DeviceGeometry,
        supports_trim: bool,
    }

    impl RawBlockDevice {
        /// Opens a raw block device.
        ///
        /// # Safety considerations
        ///
        /// The caller is responsible for ensuring the device is not currently
        /// in use by another filesystem.  Use [`probe_device`] first to
        /// gather safety information.
        pub fn open(path: impl AsRef<Path>, read_only: bool) -> CoreFsResult<Self> {
            let path = path.as_ref();
            let info = probe_device(path)?;

            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(!read_only)
                .open(path)
                .map_err(|e| {
                    CoreFsError::State(format!(
                        "failed to open block device {}: {e}",
                        path.display()
                    ))
                })?;

            let sector_size = info.logical_sector_size;
            let capacity_bytes = info.capacity_bytes;
            let supports_trim = check_trim_support(path);

            Ok(Self {
                path: path.to_path_buf(),
                file,
                geometry: DeviceGeometry {
                    sector_size,
                    sector_count: capacity_bytes / u64::from(sector_size),
                    capacity_bytes,
                    read_only,
                },
                supports_trim,
            })
        }

        /// Returns the canonical device path.
        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Returns device probe information.
        pub fn probe(&self) -> CoreFsResult<DeviceInfo> {
            probe_device(&self.path)
        }
    }

    impl BlockDevice for RawBlockDevice {
        fn geometry(&self) -> &DeviceGeometry {
            &self.geometry
        }

        fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
            check_alignment(offset, length, self.geometry.sector_size)?;
            check_bounds(offset, length, self.geometry.capacity_bytes)?;
            if length == 0 {
                return Ok(Vec::new());
            }

            use std::io::{Read, Seek, SeekFrom};
            let mut file = &self.file;
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| CoreFsError::State(format!("seek failed at offset {offset}: {e}")))?;
            let mut buf = vec![0u8; length as usize];
            file.read_exact(&mut buf).map_err(|e| {
                CoreFsError::State(format!(
                    "read failed on {} at offset {offset}, length {length}: {e}",
                    self.path.display()
                ))
            })?;
            Ok(buf)
        }

        fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
            check_write_permission(self.geometry.read_only)?;
            let length = data.len() as u64;
            check_alignment(offset, length, self.geometry.sector_size)?;
            check_bounds(offset, length, self.geometry.capacity_bytes)?;
            if data.is_empty() {
                return Ok(());
            }

            use std::io::{Seek, SeekFrom, Write};
            let mut file = &self.file;
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| CoreFsError::State(format!("seek failed at offset {offset}: {e}")))?;
            file.write_all(data).map_err(|e| {
                CoreFsError::State(format!(
                    "write failed on {} at offset {offset}, length {}: {e}",
                    self.path.display(),
                    data.len()
                ))
            })?;
            Ok(())
        }

        fn sync(&mut self) -> CoreFsResult<()> {
            // fdatasync is sufficient for block devices (no metadata to flush).
            self.file.sync_data().map_err(|e| {
                CoreFsError::State(format!("fdatasync failed on {}: {e}", self.path.display()))
            })
        }

        fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
            if !self.supports_trim {
                return Ok(());
            }
            check_alignment(offset, length, self.geometry.sector_size)?;
            check_bounds(offset, length, self.geometry.capacity_bytes)?;
            if length == 0 {
                return Ok(());
            }

            use std::os::unix::io::AsRawFd;
            let fd = self.file.as_raw_fd();
            let range: [u64; 2] = [offset, length];
            // SAFETY: BLKDISCARD takes a pointer to two u64 values [offset, length].
            let ret = unsafe { libc::ioctl(fd, BLKDISCARD, range.as_ptr()) };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                // EOPNOTSUPP / ENOTTY → device doesn't actually support discard
                if err.raw_os_error() == Some(libc::EOPNOTSUPP)
                    || err.raw_os_error() == Some(libc::ENOTTY)
                {
                    self.supports_trim = false;
                    return Ok(());
                }
                return Err(CoreFsError::State(format!(
                    "BLKDISCARD ioctl failed on {}: {err}",
                    self.path.display()
                )));
            }
            Ok(())
        }

        fn supports_trim(&self) -> bool {
            self.supports_trim
        }
    }

    fn check_trim_support(path: &Path) -> bool {
        let name = match device_name(path) {
            Some(n) => n,
            None => return false,
        };
        let disk_name = name.trim_end_matches(|c: char| c.is_ascii_digit());
        let sysfs = format!("/sys/block/{disk_name}/queue/discard_max_bytes");
        std::fs::read_to_string(&sysfs)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .is_some_and(|v| v > 0)
    }
}


// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "block_device_tests.rs"]
mod tests;
