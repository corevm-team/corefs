use crate::error::{CoreFsError, CoreFsResult};
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Sector size constants
// ---------------------------------------------------------------------------

/// Traditional hard-disk sector size (512 bytes).
pub const SECTOR_SIZE_512: u32 = 512;

/// Advanced-format / NVMe / flash sector size (4096 bytes).
pub const SECTOR_SIZE_4K: u32 = 4096;

// ---------------------------------------------------------------------------
// DeviceGeometry — physical parameters of the underlying device
// ---------------------------------------------------------------------------

/// Describes the physical geometry of a block device or image file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGeometry {
    /// Logical sector size in bytes (minimum addressable unit).
    pub sector_size: u32,
    /// Total number of sectors on the device.
    pub sector_count: u64,
    /// Total capacity in bytes (`sector_size * sector_count`).
    pub capacity_bytes: u64,
    /// `true` if the device is read-only (e.g. write-protected USB stick).
    pub read_only: bool,
}

// ---------------------------------------------------------------------------
// BlockDevice trait — the core abstraction
// ---------------------------------------------------------------------------

/// Abstraction over a raw, sector-addressable storage medium.
///
/// Implementations exist for file-backed images ([`FileImageDevice`]) and,
/// on Linux, for raw block devices ([`RawBlockDevice`]).
///
/// All offsets and lengths are in **bytes** and must be aligned to the
/// device's sector size.  Implementations return
/// [`CoreFsError::InvalidInput`] for misaligned operations and
/// [`CoreFsError::State`] for I/O failures.
pub trait BlockDevice: fmt::Debug + Send {
    /// Returns the physical geometry of the device.
    fn geometry(&self) -> &DeviceGeometry;

    /// Reads `length` bytes starting at byte offset `offset`.
    ///
    /// Both `offset` and `length` must be multiples of the sector size.
    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>>;

    /// Writes `data` starting at byte offset `offset`.
    ///
    /// `offset` must be a multiple of the sector size.
    /// `data.len()` must be a multiple of the sector size.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()>;

    /// Flushes any buffered writes to persistent storage.
    fn sync(&mut self) -> CoreFsResult<()>;

    /// Informs the device that the byte range `[offset, offset + length)` is
    /// no longer in use and may be discarded (TRIM / UNMAP).
    ///
    /// `offset` and `length` must be multiples of the sector size.
    /// Devices that do not support TRIM return `Ok(())` silently.
    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()>;

    /// Returns `true` if the device supports TRIM / discard.
    fn supports_trim(&self) -> bool;

    /// Returns the total capacity in bytes.
    fn capacity(&self) -> u64 {
        self.geometry().capacity_bytes
    }

    /// Returns the sector size in bytes.
    fn sector_size(&self) -> u32 {
        self.geometry().sector_size
    }

    /// Returns `true` if the device is read-only.
    fn is_read_only(&self) -> bool {
        self.geometry().read_only
    }
}

// ---------------------------------------------------------------------------
// Alignment helpers
// ---------------------------------------------------------------------------

fn check_alignment(offset: u64, length: u64, sector_size: u32) -> CoreFsResult<()> {
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

fn check_bounds(offset: u64, length: u64, capacity: u64) -> CoreFsResult<()> {
    let end = offset.checked_add(length).ok_or_else(|| {
        CoreFsError::InvalidInput(format!(
            "offset {offset} + length {length} overflows u64"
        ))
    })?;
    if end > capacity {
        return Err(CoreFsError::InvalidInput(format!(
            "access past end of device: offset {offset} + length {length} = {end} > capacity {capacity}"
        )));
    }
    Ok(())
}

fn check_write_permission(read_only: bool) -> CoreFsResult<()> {
    if read_only {
        return Err(CoreFsError::PolicyViolation(
            "device is read-only".to_string(),
        ));
    }
    Ok(())
}

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

/// A [`BlockDevice`] backed by a regular file (`.img`).
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
}

impl FileImageDevice {
    /// Opens an existing image file.
    pub fn open(path: impl AsRef<Path>, read_only: bool) -> CoreFsResult<Self> {
        let path = path.as_ref();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(path)
            .map_err(|e| {
                CoreFsError::State(format!(
                    "failed to open image file {}: {e}",
                    path.display()
                ))
            })?;

        let metadata = file.metadata().map_err(|e| {
            CoreFsError::State(format!(
                "failed to stat image file {}: {e}",
                path.display()
            ))
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

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| {
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
        self.file.set_len(new_capacity).map_err(|e| {
            CoreFsError::State(format!("failed to resize image file: {e}"))
        })?;
        self.geometry.capacity_bytes = new_capacity;
        self.geometry.sector_count = new_capacity / ss;
        Ok(())
    }
}

impl BlockDevice for FileImageDevice {
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
        file.seek(SeekFrom::Start(offset)).map_err(|e| {
            CoreFsError::State(format!("seek failed at offset {offset}: {e}"))
        })?;
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
        file.seek(SeekFrom::Start(offset)).map_err(|e| {
            CoreFsError::State(format!("seek failed at offset {offset}: {e}"))
        })?;
        file.write_all(data).map_err(|e| {
            CoreFsError::State(format!(
                "write failed at offset {offset}, length {}: {e}",
                data.len()
            ))
        })?;
        Ok(())
    }

    fn sync(&mut self) -> CoreFsResult<()> {
        self.file.sync_all().map_err(|e| {
            CoreFsError::State(format!("sync failed on {}: {e}", self.path.display()))
        })
    }

    fn trim(&mut self, _offset: u64, _length: u64) -> CoreFsResult<()> {
        // File-backed images do not support TRIM — no-op.
        Ok(())
    }

    fn supports_trim(&self) -> bool {
        false
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
            CoreFsError::State(format!(
                "failed to stat device {}: {e}",
                path.display()
            ))
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
        let ret = unsafe {
            libc::ioctl(fd, BLKGETSIZE64, &mut size as *mut u64)
        };
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
            !name.contains('p') || name.ends_with(
                &name[name.rfind('n').map(|i| i + 1).unwrap_or(0)..]
            )
        } else {
            // sd*, vd*, hd*, xvd*: whole disk has no trailing digits
            !name.ends_with(|c: char| c.is_ascii_digit())
        }
    }

    fn detect_mounted(path: &Path) -> bool {
        let canonical = std::fs::canonicalize(path)
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()));
        let device_str = canonical.as_deref().unwrap_or_else(|| {
            path.to_str().unwrap_or("")
        });
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

    /// A [`BlockDevice`] backed by a raw Linux block device (`/dev/sdX1`).
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
            file.seek(SeekFrom::Start(offset)).map_err(|e| {
                CoreFsError::State(format!("seek failed at offset {offset}: {e}"))
            })?;
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
            file.seek(SeekFrom::Start(offset)).map_err(|e| {
                CoreFsError::State(format!("seek failed at offset {offset}: {e}"))
            })?;
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
                CoreFsError::State(format!(
                    "fdatasync failed on {}: {e}",
                    self.path.display()
                ))
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
            let ret = unsafe {
                libc::ioctl(fd, BLKDISCARD, range.as_ptr())
            };
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
// MemoryDevice — in-memory block device for testing
// ===========================================================================

/// A [`BlockDevice`] backed entirely by an in-memory buffer.
///
/// Useful for unit tests and as a reference implementation.
#[derive(Debug, Clone)]
pub struct MemoryDevice {
    data: Vec<u8>,
    geometry: DeviceGeometry,
    trim_supported: bool,
    trimmed_ranges: Vec<(u64, u64)>,
}

impl MemoryDevice {
    /// Creates a new zero-filled in-memory device.
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

    /// Creates a read-only in-memory device from existing data.
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

    /// Enables or disables TRIM support (for testing trim code paths).
    pub fn set_trim_supported(&mut self, supported: bool) {
        self.trim_supported = supported;
    }

    /// Sets the device as read-only (for testing write-protection code paths).
    pub fn set_read_only(&mut self, read_only: bool) {
        self.geometry.read_only = read_only;
    }

    /// Returns a snapshot of the underlying data buffer.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Returns ranges that were trimmed since creation or last clear.
    pub fn trimmed_ranges(&self) -> &[(u64, u64)] {
        &self.trimmed_ranges
    }

    /// Clears the list of recorded TRIM ranges.
    pub fn clear_trimmed_ranges(&mut self) {
        self.trimmed_ranges.clear();
    }
}

impl BlockDevice for MemoryDevice {
    fn geometry(&self) -> &DeviceGeometry {
        &self.geometry
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
        // No-op for memory device.
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
        // Zero-fill the trimmed range (simulates device discard behaviour).
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SECTOR: u32 = 4096;
    const FOUR_SECTORS: u64 = 4 * 4096;

    fn unique_path(label: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "corefs-blkdev-{label}-{}-{ts}.img",
            std::process::id()
        ))
    }

    // -----------------------------------------------------------------------
    // Alignment & bounds helpers
    // -----------------------------------------------------------------------

    #[test]
    fn check_alignment_accepts_aligned_values() {
        assert!(check_alignment(0, 0, 512).is_ok());
        assert!(check_alignment(512, 1024, 512).is_ok());
        assert!(check_alignment(4096, 4096, 4096).is_ok());
    }

    #[test]
    fn check_alignment_rejects_misaligned_offset() {
        let err = check_alignment(1, 512, 512).unwrap_err();
        assert!(err.to_string().contains("offset 1"));
    }

    #[test]
    fn check_alignment_rejects_misaligned_length() {
        let err = check_alignment(512, 100, 512).unwrap_err();
        assert!(err.to_string().contains("length 100"));
    }

    #[test]
    fn check_bounds_accepts_exact_capacity() {
        assert!(check_bounds(0, 4096, 4096).is_ok());
    }

    #[test]
    fn check_bounds_rejects_past_end() {
        let err = check_bounds(4096, 1, 4096).unwrap_err();
        assert!(err.to_string().contains("past end"));
    }

    #[test]
    fn check_bounds_rejects_overflow() {
        let err = check_bounds(u64::MAX, 1, u64::MAX).unwrap_err();
        assert!(err.to_string().contains("overflows"));
    }

    // -----------------------------------------------------------------------
    // DeviceGeometry
    // -----------------------------------------------------------------------

    #[test]
    fn device_geometry_stores_correct_values() {
        let g = DeviceGeometry {
            sector_size: 512,
            sector_count: 2048,
            capacity_bytes: 512 * 2048,
            read_only: false,
        };
        assert_eq!(g.capacity_bytes, 1_048_576);
        assert_eq!(g.sector_count, 2048);
        assert!(!g.read_only);
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — core operations
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_creation_and_geometry() {
        let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        assert_eq!(dev.capacity(), FOUR_SECTORS);
        assert_eq!(dev.sector_size(), SECTOR);
        assert_eq!(dev.geometry().sector_count, 4);
        assert!(!dev.is_read_only());
    }

    #[test]
    fn memory_device_rejects_zero_capacity() {
        let err = MemoryDevice::new(0, SECTOR).unwrap_err();
        assert!(err.to_string().contains("greater than zero"));
    }

    #[test]
    fn memory_device_rejects_non_power_of_two_sector() {
        let err = MemoryDevice::new(4096, 3000).unwrap_err();
        assert!(err.to_string().contains("power of two"));
    }

    #[test]
    fn memory_device_rejects_unaligned_capacity() {
        let err = MemoryDevice::new(5000, SECTOR).unwrap_err();
        assert!(err.to_string().contains("multiple of sector size"));
    }

    #[test]
    fn memory_device_write_then_read() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let mut payload = vec![0u8; SECTOR as usize];
        payload[0..4].copy_from_slice(b"TEST");

        dev.write_at(SECTOR as u64, &payload).unwrap();

        let readback = dev.read_at(SECTOR as u64, SECTOR as u64).unwrap();
        assert_eq!(&readback[0..4], b"TEST");
    }

    #[test]
    fn memory_device_write_multiple_sectors() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let payload = vec![0xABu8; 2 * SECTOR as usize];

        dev.write_at(0, &payload).unwrap();

        let readback = dev.read_at(0, 2 * SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0xAB));

        // Sectors 2–3 should still be zero
        let untouched = dev.read_at(2 * SECTOR as u64, 2 * SECTOR as u64).unwrap();
        assert!(untouched.iter().all(|&b| b == 0));
    }

    #[test]
    fn memory_device_read_zero_length() {
        let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let result = dev.read_at(0, 0).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn memory_device_write_zero_length() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        dev.write_at(0, &[]).unwrap();
    }

    #[test]
    fn memory_device_rejects_misaligned_read() {
        let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let err = dev.read_at(100, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn memory_device_rejects_misaligned_write() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let payload = vec![0u8; 100];
        let err = dev.write_at(0, &payload).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn memory_device_rejects_out_of_bounds_read() {
        let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let err = dev.read_at(FOUR_SECTORS, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn memory_device_rejects_out_of_bounds_write() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let payload = vec![0u8; SECTOR as usize];
        let err = dev.write_at(FOUR_SECTORS, &payload).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — read-only enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_read_only_blocks_writes() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        dev.set_read_only(true);

        let payload = vec![0u8; SECTOR as usize];
        let err = dev.write_at(0, &payload).unwrap_err();
        assert!(matches!(err, CoreFsError::PolicyViolation(_)));
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn memory_device_read_only_allows_reads() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        dev.set_read_only(true);

        let result = dev.read_at(0, SECTOR as u64);
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — TRIM
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_trim_zeros_range() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let payload = vec![0xFFu8; SECTOR as usize];
        dev.write_at(0, &payload).unwrap();

        dev.trim(0, SECTOR as u64).unwrap();

        let readback = dev.read_at(0, SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0));
    }

    #[test]
    fn memory_device_trim_records_ranges() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        dev.trim(0, SECTOR as u64).unwrap();
        dev.trim(2 * SECTOR as u64, SECTOR as u64).unwrap();

        assert_eq!(dev.trimmed_ranges().len(), 2);
        assert_eq!(dev.trimmed_ranges()[0], (0, SECTOR as u64));
        assert_eq!(dev.trimmed_ranges()[1], (2 * SECTOR as u64, SECTOR as u64));
    }

    #[test]
    fn memory_device_trim_disabled_is_noop() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let payload = vec![0xFFu8; SECTOR as usize];
        dev.write_at(0, &payload).unwrap();
        dev.set_trim_supported(false);

        assert!(!dev.supports_trim());
        dev.trim(0, SECTOR as u64).unwrap();

        // Data should NOT be zeroed
        let readback = dev.read_at(0, SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0xFF));
        assert!(dev.trimmed_ranges().is_empty());
    }

    #[test]
    fn memory_device_trim_rejects_misaligned() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let err = dev.trim(100, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn memory_device_trim_rejects_out_of_bounds() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let err = dev.trim(FOUR_SECTORS, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — from_bytes
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_from_bytes_preserves_content() {
        let mut original = vec![0u8; FOUR_SECTORS as usize];
        original[0..5].copy_from_slice(b"HELLO");

        let dev = MemoryDevice::from_bytes(original, SECTOR).unwrap();
        let readback = dev.read_at(0, SECTOR as u64).unwrap();
        assert_eq!(&readback[0..5], b"HELLO");
    }

    #[test]
    fn memory_device_from_bytes_rejects_unaligned() {
        let data = vec![0u8; 100];
        let err = MemoryDevice::from_bytes(data, SECTOR).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — sync
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_sync_succeeds() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        assert!(dev.sync().is_ok());
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — data() accessor
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_data_reflects_writes() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let mut payload = vec![0u8; SECTOR as usize];
        payload[0] = 42;
        dev.write_at(SECTOR as u64, &payload).unwrap();

        assert_eq!(dev.data()[SECTOR as usize], 42);
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — different sector sizes
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_512_byte_sectors() {
        let mut dev = MemoryDevice::new(8 * 512, 512).unwrap();
        assert_eq!(dev.sector_size(), 512);
        assert_eq!(dev.geometry().sector_count, 8);

        let payload = vec![0xCDu8; 512];
        dev.write_at(512, &payload).unwrap();
        let readback = dev.read_at(512, 512).unwrap();
        assert!(readback.iter().all(|&b| b == 0xCD));
    }

    // -----------------------------------------------------------------------
    // MemoryDevice — clear_trimmed_ranges
    // -----------------------------------------------------------------------

    #[test]
    fn memory_device_clear_trimmed_ranges() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        dev.trim(0, SECTOR as u64).unwrap();
        assert_eq!(dev.trimmed_ranges().len(), 1);

        dev.clear_trimmed_ranges();
        assert!(dev.trimmed_ranges().is_empty());
    }

    // -----------------------------------------------------------------------
    // FileImageDevice — core operations
    // -----------------------------------------------------------------------

    #[test]
    fn file_image_create_and_geometry() {
        let path = unique_path("create-geom");
        let dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        assert_eq!(dev.capacity(), FOUR_SECTORS);
        assert_eq!(dev.sector_size(), SECTOR);
        assert_eq!(dev.geometry().sector_count, 4);
        assert!(!dev.is_read_only());

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_create_rejects_zero_capacity() {
        let path = unique_path("create-zero");
        let err = FileImageDevice::create(&path, 0, SECTOR).unwrap_err();
        assert!(err.to_string().contains("greater than zero"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_create_rejects_bad_sector_size() {
        let path = unique_path("create-bad-ss");
        let err = FileImageDevice::create(&path, 4096, 3000).unwrap_err();
        assert!(err.to_string().contains("power of two"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_create_rejects_unaligned_capacity() {
        let path = unique_path("create-unaligned");
        let err = FileImageDevice::create(&path, 5000, SECTOR).unwrap_err();
        assert!(err.to_string().contains("multiple of sector size"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_write_then_read() {
        let path = unique_path("write-read");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        let mut payload = vec![0u8; SECTOR as usize];
        payload[0..6].copy_from_slice(b"COREFS");
        dev.write_at(0, &payload).unwrap();

        let readback = dev.read_at(0, SECTOR as u64).unwrap();
        assert_eq!(&readback[0..6], b"COREFS");

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_write_multiple_sectors_and_read_back() {
        let path = unique_path("multi-sector");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        let payload = vec![0xABu8; 2 * SECTOR as usize];
        dev.write_at(SECTOR as u64, &payload).unwrap();

        let readback = dev.read_at(SECTOR as u64, 2 * SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0xAB));

        // First sector should still be zero
        let first = dev.read_at(0, SECTOR as u64).unwrap();
        assert!(first.iter().all(|&b| b == 0));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_persistence_across_reopen() {
        let path = unique_path("persist");
        {
            let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
            let mut payload = vec![0u8; SECTOR as usize];
            payload[0..7].copy_from_slice(b"persist");
            dev.write_at(0, &payload).unwrap();
            dev.sync().unwrap();
        }

        let dev2 = FileImageDevice::open(&path, true).unwrap();
        let readback = dev2.read_at(0, SECTOR as u64).unwrap();
        assert_eq!(&readback[0..7], b"persist");
        assert!(dev2.is_read_only());

        drop(dev2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_read_only_blocks_writes() {
        let path = unique_path("ro-write");
        {
            let _dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        }

        let mut dev = FileImageDevice::open(&path, true).unwrap();
        let payload = vec![0u8; SECTOR as usize];
        let err = dev.write_at(0, &payload).unwrap_err();
        assert!(matches!(err, CoreFsError::PolicyViolation(_)));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_rejects_misaligned_operations() {
        let path = unique_path("misaligned");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        let err = dev.read_at(100, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));

        let payload = vec![0u8; 100];
        let err = dev.write_at(0, &payload).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_rejects_out_of_bounds() {
        let path = unique_path("oob");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        let err = dev.read_at(FOUR_SECTORS, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));

        let payload = vec![0u8; SECTOR as usize];
        let err = dev.write_at(FOUR_SECTORS, &payload).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_trim_is_noop() {
        let path = unique_path("trim-noop");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        assert!(!dev.supports_trim());
        assert!(dev.trim(0, SECTOR as u64).is_ok());

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_sync_succeeds() {
        let path = unique_path("sync");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        assert!(dev.sync().is_ok());

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_read_zero_length() {
        let path = unique_path("read-zero");
        let dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        let result = dev.read_at(0, 0).unwrap();
        assert!(result.is_empty());

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_write_zero_length() {
        let path = unique_path("write-zero");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        dev.write_at(0, &[]).unwrap();

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // FileImageDevice — resize
    // -----------------------------------------------------------------------

    #[test]
    fn file_image_resize_grow() {
        let path = unique_path("resize-grow");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        let new_cap = 8 * SECTOR as u64;
        dev.resize(new_cap).unwrap();
        assert_eq!(dev.capacity(), new_cap);
        assert_eq!(dev.geometry().sector_count, 8);

        // Can now write to the extended region
        let payload = vec![0xBBu8; SECTOR as usize];
        dev.write_at(FOUR_SECTORS, &payload).unwrap();
        let readback = dev.read_at(FOUR_SECTORS, SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0xBB));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_resize_shrink() {
        let path = unique_path("resize-shrink");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

        let new_cap = 2 * SECTOR as u64;
        dev.resize(new_cap).unwrap();
        assert_eq!(dev.capacity(), new_cap);
        assert_eq!(dev.geometry().sector_count, 2);

        // Out of bounds now
        let err = dev.read_at(new_cap, SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_resize_rejects_unaligned() {
        let path = unique_path("resize-unaligned");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        let err = dev.resize(5000).unwrap_err();
        assert!(matches!(err, CoreFsError::InvalidInput(_)));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_image_resize_rejects_read_only() {
        let path = unique_path("resize-ro");
        {
            let _dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        }
        let mut dev = FileImageDevice::open(&path, true).unwrap();
        let err = dev.resize(8 * SECTOR as u64).unwrap_err();
        assert!(matches!(err, CoreFsError::PolicyViolation(_)));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // FileImageDevice — open non-existent
    // -----------------------------------------------------------------------

    #[test]
    fn file_image_open_nonexistent_fails() {
        let err = FileImageDevice::open("/tmp/nonexistent-corefs-image.img", false).unwrap_err();
        assert!(matches!(err, CoreFsError::State(_)));
    }

    // -----------------------------------------------------------------------
    // FileImageDevice — create refuses overwrite
    // -----------------------------------------------------------------------

    #[test]
    fn file_image_create_refuses_existing_file() {
        let path = unique_path("create-exists");
        {
            let _dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        }
        let err = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap_err();
        assert!(matches!(err, CoreFsError::State(_)));

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // FileImageDevice — 512-byte sector size
    // -----------------------------------------------------------------------

    #[test]
    fn file_image_512_byte_sectors() {
        let path = unique_path("512-sectors");
        let mut dev = FileImageDevice::create(&path, 8 * 512, 512).unwrap();
        assert_eq!(dev.sector_size(), 512);
        assert_eq!(dev.geometry().sector_count, 8);

        let payload = vec![0xEFu8; 512];
        dev.write_at(512, &payload).unwrap();
        let readback = dev.read_at(512, 512).unwrap();
        assert!(readback.iter().all(|&b| b == 0xEF));

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // Trait object usage (dyn BlockDevice)
    // -----------------------------------------------------------------------

    #[test]
    fn trait_object_dispatch_works() {
        let mut dev: Box<dyn BlockDevice> =
            Box::new(MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap());

        let mut payload = vec![0u8; SECTOR as usize];
        payload[0] = 0xFF;
        dev.write_at(0, &payload).unwrap();

        let readback = dev.read_at(0, SECTOR as u64).unwrap();
        assert_eq!(readback[0], 0xFF);

        assert_eq!(dev.capacity(), FOUR_SECTORS);
        assert_eq!(dev.sector_size(), SECTOR);
        assert!(!dev.is_read_only());
    }

    // -----------------------------------------------------------------------
    // Full-device read/write round trip
    // -----------------------------------------------------------------------

    #[test]
    fn full_device_round_trip_memory() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let data = (0..FOUR_SECTORS as usize)
            .map(|i| (i % 256) as u8)
            .collect::<Vec<_>>();

        dev.write_at(0, &data).unwrap();
        dev.sync().unwrap();

        let readback = dev.read_at(0, FOUR_SECTORS).unwrap();
        assert_eq!(readback, data);
    }

    #[test]
    fn full_device_round_trip_file() {
        let path = unique_path("full-rt");
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        let data = (0..FOUR_SECTORS as usize)
            .map(|i| (i % 256) as u8)
            .collect::<Vec<_>>();

        dev.write_at(0, &data).unwrap();
        dev.sync().unwrap();

        let readback = dev.read_at(0, FOUR_SECTORS).unwrap();
        assert_eq!(readback, data);

        drop(dev);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // Sector-by-sector access pattern
    // -----------------------------------------------------------------------

    #[test]
    fn sector_by_sector_write_and_verify() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let ss = SECTOR as u64;

        for i in 0..4u64 {
            let payload = vec![(i + 1) as u8; SECTOR as usize];
            dev.write_at(i * ss, &payload).unwrap();
        }

        for i in 0..4u64 {
            let readback = dev.read_at(i * ss, ss).unwrap();
            assert!(readback.iter().all(|&b| b == (i + 1) as u8));
        }
    }

    // -----------------------------------------------------------------------
    // Overwrite semantics
    // -----------------------------------------------------------------------

    #[test]
    fn overwrite_replaces_previous_data() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();

        let first = vec![0xAAu8; SECTOR as usize];
        dev.write_at(0, &first).unwrap();

        let second = vec![0xBBu8; SECTOR as usize];
        dev.write_at(0, &second).unwrap();

        let readback = dev.read_at(0, SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0xBB));
    }

    // -----------------------------------------------------------------------
    // Edge: exact capacity boundary
    // -----------------------------------------------------------------------

    #[test]
    fn write_at_exact_end_boundary() {
        let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let payload = vec![0xCCu8; SECTOR as usize];
        // Write the very last sector
        dev.write_at(3 * SECTOR as u64, &payload).unwrap();
        let readback = dev.read_at(3 * SECTOR as u64, SECTOR as u64).unwrap();
        assert!(readback.iter().all(|&b| b == 0xCC));
    }

    #[test]
    fn read_entire_device() {
        let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
        let result = dev.read_at(0, FOUR_SECTORS).unwrap();
        assert_eq!(result.len(), FOUR_SECTORS as usize);
    }

    // -----------------------------------------------------------------------
    // Fake-stick detection helpers
    // -----------------------------------------------------------------------

    /// A MemoryDevice that rejects writes past a certain offset — simulates a
    /// fake USB stick.
    #[derive(Debug)]
    struct FakeStickDevice {
        inner: MemoryDevice,
        writable_until: u64,
    }

    impl BlockDevice for FakeStickDevice {
        fn geometry(&self) -> &DeviceGeometry {
            self.inner.geometry()
        }
        fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
            self.inner.read_at(offset, length)
        }
        fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
            if offset + data.len() as u64 > self.writable_until {
                return Err(CoreFsError::State("write protected".to_string()));
            }
            self.inner.write_at(offset, data)
        }
        fn sync(&mut self) -> CoreFsResult<()> {
            self.inner.sync()
        }
        fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
            self.inner.trim(offset, length)
        }
        fn supports_trim(&self) -> bool {
            false
        }
    }

    const ONE_MIB: u64 = 1024 * 1024;

    #[test]
    fn sanity_check_on_honest_device_passes() {
        let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
        let report = sanity_check_writable(&mut dev, 0).unwrap();
        assert!(report.is_honest());
        assert!(report.failed_offsets.is_empty());
        assert_eq!(report.estimated_usable_bytes, report.advertised_bytes);
        assert_eq!(report.fake_ratio_percent(), 0);
    }

    #[test]
    fn sanity_check_detects_fake_stick() {
        // Advertises 16 MiB, actually only writable up to 2 MiB.
        let fake = FakeStickDevice {
            inner: MemoryDevice::new(16 * ONE_MIB, 4096).unwrap(),
            writable_until: 2 * ONE_MIB,
        };
        let mut dev = fake;
        let report = sanity_check_writable(&mut dev, 0).unwrap();
        assert!(!report.is_honest());
        assert!(!report.failed_offsets.is_empty());
        assert!(report.estimated_usable_bytes < report.advertised_bytes);
        assert!(report.fake_ratio_percent() > 50);
    }

    #[test]
    fn sanity_check_zero_fills_probe_regions() {
        let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
        // Write known markers everywhere so we can detect that probe regions
        // were zeroed afterwards.
        let marker = vec![0xAB; (16 * ONE_MIB) as usize];
        dev.write_at(0, &marker).unwrap();

        let report = sanity_check_writable(&mut dev, 0).unwrap();
        assert!(report.is_honest());

        // Every probe region must be zero-filled after the check.
        for &offset in &report.probed_offsets {
            let read = dev.read_at(offset, 4096).unwrap();
            assert!(
                read.iter().all(|&b| b == 0),
                "probe at offset {offset} was not zero-filled"
            );
        }
    }

    #[test]
    fn sanity_check_respects_skip_below() {
        let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
        // Write marker within the skip region.
        let marker = vec![0xCD; 4096];
        dev.write_at(0, &marker).unwrap();

        let report = sanity_check_writable(&mut dev, 1 * ONE_MIB).unwrap();
        assert!(report.is_honest());

        // Marker at offset 0 should be preserved (below skip_below=1 MiB).
        let read = dev.read_at(0, 4096).unwrap();
        assert_eq!(read, marker);
    }

    #[test]
    fn verify_device_capacity_on_honest_device() {
        let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
        let report = verify_device_capacity(&mut dev, 64 * 1024, 10).unwrap();
        assert!(report.is_honest());
        assert_eq!(report.probed_offsets.len(), 10);
        assert_eq!(report.fake_ratio_percent(), 0);
    }

    #[test]
    fn verify_device_capacity_detects_fake() {
        let fake = FakeStickDevice {
            inner: MemoryDevice::new(16 * ONE_MIB, 4096).unwrap(),
            writable_until: ONE_MIB,
        };
        let mut dev = fake;
        let report = verify_device_capacity(&mut dev, 64 * 1024, 20).unwrap();
        assert!(!report.is_honest());
        // Most probes should fail since writable region is tiny.
        assert!(report.failed_offsets.len() > 10);
        assert!(report.fake_ratio_percent() > 80);
    }

    #[test]
    fn verify_device_capacity_rejects_zero_chunks() {
        let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
        let err = verify_device_capacity(&mut dev, 64 * 1024, 0).unwrap_err();
        assert!(err.to_string().contains("chunk_count"));
    }

    #[test]
    fn verify_device_rejects_read_only() {
        let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
        dev.set_read_only(true);
        let err = verify_device_capacity(&mut dev, 64 * 1024, 10).unwrap_err();
        assert!(matches!(err, CoreFsError::PolicyViolation(_)));
    }

    #[test]
    fn generate_test_pattern_is_offset_dependent() {
        let p1 = generate_test_pattern(0, 4096);
        let p2 = generate_test_pattern(4096, 4096);
        let p3 = generate_test_pattern(8192, 4096);
        assert_ne!(p1, p2);
        assert_ne!(p2, p3);
        assert_ne!(p1, p3);
        assert_eq!(p1.len(), 4096);
    }

    #[test]
    fn generate_test_pattern_is_deterministic() {
        let p1 = generate_test_pattern(12345, 4096);
        let p2 = generate_test_pattern(12345, 4096);
        assert_eq!(p1, p2);
    }

    #[test]
    fn device_verification_report_fake_ratio_calculation() {
        let report = DeviceVerificationReport {
            advertised_bytes: 1000,
            probed_offsets: vec![0, 500, 1000],
            failed_offsets: vec![500, 1000],
            highest_verified_offset: 100,
            estimated_usable_bytes: 100,
        };
        assert!(!report.is_honest());
        assert_eq!(report.fake_ratio_percent(), 90);
    }
}
