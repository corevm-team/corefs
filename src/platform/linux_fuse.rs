// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::app::{CoreFsService, PersistedState};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use corefs_core::platform::Timestamp;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::volume_wal::{VolumeWal, WalOperation};
use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use libc::{EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, EROFS};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(1);
const ROOT_INO: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinuxMountOptions {
    pub create_if_missing: bool,
}

#[derive(Debug, Clone)]
struct FuseNode {
    path: String,
    parent_path: String,
    inode: Option<Inode>,
    data: Vec<u8>,
}

impl FuseNode {
    fn ino(&self) -> u64 {
        self.inode
            .as_ref()
            .map(|inode| inode.id.0 + 1)
            .unwrap_or(ROOT_INO)
    }

    fn kind(&self) -> InodeKind {
        self.inode
            .as_ref()
            .map(|inode| inode.kind)
            .unwrap_or(InodeKind::Directory)
    }

    fn attr(&self) -> FileAttr {
        let now = SystemTime::now();
        let default_perm = match self.kind() {
            InodeKind::File => 0o644,
            InodeKind::Directory => 0o755,
            InodeKind::Symlink => 0o777,
        };
        let (uid, gid, perm) = self
            .inode
            .as_ref()
            .map(|inode| {
                let m = &inode.metadata;
                let mode: u16 = if m.mode == 0 {
                    default_perm
                } else {
                    (m.mode & 0o7777) as u16
                };
                (m.uid, m.gid, mode)
            })
            .unwrap_or((current_uid(), current_gid(), default_perm));
        let size = match self.kind() {
            InodeKind::File | InodeKind::Symlink => {
                // Prefer inode.size (always the logical/uncompressed size) over
                // data.len() — node.data may be empty for streaming files or
                // hold compressed bytes for files with compression enabled.
                self.inode
                    .as_ref()
                    .map(|i| i.size as u64)
                    .unwrap_or_else(|| self.data.len() as u64)
            }
            InodeKind::Directory => 0,
        };
        let mtime = self
            .inode
            .as_ref()
            .map(|inode| inode.modified_at.into())
            .unwrap_or(now);
        // POSIX ctime — status-change time.  Falls back to mtime when the
        // inode has no explicit changed_at yet.
        let ctime = self
            .inode
            .as_ref()
            .map(|inode| inode.changed_at.into())
            .unwrap_or(now);
        let crtime = self
            .inode
            .as_ref()
            .map(|inode| inode.created_at.into())
            .unwrap_or(now);

        FileAttr {
            ino: self.ino(),
            size,
            blocks: 1,
            atime: mtime,
            mtime,
            ctime,
            crtime,
            kind: match self.kind() {
                InodeKind::File => FileType::RegularFile,
                InodeKind::Directory => FileType::Directory,
                InodeKind::Symlink => FileType::Symlink,
            },
            perm,
            nlink: if matches!(self.kind(), InodeKind::Directory) {
                2
            } else {
                1
            },
            uid,
            gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct CoreFsFuseView {
    nodes_by_ino: HashMap<u64, FuseNode>,
    ino_by_path: HashMap<String, u64>,
    children: BTreeMap<String, Vec<String>>,
    volume_name: String,
}

impl CoreFsFuseView {
    fn from_state(state: PersistedState) -> Self {
        let encryption_service = if state.config.security.encryption_at_rest {
            let mut enc = crate::services::encryption::EncryptionService::default();
            enc.derive_key_from(state.config.volume_name.as_bytes());
            Some(enc)
        } else {
            None
        };
        Self::from_state_with_encryption(state, encryption_service.as_ref())
    }

    fn from_state_with_encryption(
        state: PersistedState,
        encryption_service: Option<&crate::services::encryption::EncryptionService>,
    ) -> Self {
        // BlockRecord is now metadata-only — bytes are not stored in PersistedState.
        // For the ODF/read-only path the block_map is empty; use from_service() for
        // live mounts that need actual file content.
        let block_map: HashMap<InodeId, Vec<u8>> = HashMap::new();
        Self::from_state_with_encryption_and_bytes(state, encryption_service, block_map)
    }

    fn from_state_with_encryption_and_bytes(
        state: PersistedState,
        encryption_service: Option<&crate::services::encryption::EncryptionService>,
        block_map: HashMap<InodeId, Vec<u8>>,
    ) -> Self {
        let mut nodes_by_ino = HashMap::new();
        let mut ino_by_path = HashMap::new();
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();

        let root = FuseNode {
            path: "/".to_string(),
            parent_path: "/".to_string(),
            inode: None,
            data: Vec::new(),
        };
        nodes_by_ino.insert(ROOT_INO, root);
        ino_by_path.insert("/".to_string(), ROOT_INO);
        children.entry("/".to_string()).or_default();

        for inode in state.active_inodes {
            let ino = inode.id.0 + 1;
            let parent_path = parent_path(&inode.path);
            let raw = block_map.get(&inode.id).cloned().unwrap_or_default();
            // Reverse pipeline: decrypt → decompress.  Block bytes may be encrypted
            // and/or LZ4-compressed when the file was written with those features.
            let mut data = raw;
            if inode.metadata.encrypted && !data.is_empty() {
                if let Some(enc) = encryption_service {
                    data = enc.decrypt(&data).unwrap_or(data);
                }
            }
            if inode.metadata.compressed && !data.is_empty() {
                let mut dec = lz4_flex::frame::FrameDecoder::new(data.as_slice());
                let mut out = Vec::new();
                data = std::io::Read::read_to_end(&mut dec, &mut out)
                    .map(|_| out)
                    .unwrap_or(data);
            }
            children
                .entry(parent_path.clone())
                .or_default()
                .push(base_name(&inode.path));
            children.entry(inode.path.clone()).or_default();
            ino_by_path.insert(inode.path.clone(), ino);
            nodes_by_ino.insert(
                ino,
                FuseNode {
                    path: inode.path.clone(),
                    parent_path,
                    inode: Some(inode),
                    data,
                },
            );
        }

        for names in children.values_mut() {
            names.sort();
            names.dedup();
        }

        Self {
            nodes_by_ino,
            ino_by_path,
            children,
            volume_name: state.volume.name,
        }
    }

    fn load_image(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let fs = CoreFsService::load_image_from_path(path)?;
        let block_bytes = fs.read_all_block_bytes();
        let encryption_service = if fs.export_state().config.security.encryption_at_rest {
            let mut enc = crate::services::encryption::EncryptionService::default();
            enc.derive_key_from(fs.volume_name().as_bytes());
            Some(enc)
        } else {
            None
        };
        let state = fs.export_state();
        Ok(Self::from_state_with_encryption_and_bytes(
            state,
            encryption_service.as_ref(),
            block_bytes,
        ))
    }

    /// Load a `CoreFsFuseView` from an ODF-native volume image.  The
    /// file is opened read-only via [`crate::storage::block_device::FileImageDevice`] and the state
    /// is reconstructed through [`crate::storage::ondisk::native::load_state_native`].
    /// Pending journal transactions (if any) are **not** replayed by
    /// this path — read-only mounts treat the on-disk state as-is.
    fn load_odf_image(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        use crate::storage::block_device::FileImageDevice;
        use crate::storage::ondisk::native::load_state_native;
        let device = FileImageDevice::open(path.as_ref(), true)?;
        let state = load_state_native(&device)?;
        Ok(Self::from_state(state))
    }

    fn node(&self, ino: u64) -> Option<&FuseNode> {
        self.nodes_by_ino.get(&ino)
    }

    fn lookup_child(&self, parent: u64, name: &OsStr) -> Option<&FuseNode> {
        let parent_node = self.node(parent)?;
        let child_name = name.to_str()?;
        let child_path = if parent_node.path == "/" {
            format!("/{child_name}")
        } else {
            format!("{}/{}", parent_node.path, child_name)
        };
        let ino = self.ino_by_path.get(&child_path)?;
        self.node(*ino)
    }

    fn directory_entries(&self, ino: u64) -> Vec<(u64, FileType, String)> {
        let Some(node) = self.node(ino) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        entries.push((node.ino(), FileType::Directory, ".".to_string()));
        let parent_ino = if ino == ROOT_INO {
            ROOT_INO
        } else {
            *self.ino_by_path.get(&node.parent_path).unwrap_or(&ROOT_INO)
        };
        entries.push((parent_ino, FileType::Directory, "..".to_string()));

        if let Some(children) = self.children.get(&node.path) {
            for child_name in children {
                let child_path = if node.path == "/" {
                    format!("/{child_name}")
                } else {
                    format!("{}/{}", node.path, child_name)
                };
                if let Some(child_ino) = self.ino_by_path.get(&child_path) {
                    if let Some(child) = self.node(*child_ino) {
                        let file_type = match child.kind() {
                            InodeKind::File => FileType::RegularFile,
                            InodeKind::Directory => FileType::Directory,
                            InodeKind::Symlink => FileType::Symlink,
                        };
                        entries.push((*child_ino, file_type, child_name.clone()));
                    }
                }
            }
        }

        entries
    }
}

pub fn create_image(path: impl AsRef<Path>, include_demo: bool) -> CoreFsResult<()> {
    let fs = if include_demo {
        demo_fs()?
    } else {
        CoreFsService::format(crate::config::CoreFsConfig::default())
    };
    fs.save_image_to_path(path)
}

/// Read-only FUSE mount of an ODF-native image.
///
/// The image file is opened through [`crate::storage::block_device::FileImageDevice`] in read-only
/// mode; the filesystem view is reconstructed via
/// [`crate::storage::ondisk::native::load_state_native`].  No writes
/// are accepted — analogous to [`mount_image`] for the legacy
/// volume_image format.
pub fn mount_odf_image(
    image_path: impl AsRef<Path>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let image_path = image_path.as_ref();
    let mount_point = mount_point.as_ref();
    let view = CoreFsFuseView::load_odf_image(image_path)?;
    let mount = CoreFsFuseMount { view };
    let fs_name = format!("corefs-odf:{}", mount.view.volume_name);

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RO,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount ODF image {} on {}: {error}",
            image_path.display(),
            mount_point.display()
        ))
    })
}

pub fn mount_image(
    image_path: impl AsRef<Path>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let image_path = image_path.as_ref();
    let mount_point = mount_point.as_ref();
    let view = CoreFsFuseView::load_image(image_path)?;
    let mount = CoreFsFuseMount { view };
    let fs_name = format!("corefs:{}", mount.view.volume_name);

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RO,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount CoreFS image {} on {}: {error}",
            image_path.display(),
            mount_point.display()
        ))
    })
}

#[derive(Debug, Clone)]
struct CoreFsFuseMount {
    view: CoreFsFuseView,
}

impl Filesystem for CoreFsFuseMount {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.view.lookup_child(parent, name) {
            Some(node) => reply.entry(&TTL, &node.attr(), 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        match self.view.node(ino) {
            Some(node) => reply.attr(&TTL, &node.attr()),
            None => reply.error(ENOENT),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.view.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::Symlink) => reply.data(&node.data),
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.view.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::File) => {
                if flags & libc::O_ACCMODE != libc::O_RDONLY {
                    reply.error(EROFS);
                } else {
                    reply.opened(0, 0);
                }
            }
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        match self.view.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::File) => {
                let start = offset.max(0) as usize;
                let end = start.saturating_add(size as usize).min(node.data.len());
                let slice = node.data.get(start..end).unwrap_or(&[]);
                reply.data(slice);
            }
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let entries = self.view.directory_entries(ino);
        if entries.is_empty() {
            reply.error(ENOENT);
            return;
        }

        for (index, (entry_ino, kind, name)) in
            entries.into_iter().enumerate().skip(offset as usize)
        {
            let next_offset = (index + 1) as i64;
            if reply.add(entry_ino, next_offset, kind, name) {
                break;
            }
        }

        reply.ok();
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        reply.statfs(
            fuse_total_blocks(),
            fuse_free_blocks(&self.view.nodes_by_ino),
            fuse_free_blocks(&self.view.nodes_by_ino),
            self.view.nodes_by_ino.len() as u64,
            fuse_free_inodes(self.view.nodes_by_ino.len()),
            FUSE_BLOCK_SIZE,
            255,
            FUSE_BLOCK_SIZE,
        );
    }
}

// ── Read-write FUSE mount ────────────────────────────────────────────────────

/// When the uncommitted per-handle write buffer reaches this size it is flushed
/// to the service and cleared, keeping peak RAM proportional to the threshold
/// rather than to the full file size.
///
/// Raising this reduces the number of `extend_file` calls during streaming
/// writes, which matters because the current [`BlockStore::append_to_inode`]
/// implementation is O(existing bytes) per call (read-modify-write).  Smaller
/// thresholds turn a sequential write into a quadratic operation.
///
/// Can be overridden at runtime via `COREFS_STREAM_FLUSH_MIB` (unit: MiB).
const STREAM_FLUSH_THRESHOLD: usize = 64 * 1024 * 1024; // 64 MiB

/// Returns the effective streaming-flush threshold in bytes, honouring the
/// `COREFS_STREAM_FLUSH_MIB` env var when it parses to a positive integer.
fn stream_flush_threshold() -> usize {
    std::env::var("COREFS_STREAM_FLUSH_MIB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .map(|mib| mib.saturating_mul(1024 * 1024))
        .unwrap_or(STREAM_FLUSH_THRESHOLD)
}

// ── Virtual node INO space ────────────────────────────────────────────────────
// Real CoreFS InodeIds are small sequential numbers; virtual nodes use the top
// of the u64 range to avoid collisions.

/// INO of the virtual `.snapshots/` root directory.
const SNAPSHOTS_DIR_INO: u64 = u64::MAX - 1;

/// INOs for snapshot subdirectories within `.snapshots/`.
/// Snapshot N's root dir has INO = SNAP_SUBDIR_BASE + N.
const SNAP_SUBDIR_BASE: u64 = u64::MAX / 2 + 1_000_000;

/// First INO assigned to dynamically created virtual file/dir nodes
/// (snapshot path dirs and time-travel files).
const VIRT_INO_BASE: u64 = u64::MAX / 4;

// ── Virtual node types ────────────────────────────────────────────────────────

/// Identifies a unique virtual node for deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum VirtKey {
    /// A subdirectory of `.snapshots/snap-N/` mirroring `fs_path` in snapshot N.
    SnapDir { snapshot_id: u64, fs_path: String },
    /// A file inside a snapshot, serving content at snapshot time.
    SnapFile { snapshot_id: u64, fs_path: String },
    /// A time-travel file: `path` at a specific version.
    TimeTravel { fs_path: String, version_id: u64 },
}

/// A virtual read-only directory node (snapshot subtree or time-travel container).
#[derive(Debug, Clone)]
struct VirtDir {
    /// Which snapshot this belongs to.
    snapshot_id: u64,
    /// The corresponding real path inside the snapshot (e.g. "/" or "/etc").
    fs_path: String,
    modified_at: Timestamp,
}

/// A virtual read-only file node (snapshot version or time-travel).
#[derive(Debug, Clone)]
struct VirtFile {
    bytes: Vec<u8>,
    modified_at: Timestamp,
}

/// Backing store for the FUSE RW mount.
///
/// Three persistence targets are supported:
///
/// * [`FuseBacking::File`] — `volume_image` file format, persisted via
///   segment-incremental writes against an open
///   [`crate::storage::block_device::FileImageDevice`].  Post-P3 only the
///   segments whose payloads actually changed are rewritten in-place;
///   unchanged segments are skipped entirely.  Crash consistency relies on
///   the dual-superblock + per-segment checksum layout of the on-disk
///   format, the same way the Device backing does.
/// * [`FuseBacking::Device`] — legacy `volume_image` on a
///   [`crate::storage::block_device::BlockDevice`], incremental
///   segment-level writes via `persist_to_device_incremental`.
/// * [`FuseBacking::Odf`] — native ODF v1 volume on a file-backed
///   [`crate::storage::block_device::FileImageDevice`], persisted
///   through
///   [`crate::storage::ondisk::native::save_state_native_incremental`]
///   with crash-consistent journal semantics.
enum FuseBacking {
    /// Image on the host filesystem, opened as a [`FileImageDevice`] for
    /// incremental segment writes.
    ///
    /// `path` is kept so `statfs` / diagnostics can refer to the backing
    /// file by name.  `device` is the live handle used for persists;
    /// `cache` tracks the per-segment layout + payloads so consecutive
    /// checkpoints only rewrite the segments that actually changed.
    ///
    /// `device` is `Option` so unit tests that only exercise the
    /// in-memory FUSE mount state (no persist) can construct a
    /// `FuseBacking::File` with a dummy path; `persist()` opens the
    /// device lazily on first checkpoint if it is not already present.
    File {
        path: PathBuf,
        device: Option<crate::storage::block_device::FileImageDevice>,
        cache: Option<crate::storage::volume_image::DeviceImageCache>,
    },
    Device {
        device: Box<dyn crate::storage::block_device::BlockDevice>,
        /// Cache of segment layout and payloads used for incremental persists.
        /// Populated on first write; reset whenever the image layout changes.
        cache: Option<crate::storage::volume_image::DeviceImageCache>,
    },
    /// ODF-native backing — the FUSE daemon mutates an in-memory
    /// `CoreFsService`, and every sync / unmount goes through
    /// `save_state_native_incremental` against the held FileImageDevice.
    Odf {
        device: crate::storage::block_device::FileImageDevice,
        image_path: PathBuf,
        /// Cache of InodeId → (content_crc, extents) for blocks already
        /// written to the ODF device.  Avoids re-writing unchanged data.
        odf_extents: std::collections::HashMap<
            crate::domain::inode::InodeId,
            (u32, Vec<crate::storage::block_store::ExtentRef>),
        >,
    },
}

impl std::fmt::Debug for FuseBacking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File { path, .. } => write!(f, "File({:?})", path),
            Self::Device { device, .. } => write!(f, "Device({:?})", device.geometry()),
            Self::Odf { image_path, .. } => write!(f, "Odf({:?})", image_path),
        }
    }
}

#[derive(Debug)]
struct CoreFsFuseMountRw {
    service: CoreFsService,
    backing: FuseBacking,
    pending_wal: Option<VolumeWal>,
    nodes_by_ino: HashMap<u64, FuseNode>,
    ino_by_path: HashMap<String, u64>,
    children: BTreeMap<String, Vec<String>>,
    next_handle: u64,
    open_files: HashMap<u64, OpenFileHandle>,
    dirty: bool,
    /// Virtual read-only directory nodes (snapshot subdirs, time-travel dirs).
    virt_dirs: HashMap<u64, VirtDir>,
    /// Virtual read-only file nodes (snapshot files, time-travel files).
    virt_files: HashMap<u64, VirtFile>,
    /// Deduplication: VirtKey → assigned INO.
    virt_ino_map: HashMap<VirtKey, u64>,
    /// Next INO to assign for a new virtual node.
    next_virt_ino: u64,
    /// Online-tool control socket listener (if mounted with IPC enabled).
    ctl_listener: Option<crate::platform::online_ctl::CtlListener>,
}

#[derive(Debug, Clone)]
struct OpenFileHandle {
    ino: u64,
    path: String,
    /// Uncommitted write buffer.  For small files this holds all content; for
    /// streaming files it holds only the bytes since the last intermediate flush.
    data: Vec<u8>,
    /// Bytes already committed to the service via intermediate streaming flushes.
    /// Zero for non-streaming handles.  Total logical file size =
    /// `committed_size + data.len()`.
    committed_size: usize,
    dirty: bool,
}

/// Describes how a time-travel lookup should resolve the historical version.
#[derive(Debug, Clone)]
enum TimeTravelSpec {
    /// Find the version at or before this instant.
    At(Timestamp),
    /// Find the exact version by ID.
    VersionId(u64),
}

/// Parse `YYYY-MM-DD` into a `Timestamp` at midnight UTC.
fn parse_date(s: &str) -> Option<Timestamp> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let d: u64 = parts[2].parse().ok()?;
    if m < 1 || m > 12 || d < 1 || d > 31 {
        return None;
    }
    // Approximate seconds since UNIX epoch (good enough for version lookups).
    let days = days_since_epoch(y, m, d)?;
    Some(Timestamp::from_secs(days * 86400))
}

/// Parse `YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS`.
fn parse_datetime(s: &str) -> Option<Timestamp> {
    let (date_part, time_part) = s.split_once('T')?;
    let base = parse_date(date_part)?;
    let time_parts: Vec<&str> = time_part.split(':').collect();
    let h: u64 = time_parts.first()?.parse().ok()?;
    let min: u64 = time_parts.get(1)?.parse().ok()?;
    let sec: u64 = time_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let offset_secs = h * 3600 + min * 60 + sec;
    Some(Timestamp::from_secs(base.as_secs().saturating_add(offset_secs)))
}

/// Returns days since Unix epoch (1970-01-01) for a proleptic Gregorian date.
fn days_since_epoch(y: u64, m: u64, d: u64) -> Option<u64> {
    if y < 1970 {
        return None;
    }
    // Days in each month (non-leap).
    let days_in_month = [0u64, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = |yr: u64| (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0;

    let mut days: u64 = 0;
    for yr in 1970..y {
        days += if is_leap(yr) { 366 } else { 365 };
    }
    for mo in 1..m {
        days += days_in_month[mo as usize];
        if mo == 2 && is_leap(y) {
            days += 1;
        }
    }
    days += d.saturating_sub(1);
    Some(days)
}

impl CoreFsFuseMountRw {
    fn from_service(service: CoreFsService, backing: FuseBacking) -> Self {
        let mut mount = Self {
            service,
            backing,
            pending_wal: None,
            nodes_by_ino: HashMap::new(),
            ino_by_path: HashMap::new(),
            children: BTreeMap::new(),
            next_handle: 1,
            open_files: HashMap::new(),
            dirty: false,
            virt_dirs: HashMap::new(),
            virt_files: HashMap::new(),
            virt_ino_map: HashMap::new(),
            next_virt_ino: VIRT_INO_BASE,
            ctl_listener: None,
        };
        mount.rebuild_indexes();
        mount
    }

    /// Start the online-tool control socket listener for the given mount
    /// point.  Silently does nothing if the socket cannot be bound
    /// (non-fatal — the daemon simply has no online-tool support).
    fn start_ctl_listener(&mut self, mount_point: &Path) {
        match crate::platform::online_ctl::CtlListener::bind(mount_point) {
            Ok(listener) => {
                self.ctl_listener = Some(listener);
            }
            Err(_) => { /* non-fatal */ }
        }
    }

    /// Drain any pending online-tool requests and execute them.
    fn process_online_requests(&mut self) {
        if self.ctl_listener.is_none() {
            return;
        }
        // Collect all pending requests first to avoid borrow conflict.
        let pending: Vec<_> = self
            .ctl_listener
            .as_ref()
            .unwrap()
            .rx
            .try_iter()
            .collect();
        for p in pending {
            let response = self.handle_online_request(&p.request);
            let _ = p.reply_tx.send(response);
        }
    }

    fn handle_online_request(
        &mut self,
        request: &crate::platform::online_ctl::OnlineRequest,
    ) -> crate::platform::online_ctl::OnlineResponse {
        use crate::platform::online_ctl::{OnlineRequest, OnlineResponse};
        match request {
            OnlineRequest::Status => {
                let paths = self.service.list_paths();
                OnlineResponse::Ok {
                    message: format!(
                        "volume={} files={} dirty={}",
                        self.service.volume_name(),
                        paths.len(),
                        self.dirty,
                    ),
                }
            }
            OnlineRequest::Scrub { .. } => {
                let report = self.service.scrub();
                OnlineResponse::Ok {
                    message: format!(
                        "scrub: checked_paths={} valid_blocks={} invalid_blocks={}",
                        report.checked_paths,
                        report.valid_blocks,
                        report.invalid_blocks,
                    ),
                }
            }
            OnlineRequest::Defrag => {
                let report = self.service.defragment();
                OnlineResponse::Ok {
                    message: format!(
                        "defrag: moved={} gaps_reclaimed={}",
                        report.moved_entries, report.reclaimed_gaps,
                    ),
                }
            }
            OnlineRequest::SnapshotCreate { name } => {
                let snap = self.service.create_snapshot(name);
                OnlineResponse::Ok {
                    message: format!("snapshot created: id={} name={}", snap.id, snap.name),
                }
            }
            OnlineRequest::SnapshotList => {
                let names: Vec<_> = self
                    .service
                    .snapshots()
                    .iter()
                    .map(|s| format!("{}:{}", s.id, s.name))
                    .collect();
                OnlineResponse::Ok {
                    message: if names.is_empty() {
                        "no snapshots".into()
                    } else {
                        names.join(", ")
                    },
                }
            }
        }
    }

    fn open_session(_service: CoreFsService, image_path: PathBuf) -> CoreFsResult<Self> {
        use crate::storage::block_device::FileImageDevice;

        let mut service = CoreFsService::load_image_from_path(&image_path)?;
        service.mark_unclean_shutdown();

        // Mark the image unclean on disk before handing out the mount.  We
        // stay on the legacy tmp+rename path for this one-shot write so the
        // starting state is unambiguous even if a crash interrupts us
        // before the first incremental checkpoint runs.
        service.save_image_to_path(&image_path)?;

        // Open the image as a read-write FileImageDevice.  All subsequent
        // checkpoints go through persist_to_device_incremental_with_bytes,
        // so only segments that actually changed are rewritten.
        let device = FileImageDevice::open(&image_path, false)?;

        Ok(Self::from_service(
            service,
            FuseBacking::File {
                path: image_path,
                device: Some(device),
                cache: None,
            },
        ))
    }

    fn open_device_session(
        service: CoreFsService,
        device: Box<dyn crate::storage::block_device::BlockDevice>,
    ) -> CoreFsResult<Self> {
        Ok(Self::from_service(
            service,
            FuseBacking::Device {
                device,
                cache: None,
            },
        ))
    }

    /// Open an RW session backed by an ODF-native image file.
    ///
    /// Steps performed:
    ///
    /// 1. Open `image_path` as a read-write [`FileImageDevice`].
    /// 2. Replay any pending journal transactions via
    ///    [`recover_pending_transactions`] — crash-consistent recovery
    ///    from a prior interrupted persist.
    /// 3. Hydrate a [`CoreFsService`] from the on-disk state via
    ///    [`load_state_native`].
    /// 4. Mark the session as unclean on disk (so a future crash
    ///    before unmount is visible to `fsck-odf`) and flush.
    /// 5. Return the mount wrapper with `FuseBacking::Odf`.
    ///
    /// [`FileImageDevice`]: crate::storage::block_device::FileImageDevice
    /// [`recover_pending_transactions`]: crate::storage::ondisk::journaled::recover_pending_transactions
    /// [`load_state_native`]: crate::storage::ondisk::native::load_state_native
    fn open_odf_session(image_path: PathBuf) -> CoreFsResult<Self> {
        use crate::storage::block_device::FileImageDevice;
        use crate::storage::ondisk::journaled::recover_pending_transactions;
        use crate::storage::ondisk::native::{load_state_native, save_state_native_incremental};
        if !image_path.exists() {
            return Err(CoreFsError::NotFound(format!(
                "ODF RW mount: image not found: {}",
                image_path.display()
            )));
        }
        let mut device = FileImageDevice::open(&image_path, false)?;
        recover_pending_transactions(&mut device)?;
        let state = load_state_native(&device)?;
        let mut service = CoreFsService::from_persisted_state(state);

        // Dirty-marker flush: bump generation + write "unclean" state so
        // a power-loss before clean unmount is visible.  Crash-consistent
        // because the write itself goes through the journal.
        service.mark_unclean_shutdown();
        let dirty_state = service.persisted_state();
        save_state_native_incremental(&mut device, &dirty_state)?;

        // Build ODF extents cache and restore file bytes from ODF device.
        let odf_extents = crate::storage::ondisk::session::build_odf_extents_cache_pub(
            &service.export_state(),
        );
        crate::storage::ondisk::session::restore_bytes_from_odf_device_pub(
            &device,
            &mut service,
        )?;
        Ok(Self::from_service(
            service,
            FuseBacking::Odf { device, image_path, odf_extents },
        ))
    }

    /// Persists the current service state to the backing store.
    ///
    /// * [`FuseBacking::File`] — full atomic rewrite via temp-file + rename.
    /// * [`FuseBacking::Device`] — incremental segment-level writes via
    ///   `persist_to_device_incremental`; only segments whose bytes
    ///   actually changed are rewritten.
    /// * [`FuseBacking::Odf`] — incremental per-inode writes via
    ///   [`crate::storage::ondisk::native::save_state_native_incremental`]:
    ///   unchanged inodes stay untouched, changed or new inodes get
    ///   fresh slots, removed inodes are freed.  Every persist runs
    ///   through the transactional journal so a crash during the
    ///   persist is either fully replayed on next mount or leaves the
    ///   previous generation intact.
    fn persist(&mut self) -> CoreFsResult<()> {
        match &mut self.backing {
            FuseBacking::File { path, device, cache } => {
                // P3: incremental segment-level persist against the open
                // FileImageDevice.  Block content is carried through via
                // `persist_to_device_incremental_with_bytes` so the DATA
                // segment also benefits from the diff.
                //
                // Crash semantics: the same as the Device backing below.
                // Dual superblocks + per-segment checksums let the volume
                // recover to the previous generation if a segment write
                // got torn.  The WAL captured in the PersistedState makes
                // any unclean mutation that reached the image but not the
                // superblock replayable on next mount.
                if device.is_none() {
                    // Lazy-open path: tests may construct a File-backed
                    // mount without ever persisting.  Real mounts open
                    // the device eagerly in `open_session`, so this only
                    // fires if a test-constructed mount reaches persist
                    // for a path that has not yet been formatted.
                    //
                    // If the file does not exist we fall back to the
                    // legacy `save_image_to_path` path once to materialise
                    // it — that runs the full-image serialisation and
                    // atomic rename which creates the file cleanly.  All
                    // subsequent persists go through the incremental
                    // fast path against the now-open device.
                    use crate::storage::block_device::FileImageDevice;
                    let path_ref: &Path = path.as_ref();
                    if !path_ref.exists() {
                        self.service.save_image_to_path(path_ref)?;
                    }
                    *device = Some(FileImageDevice::open(path_ref, false)?);
                }
                let dev = device.as_mut().expect("device opened just above");
                let state = self.service.persisted_state();
                let block_bytes = self.service.read_all_block_bytes();

                // Phase 1e/1f status: the `*_partial_*` entry point
                // and the dirty-inode tracking it depends on are
                // wired up as a library primitive, but the FUSE hot
                // path stays on the full-build entry point for now.
                // The Phase-1f bench showed that routing fsync
                // through the partial path is a net regression on
                // the current segment-build pipeline, because
                // `split_blocks_partial` still materialises a
                // contiguous DATA `Vec<u8>` — copying from the
                // cached DATA payload instead of the service-side
                // block store, but at roughly the same memcpy cost.
                // The real win requires a sparse-range DATA emitter
                // that can reuse the cached buffer by reference and
                // only patch the dirty slice on the device.  That is
                // a separate Phase-2 architectural change (see
                // PERFORMANCE_LOG.md).
                let _report =
                    crate::storage::volume_image::persist_to_device_incremental_with_bytes_and_grow(
                        dev,
                        &state,
                        &block_bytes,
                        cache,
                        |dev, needed| {
                            use crate::storage::block_device::BlockDevice as _;
                            let sector_size = dev.sector_size() as u64;
                            let target = needed.saturating_mul(5) / 4;
                            let aligned = target.div_ceil(sector_size) * sector_size;
                            dev.resize(aligned)
                        },
                    )?;

                // Drain the dirty set every checkpoint so future
                // wake-ups for the partial path (once it is faster
                // than the full path) start from a clean slate.
                let _ = self.service.take_dirty_inodes();

                Ok(())
            }
            FuseBacking::Device { device, cache } => {
                let state = self.service.persisted_state();
                let _report = crate::storage::volume_image::persist_to_device_incremental(
                    device.as_mut(),
                    &state,
                    cache,
                )?;
                Ok(())
            }
            FuseBacking::Odf { device, odf_extents, .. } => {
                let state = self.service.persisted_state();
                let state = crate::storage::ondisk::session::write_bytes_to_odf_device_pub(
                    device,
                    &self.service,
                    state,
                    odf_extents,
                )?;
                let _report =
                    crate::storage::ondisk::native::save_state_native_incremental(device, &state)?;
                Ok(())
            }
        }
    }

    // ── Virtual node helpers ─────────────────────────────────────────────────

    /// Returns the INO for a virtual dir identified by `key`, creating it if needed.
    fn get_or_create_virt_dir(&mut self, key: VirtKey, dir: VirtDir) -> u64 {
        if let Some(&ino) = self.virt_ino_map.get(&key) {
            return ino;
        }
        let ino = self.next_virt_ino;
        self.next_virt_ino -= 1;
        self.virt_dirs.insert(ino, dir);
        self.virt_ino_map.insert(key, ino);
        ino
    }

    /// Returns the INO for a virtual file identified by `key`, creating it if needed.
    fn get_or_create_virt_file(&mut self, key: VirtKey, file: VirtFile) -> u64 {
        if let Some(&ino) = self.virt_ino_map.get(&key) {
            return ino;
        }
        let ino = self.next_virt_ino;
        self.next_virt_ino -= 1;
        self.virt_files.insert(ino, file);
        self.virt_ino_map.insert(key, ino);
        ino
    }

    fn virt_dir_attr(ino: u64, modified_at: Timestamp) -> FileAttr {
        let t: SystemTime = modified_at.into();
        FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn virt_file_attr(ino: u64, size: u64, modified_at: Timestamp) -> FileAttr {
        let t: SystemTime = modified_at.into();
        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    // ── Snapshot / time-travel lookup helpers ────────────────────────────────

    /// Returns `(snapshot_id, snapshot.created_at)` for the snapshot whose subdir INO
    /// is `ino`, or `None` if `ino` is not a top-level snapshot dir.
    fn snapshot_for_subdir_ino(&self, ino: u64) -> Option<(u64, Timestamp)> {
        if ino <= SNAP_SUBDIR_BASE {
            return None;
        }
        let id = ino - SNAP_SUBDIR_BASE;
        self.service
            .snapshots()
            .iter()
            .find(|s| s.id == id)
            .map(|s| (s.id, s.created_at))
    }

    /// Given a snapshot virtual dir INO, returns the corresponding real fs_path.
    fn fs_path_for_virt_dir(&self, ino: u64) -> Option<String> {
        if ino == SNAPSHOTS_DIR_INO {
            return None;
        }
        // Top-level snapshot subdir (represents "/" of that snapshot).
        if let Some((_, _)) = self.snapshot_for_subdir_ino(ino) {
            return Some("/".to_string());
        }
        // Deeper virtual dir.
        self.virt_dirs.get(&ino).map(|d| d.fs_path.clone())
    }

    /// Direct children of `parent_fs_path` present in `snapshot.paths`.
    /// Returns `(child_name, full_child_path, is_dir)`.
    fn snapshot_children(
        &self,
        snapshot_paths: &[String],
        parent_fs_path: &str,
    ) -> Vec<(String, String, bool)> {
        let prefix = if parent_fs_path == "/" {
            "/".to_string()
        } else {
            format!("{parent_fs_path}/")
        };

        let mut seen: HashMap<String, bool> = HashMap::new();

        for path in snapshot_paths {
            let rest = if parent_fs_path == "/" {
                path.strip_prefix('/')
            } else {
                path.strip_prefix(&prefix)
            };
            let Some(rest) = rest else { continue };
            if rest.is_empty() {
                continue;
            }
            let component = rest.split('/').next().unwrap_or(rest);
            let is_dir = rest.contains('/');
            seen.entry(component.to_string())
                .and_modify(|d| *d = *d || is_dir)
                .or_insert(is_dir);
        }

        seen.into_iter()
            .map(|(name, is_dir)| {
                let full = if parent_fs_path == "/" {
                    format!("/{name}")
                } else {
                    format!("{parent_fs_path}/{name}")
                };
                (name, full, is_dir)
            })
            .collect()
    }

    /// Try to parse a time-travel suffix: `@YYYY-MM-DD`, `@YYYY-MM-DDTHH:MM`,
    /// `@YYYY-MM-DDTHH:MM:SS`, or `@vN` (version ID).
    /// Returns `Some(SystemTime)` for timestamp forms, or calls the version-id path.
    fn parse_time_travel(suffix: &str) -> Option<TimeTravelSpec> {
        if let Some(id_str) = suffix.strip_prefix('v') {
            if let Ok(id) = id_str.parse::<u64>() {
                return Some(TimeTravelSpec::VersionId(id));
            }
        }
        // Try date-only: YYYY-MM-DD
        if suffix.len() == 10 {
            if let Some(t) = parse_date(suffix) {
                return Some(TimeTravelSpec::At(t));
            }
        }
        // Try datetime: YYYY-MM-DDTHH:MM or YYYY-MM-DDTHH:MM:SS
        if suffix.len() >= 16 {
            if let Some(t) = parse_datetime(suffix) {
                return Some(TimeTravelSpec::At(t));
            }
        }
        None
    }

    /// Handle `lookup` for a name inside a snapshot directory at `parent_fs_path`.
    fn lookup_in_snapshot(
        &mut self,
        snap_id: u64,
        snap_ts: Timestamp,
        parent_fs_path: &str,
        name: &str,
        reply: ReplyEntry,
    ) {
        let child_fs_path = if parent_fs_path == "/" {
            format!("/{name}")
        } else {
            format!("{parent_fs_path}/{name}")
        };

        // Find the snapshot to get its paths list.
        let snap_paths: Vec<String> = self
            .service
            .snapshots()
            .iter()
            .find(|s| s.id == snap_id)
            .map(|s| s.paths.clone())
            .unwrap_or_default();

        // Is the child a directory (any snapshot path starts with child_fs_path/)?
        let is_dir = snap_paths.iter().any(|p| {
            p.starts_with(&format!("{child_fs_path}/"))
                || *p == child_fs_path
                    && snap_paths
                        .iter()
                        .any(|p2| p2.starts_with(&format!("{child_fs_path}/")))
        });
        let is_file = snap_paths.contains(&child_fs_path);

        if is_dir && !is_file {
            let key = VirtKey::SnapDir {
                snapshot_id: snap_id,
                fs_path: child_fs_path.clone(),
            };
            let dir = VirtDir {
                snapshot_id: snap_id,
                fs_path: child_fs_path,
                modified_at: snap_ts,
            };
            let ino = self.get_or_create_virt_dir(key, dir);
            reply.entry(&TTL, &Self::virt_dir_attr(ino, snap_ts), 0);
            return;
        }

        if is_file || (is_file && is_dir) {
            let bytes = self
                .service
                .version_bytes_at(&child_fs_path, snap_ts)
                .unwrap_or_default();
            let size = bytes.len() as u64;
            let key = VirtKey::SnapFile {
                snapshot_id: snap_id,
                fs_path: child_fs_path,
            };
            let ino = self.get_or_create_virt_file(
                key,
                VirtFile {
                    bytes,
                    modified_at: snap_ts,
                },
            );
            reply.entry(&TTL, &Self::virt_file_attr(ino, size, snap_ts), 0);
            return;
        }

        reply.error(ENOENT);
    }

    /// Rebuild all FUSE index maps from the current service state.
    /// Called after `from_service` and after any operation that changes paths (rename).
    fn rebuild_indexes(&mut self) {
        let state = self.service.export_state();
        // BlockRecord is now metadata-only; read actual bytes from the compat device.
        let block_map: HashMap<crate::domain::inode::InodeId, Vec<u8>> =
            self.service.read_all_block_bytes();

        let mut nodes_by_ino = HashMap::new();
        let mut ino_by_path = HashMap::new();
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();

        nodes_by_ino.insert(
            ROOT_INO,
            FuseNode {
                path: "/".to_string(),
                parent_path: "/".to_string(),
                inode: None,
                data: Vec::new(),
            },
        );
        ino_by_path.insert("/".to_string(), ROOT_INO);
        children.entry("/".to_string()).or_default();

        for inode in state.active_inodes {
            let ino = inode.id.0 + 1;
            let par = parent_path(&inode.path);
            let data = block_map.get(&inode.id).cloned().unwrap_or_default();
            children
                .entry(par.clone())
                .or_default()
                .push(base_name(&inode.path));
            children.entry(inode.path.clone()).or_default();
            ino_by_path.insert(inode.path.clone(), ino);
            nodes_by_ino.insert(
                ino,
                FuseNode {
                    path: inode.path.clone(),
                    parent_path: par,
                    inode: Some(inode),
                    data,
                },
            );
        }
        for names in children.values_mut() {
            names.sort();
            names.dedup();
        }

        self.nodes_by_ino = nodes_by_ino;
        self.ino_by_path = ino_by_path;
        self.children = children;
    }

    fn node(&self, ino: u64) -> Option<&FuseNode> {
        self.nodes_by_ino.get(&ino)
    }

    fn lookup_child(&self, parent: u64, name: &OsStr) -> Option<&FuseNode> {
        let par = self.nodes_by_ino.get(&parent)?;
        let child_name = name.to_str()?;
        let child_path = if par.path == "/" {
            format!("/{child_name}")
        } else {
            format!("{}/{child_name}", par.path)
        };
        let ino = self.ino_by_path.get(&child_path)?;
        self.nodes_by_ino.get(ino)
    }

    fn child_path(&self, parent: u64, name: &OsStr) -> Option<String> {
        let par = self.nodes_by_ino.get(&parent)?;
        let child_name = name.to_str()?;
        Some(if par.path == "/" {
            format!("/{child_name}")
        } else {
            format!("{}/{child_name}", par.path)
        })
    }

    fn directory_entries(&self, ino: u64) -> Vec<(u64, FileType, String)> {
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        entries.push((node.ino(), FileType::Directory, ".".to_string()));
        let parent_ino = if ino == ROOT_INO {
            ROOT_INO
        } else {
            *self.ino_by_path.get(&node.parent_path).unwrap_or(&ROOT_INO)
        };
        entries.push((parent_ino, FileType::Directory, "..".to_string()));

        if let Some(children) = self.children.get(&node.path) {
            for child_name in children {
                let child_path = if node.path == "/" {
                    format!("/{child_name}")
                } else {
                    format!("{}/{child_name}", node.path)
                };
                if let Some(child_ino) = self.ino_by_path.get(&child_path) {
                    if let Some(child) = self.nodes_by_ino.get(child_ino) {
                        let ft = match child.kind() {
                            InodeKind::File => FileType::RegularFile,
                            InodeKind::Directory => FileType::Directory,
                            InodeKind::Symlink => FileType::Symlink,
                        };
                        entries.push((*child_ino, ft, child_name.clone()));
                    }
                }
            }
        }
        entries
    }

    /// Register a freshly created path in all index maps.
    fn register_node(&mut self, node: FuseNode) {
        let ino = node.ino();
        let par = node.parent_path.clone();
        let name = base_name(&node.path);
        self.ino_by_path.insert(node.path.clone(), ino);
        self.children.entry(node.path.clone()).or_default();
        self.children.entry(par).or_default().push(name);
        // keep children sorted + deduplicated
        for names in self.children.values_mut() {
            names.sort();
            names.dedup();
        }
        self.nodes_by_ino.insert(ino, node);
    }

    /// Remove a path from all index maps.
    fn unregister_ino(&mut self, ino: u64) {
        if let Some(node) = self.nodes_by_ino.remove(&ino) {
            self.ino_by_path.remove(&node.path);
            self.children.remove(&node.path);
            if let Some(siblings) = self.children.get_mut(&node.parent_path) {
                let name = base_name(&node.path);
                siblings.retain(|n| n != &name);
            }
        }
    }

    /// Flush all pending writes and save the volume to the backing store.
    fn flush_to_backing(&mut self) -> CoreFsResult<()> {
        if self.flush_dirty_open_files().is_err() {
            return Err(CoreFsError::State(
                "failed to flush dirty Linux FUSE write cache".to_string(),
            ));
        }
        self.service.commit_write_transaction();
        self.service.clear_pending_wal();
        self.service.mark_clean_shutdown();
        match self.persist() {
            Ok(()) => {
                self.pending_wal = None;
                self.dirty = false;
                Ok(())
            }
            Err(error) => {
                self.service.mark_unclean_shutdown();
                Err(error)
            }
        }
    }

    /// Prepares the in-memory state for a mutation.
    ///
    /// Pre-P1 behaviour used to persist the image up to twice from here: once
    /// to record the unclean-shutdown flag and once to anchor the new WAL
    /// transaction on disk before the operation proper ran.  That turned every
    /// `create`/`mkdir`/`unlink`/... into a 3×-full-image-rewrite, which is
    /// the dominant cause of the Phase-0 slowdown factors.
    ///
    /// P1 removes those eager persists.  Crash safety is preserved because
    /// neither the unclean-shutdown flag nor the pending-WAL transaction are
    /// visible on disk until the next explicit checkpoint ([`persist`]):
    ///
    /// * If the daemon crashes before any checkpoint runs, the on-disk image
    ///   is still the one from the previous clean shutdown — no recovery is
    ///   required because nothing from this session ever reached the disk.
    /// * Once a checkpoint runs (triggered by `fsync`, unmount, or an
    ///   explicit sync), the unclean-shutdown flag and the accumulated WAL
    ///   entries hit the disk together in one atomic image rewrite.  A crash
    ///   between checkpoints loses unfsynced data (the POSIX contract).
    fn ensure_mutation_session(&mut self, label: &str) -> CoreFsResult<()> {
        if !self.service.had_unclean_shutdown() {
            self.service.mark_unclean_shutdown();
        }
        if !self.service.has_pending_transaction() {
            let transaction_id = self.service.begin_write_transaction(label);
            let wal = VolumeWal::new(transaction_id, label);
            self.service.set_pending_wal(wal.clone());
            self.pending_wal = Some(wal);
        }
        self.dirty = true;
        Ok(())
    }

    fn record_wal_operation(&mut self, operation: WalOperation) -> CoreFsResult<()> {
        self.service.update_pending_wal(|wal| {
            wal.push(operation.clone());
            Ok(())
        })?;
        if let Some(wal) = self.pending_wal.as_mut() {
            wal.push(operation);
        }
        Ok(())
    }

    /// Records a WAL operation and marks the volume dirty.
    ///
    /// Pre-P1 this used to call `persist()` immediately after each WAL
    /// record, so every metadata mutation rewrote the whole image on disk.
    /// Post-P1 the persist is deferred to the next checkpoint point — see
    /// [`ensure_mutation_session`] for the crash-safety argument.
    fn record_wal_operation_and_save(&mut self, operation: WalOperation) -> CoreFsResult<()> {
        self.record_wal_operation(operation)?;
        self.dirty = true;
        Ok(())
    }

    fn open_file_handle(&mut self, ino: u64, flags: i32) -> CoreFsResult<u64> {
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            return Err(CoreFsError::NotFound(format!("inode not found: {ino}")));
        };
        if !matches!(node.kind(), InodeKind::File) {
            return Err(CoreFsError::InvalidInput(format!(
                "inode is not a file: {ino}"
            )));
        }

        // Always seed from the service so that compressed files are transparently
        // decompressed.  node.data holds the raw (possibly compressed) block bytes
        // and must not be used directly as file content.
        let initial_data = if node.inode.as_ref().is_some_and(|i| i.size > 0) {
            self.service.read_file(&node.path).unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut handle = OpenFileHandle {
            ino,
            path: node.path.clone(),
            data: initial_data,
            committed_size: 0,
            dirty: false,
        };
        if flags & libc::O_TRUNC != 0 {
            handle.data.clear();
            handle.dirty = true;
            if let Some(node) = self.nodes_by_ino.get_mut(&ino) {
                node.data.clear();
                if let Some(ref mut inode) = node.inode {
                    inode.size = 0;
                    inode.touch_modified();
                }
            }
            self.dirty = true;
        }

        let fh = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1);
        self.open_files.insert(fh, handle);
        Ok(fh)
    }

    fn read_from_handle(&self, ino: u64, fh: u64, offset: i64, size: u32) -> CoreFsResult<Vec<u8>> {
        let Some(handle) = self.open_files.get(&fh) else {
            // No open handle: fall back to node cache (populated at open/flush time).
            let data = &self
                .nodes_by_ino
                .get(&ino)
                .ok_or_else(|| CoreFsError::NotFound(format!("inode not found: {ino}")))?
                .data;
            let start = offset.max(0) as usize;
            let end = start.saturating_add(size as usize).min(data.len());
            return Ok(data.get(start..end).unwrap_or(&[]).to_vec());
        };

        if handle.ino != ino {
            return Err(CoreFsError::State(format!(
                "file handle {fh} does not match inode {ino}"
            )));
        }

        let start = offset.max(0) as usize;
        let total_size = handle.committed_size + handle.data.len();
        let end = start.saturating_add(size as usize).min(total_size);

        if start >= end {
            return Ok(vec![]);
        }

        if handle.committed_size == 0 || start >= handle.committed_size {
            // Entirely within the uncommitted buffer.
            let buf_start = start.saturating_sub(handle.committed_size);
            let buf_end = (end - handle.committed_size).min(handle.data.len());
            return Ok(handle.data.get(buf_start..buf_end).unwrap_or(&[]).to_vec());
        }

        if end <= handle.committed_size {
            // Entirely within the committed portion — read from the service.
            let service_bytes = self.service.read_file(&handle.path)?;
            let s = start.min(service_bytes.len());
            let e = end.min(service_bytes.len());
            return Ok(service_bytes[s..e].to_vec());
        }

        // Spans the committed prefix and the uncommitted buffer — combine both.
        let service_bytes = self.service.read_file(&handle.path)?;
        let mut result = Vec::with_capacity(end - start);
        let committed_slice = &service_bytes[start..handle.committed_size.min(service_bytes.len())];
        result.extend_from_slice(committed_slice);
        let buf_end = (end - handle.committed_size).min(handle.data.len());
        result.extend_from_slice(&handle.data[..buf_end]);
        Ok(result)
    }

    fn write_to_handle(
        &mut self,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
    ) -> CoreFsResult<u32> {
        let write_start = offset.max(0) as usize;
        let write_end = write_start.saturating_add(data.len());

        // --- Phase 1: read handle state with a shared borrow (no &mut self needed) ---
        //
        // Pre-P1 the streaming trigger compared the *logical file size* against
        // the threshold (`logical_end + data.len() > THRESHOLD`).  Once the file
        // grew past the threshold, every subsequent sequential write — even a
        // tiny one — triggered a buffer flush, and each flush called
        // `append_to_inode` which is O(existing_bytes).  That turned a
        // sequential 128 MiB write into a quadratic read-modify-write loop
        // (~15 GiB of RAM traffic for a 1-MiB-chunked workload).  Phase 0
        // measured 25 MiB/s; removing the per-op persist then exposed the
        // full O(n²) as ~0.6 MiB/s.
        //
        // The correct metric is the *buffer* size: flush when the uncommitted
        // per-handle buffer would exceed the threshold, independent of how
        // much of the file is already on the service side.  That bounds
        // `extend_file` calls to `file_size / threshold`.
        let flush_threshold = stream_flush_threshold();
        let (is_streaming_flush, needs_mutation_session) = {
            let handle = self
                .open_files
                .get(&fh)
                .ok_or_else(|| CoreFsError::State(format!("unknown file handle: {fh}")))?;
            if handle.ino != ino {
                return Err(CoreFsError::State(format!(
                    "file handle {fh} does not match inode {ino}"
                )));
            }
            let logical_end = handle.committed_size + handle.data.len();
            let is_sequential = write_start == logical_end;
            let buffer_would_exceed = handle.data.len() + data.len() > flush_threshold;
            let streaming = is_sequential
                && buffer_would_exceed
                && write_start >= handle.committed_size;
            let needs_session = streaming && handle.committed_size == 0;
            (streaming, needs_session)
        };

        // --- Phase 2: start mutation session if this is the first streaming flush ---
        // This must happen before re-borrowing handle mutably so that
        // ensure_mutation_session can take &mut self freely.
        if needs_mutation_session {
            self.ensure_mutation_session("streaming-write")?;
        }

        // --- Phase 3: perform the write with a mutable borrow ---
        let new_total = {
            let handle = self
                .open_files
                .get_mut(&fh)
                .ok_or_else(|| CoreFsError::State(format!("unknown file handle: {fh}")))?;

            if is_streaming_flush {
                // Flush the current buffer to the service so that handle.data stays
                // bounded to ≤ STREAM_FLUSH_THRESHOLD.
                let buf_to_flush = std::mem::take(&mut handle.data);
                if !buf_to_flush.is_empty() {
                    let result = if handle.committed_size == 0 {
                        self.service.write_file(&handle.path, &buf_to_flush)
                    } else {
                        self.service.extend_file(&handle.path, &buf_to_flush)
                    };
                    let handle = self.open_files.get_mut(&fh).expect("handle must exist");
                    match result {
                        Ok(()) => handle.committed_size += buf_to_flush.len(),
                        Err(_) => {
                            handle.data = buf_to_flush;
                            return Err(CoreFsError::State("streaming flush failed".into()));
                        }
                    }
                }
                self.open_files
                    .get_mut(&fh)
                    .expect("handle must exist")
                    .data
                    .extend_from_slice(data);
            } else {
                // Small file or non-sequential write: keep everything in the buffer.
                let buf_needed = write_end.saturating_sub(handle.committed_size);
                if handle.data.len() < buf_needed {
                    handle.data.resize(buf_needed, 0);
                }
                let buf_start = write_start.saturating_sub(handle.committed_size);
                let buf_end = write_end.saturating_sub(handle.committed_size);
                handle.data[buf_start..buf_end].copy_from_slice(data);
            }

            let handle = self.open_files.get_mut(&fh).expect("handle must exist");
            handle.dirty = true;
            handle.committed_size + handle.data.len()
        };

        // Update only cached inode metadata; reads on an open handle go through
        // read_from_handle (checks handle.data / service), not node.data.
        if let Some(node) = self.nodes_by_ino.get_mut(&ino) {
            if let Some(ref mut inode) = node.inode {
                inode.size = new_total;
                inode.modified_at = Timestamp::now();
            }
        }
        self.dirty = true;
        Ok(data.len() as u32)
    }

    fn flush_file_handle(&mut self, fh: u64) -> CoreFsResult<bool> {
        let Some(handle) = self.open_files.get(&fh).cloned() else {
            return Ok(false);
        };
        if !handle.dirty {
            return Ok(false);
        }

        let inode_id = self
            .nodes_by_ino
            .get(&handle.ino)
            .and_then(|node| node.inode.as_ref().map(|inode| inode.id))
            .ok_or_else(|| CoreFsError::State(format!("missing inode metadata for handle {fh}")))?;
        self.ensure_mutation_session("write-cache-flush")?;

        let total_size = handle.committed_size + handle.data.len();

        if handle.committed_size == 0 {
            // Non-streaming path: write the full buffer to the service in one call.
            self.service.write_file(&handle.path, &handle.data)?;
        } else if !handle.data.is_empty() {
            // Streaming: flush the remaining uncommitted tail.
            self.service.extend_file(&handle.path, &handle.data)?;
        }
        // If committed_size > 0 and handle.data is empty: all bytes already committed.

        // WAL strategy for file data:
        //
        // PatchExtent records that embed the full file bytes are intentionally NOT
        // recorded here.  The image save below is atomic (write-then-rename), so after
        // the save the image is authoritative and no WAL replay is needed.  Recording
        // PatchExtent for a large file would:
        //   1. Clone the full blob from the service  (O(file_size) RAM)
        //   2. Generate O(file_size / block_size) WAL entries with embedded bytes
        //   3. Double the image size written to disk
        //
        // Only truncation-to-zero is recorded because it changes structural metadata
        // (inode.size) and must survive a crash before the next persist().
        if total_size == 0 {
            self.record_wal_operation(WalOperation::TruncateInode {
                inode: inode_id,
                size: 0,
            })?;
        }
        // For non-zero writes: the data is in the service and will be committed
        // atomically by the next checkpoint; no WAL entry required.
        //
        // P1: we intentionally do NOT persist here.  Pre-P1 every handle
        // flush rewrote the whole image, which is why sequential writes
        // maxed out at ~25 MiB/s in the Phase-0 baseline.  Post-P1 the
        // checkpoint runs only at fsync / unmount / background timer, so
        // the handle buffer hits the service (in-memory) and the next
        // persist coalesces many writes into one atomic image save.

        self.dirty = true;

        // Sync node.data once from the service so subsequent opens seed correct content.
        // For large streaming files node.data is not kept in RAM — the service blob
        // is the authoritative copy; node.data is cleared to avoid a second copy.
        if let Some(node) = self.nodes_by_ino.get_mut(&handle.ino) {
            if handle.committed_size == 0 {
                node.data = handle.data.clone();
            } else {
                // Streaming: don't duplicate the large blob in node.data.
                // Subsequent opens will re-populate from the service via open_file_handle.
                node.data = Vec::new();
            }
        }
        if let Some(open) = self.open_files.get_mut(&fh) {
            open.dirty = false;
        }
        Ok(true)
    }

    fn flush_dirty_open_files(&mut self) -> CoreFsResult<()> {
        let handles: Vec<u64> = self
            .open_files
            .iter()
            .filter_map(|(fh, handle)| handle.dirty.then_some(*fh))
            .collect();
        for fh in handles {
            self.flush_file_handle(fh)?;
        }
        Ok(())
    }

    fn release_file_handle(&mut self, fh: u64) -> CoreFsResult<()> {
        self.flush_file_handle(fh)?;
        self.open_files.remove(&fh);
        Ok(())
    }

    fn statfs_view(&self) -> (u64, u64) {
        match &self.backing {
            FuseBacking::File { path, .. } => fuse_capacity_blocks(path, &self.nodes_by_ino),
            FuseBacking::Device { device, .. } => {
                let used_bytes: u64 = self
                    .nodes_by_ino
                    .values()
                    .map(|n| n.data.len() as u64)
                    .sum();
                let used_blocks = used_bytes.div_ceil(FUSE_BLOCK_SIZE as u64);
                let total_blocks = device.capacity() / FUSE_BLOCK_SIZE as u64;
                let free_blocks = total_blocks.saturating_sub(used_blocks);
                (total_blocks, free_blocks)
            }
            FuseBacking::Odf { device, .. } => {
                // Use the device capacity as the statfs backbone so df(1)
                // shows a meaningful total — not the FUSE in-memory sum.
                use crate::storage::block_device::BlockDevice as _;
                let used_bytes: u64 = self
                    .nodes_by_ino
                    .values()
                    .map(|n| n.data.len() as u64)
                    .sum();
                let used_blocks = used_bytes.div_ceil(FUSE_BLOCK_SIZE as u64);
                let total_blocks = device.capacity() / FUSE_BLOCK_SIZE as u64;
                let free_blocks = total_blocks.saturating_sub(used_blocks);
                (total_blocks, free_blocks)
            }
        }
    }
}

impl Filesystem for CoreFsFuseMountRw {
    fn init(&mut self, _req: &Request<'_>, config: &mut KernelConfig) -> Result<(), libc::c_int> {
        // FUSE_WRITEBACK_CACHE (bit 16): the kernel buffers writes in the page cache and
        // delivers larger, batched write calls to the daemon instead of one call per
        // application write syscall.  This improves sequential write throughput and
        // decouples the application from our per-flush latency.
        const FUSE_WRITEBACK_CACHE: u32 = 1 << 16;
        let _ = config.add_capabilities(FUSE_WRITEBACK_CACHE);

        // Increase the maximum write request size from the default 128 KiB to 1 MiB.
        // This reduces the number of kernel↔daemon round-trips for large sequential
        // writes by ~8× (e.g. 250 MB: ~2 000 calls → ~250 calls).
        let _ = config.set_max_write(1024 * 1024);
        Ok(())
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();

        // ── .snapshots/ virtual root ─────────────────────────────────────────
        if parent == ROOT_INO && name_str == ".snapshots" {
            let attr = Self::virt_dir_attr(SNAPSHOTS_DIR_INO, Timestamp::EPOCH);
            reply.entry(&TTL, &attr, 0);
            return;
        }

        // ── Snapshot top-level subdir (.snapshots/snap-N-name) ───────────────
        if parent == SNAPSHOTS_DIR_INO {
            let snapshots: Vec<_> = self.service.snapshots().to_vec();
            for snap in &snapshots {
                let dir_name = format!("{}-{}", snap.id, snap.name);
                if name_str == dir_name {
                    let ino = SNAP_SUBDIR_BASE + snap.id;
                    let attr = Self::virt_dir_attr(ino, snap.created_at);
                    reply.entry(&TTL, &attr, 0);
                    return;
                }
            }
            reply.error(ENOENT);
            return;
        }

        // ── Snapshot subtree (inside snap-N/ or a deeper snapshot dir) ───────
        if let Some((snap_id, snap_ts)) = self.snapshot_for_subdir_ino(parent) {
            self.lookup_in_snapshot(snap_id, snap_ts, "/", &name_str, reply);
            return;
        }
        if let Some(virt_dir) = self.virt_dirs.get(&parent).cloned() {
            let (snap_id, snap_ts) = (virt_dir.snapshot_id, virt_dir.modified_at);
            let fs_path = virt_dir.fs_path.clone();
            self.lookup_in_snapshot(snap_id, snap_ts, &fs_path, &name_str, reply);
            return;
        }

        // ── Time-travel: filename@<spec> ─────────────────────────────────────
        if let Some(at_pos) = name_str.rfind('@') {
            let base = &name_str[..at_pos];
            let spec_str = &name_str[at_pos + 1..];
            if let Some(spec) = CoreFsFuseMountRw::parse_time_travel(spec_str) {
                let parent_path = self.nodes_by_ino.get(&parent).map(|n| n.path.clone());
                if let Some(parent_path) = parent_path {
                    let file_path = if parent_path == "/" {
                        format!("/{base}")
                    } else {
                        format!("{parent_path}/{base}")
                    };
                    let (version, version_id) = match &spec {
                        TimeTravelSpec::At(t) => {
                            let ids = self.service.file_version_ids(&file_path);
                            let matched = ids.iter().rev().find(|(_, ts)| ts <= t).copied();
                            let vid = matched.map(|(id, _)| id).unwrap_or(0);
                            (self.service.version_bytes_at(&file_path, *t), vid)
                        }
                        TimeTravelSpec::VersionId(id) => {
                            (self.service.version_bytes_by_id(&file_path, *id), *id)
                        }
                    };
                    if let Some(bytes) = version {
                        let size = bytes.len() as u64;
                        let key = VirtKey::TimeTravel {
                            fs_path: file_path,
                            version_id,
                        };
                        let mtime = Timestamp::now();
                        let ino = self.get_or_create_virt_file(
                            key,
                            VirtFile {
                                bytes,
                                modified_at: mtime,
                            },
                        );
                        let attr = Self::virt_file_attr(ino, size, mtime);
                        reply.entry(&TTL, &attr, 0);
                        return;
                    }
                }
            }
        }

        // ── Normal real-filesystem lookup ────────────────────────────────────
        match self.lookup_child(parent, name) {
            Some(node) => reply.entry(&TTL, &node.attr(), 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        if ino == SNAPSHOTS_DIR_INO {
            reply.attr(&TTL, &Self::virt_dir_attr(ino, Timestamp::EPOCH));
            return;
        }
        if let Some((_, ts)) = self.snapshot_for_subdir_ino(ino) {
            reply.attr(&TTL, &Self::virt_dir_attr(ino, ts));
            return;
        }
        if let Some(vd) = self.virt_dirs.get(&ino).cloned() {
            reply.attr(&TTL, &Self::virt_dir_attr(ino, vd.modified_at));
            return;
        }
        if let Some(vf) = self.virt_files.get(&ino).cloned() {
            reply.attr(
                &TTL,
                &Self::virt_file_attr(ino, vf.bytes.len() as u64, vf.modified_at),
            );
            return;
        }
        match self.node(ino) {
            Some(node) => reply.attr(&TTL, &node.attr()),
            None => reply.error(ENOENT),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // Virtual nodes are read-only; reject any attribute mutation.
        if self.virt_files.contains_key(&ino)
            || self.virt_dirs.contains_key(&ino)
            || ino == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(ino).is_some()
        {
            reply.error(EROFS);
            return;
        }
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            reply.error(ENOENT);
            return;
        };
        let path = node.path.clone();
        let is_file = matches!(node.kind(), InodeKind::File);

        // Metadata-only changes (chown/chmod): apply to service and
        // in-memory node cache, mark dirty.  Do NOT persist on every call —
        // the kernel will trigger persist via fsync/flush/umount when
        // durability is required.  This matches ext4/xfs semantics and
        // avoids a full image rewrite per chown (critical on slow devices
        // like USB sticks).
        if uid.is_some() || gid.is_some() {
            if self.service.set_owner(&path, uid, gid).is_err() {
                reply.error(EIO);
                return;
            }
            if let Some(n) = self.nodes_by_ino.get_mut(&ino) {
                if let Some(ref mut inode) = n.inode {
                    if let Some(u) = uid {
                        inode.metadata.uid = u;
                    }
                    if let Some(g) = gid {
                        inode.metadata.gid = g;
                    }
                    // POSIX: chown updates ctime, not mtime.  Versioning is
                    // not triggered — metadata-only change.
                }
            }
            self.dirty = true;
        }

        if let Some(new_mode) = mode {
            if self.service.set_mode(&path, new_mode).is_err() {
                reply.error(EIO);
                return;
            }
            if let Some(n) = self.nodes_by_ino.get_mut(&ino) {
                if let Some(ref mut inode) = n.inode {
                    inode.metadata.mode = new_mode & 0o7777;
                    // POSIX: chmod updates ctime, not mtime.
                }
            }
            self.dirty = true;
        }

        if !is_file {
            // Directories and symlinks: no size handling needed.
            match self.nodes_by_ino.get(&ino) {
                Some(node) => reply.attr(&TTL, &node.attr()),
                None => reply.error(ENOENT),
            }
            return;
        }

        if let Some(new_size) = size {
            let inode_id = self
                .nodes_by_ino
                .get(&ino)
                .and_then(|n| n.inode.as_ref().map(|i| i.id))
                .unwrap_or(InodeId(0));
            let mut buf = if let Some(fh) = _fh {
                self.open_files
                    .get(&fh)
                    .map(|handle| handle.data.clone())
                    .unwrap_or_else(|| self.service.read_file(&path).unwrap_or_default())
            } else {
                self.service.read_file(&path).unwrap_or_default()
            };
            buf.resize(new_size as usize, 0);
            if let Some(fh) = _fh {
                if let Some(handle) = self.open_files.get_mut(&fh) {
                    handle.data = buf.clone();
                    handle.dirty = true;
                }
                self.dirty = true;
            } else {
                if self.ensure_mutation_session("setattr").is_err() {
                    reply.error(EIO);
                    return;
                }
                if let Err(_) = self.service.write_file(&path, &buf) {
                    reply.error(EIO);
                    return;
                }
                if self
                    .record_wal_operation_and_save(WalOperation::TruncateInode {
                        inode: inode_id,
                        size: new_size as usize,
                    })
                    .is_err()
                {
                    reply.error(EIO);
                    return;
                }
                self.dirty = true;
            }
            if let Some(n) = self.nodes_by_ino.get_mut(&ino) {
                n.data = buf;
                if let Some(ref mut inode) = n.inode {
                    inode.touch_modified();
                    inode.size = new_size as usize;
                }
            }
        }
        match self.nodes_by_ino.get(&ino) {
            Some(node) => reply.attr(&TTL, &node.attr()),
            None => reply.error(ENOENT),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::Symlink) => reply.data(&node.data),
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        // Virtual files (snapshot content / time-travel) are read-only.
        if self.virt_files.contains_key(&ino) {
            if flags & libc::O_ACCMODE != libc::O_RDONLY {
                reply.error(EROFS);
            } else {
                reply.opened(0, 0);
            }
            return;
        }
        // Virtual directories cannot be opened as files.
        if ino == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(ino).is_some()
            || self.virt_dirs.contains_key(&ino)
        {
            reply.error(EISDIR);
            return;
        }
        match self.open_file_handle(ino, flags) {
            Ok(fh) => reply.opened(fh, 0),
            Err(CoreFsError::InvalidInput(_)) => reply.error(EIO),
            Err(CoreFsError::NotFound(_)) => reply.error(ENOENT),
            Err(_) => reply.error(EIO),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        // Serve virtual file (snapshot / time-travel) content directly.
        if let Some(vf) = self.virt_files.get(&ino) {
            let start = offset.max(0) as usize;
            let end = start.saturating_add(size as usize).min(vf.bytes.len());
            reply.data(vf.bytes.get(start..end).unwrap_or(&[]));
            return;
        }
        match self.read_from_handle(ino, _fh, offset, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(CoreFsError::NotFound(_)) => reply.error(ENOENT),
            Err(_) => reply.error(EIO),
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        // Virtual nodes (snapshots, time-travel) are read-only.
        if self.virt_files.contains_key(&ino)
            || self.virt_dirs.contains_key(&ino)
            || ino == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(ino).is_some()
        {
            reply.error(EROFS);
            return;
        }
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            reply.error(ENOENT);
            return;
        };
        if !matches!(node.kind(), InodeKind::File) {
            reply.error(EISDIR);
            return;
        }
        match self.write_to_handle(ino, _fh, offset, data) {
            Ok(written) => reply.written(written),
            Err(CoreFsError::State(_)) => reply.error(EIO),
            Err(_) => reply.error(EIO),
        }
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        // Reject creates inside virtual (read-only) directories.
        if parent == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(parent).is_some()
            || self.virt_dirs.contains_key(&parent)
        {
            reply.error(EROFS);
            return;
        }
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        if self.ino_by_path.contains_key(&path) {
            reply.error(EEXIST);
            return;
        }
        let req_uid = _req.uid();
        let req_gid = _req.gid();
        let req_mode = if _mode == 0 {
            0o644
        } else {
            _mode & !_umask & 0o7777
        };
        if self.ensure_mutation_session("create").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.create_file(&path, b"", &[]) {
            reply.error(EIO);
            return;
        }
        // Apply initial ownership and mode from the caller's credentials.
        let _ = self.service.set_owner(&path, Some(req_uid), Some(req_gid));
        let _ = self.service.set_mode(&path, req_mode);
        let Some(inode_id) = self.service.inode_for_path(&path) else {
            reply.error(EIO);
            return;
        };
        if self
            .record_wal_operation_and_save(WalOperation::CreateFile {
                path: path.clone(),
                inode: inode_id,
            })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        let inode = self.service.get_inode(&path).cloned();
        let par = parent_path(&path);
        let node = FuseNode {
            path,
            parent_path: par,
            inode,
            data: Vec::new(),
        };
        let attr = node.attr();
        self.register_node(node);
        let ino = inode_id.0 + 1;
        let Ok(fh) = self.open_file_handle(ino, _flags) else {
            reply.error(EIO);
            return;
        };
        self.dirty = true;
        reply.created(&TTL, &attr, 0, fh, 0);
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        // Reject mkdir inside virtual (read-only) directories.
        if parent == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(parent).is_some()
            || self.virt_dirs.contains_key(&parent)
        {
            reply.error(EROFS);
            return;
        }
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        if self.ino_by_path.contains_key(&path) {
            reply.error(EEXIST);
            return;
        }
        let req_uid = _req.uid();
        let req_gid = _req.gid();
        let req_mode = if _mode == 0 {
            0o755
        } else {
            _mode & !_umask & 0o7777
        };
        if self.ensure_mutation_session("mkdir").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.create_directory(&path) {
            reply.error(EIO);
            return;
        }
        // Apply initial ownership and mode from the caller's credentials.
        let _ = self.service.set_owner(&path, Some(req_uid), Some(req_gid));
        let _ = self.service.set_mode(&path, req_mode);
        let Some(inode_id) = self.service.inode_for_path(&path) else {
            reply.error(EIO);
            return;
        };
        if self
            .record_wal_operation_and_save(WalOperation::CreateDirectory {
                path: path.clone(),
                inode: inode_id,
            })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        let inode = self.service.get_inode(&path).cloned();
        let par = parent_path(&path);
        let node = FuseNode {
            path,
            parent_path: par,
            inode,
            data: Vec::new(),
        };
        let attr = node.attr();
        self.register_node(node);
        self.dirty = true;
        reply.entry(&TTL, &attr, 0);
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        if parent == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(parent).is_some()
            || self.virt_dirs.contains_key(&parent)
        {
            reply.error(EROFS);
            return;
        }
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        let Some(&ino) = self.ino_by_path.get(&path) else {
            reply.error(ENOENT);
            return;
        };
        if let Some(node) = self.nodes_by_ino.get(&ino) {
            if matches!(node.kind(), InodeKind::Directory) {
                reply.error(EISDIR);
                return;
            }
        }
        if self.ensure_mutation_session("unlink").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.delete_file(&path, false) {
            reply.error(EIO);
            return;
        }
        if self
            .record_wal_operation_and_save(WalOperation::DeletePath { path: path.clone() })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        self.unregister_ino(ino);
        self.dirty = true;
        reply.ok();
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        if parent == SNAPSHOTS_DIR_INO
            || self.snapshot_for_subdir_ino(parent).is_some()
            || self.virt_dirs.contains_key(&parent)
        {
            reply.error(EROFS);
            return;
        }
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        let Some(&ino) = self.ino_by_path.get(&path) else {
            reply.error(ENOENT);
            return;
        };
        if let Some(node) = self.nodes_by_ino.get(&ino) {
            if !matches!(node.kind(), InodeKind::Directory) {
                reply.error(ENOTDIR);
                return;
            }
        }
        // refuse non-empty directories
        let is_empty = self
            .children
            .get(&path)
            .map(|c| c.is_empty())
            .unwrap_or(true);
        if !is_empty {
            reply.error(ENOTEMPTY);
            return;
        }
        if self.ensure_mutation_session("rmdir").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.delete_file(&path, false) {
            reply.error(EIO);
            return;
        }
        if self
            .record_wal_operation_and_save(WalOperation::DeletePath { path: path.clone() })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        self.unregister_ino(ino);
        self.dirty = true;
        reply.ok();
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        // Reject rename involving virtual (read-only) directories.
        let is_virt_parent = |p: u64| -> bool {
            p == SNAPSHOTS_DIR_INO
                || self.snapshot_for_subdir_ino(p).is_some()
                || self.virt_dirs.contains_key(&p)
        };
        if is_virt_parent(parent) || is_virt_parent(newparent) {
            reply.error(EROFS);
            return;
        }
        // RENAME_EXCHANGE (2) is not supported.
        if flags & 2 != 0 {
            reply.error(EINVAL);
            return;
        }
        let Some(src_path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        let Some(dst_path) = self.child_path(newparent, newname) else {
            reply.error(ENOENT);
            return;
        };
        let Some(&src_ino) = self.ino_by_path.get(&src_path) else {
            reply.error(ENOENT);
            return;
        };
        // RENAME_NOREPLACE (1): fail if target already exists.
        if flags & 1 != 0 && self.ino_by_path.contains_key(&dst_path) {
            reply.error(EEXIST);
            return;
        }
        let src_is_dir = self
            .nodes_by_ino
            .get(&src_ino)
            .map(|n| matches!(n.kind(), InodeKind::Directory))
            .unwrap_or(false);

        // Type-compatibility and emptiness checks when overwriting an existing target.
        if let Some(&dst_ino) = self.ino_by_path.get(&dst_path) {
            let dst_is_dir = self
                .nodes_by_ino
                .get(&dst_ino)
                .map(|n| matches!(n.kind(), InodeKind::Directory))
                .unwrap_or(false);
            if src_is_dir && !dst_is_dir {
                reply.error(ENOTDIR);
                return;
            }
            if !src_is_dir && dst_is_dir {
                reply.error(EISDIR);
                return;
            }
            if dst_is_dir {
                let empty = self
                    .children
                    .get(&dst_path)
                    .map(|c| c.is_empty())
                    .unwrap_or(true);
                if !empty {
                    reply.error(ENOTEMPTY);
                    return;
                }
            }
        }

        if self.ensure_mutation_session("rename").is_err() {
            reply.error(EIO);
            return;
        }
        if self.service.rename_entry(&src_path, &dst_path).is_err() {
            reply.error(EIO);
            return;
        }
        if self
            .record_wal_operation_and_save(WalOperation::RenamePath {
                from: src_path.clone(),
                to: dst_path.clone(),
            })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        self.rebuild_indexes();
        self.dirty = true;
        reply.ok();
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        let (total_blocks, free_blocks) = self.statfs_view();
        reply.statfs(
            total_blocks,
            free_blocks,
            free_blocks,
            self.nodes_by_ino.len() as u64,
            fuse_free_inodes(self.nodes_by_ino.len()),
            FUSE_BLOCK_SIZE,
            255,
            FUSE_BLOCK_SIZE,
        );
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        // `flush` is called on every `close(2)`.  POSIX does not guarantee
        // durability on close — only `fsync(2)` does — so P1 reduces this to
        // pushing the uncommitted handle buffer into the in-memory service.
        // The actual on-disk checkpoint is deferred to `fsync` / unmount /
        // the background checkpoint timer.  This keeps `echo foo > file`
        // (which implicitly calls close) from rewriting the whole image.
        self.process_online_requests();

        if self.flush_file_handle(_fh).is_err() {
            reply.error(EIO);
            return;
        }
        reply.ok();
    }

    fn fsync(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        // Process any pending online-tool requests before persisting.
        self.process_online_requests();

        if self.flush_file_handle(_fh).is_err() {
            reply.error(EIO);
            return;
        }
        match self.persist() {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(persist_errno(&error)),
        }
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.release_file_handle(fh) {
            Ok(()) => reply.ok(),
            Err(_) => reply.error(EIO),
        }
    }

    fn destroy(&mut self) {
        if self.dirty || self.service.had_unclean_shutdown() {
            let _ = self.flush_to_backing();
        }
    }

    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        _flags: u32,
        reply: ReplyWrite,
    ) {
        // Virtual (read-only) nodes cannot be copy destinations.
        if self.virt_files.contains_key(&ino_out) || self.virt_dirs.contains_key(&ino_out) {
            reply.error(libc::EROFS);
            return;
        }

        // Read the requested range from the source handle.
        let src_data = match self.open_files.get(&fh_in) {
            Some(handle) => {
                let start = offset_in.max(0) as usize;
                let end = (start.saturating_add(len as usize)).min(handle.data.len());
                if start >= handle.data.len() {
                    Vec::new()
                } else {
                    handle.data[start..end].to_vec()
                }
            }
            None => {
                // Source handle missing — try reading from a virtual file node.
                if let Some(vf) = self.virt_files.get(&ino_in) {
                    let start = offset_in.max(0) as usize;
                    let end = (start.saturating_add(len as usize)).min(vf.bytes.len());
                    if start >= vf.bytes.len() {
                        Vec::new()
                    } else {
                        vf.bytes[start..end].to_vec()
                    }
                } else {
                    reply.error(libc::EBADF);
                    return;
                }
            }
        };

        if src_data.is_empty() {
            reply.written(0);
            return;
        }

        // Write the range into the destination handle.
        match self.open_files.get_mut(&fh_out) {
            Some(handle) => {
                let start = offset_out.max(0) as usize;
                let end = start.saturating_add(src_data.len());
                if end > handle.data.len() {
                    handle.data.resize(end, 0);
                }
                handle.data[start..end].copy_from_slice(&src_data);
                self.dirty = true;
                reply.written(src_data.len() as u32);
            }
            None => {
                reply.error(libc::EBADF);
            }
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        // ── .snapshots/ root ──────────────────────────────────────────────────
        if ino == SNAPSHOTS_DIR_INO {
            let mut entries: Vec<(u64, FileType, String)> = vec![
                (SNAPSHOTS_DIR_INO, FileType::Directory, ".".to_string()),
                (ROOT_INO, FileType::Directory, "..".to_string()),
            ];
            let snapshots: Vec<_> = self.service.snapshots().to_vec();
            for snap in &snapshots {
                let dir_name = format!("{}-{}", snap.id, snap.name);
                entries.push((SNAP_SUBDIR_BASE + snap.id, FileType::Directory, dir_name));
            }
            for (index, (e_ino, ft, name)) in entries.into_iter().enumerate().skip(offset as usize)
            {
                if reply.add(e_ino, (index + 1) as i64, ft, name) {
                    break;
                }
            }
            reply.ok();
            return;
        }

        // ── Snapshot root dir (.snapshots/snap-N-name/) or deeper snapshot virt_dir ──
        let snapshot_info: Option<(u64, Timestamp)> =
            if let Some(info) = self.snapshot_for_subdir_ino(ino) {
                Some(info)
            } else if let Some(d) = self.virt_dirs.get(&ino) {
                Some((d.snapshot_id, d.modified_at))
            } else {
                None
            };

        if let Some((snap_id, snap_ts)) = snapshot_info {
            // Determine parent INO for ".." entry.
            let parent_ino = if self.snapshot_for_subdir_ino(ino).is_some() {
                SNAPSHOTS_DIR_INO
            } else {
                // Deeper virt_dir: approximate parent as the snapshot root dir.
                SNAP_SUBDIR_BASE + snap_id
            };

            // Get the fs_path this virtual dir mirrors in the snapshot.
            let fs_path = self
                .fs_path_for_virt_dir(ino)
                .unwrap_or_else(|| "/".to_string());

            // Clone snapshot paths before mutating self.
            let snap_paths: Vec<String> = self
                .service
                .snapshots()
                .iter()
                .find(|s| s.id == snap_id)
                .map(|s| s.paths.clone())
                .unwrap_or_default();

            let children = self.snapshot_children(&snap_paths, &fs_path);

            let mut entries: Vec<(u64, FileType, String)> = vec![
                (ino, FileType::Directory, ".".to_string()),
                (parent_ino, FileType::Directory, "..".to_string()),
            ];

            for (name, child_fs_path, is_dir) in children {
                let child_ino = if is_dir {
                    let key = VirtKey::SnapDir {
                        snapshot_id: snap_id,
                        fs_path: child_fs_path.clone(),
                    };
                    let dir = VirtDir {
                        snapshot_id: snap_id,
                        fs_path: child_fs_path,
                        modified_at: snap_ts,
                    };
                    self.get_or_create_virt_dir(key, dir)
                } else {
                    let bytes = self
                        .service
                        .version_bytes_at(&child_fs_path, snap_ts)
                        .unwrap_or_default();
                    let key = VirtKey::SnapFile {
                        snapshot_id: snap_id,
                        fs_path: child_fs_path,
                    };
                    self.get_or_create_virt_file(
                        key,
                        VirtFile {
                            bytes,
                            modified_at: snap_ts,
                        },
                    )
                };
                let ft = if is_dir {
                    FileType::Directory
                } else {
                    FileType::RegularFile
                };
                entries.push((child_ino, ft, name));
            }

            for (index, (e_ino, ft, name)) in entries.into_iter().enumerate().skip(offset as usize)
            {
                if reply.add(e_ino, (index + 1) as i64, ft, name) {
                    break;
                }
            }
            reply.ok();
            return;
        }

        // ── Normal real-filesystem readdir ────────────────────────────────────
        let mut entries = self.directory_entries(ino);
        if entries.is_empty() {
            reply.error(ENOENT);
            return;
        }
        // Inject the virtual `.snapshots/` entry when listing the root directory.
        if ino == ROOT_INO {
            entries.push((
                SNAPSHOTS_DIR_INO,
                FileType::Directory,
                ".snapshots".to_string(),
            ));
        }
        for (index, (entry_ino, kind, name)) in
            entries.into_iter().enumerate().skip(offset as usize)
        {
            if reply.add(entry_ino, (index + 1) as i64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

/// RW FUSE mount of an ODF-native image file.
///
/// End-to-end crash-consistent mount path:
///
/// 1. Opens `image_path` as a [`crate::storage::block_device::FileImageDevice`]
///    in read-write mode.
/// 2. Replays any pending journal transactions left over from a
///    previous interrupted persist via
///    [`crate::storage::ondisk::journaled::recover_pending_transactions`].
/// 3. Hydrates a [`CoreFsService`] from the on-disk state through
///    [`crate::storage::ondisk::native::load_state_native`].
/// 4. Writes a dirty marker (unclean_shutdown flag) so a crash before
///    unmount is visible to `fsck-odf` on next boot.
/// 5. Enters the FUSE event loop.  Every sync / fsync / unmount
///    triggers [`crate::storage::ondisk::native::save_state_native_incremental`],
///    rewriting only the inode slots that changed and bumping the
///    superblock generation atomically through the journal.
///
/// This is the ODF-native counterpart of [`mount_image_rw`].  Unlike
/// the legacy `volume_image` RW mount, every persist here:
///
/// * is **crash-consistent** — the commit record lands before any
///   metadata write is applied, so a power loss at any point either
///   preserves the prior generation or rolls forward to the new one
///   on next mount;
/// * is **incremental** — unchanged inodes stay untouched, so a
///   100 000-file volume where one file changes writes O(1) slots
///   instead of O(N);
/// * goes through the same `save_state_native_incremental` path that
///   has been covered by section-C resilience and stress tests.
pub fn mount_odf_image_rw(
    image_path: impl AsRef<Path>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let image_path = image_path.as_ref();
    let mount_point = mount_point.as_ref();
    let mut mount = CoreFsFuseMountRw::open_odf_session(image_path.to_path_buf())?;
    mount.start_ctl_listener(mount_point);
    let fs_name = format!("corefs-odf:{}", mount.service.volume_name());

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RW,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to RW-mount ODF image {} on {}: {error}",
            image_path.display(),
            mount_point.display()
        ))
    })
}

pub fn mount_image_rw(
    image_path: impl AsRef<Path>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let image_path = image_path.as_ref();
    let mount_point = mount_point.as_ref();
    let service = CoreFsService::load_image_from_path(image_path)?;
    let fs_name = format!("corefs:{}", service.volume_name());
    let mut mount = CoreFsFuseMountRw::open_session(service, image_path.to_path_buf())?;
    mount.start_ctl_listener(mount_point);

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RW,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount CoreFS image {} on {}: {error}",
            image_path.display(),
            mount_point.display()
        ))
    })
}

// ── Block-device mount helpers ──────────────────────────────────────────────

/// Mounts a CoreFS volume from a [`crate::storage::block_device::BlockDevice`] read-write via FUSE.
///
/// The device is loaded into memory, served through the FUSE RW stack,
/// and flushed directly back to the device on sync/unmount — no temporary
/// image file is created.
pub fn mount_device_rw(
    mut device: Box<dyn crate::storage::block_device::BlockDevice>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let mount_point = mount_point.as_ref();
    let state = crate::storage::volume_image::load_from_device(device.as_ref())?;
    let mut service = CoreFsService::from_persisted_state(state);
    if service.has_pending_wal() {
        service.recover_pending_wal()?;
        let recovered_state = service.persisted_state();
        crate::storage::volume_image::save_to_device(device.as_mut(), &recovered_state)?;
    }

    // Write an unclean-shutdown marker directly to the device.
    service.mark_unclean_shutdown();
    let dirty_state = service.persisted_state();
    crate::storage::volume_image::save_to_device(device.as_mut(), &dirty_state)?;

    let fs_name = format!("corefs:{}", service.volume_name());
    let mount = CoreFsFuseMountRw::open_device_session(service, device)?;

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RW,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount CoreFS device on {}: {error}",
            mount_point.display()
        ))
    })
}

/// Formats a [`crate::storage::block_device::BlockDevice`] with a new empty CoreFS volume.
pub fn format_device(
    device: &mut dyn crate::storage::block_device::BlockDevice,
    config: crate::config::CoreFsConfig,
) -> CoreFsResult<()> {
    let service = CoreFsService::format(config);
    let state = service.persisted_state();
    crate::storage::volume_image::save_to_device(device, &state)
}

// ── statfs helpers ───────────────────────────────────────────────────────────

/// Logical block size reported to the kernel.
const FUSE_BLOCK_SIZE: u32 = 4096;
/// Virtual total capacity: 1 GiB expressed in 4 KiB blocks.
const FUSE_TOTAL_BLOCKS: u64 = 1024 * 1024 * 1024 / FUSE_BLOCK_SIZE as u64; // 262 144

fn fuse_total_blocks() -> u64 {
    FUSE_TOTAL_BLOCKS
}

/// Compute used blocks from the in-memory node cache and return free blocks.
fn fuse_free_blocks(nodes: &HashMap<u64, FuseNode>) -> u64 {
    let used_bytes: u64 = nodes.values().map(|n| n.data.len() as u64).sum();
    let used_blocks = used_bytes.div_ceil(FUSE_BLOCK_SIZE as u64);
    FUSE_TOTAL_BLOCKS.saturating_sub(used_blocks)
}

/// Estimate free inode slots against the same virtual capacity.
fn fuse_free_inodes(active: usize) -> u64 {
    FUSE_TOTAL_BLOCKS.saturating_sub(active as u64)
}

fn fuse_capacity_blocks(image_path: &Path, nodes: &HashMap<u64, FuseNode>) -> (u64, u64) {
    let used_bytes: u64 = nodes.values().map(|n| n.data.len() as u64).sum();
    let used_blocks = used_bytes.div_ceil(FUSE_BLOCK_SIZE as u64);
    let fallback_free = FUSE_TOTAL_BLOCKS.saturating_sub(used_blocks);

    let Some(backing_free_bytes) = backing_store_free_bytes(image_path) else {
        return (FUSE_TOTAL_BLOCKS, fallback_free);
    };
    let current_image_size = std::fs::metadata(image_path)
        .map(|meta| meta.len())
        .unwrap_or(0);

    // We persist atomically via sibling tmp file + rename, so growth is limited
    // by free host space minus the current on-disk image footprint.
    let writable_growth_bytes = backing_free_bytes.saturating_sub(current_image_size);
    let writable_growth_blocks = writable_growth_bytes / FUSE_BLOCK_SIZE as u64;
    let total_blocks = used_blocks
        .saturating_add(writable_growth_blocks)
        .min(FUSE_TOTAL_BLOCKS);
    let free_blocks = total_blocks.saturating_sub(used_blocks);

    (total_blocks, free_blocks)
}

fn backing_store_free_bytes(image_path: &Path) -> Option<u64> {
    let probe = if image_path.exists() {
        image_path
    } else {
        image_path.parent().unwrap_or_else(|| Path::new("."))
    };
    let c_path = std::ffi::CString::new(probe.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

fn persist_errno(error: &CoreFsError) -> i32 {
    if error
        .to_string()
        .to_ascii_lowercase()
        .contains("no space left on device")
    {
        ENOSPC
    } else {
        EIO
    }
}

fn demo_fs() -> CoreFsResult<CoreFsService> {
    let mut fs = CoreFsService::format(crate::config::CoreFsConfig::default());
    fs.create_directory("/etc")?;
    fs.create_directory("/var")?;
    fs.create_file(
        "/etc/corefs.conf",
        b"volume=corefs\ncompression=on\nencryption=on\n",
        &["config".to_string(), "system".to_string()],
    )?;
    fs.create_file(
        "/var/readme.txt",
        b"CoreFS Linux FUSE image",
        &["docs".to_string()],
    )?;
    fs.create_symlink("/etc/corefs-current", "/etc/corefs.conf")?;
    Ok(fs)
}

fn parent_path(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or_default().to_string()
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn current_gid() -> u32 {
    unsafe { libc::getgid() }
}

#[cfg(test)]
#[path = "linux_fuse_tests.rs"]
mod tests;
