// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::config::CoreFsConfig;
use crate::domain::acl::{AclEntry, Principal};
use corefs_core::platform::Timestamp;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::{Snapshot, SnapshotInode};
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::runtime::RuntimeIntegrationBlueprint;
use crate::platform::tools::ToolRegistry;
use crate::services::compression::CompressionService;
use crate::services::encryption::EncryptionService;
use crate::services::hot_paths::{HotPathRecord, HotPathService};
use crate::services::indexing::IndexingService;
use crate::services::integrity::{IntegrityReport, IntegrityService};
use crate::services::journal::{JournalRecoverySummary, JournalRuntimeState, JournalService};
use crate::services::quota::QuotaService;
use crate::services::recovery::RecoveryService;
use crate::services::security::SecurityService;
use crate::services::sync::SyncService;
use crate::services::versioning::VersioningService;
use crate::storage::allocator::InodeAllocator;
use crate::storage::block_store::{
    AllocatorPolicy, BlockStore, CowStats, DefragmentationReport, FragmentationReport,
    FreeExtentRecord, HeatReallocationReport, OptimizationReport,
};
use crate::storage::catalog::Catalog;
use crate::storage::volume_image;
use crate::storage::volume_wal::{self, VolumeWal};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStats {
    pub files: usize,
    pub deleted_files: usize,
    pub versions: usize,
    pub snapshots: usize,
    pub journal_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminReport {
    pub volume: VolumeDescriptor,
    pub runtime: RuntimeIntegrationBlueprint,
    pub tools: ToolRegistry,
    pub stats: FsStats,
}

/// Report returned by `restore_snapshot`, describing what was restored and what was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRestoreReport {
    pub snapshot_id: u64,
    pub snapshot_name: String,
    /// Number of files successfully written back from snapshot data.
    pub restored_files: usize,
    /// Paths that could not be restored, with an error description each.
    pub skipped_paths: Vec<String>,
}

/// Report returned by `clone_tree`, describing what was cloned and what was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneTreeReport {
    pub cloned_files: usize,
    pub cloned_directories: usize,
    pub skipped_paths: Vec<String>,
}

/// Diff between two snapshots: which files were added, removed, modified, or unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDiff {
    /// Paths present in B but absent in A.
    pub added: Vec<String>,
    /// Paths present in A but absent in B.
    pub removed: Vec<String>,
    /// Paths present in both but with different content.
    pub modified: Vec<String>,
    /// Paths present in both with identical content.
    pub unchanged: Vec<String>,
}

/// Top-level copy-on-write health report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CowReport {
    /// Whether copy-on-write is enabled in the current configuration.
    pub copy_on_write_enabled: bool,
    /// Detailed sharing statistics from the block store.
    pub stats: CowStats,
    /// Number of snapshots currently held in memory.
    pub snapshot_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InodeDataLayout {
    pub data_offset: u64,
    pub length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataExtent {
    pub device_block: u64,
    pub data_offset: u64,
    pub inode_offset: usize,
    pub length: usize,
}

/// Re-export des plattformneutralen `PersistedState`-Aggregats aus
/// [`corefs_core::storage::persisted_state`]. Bestehende Konsumenten
/// erreichen den Typ unverändert über `crate::app::PersistedState`.
pub use corefs_core::storage::persisted_state::PersistedState;

#[derive(Debug)]
pub struct CoreFsService {
    config: CoreFsConfig,
    volume: VolumeDescriptor,
    allocator: InodeAllocator,
    catalog: Catalog,
    blocks: BlockStore,
    journal: JournalService,
    versioning: VersioningService,
    compression: CompressionService,
    encryption: EncryptionService,
    quota: QuotaService,
    recovery: RecoveryService,
    integrity: IntegrityService,
    indexing: IndexingService,
    hot_paths: HotPathService,
    security: SecurityService,
    sync: SyncService,
    clean_unmount: bool,
    pending_wal: Option<VolumeWal>,
    snapshots: Vec<Snapshot>,
    next_snapshot_id: u64,
    /// Inodes whose on-disk representation is out of sync with the last
    /// persisted image.  Populated by every mutation that touches block
    /// content or extent layout (create/write/extend/delete/truncate
    /// etc.), drained by
    /// [`CoreFsService::take_dirty_inodes`] as part of a checkpoint.
    ///
    /// Introduced in Phase 1e / P3b-b so the FUSE persist path can
    /// rebuild only the DATA regions that actually changed since the
    /// last checkpoint instead of re-materialising the full segment
    /// on every call.  Metadata-only mutations (owner, mode, rename,
    /// timestamps) do *not* dirty this set — they change catalog /
    /// inode segments but leave block content untouched.
    dirty_inodes: std::collections::HashSet<crate::domain::inode::InodeId>,
}

impl CoreFsService {
    pub fn format(config: CoreFsConfig) -> Self {
        let block_size = config.block_size;
        let volume = VolumeDescriptor::from_config(&config);
        let mut journal = JournalService::default();
        journal.record("format", "/", format!("volume={}", volume.name));

        let mut encryption = EncryptionService::default();
        if config.security.encryption_at_rest {
            encryption.derive_key_from(config.volume_name.as_bytes());
        }

        Self {
            config,
            volume,
            allocator: InodeAllocator::default(),
            catalog: Catalog::default(),
            blocks: BlockStore::with_block_size(block_size),
            journal,
            versioning: VersioningService::default(),
            compression: CompressionService,
            encryption,
            quota: QuotaService,
            recovery: RecoveryService::default(),
            integrity: IntegrityService,
            indexing: IndexingService,
            hot_paths: HotPathService::default(),
            security: SecurityService,
            sync: SyncService::default(),
            clean_unmount: true,
            pending_wal: None,
            snapshots: Vec::new(),
            next_snapshot_id: 0,
            dirty_inodes: std::collections::HashSet::new(),
        }
    }

    /// Returns and clears the set of inodes whose block content has
    /// mutated since the previous call.  Intended to be called once per
    /// checkpoint by the persist path — unchanged inodes can then be
    /// served from the cached DATA segment instead of rematerialised.
    pub fn take_dirty_inodes(&mut self) -> std::collections::HashSet<crate::domain::inode::InodeId> {
        core::mem::take(&mut self.dirty_inodes)
    }

    /// Returns `true` if at least one inode has dirty block content
    /// since the last `take_dirty_inodes` call.
    pub fn has_dirty_inodes(&self) -> bool {
        !self.dirty_inodes.is_empty()
    }

    /// Marks `inode` as having mutated block content.  Callers use this
    /// alongside direct mutations of [`BlockStore`] so the persist path
    /// can tell which DATA regions need a fresh build.
    fn mark_inode_dirty(&mut self, inode: crate::domain::inode::InodeId) {
        self.dirty_inodes.insert(inode);
    }

    pub fn create_file(&mut self, path: &str, bytes: &[u8], tags: &[String]) -> CoreFsResult<()> {
        let inode_id = self.allocator.allocate();
        self.create_file_with_inode(path, bytes, tags, inode_id)
    }

    pub fn create_file_with_inode(
        &mut self,
        path: &str,
        bytes: &[u8],
        tags: &[String],
        inode_id: InodeId,
    ) -> CoreFsResult<()> {
        validate_path(path)?;
        if self.catalog.get(path).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {path}"
            )));
        }

        // Enforce quota before allocating anything.
        let (cur_files, cur_bytes) = self.catalog.quota_stats();
        self.quota.check_stats(
            &self.config.quotas,
            cur_files,
            cur_bytes,
            1,
            bytes.len() as isize,
        )?;

        self.allocator.allocate_specific(inode_id);
        let mut metadata = FileMetadata::default();
        metadata.tags = tags.to_vec();
        metadata.content_class = self.indexing.classify_path(path);
        metadata.encrypted = self
            .security
            .mark_encrypted(self.config.security.encryption_at_rest);
        metadata.compressed = self.config.performance.compression_enabled;
        metadata.acl = vec![AclEntry::full_access(Principal::Role("system".to_string()))];

        // Determine whether to compress and prepare the bytes to store.
        let compress =
            self.config.performance.compression_enabled && self.compression.should_compress(bytes);
        metadata.compressed = compress;

        let mut inode = Inode::new(inode_id, InodeKind::File, path.to_string(), metadata);

        // Store version before compression (versions hold original uncompressed content).
        if self.config.versioning.keep_latest > 0 {
            self.versioning.store_version(path, bytes.to_vec());
            self.versioning
                .prune(path, self.config.versioning.keep_latest);
            if let Some(budget) = self.config.versioning.max_version_bytes {
                if self.versioning.total_bytes() > budget {
                    self.versioning.prune_to_budget(budget);
                }
            }
        }

        // Pipeline: compress → encrypt → store.  inode.size always tracks logical size.
        let mut stored_bytes = if compress {
            self.compression.compress(bytes)?
        } else {
            bytes.to_vec()
        };
        let encrypt = self.config.security.encryption_at_rest && self.encryption.has_key();
        if encrypt {
            stored_bytes = self.encryption.encrypt(&stored_bytes)?;
        }
        inode.metadata.encrypted = encrypt;
        self.blocks.write(inode_id, stored_bytes);
        inode.size = bytes.len(); // logical (uncompressed, unencrypted) size

        self.catalog.insert(inode);
        self.hot_paths.record_write(path, bytes.len());
        self.journal
            .record("create_file", path, format!("bytes={}", bytes.len()));
        self.dirty_inodes.insert(inode_id);
        self.auto_optimize_storage("create_file");
        Ok(())
    }

    pub fn create_directory(&mut self, path: &str) -> CoreFsResult<()> {
        let inode_id = self.allocator.allocate();
        self.create_directory_with_inode(path, inode_id)
    }

    pub fn create_directory_with_inode(
        &mut self,
        path: &str,
        inode_id: InodeId,
    ) -> CoreFsResult<()> {
        validate_path(path)?;
        if self.catalog.get(path).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {path}"
            )));
        }

        self.allocator.allocate_specific(inode_id);
        let inode = Inode::new(
            inode_id,
            InodeKind::Directory,
            path.to_string(),
            FileMetadata::default(),
        );
        self.catalog.insert(inode);
        self.hot_paths.record_metadata(path);
        self.journal.record("create_directory", path, "");
        // Directories don't carry DATA bytes but the block_records list
        // grows by one empty record — mark so the DATA segment layout
        // gets rebuilt (the new descriptor shifts subsequent offsets).
        self.dirty_inodes.insert(inode_id);
        Ok(())
    }

    pub fn create_symlink(&mut self, path: &str, target: &str) -> CoreFsResult<()> {
        validate_path(path)?;
        if self.catalog.get(path).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {path}"
            )));
        }

        let inode_id = self.allocator.allocate();
        let mut inode = Inode::new(
            inode_id,
            InodeKind::Symlink,
            path.to_string(),
            FileMetadata::default(),
        );
        inode.size = self.blocks.write(inode_id, target.as_bytes().to_vec());
        self.catalog.insert(inode);
        self.hot_paths.record_metadata(path);
        self.journal
            .record("create_symlink", path, format!("target={target}"));
        self.dirty_inodes.insert(inode_id);
        self.auto_optimize_storage("create_symlink");
        Ok(())
    }

    pub fn write_file(&mut self, path: &str, bytes: &[u8]) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;

        if inode.kind != InodeKind::File {
            return Err(CoreFsError::InvalidInput(format!(
                "writes are only supported for files: {path}"
            )));
        }

        // Enforce quota: byte delta = new_size - old_size (may be negative for shrinks).
        let byte_delta = bytes.len() as isize - inode.size as isize;
        if byte_delta > 0 {
            let (cur_files, cur_bytes) = self.catalog.quota_stats();
            self.quota
                .check_stats(&self.config.quotas, cur_files, cur_bytes, 0, byte_delta)?;
        }

        // Store version before compression (versions hold original content).
        self.versioning.store_version(path, bytes.to_vec());
        self.versioning
            .prune(path, self.config.versioning.keep_latest);
        if let Some(budget) = self.config.versioning.max_version_bytes {
            if self.versioning.total_bytes() > budget {
                self.versioning.prune_to_budget(budget);
            }
        }

        // Pipeline: compress → encrypt → store.
        let compress =
            self.config.performance.compression_enabled && self.compression.should_compress(bytes);
        let mut stored_bytes = if compress {
            self.compression.compress(bytes)?
        } else {
            bytes.to_vec()
        };
        let encrypt = self.config.security.encryption_at_rest && self.encryption.has_key();
        if encrypt {
            stored_bytes = self.encryption.encrypt(&stored_bytes)?;
        }

        let inode = self.catalog.get_mut(path).expect("path still exists");
        inode.touch_modified(); // content changed → mtime + ctime
        inode.metadata.compressed = compress;
        inode.metadata.encrypted = encrypt;
        inode.size = bytes.len(); // logical size
        let inode_id = inode.id;
        self.blocks.write(inode_id, stored_bytes);
        self.hot_paths.record_write(path, bytes.len());
        self.journal
            .record("write_file", path, format!("bytes={}", bytes.len()));
        self.dirty_inodes.insert(inode_id);
        self.auto_optimize_storage("write_file");
        Ok(())
    }

    /// Appends `extra` bytes to the file at `path` without replacing existing content.
    ///
    /// Uses `BlockStore::append_to_inode` for O(extra.len()) amortised growth when the
    /// blob is exclusively owned.  Callers must ensure sequential append semantics; for
    /// random writes use `write_file` instead.
    pub fn extend_file(&mut self, path: &str, extra: &[u8]) -> CoreFsResult<()> {
        // Phase 1: gather what we need before any mutation.
        let (inode_id, inode_kind, was_compressed, was_encrypted) = {
            let inode = self
                .catalog
                .get(path)
                .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
            (
                inode.id,
                inode.kind,
                inode.metadata.compressed,
                inode.metadata.encrypted,
            )
        };

        if inode_kind != InodeKind::File {
            return Err(CoreFsError::InvalidInput(format!(
                "extend is only supported for files: {path}"
            )));
        }

        // Phase 2: if the block was encrypted or compressed, materialise raw bytes
        // before appending — mixed formats are invalid for append_to_inode.
        if was_encrypted || was_compressed {
            let record = self
                .blocks
                .read(inode_id)
                .ok_or_else(|| CoreFsError::State(format!("missing data blocks for {path}")))?;
            let mut raw = record.bytes.clone();
            if was_encrypted {
                raw = self.encryption.decrypt(&raw)?;
            }
            if was_compressed {
                raw = self.compression.decompress(&raw)?;
            }
            self.blocks.write(inode_id, raw);
            let inode = self.catalog.get_mut(path).expect("path still exists");
            inode.metadata.compressed = false;
            inode.metadata.encrypted = false;
        }

        // Phase 3: append the new bytes (always uncompressed for extend).
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        inode.touch_modified(); // content changed → mtime + ctime
        inode.size = self.blocks.append_to_inode(inode_id, extra);
        self.hot_paths.record_write(path, extra.len());
        self.journal
            .record("extend_file", path, format!("extra_bytes={}", extra.len()));
        self.dirty_inodes.insert(inode_id);
        Ok(())
    }

    /// Reads a file's content, transparently decrypting and/or decompressing as needed.
    ///
    /// Block pipeline (reverse of write): decrypt → decompress.
    pub fn read_file(&self, path: &str) -> CoreFsResult<Vec<u8>> {
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        let record = self
            .blocks
            .read(inode.id)
            .ok_or_else(|| CoreFsError::State(format!("missing data blocks for {path}")))?;

        // Reverse pipeline: decrypt → decompress.
        let decrypted = if inode.metadata.encrypted {
            self.encryption.decrypt(&record.bytes)?
        } else {
            record.bytes.clone()
        };
        if inode.metadata.compressed {
            self.compression.decompress(&decrypted)
        } else {
            Ok(decrypted)
        }
    }

    pub fn delete_file(&mut self, path: &str, secure: bool) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .remove(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;

        if secure {
            let _ = self.recovery.recover(path);
            let _ = self.catalog.restore_deleted(path);
            let mut record = self
                .blocks
                .remove(inode.id)
                .ok_or_else(|| CoreFsError::State(format!("missing data blocks for {path}")))?;
            self.security.secure_delete_bytes(&mut record.bytes);
            self.allocator.release(inode.id);
            self.hot_paths.record_metadata(path);
            self.journal.record("secure_delete", path, "blocks_zeroed");
            self.auto_optimize_storage("secure_delete");
        } else {
            self.recovery.remember(inode.clone());
            self.catalog.move_to_deleted(inode.clone());
            self.hot_paths.record_metadata(path);
            self.journal.record("delete", path, "recoverable=true");
        }

        // Both secure and recoverable delete change the block_records
        // list (records either removed or moved into the deleted
        // section), so every downstream DATA segment offset shifts.
        // Mark the removed inode id so the persist path knows the
        // layout is no longer identical to the cached one.
        self.dirty_inodes.insert(inode.id);

        Ok(())
    }

    pub fn restore_file(&mut self, path: &str) -> CoreFsResult<()> {
        let inode = if let Some(inode) = self.recovery.recover(path) {
            let _ = self.catalog.restore_deleted(path);
            inode
        } else {
            self.catalog.restore_deleted(path).ok_or_else(|| {
                CoreFsError::NotFound(format!("recoverable path not found: {path}"))
            })?
        };

        let inode_id = inode.id;
        self.catalog.insert(inode);
        self.hot_paths.record_metadata(path);
        self.journal.record("restore", path, "");
        self.dirty_inodes.insert(inode_id);
        Ok(())
    }

    /// Shorthand for `create_snapshot_scoped(name, "/")` — captures the entire volume.
    pub fn create_snapshot(&mut self, name: &str) -> Snapshot {
        self.create_snapshot_scoped(name, "/")
    }

    /// Creates a named snapshot limited to `scope_root` and its descendants.
    ///
    /// Only paths that equal `scope_root` or begin with `scope_root + "/"` are
    /// included.  File content is captured uncompressed in `Snapshot.file_data`
    /// so the snapshot is self-contained and independent of subsequent block
    /// mutations.
    pub fn create_snapshot_scoped(&mut self, name: &str, scope_root: &str) -> Snapshot {
        self.next_snapshot_id += 1;
        let all_paths = self.catalog.list_paths();

        let paths: Vec<String> = if scope_root == "/" {
            all_paths
        } else {
            let prefix = format!("{scope_root}/");
            all_paths
                .into_iter()
                .filter(|p| p == scope_root || p.starts_with(&prefix))
                .collect()
        };

        // Capture uncompressed content for every regular file inside the scope
        // and full per-path metadata (including symlink targets).
        let mut file_data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut inodes: BTreeMap<String, SnapshotInode> = BTreeMap::new();
        for path in &paths {
            let Some(inode) = self.catalog.get(path) else {
                continue;
            };
            let kind = inode.kind;
            let snap_inode = SnapshotInode {
                kind,
                size: inode.size,
                created_at: inode.created_at,
                modified_at: inode.modified_at,
                changed_at: inode.changed_at,
                metadata: inode.metadata.clone(),
                symlink_target: None,
            };
            match kind {
                InodeKind::File => {
                    if let Ok(bytes) = self.read_file(path) {
                        file_data.insert(path.clone(), bytes);
                    }
                    inodes.insert(path.clone(), snap_inode);
                }
                InodeKind::Symlink => {
                    let target = self
                        .read_file(path)
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned());
                    inodes.insert(
                        path.clone(),
                        SnapshotInode {
                            symlink_target: target,
                            ..snap_inode
                        },
                    );
                }
                InodeKind::Directory => {
                    inodes.insert(path.clone(), snap_inode);
                }
            }
        }

        let snapshot = Snapshot {
            id: self.next_snapshot_id,
            name: name.to_string(),
            scope_root: scope_root.to_string(),
            created_at: Timestamp::now(),
            paths,
            file_data,
            inodes,
        };
        self.journal.record(
            "snapshot",
            "/",
            format!(
                "name={name} id={} files={}",
                snapshot.id,
                snapshot.file_data.len()
            ),
        );
        self.snapshots.push(snapshot.clone());
        snapshot
    }

    /// Removes the snapshot with the given `snapshot_id`.
    ///
    /// Returns `Err(NotFound)` when no snapshot with that id exists.
    pub fn delete_snapshot(&mut self, snapshot_id: u64) -> CoreFsResult<()> {
        let pos = self
            .snapshots
            .iter()
            .position(|s| s.id == snapshot_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {snapshot_id} not found")))?;
        let snapshot = self.snapshots.remove(pos);
        self.journal.record(
            "delete_snapshot",
            "/",
            format!("id={snapshot_id} name={}", snapshot.name),
        );
        Ok(())
    }

    /// Returns the historical metadata captured for `path` in snapshot
    /// `snapshot_id`, if the path existed at snapshot time.
    ///
    /// Lets callers inspect how an entry looked in the past — mode, uid, gid,
    /// timestamps, ACLs, tags, xattrs, symlink target — without performing a
    /// full restore.
    pub fn snapshot_inode(&self, snapshot_id: u64, path: &str) -> CoreFsResult<&SnapshotInode> {
        let snapshot = self
            .snapshots
            .iter()
            .find(|s| s.id == snapshot_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {snapshot_id} not found")))?;
        snapshot
            .inodes
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path {path} not in snapshot")))
    }

    /// Restores the filesystem to the state recorded in snapshot `snapshot_id`.
    ///
    /// For every entry captured in `snapshot.inodes` (ordered shallow-first):
    /// - Missing directories are recreated via `create_directory`.
    /// - Missing symlinks are recreated via `create_symlink` with the captured target.
    /// - Files are overwritten via `write_file` or recreated via `create_file`
    ///   from `snapshot.file_data`.
    /// - Captured metadata (mode, uid, gid, ACLs, tags, xattrs) and timestamps
    ///   are reapplied to the restored inode.
    ///
    /// Paths that cannot be written (e.g. quota exceeded) are reported in
    /// `SnapshotRestoreReport.skipped_paths` rather than aborting the whole restore.
    pub fn restore_snapshot(&mut self, snapshot_id: u64) -> CoreFsResult<SnapshotRestoreReport> {
        let snapshot = self
            .snapshots
            .iter()
            .find(|s| s.id == snapshot_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {snapshot_id} not found")))?
            .clone();

        let mut restored_files = 0usize;
        let mut skipped_paths: Vec<String> = Vec::new();

        // Ordered iteration (BTreeMap) → parent directories before their children.
        for (path, snap_inode) in &snapshot.inodes {
            let current_kind = self.catalog.get(path).map(|i| i.kind);
            let ensure_result: CoreFsResult<()> = match snap_inode.kind {
                InodeKind::Directory => match current_kind {
                    Some(InodeKind::Directory) => Ok(()),
                    Some(_) => Err(CoreFsError::InvalidInput(format!(
                        "path exists with different kind: {path}"
                    ))),
                    None => self.create_directory(path),
                },
                InodeKind::Symlink => {
                    let target = snap_inode.symlink_target.as_deref().unwrap_or("");
                    match current_kind {
                        Some(InodeKind::Symlink) => Ok(()),
                        Some(_) => Err(CoreFsError::InvalidInput(format!(
                            "path exists with different kind: {path}"
                        ))),
                        None => self.create_symlink(path, target),
                    }
                }
                InodeKind::File => {
                    let bytes = snapshot.file_data.get(path).cloned().unwrap_or_default();
                    match current_kind {
                        Some(InodeKind::File) => self.write_file(path, &bytes),
                        Some(_) => Err(CoreFsError::InvalidInput(format!(
                            "path exists with different kind: {path}"
                        ))),
                        None => self.create_file(path, &bytes, &[]),
                    }
                }
            };

            match ensure_result {
                Ok(()) => {
                    // Reapply captured metadata and timestamps.
                    if let Some(inode) = self.catalog.get_mut(path) {
                        inode.metadata = snap_inode.metadata.clone();
                        inode.created_at = snap_inode.created_at;
                        inode.modified_at = snap_inode.modified_at;
                        inode.changed_at = snap_inode.changed_at;
                    }
                    if snap_inode.kind == InodeKind::File {
                        restored_files += 1;
                    }
                }
                Err(e) => skipped_paths.push(format!("{path}: {e}")),
            }
        }

        self.journal.record(
            "restore_snapshot",
            "/",
            format!(
                "id={snapshot_id} restored={restored_files} skipped={}",
                skipped_paths.len()
            ),
        );

        Ok(SnapshotRestoreReport {
            snapshot_id,
            snapshot_name: snapshot.name.clone(),
            restored_files,
            skipped_paths,
        })
    }

    /// Creates a copy-on-write clone of the file at `from` under the new path `to`.
    ///
    /// The clone initially shares the same underlying blob as the source — no data
    /// is physically copied until one of the two inodes is written.  The next
    /// `write_file` or `extend_file` call on either path will materialise an
    /// independent copy via `BlockStore`'s reference-count tracking.
    ///
    /// Returns `Err(NotFound)` when `from` does not exist, `Err(AlreadyExists)` when
    /// `to` already exists, and `Err(InvalidInput)` when `from` is not a regular file.
    /// Creates a copy of the file at `from` under the new path `to`.
    ///
    /// When `config.performance.copy_on_write` is **enabled** (default) the clone
    /// initially shares the underlying blob — no data is physically copied until
    /// one of the two inodes is written.  When CoW is **disabled** the file is
    /// eagerly copied (full read + create) so that no blob sharing occurs.
    pub fn clone_file(&mut self, from: &str, to: &str) -> CoreFsResult<()> {
        validate_path(to)?;

        let source = self
            .catalog
            .get(from)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {from}")))?;

        if source.kind != InodeKind::File {
            return Err(CoreFsError::InvalidInput(format!(
                "source is not a regular file: {from}"
            )));
        }
        if self.catalog.get(to).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "target already exists: {to}"
            )));
        }

        if self.config.performance.copy_on_write {
            // Lazy CoW path: share the blob (ref_count++).
            let source_id = source.id;
            let source_size = source.size;
            let source_meta = source.metadata.clone();

            let (cur_files, cur_bytes) = self.catalog.quota_stats();
            self.quota.check_stats(
                &self.config.quotas,
                cur_files,
                cur_bytes,
                1,
                source_size as isize,
            )?;

            let target_id = self.allocator.allocate();
            if !self.blocks.clone_for_inode(source_id, target_id) {
                self.allocator.release(target_id);
                return Err(CoreFsError::State(format!(
                    "source has no allocated data block: {from}"
                )));
            }

            let mut target = Inode::new(target_id, InodeKind::File, to.to_string(), source_meta);
            target.size = source_size;
            self.catalog.insert(target);
            self.hot_paths.record_write(to, source_size);
            self.journal
                .record("clone_file", from, format!("to={to} cow=true"));
        } else {
            // Eager copy path: read source bytes, create independent file.
            let source_tags = source.metadata.tags.clone();
            let bytes = self.read_file(from)?;
            self.create_file(to, &bytes, &source_tags)?;
            self.journal
                .record("clone_file", from, format!("to={to} cow=false"));
        }

        Ok(())
    }

    /// Recursively clones a directory tree from `from` to `to` using CoW
    /// semantics for each regular file (or eager copy when CoW is disabled).
    ///
    /// Directories are created in order from shallowest to deepest so that
    /// parent directories exist before their children.  Symlinks are
    /// re-created with the same target path.
    pub fn clone_tree(&mut self, from: &str, to: &str) -> CoreFsResult<CloneTreeReport> {
        validate_path(to)?;

        let source_kind = self
            .catalog
            .get(from)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {from}")))?
            .kind;

        if source_kind != InodeKind::Directory {
            return Err(CoreFsError::InvalidInput(format!(
                "source is not a directory: {from}"
            )));
        }
        if self.catalog.get(to).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "target already exists: {to}"
            )));
        }

        // Collect all paths under `from` (including `from` itself).
        let prefix = format!("{from}/");
        let mut paths: Vec<String> = self
            .catalog
            .list_paths()
            .into_iter()
            .filter(|p| p == from || p.starts_with(&prefix))
            .collect();
        paths.sort(); // Shallowest directories first → parents before children.

        let mut cloned_files = 0usize;
        let mut cloned_directories = 0usize;
        let mut skipped_paths: Vec<String> = Vec::new();

        for path in &paths {
            let target_path = format!("{to}{}", &path[from.len()..]);
            let kind = self.catalog.get(path).map(|i| i.kind);

            match kind {
                Some(InodeKind::Directory) => match self.create_directory(&target_path) {
                    Ok(()) => cloned_directories += 1,
                    Err(e) => skipped_paths.push(format!("{target_path}: {e}")),
                },
                Some(InodeKind::File) => match self.clone_file(path, &target_path) {
                    Ok(()) => cloned_files += 1,
                    Err(e) => skipped_paths.push(format!("{target_path}: {e}")),
                },
                Some(InodeKind::Symlink) => {
                    // Symlinks store the target path as raw bytes in the block store.
                    if let Ok(target_bytes) = self.read_file(path) {
                        let link_target = String::from_utf8_lossy(&target_bytes);
                        match self.create_symlink(&target_path, &link_target) {
                            Ok(()) => cloned_files += 1,
                            Err(e) => skipped_paths.push(format!("{target_path}: {e}")),
                        }
                    } else {
                        skipped_paths.push(format!("{target_path}: unable to read symlink target"));
                    }
                }
                None => skipped_paths.push(format!("{path}: disappeared during clone")),
            }
        }

        self.journal.record(
            "clone_tree",
            from,
            format!(
                "to={to} files={cloned_files} dirs={cloned_directories} skipped={}",
                skipped_paths.len()
            ),
        );

        Ok(CloneTreeReport {
            cloned_files,
            cloned_directories,
            skipped_paths,
        })
    }

    /// Compares two snapshots and returns which files were added, removed,
    /// modified, or unchanged between snapshot `a_id` (older) and `b_id` (newer).
    ///
    /// Only `file_data` entries are compared; directory and symlink paths are
    /// not included in the diff.
    pub fn diff_snapshots(&self, a_id: u64, b_id: u64) -> CoreFsResult<SnapshotDiff> {
        let a = self
            .snapshots
            .iter()
            .find(|s| s.id == a_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {a_id} not found")))?;
        let b = self
            .snapshots
            .iter()
            .find(|s| s.id == b_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {b_id} not found")))?;

        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut modified = Vec::new();
        let mut unchanged = Vec::new();

        for path in a.file_data.keys() {
            if !b.file_data.contains_key(path) {
                removed.push(path.clone());
            }
        }

        for (path, b_bytes) in &b.file_data {
            match a.file_data.get(path) {
                None => added.push(path.clone()),
                Some(a_bytes) if a_bytes == b_bytes => unchanged.push(path.clone()),
                Some(_) => modified.push(path.clone()),
            }
        }

        Ok(SnapshotDiff {
            added,
            removed,
            modified,
            unchanged,
        })
    }

    /// Permanently deletes a soft-deleted file, releasing its blocks.
    ///
    /// Unlike `delete_file(…, secure=false)` which keeps the blocks alive for
    /// potential recovery, `expunge_file` decrements the blob reference count and
    /// frees the device extent.  If the blob is still referenced by another inode
    /// (e.g. a CoW clone), only the reference is dropped — the data itself is
    /// preserved for the remaining owner.
    ///
    /// Returns `Err(NotFound)` when `path` is not in the soft-deleted catalog.
    pub fn expunge_file(&mut self, path: &str) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .remove_from_deleted(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("no soft-deleted file at: {path}")))?;

        self.recovery.forget(path);
        // Release blocks: decrements blob ref_count; frees blob only if last reference.
        let _ = self.blocks.remove(inode.id);
        self.allocator.release(inode.id);
        self.journal
            .record("expunge", path, "permanent_delete=true");
        Ok(())
    }

    /// Returns a copy-on-write health report for monitoring and diagnostics.
    pub fn cow_report(&self) -> CowReport {
        CowReport {
            copy_on_write_enabled: self.config.performance.copy_on_write,
            stats: self.blocks.cow_stats(),
            snapshot_count: self.snapshots.len(),
        }
    }

    /// Runs an explicit deduplication pass over the block store.
    ///
    /// Returns `Err(PolicyViolation)` when `config.performance.deduplication_enabled`
    /// is `false` — callers must opt in via configuration.
    pub fn run_dedup(&mut self) -> CoreFsResult<crate::storage::block_store::DedupePassReport> {
        if !self.config.performance.deduplication_enabled {
            return Err(CoreFsError::PolicyViolation(
                "deduplication is disabled in configuration".to_string(),
            ));
        }
        let report = self.blocks.dedup_pass();
        self.journal.record(
            "dedup_pass",
            "/",
            format!(
                "scanned={} consolidated={} reclaimed={} collisions={} ref_mismatches={}",
                report.blobs_scanned,
                report.duplicates_consolidated,
                report.bytes_reclaimed,
                report.hash_collisions,
                report.ref_count_mismatches,
            ),
        );
        Ok(report)
    }

    /// Runs a comprehensive in-memory consistency check (deep fsck).
    pub fn fsck(&self) -> crate::services::integrity::FsckReport {
        self.integrity.deep_fsck(
            &self.catalog,
            &self.blocks,
            &self.compression,
            &self.encryption,
        )
    }

    pub fn save_image_to_path(&self, path: impl AsRef<Path>) -> CoreFsResult<()> {
        let path = path.as_ref();
        let state = self.persisted_state();
        let block_bytes = self.read_all_block_bytes();
        // Write to a sibling temp file first, then rename atomically so a crash
        // during the write never leaves a partially-written image behind.
        let tmp_name = path
            .file_name()
            .map(|n| format!("{}.tmp", n.to_string_lossy()))
            .unwrap_or_else(|| "corefs.img.tmp".to_string());
        let tmp_path = path.with_file_name(tmp_name);
        volume_image::save_volume_image_with_bytes(&tmp_path, &state, &block_bytes)?;
        std::fs::rename(&tmp_path, path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp_path);
            CoreFsError::State(format!("atomic rename of image failed: {error}"))
        })
    }

    pub fn load_image_from_path(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let path = path.as_ref();
        let (state, block_bytes) = volume_image::load_volume_image_with_bytes(path)?;
        let mut service = Self::from_persisted_state(state);
        // Write file bytes back into the compat device so read_file() works.
        for (inode_id, bytes) in block_bytes {
            service.blocks.write(inode_id, bytes);
        }
        if service.has_pending_wal() {
            service.recover_pending_wal()?;
            service.save_image_to_path(path)?;
        }
        Ok(service)
    }

    pub fn scrub(&self) -> IntegrityReport {
        let inode_ids = self
            .catalog
            .list_paths()
            .into_iter()
            .filter_map(|path| self.catalog.get(&path).map(|inode| inode.id))
            .filter(|inode_id| self.blocks.contains(*inode_id));
        self.integrity.scrub(inode_ids, &self.blocks)
    }

    pub fn mark_synced(&mut self, path: &str, target: &str) -> CoreFsResult<()> {
        if self.catalog.get(path).is_none() {
            return Err(CoreFsError::NotFound(format!("path not found: {path}")));
        }
        self.sync.mark_synced(path, target);
        self.journal
            .record("sync", path, format!("target={target}"));
        Ok(())
    }

    pub fn export_state(&self) -> PersistedState {
        self.persisted_state()
    }

    pub fn defragment(&mut self) -> DefragmentationReport {
        let report = self.blocks.defragment();
        self.journal.record(
            "defragment",
            "/",
            format!(
                "moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                report.moved_entries, report.reclaimed_gaps, report.final_device_blocks
            ),
        );
        report
    }

    pub fn fragmentation_report(&self) -> FragmentationReport {
        self.blocks.fragmentation_report()
    }

    pub fn optimize_storage(&mut self) -> OptimizationReport {
        let prioritized = self.prioritized_hot_inodes(8);
        let report = self.blocks.optimize_with_priorities(&prioritized);
        if let Some(heat_reallocation) = &report.heat_reallocation {
            self.record_heat_reallocation_journal("optimize_storage", heat_reallocation);
        } else if let Some(defragmentation) = &report.defragmentation {
            self.journal.record(
                "optimize_storage",
                "/",
                format!(
                    "fragmentation={} moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                    report.before.fragmentation_percent,
                    defragmentation.moved_entries,
                    defragmentation.reclaimed_gaps,
                    defragmentation.final_device_blocks
                ),
            );
        } else {
            self.journal.record(
                "optimize_storage",
                "/",
                format!(
                    "fragmentation={} action=skipped",
                    report.before.fragmentation_percent
                ),
            );
        }
        report
    }

    pub fn set_allocator_policy(&mut self, policy: AllocatorPolicy) {
        self.blocks.set_allocator_policy(policy);
    }

    pub fn begin_write_transaction(&mut self, label: &str) -> u64 {
        self.journal.begin_transaction(label)
    }

    pub fn has_pending_transaction(&self) -> bool {
        self.journal.has_pending_transaction()
    }

    pub fn commit_write_transaction(&mut self) -> bool {
        self.journal.commit_transaction().is_some()
    }

    pub fn mark_unclean_shutdown(&mut self) {
        self.clean_unmount = false;
        self.journal.mark_unclean_shutdown();
    }

    pub fn mark_clean_shutdown(&mut self) {
        self.clean_unmount = true;
        self.journal.mark_clean_shutdown();
    }

    pub fn recover_runtime_state(&mut self) -> JournalRecoverySummary {
        let summary = self.journal.recover_on_load();
        if summary.cleared_unclean_shutdown || summary.aborted_pending_transaction {
            self.clean_unmount = true;
        }
        summary
    }

    pub fn had_unclean_shutdown(&self) -> bool {
        !self.clean_unmount
    }

    pub fn block_size(&self) -> usize {
        self.volume.block_size
    }

    pub fn data_layout_for_inode(&self, inode_id: InodeId) -> Option<InodeDataLayout> {
        self.blocks.read(inode_id).map(|record| InodeDataLayout {
            data_offset: record.device_block.saturating_mul(self.block_size() as u64),
            length: record.bytes.len(),
        })
    }

    pub fn data_extents_for_inode(&self, inode_id: InodeId) -> Vec<DataExtent> {
        let Some(layout) = self.data_layout_for_inode(inode_id) else {
            return Vec::new();
        };
        let block_size = self.block_size().max(1);
        let mut extents = Vec::new();
        let mut consumed = 0usize;

        while consumed < layout.length {
            let data_offset = layout.data_offset.saturating_add(consumed as u64);
            let extent_len = (layout.length - consumed).min(block_size);
            extents.push(DataExtent {
                device_block: data_offset / block_size as u64,
                data_offset,
                inode_offset: consumed,
                length: extent_len,
            });
            consumed += extent_len;
        }

        if extents.is_empty() && layout.length == 0 {
            extents.push(DataExtent {
                device_block: layout.data_offset / block_size as u64,
                data_offset: layout.data_offset,
                inode_offset: 0,
                length: 0,
            });
        }

        extents
    }

    pub fn path_for_inode(&self, inode_id: InodeId) -> Option<String> {
        self.catalog
            .inode_by_id(inode_id)
            .map(|inode| inode.path.clone())
    }

    pub fn pending_wal(&self) -> Option<&VolumeWal> {
        self.pending_wal.as_ref()
    }

    pub fn has_pending_wal(&self) -> bool {
        self.pending_wal.is_some()
    }

    pub fn set_pending_wal(&mut self, wal: VolumeWal) {
        self.pending_wal = Some(wal);
    }

    pub fn update_pending_wal(
        &mut self,
        mutator: impl FnOnce(&mut VolumeWal) -> CoreFsResult<()>,
    ) -> CoreFsResult<()> {
        let wal = self
            .pending_wal
            .as_mut()
            .ok_or_else(|| CoreFsError::State("missing pending WAL".to_string()))?;
        mutator(wal)
    }

    pub fn clear_pending_wal(&mut self) {
        self.pending_wal = None;
    }

    pub fn recover_pending_wal(&mut self) -> CoreFsResult<()> {
        let Some(wal) = self.pending_wal.clone() else {
            return Ok(());
        };
        self.begin_write_transaction(&wal.label);
        for operation in &wal.operations {
            volume_wal::apply_operation(self, operation)?;
        }
        self.commit_write_transaction();
        self.clear_pending_wal();
        self.mark_clean_shutdown();
        Ok(())
    }

    pub fn stats(&self) -> FsStats {
        let versions = self
            .catalog
            .list_paths()
            .into_iter()
            .map(|path| self.versioning.list_versions(&path).len())
            .sum();

        FsStats {
            files: self.catalog.list_paths().len(),
            deleted_files: self.catalog.list_deleted_paths().len(),
            versions,
            snapshots: self.snapshots.len(),
            journal_entries: self.journal.entries().len(),
        }
    }

    pub fn admin_report(&self) -> AdminReport {
        AdminReport {
            volume: self.volume.clone(),
            runtime: RuntimeIntegrationBlueprint::default(),
            tools: ToolRegistry::default(),
            stats: self.stats(),
        }
    }

    pub fn list_paths(&self) -> Vec<String> {
        self.catalog.list_paths()
    }

    pub fn recoverable_paths(&self) -> Vec<String> {
        self.recovery.recoverable_paths()
    }

    pub fn snapshot_names(&self) -> Vec<String> {
        self.snapshots
            .iter()
            .map(|snapshot| snapshot.name.clone())
            .collect()
    }

    /// All snapshots in creation order.
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }

    /// Content of `path` at the latest version at or before `at`.
    /// Returns `None` if no version exists at or before that instant.
    pub fn version_bytes_at(&self, path: &str, at: Timestamp) -> Option<Vec<u8>> {
        self.versioning
            .version_at_or_before(path, at)
            .map(|v| v.bytes.clone())
    }

    /// Content of `path` at a specific version ID.
    pub fn version_bytes_by_id(&self, path: &str, version_id: u64) -> Option<Vec<u8>> {
        self.versioning
            .version_by_id(path, version_id)
            .map(|v| v.bytes.clone())
    }

    /// `(version_id, created_at)` pairs for all versions of `path`, oldest first.
    pub fn file_version_ids(&self, path: &str) -> Vec<(u64, Timestamp)> {
        self.versioning
            .list_versions(path)
            .iter()
            .map(|v| (v.version_id, v.created_at))
            .collect()
    }

    pub fn volume_name(&self) -> &str {
        &self.volume.name
    }

    pub fn journal_entries(&self) -> usize {
        self.journal.entries().len()
    }

    pub fn synced_paths(&self) -> usize {
        self.sync.statuses().len()
    }

    pub fn inode_for_path(&self, path: &str) -> Option<InodeId> {
        self.catalog.get(path).map(|inode| inode.id)
    }

    pub fn get_inode(&self, path: &str) -> Option<&Inode> {
        self.catalog.get(path)
    }

    /// Updates the POSIX owner (uid/gid) of a path.  Either value can be
    /// `None` to leave it unchanged (matching `chown`/`lchown` semantics).
    ///
    /// Bumps `changed_at` (ctime) but not `modified_at` (mtime).  Does not
    /// create a new file version — versioning captures content history only.
    pub fn set_owner(
        &mut self,
        path: &str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        if let Some(u) = uid {
            inode.metadata.uid = u;
        }
        if let Some(g) = gid {
            inode.metadata.gid = g;
        }
        inode.touch_changed();
        Ok(())
    }

    /// Updates the POSIX mode bits of a path (permissions only; `0o7777` mask
    /// — type bits are derived from `InodeKind`).
    ///
    /// Bumps `changed_at` (ctime) but not `modified_at` (mtime).  Does not
    /// create a new file version.
    pub fn set_mode(&mut self, path: &str, mode: u32) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        inode.metadata.mode = mode & 0o7777;
        inode.touch_changed();
        Ok(())
    }

    /// Rename `from` to `to`, cascading to all descendants.
    /// If `to` already exists it is soft-deleted before the rename.
    pub fn rename_entry(&mut self, from: &str, to: &str) -> CoreFsResult<()> {
        if self.catalog.get(from).is_none() {
            return Err(CoreFsError::NotFound(format!("path not found: {from}")));
        }
        // Overwrite target with soft-delete semantics if it exists.
        if self.catalog.get(to).is_some() {
            self.delete_file(to, false)?;
        }
        // Collect all paths that share the `from` prefix (entry + descendants).
        let prefix = format!("{from}/");
        let old_paths: Vec<String> = self
            .catalog
            .list_paths()
            .into_iter()
            .filter(|p| p == from || p.starts_with(&prefix))
            .collect();

        for old_path in old_paths {
            let new_path = format!("{to}{}", &old_path[from.len()..]);
            if let Some(mut inode) = self.catalog.remove(&old_path) {
                inode.path = new_path;
                inode.touch_changed(); // rename is metadata change → ctime only
                self.catalog.insert(inode);
            }
        }
        self.hot_paths.record_metadata(from);
        self.hot_paths.record_metadata(to);
        self.journal.record("rename", from, format!("to={to}"));
        Ok(())
    }

    /// Low-level: write raw bytes for `inode` into the compat device.
    /// Use this only for restoring bytes obtained from an external source
    /// (disk, ODF device, etc.).  Prefer the higher-level write/create APIs.
    pub fn blocks_write(&mut self, inode: crate::domain::inode::InodeId, bytes: Vec<u8>) {
        self.blocks.write(inode, bytes);
    }

    /// Writes file bytes back into the compat device after a state restore.
    /// Used when the bytes were obtained from a previous service via
    /// `read_all_block_bytes()` and the state was transferred without disk I/O.
    pub fn restore_block_bytes(
        &mut self,
        block_bytes: std::collections::HashMap<crate::domain::inode::InodeId, Vec<u8>>,
    ) {
        for (inode_id, bytes) in block_bytes {
            self.blocks.write(inode_id, bytes);
        }
    }

    /// Returns a map of `InodeId → raw stored bytes` for every inode that has
    /// a block record in the compat device.  Used by code paths (e.g. FUSE) that
    /// need to materialise file content without going through the full read pipeline.
    pub fn read_all_block_bytes(&self) -> std::collections::HashMap<crate::domain::inode::InodeId, Vec<u8>> {
        self.catalog
            .list_paths()
            .into_iter()
            .filter_map(|path| {
                let inode = self.catalog.get(&path)?;
                let record = self.blocks.read(inode.id)?;
                Some((inode.id, record.bytes))
            })
            .collect()
    }

    pub fn persisted_state(&self) -> PersistedState {
        PersistedState {
            config: self.config.clone(),
            volume: self.volume.clone(),
            clean_unmount: self.clean_unmount,
            pending_wal: self.pending_wal.clone(),
            active_inodes: self.catalog.active_entries(),
            deleted_inodes: self.catalog.deleted_entries(),
            allocator_policy: self.blocks.allocator_policy().clone(),
            free_extents: self.blocks.free_extents(),
            hot_path_records: self.hot_paths.records(),
            block_records: self.blocks.records(),
            journal_entries: self.journal.entries().to_vec(),
            journal_runtime: self.journal.runtime_state().clone(),
            versions: self.versioning.all_versions(),
            sync_statuses: self.sync.statuses().to_vec(),
            snapshots: self.snapshots.clone(),
            next_snapshot_id: self.next_snapshot_id,
        }
    }

    pub fn from_persisted_state(state: PersistedState) -> Self {
        let block_size = state.volume.block_size;
        let next_inode = state
            .active_inodes
            .iter()
            .chain(state.deleted_inodes.iter())
            .map(|inode| inode.id.0)
            .max()
            .unwrap_or(0);

        let mut recovery = RecoveryService::default();
        for inode in &state.deleted_inodes {
            recovery.remember(inode.clone());
        }

        let mut encryption = EncryptionService::default();
        if state.config.security.encryption_at_rest {
            encryption.derive_key_from(state.config.volume_name.as_bytes());
        }

        let mut service = Self {
            config: state.config,
            volume: state.volume,
            allocator: InodeAllocator::with_next_inode(next_inode),
            catalog: Catalog::from_parts(state.active_inodes, state.deleted_inodes),
            blocks: BlockStore::from_records_with_allocator(
                state.block_records,
                block_size,
                state.allocator_policy,
                state.free_extents,
            ),
            journal: JournalService::from_entries_with_runtime(
                state.journal_entries,
                state.journal_runtime,
            ),
            versioning: VersioningService::from_versions(state.versions),
            compression: CompressionService,
            encryption,
            quota: QuotaService,
            recovery,
            integrity: IntegrityService,
            indexing: IndexingService,
            hot_paths: HotPathService::from_records(state.hot_path_records),
            security: SecurityService,
            sync: SyncService::from_statuses(state.sync_statuses),
            clean_unmount: state.clean_unmount,
            pending_wal: state.pending_wal,
            snapshots: state.snapshots,
            next_snapshot_id: state.next_snapshot_id,
            // The on-disk image we just loaded is, by construction,
            // clean — nothing to mark dirty.  Subsequent mutations
            // populate this set.
            dirty_inodes: std::collections::HashSet::new(),
        };
        service.recover_runtime_state();
        service.reconcile_from_journal();
        service
    }

    fn reconcile_from_journal(&mut self) {
        let replay = self.journal.replay();

        let active_paths = self.catalog.list_paths();
        for path in active_paths {
            if replay.deleted_paths.contains(&path) {
                if let Some(inode) = self.catalog.remove(&path) {
                    self.catalog.move_to_deleted(inode.clone());
                    self.recovery.remember(inode);
                }
            }
        }

        let deleted_paths = self.catalog.list_deleted_paths();
        for path in deleted_paths {
            if replay.active_paths.contains(&path) {
                if let Some(inode) = self.catalog.restore_deleted(&path) {
                    self.recovery.forget(&path);
                    self.catalog.insert(inode);
                }
            } else if !replay.deleted_paths.contains(&path) {
                let _ = self.catalog.restore_deleted(&path);
                self.recovery.forget(&path);
            }
        }

        if self.next_snapshot_id < replay.snapshot_count as u64 {
            self.next_snapshot_id = replay.snapshot_count as u64;
        }
    }

    fn auto_optimize_storage(&mut self, reason: &str) {
        let before = self.blocks.fragmentation_report();
        if !before.needs_compaction {
            return;
        }
        let prioritized = self.prioritized_hot_inodes(8);
        let report = self.blocks.optimize_with_priorities(&prioritized);
        if let Some(heat_reallocation) = &report.heat_reallocation {
            self.record_heat_reallocation_journal("auto_optimize_storage", heat_reallocation);
        } else if let Some(defragmentation) = report.defragmentation {
            self.journal.record(
                "auto_optimize_storage",
                "/",
                format!(
                    "reason={reason} fragmentation={} moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                    report.before.fragmentation_percent,
                    defragmentation.moved_entries,
                    defragmentation.reclaimed_gaps,
                    defragmentation.final_device_blocks
                ),
            );
        }
    }

    fn prioritized_hot_inodes(&self, limit: usize) -> Vec<InodeId> {
        self.hot_paths
            .hottest_paths(limit)
            .into_iter()
            .filter_map(|entry| self.inode_for_path(&entry.path))
            .collect()
    }

    fn record_heat_reallocation_journal(
        &mut self,
        operation: &str,
        report: &HeatReallocationReport,
    ) {
        self.journal.record(
            operation,
            "/",
            format!(
                "heat_reallocation prioritized_inodes={} promoted_hot_inodes={} moved_entries={} final_device_blocks={}",
                report.prioritized_inodes,
                report.promoted_hot_inodes,
                report.moved_entries,
                report.final_device_blocks
            ),
        );
    }
}

fn validate_path(path: &str) -> CoreFsResult<()> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(CoreFsError::InvalidInput(format!(
            "paths must be absolute and non-empty: {path}"
        )));
    }
    if path.len() > 16_384 {
        return Err(CoreFsError::InvalidInput(format!(
            "path exceeds supported limit: {path}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "concurrency_tests.rs"]
mod concurrency_tests;

#[cfg(test)]
#[path = "fault_injection_tests.rs"]
mod fault_injection_tests;

#[cfg(test)]
#[path = "stress_tests.rs"]
mod stress_tests;

#[cfg(test)]
#[path = "content_roundtrip_characterization_tests.rs"]
mod content_roundtrip_char_tests;
