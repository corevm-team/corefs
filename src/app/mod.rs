use crate::config::CoreFsConfig;
use crate::domain::acl::{AclEntry, Principal};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::runtime::RuntimeIntegrationBlueprint;
use crate::platform::tools::ToolRegistry;
use crate::services::hot_paths::{HotPathRecord, HotPathService};
use crate::services::indexing::IndexingService;
use crate::services::integrity::{IntegrityReport, IntegrityService};
use crate::services::journal::{JournalRecoverySummary, JournalRuntimeState, JournalService};
use crate::services::compression::CompressionService;
use crate::services::encryption::EncryptionService;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub config: CoreFsConfig,
    pub volume: VolumeDescriptor,
    pub clean_unmount: bool,
    pub pending_wal: Option<VolumeWal>,
    pub active_inodes: Vec<Inode>,
    pub deleted_inodes: Vec<Inode>,
    pub allocator_policy: AllocatorPolicy,
    pub free_extents: Vec<FreeExtentRecord>,
    pub hot_path_records: Vec<HotPathRecord>,
    pub block_records: Vec<crate::storage::block_store::BlockRecord>,
    pub journal_entries: Vec<crate::services::journal::JournalEntry>,
    pub journal_runtime: JournalRuntimeState,
    pub versions: Vec<crate::services::versioning::FileVersion>,
    pub sync_statuses: Vec<crate::services::sync::SyncStatus>,
    pub snapshots: Vec<Snapshot>,
    pub next_snapshot_id: u64,
}

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
        }
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
        let compress = self.config.performance.compression_enabled
            && self.compression.should_compress(bytes);
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
            self.quota.check_stats(
                &self.config.quotas,
                cur_files,
                cur_bytes,
                0,
                byte_delta,
            )?;
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
        let compress = self.config.performance.compression_enabled
            && self.compression.should_compress(bytes);
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
        inode.modified_at = SystemTime::now();
        inode.metadata.compressed = compress;
        inode.metadata.encrypted = encrypt;
        inode.size = bytes.len(); // logical size
        let inode_id = inode.id;
        self.blocks.write(inode_id, stored_bytes);
        self.hot_paths.record_write(path, bytes.len());
        self.journal
            .record("write_file", path, format!("bytes={}", bytes.len()));
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
            (inode.id, inode.kind, inode.metadata.compressed, inode.metadata.encrypted)
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
        inode.modified_at = SystemTime::now();
        inode.size = self.blocks.append_to_inode(inode_id, extra);
        self.hot_paths.record_write(path, extra.len());
        self.journal
            .record("extend_file", path, format!("extra_bytes={}", extra.len()));
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

        self.catalog.insert(inode);
        self.hot_paths.record_metadata(path);
        self.journal.record("restore", path, "");
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

        // Capture uncompressed content for every regular file inside the scope.
        let mut file_data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for path in &paths {
            let is_file = self
                .catalog
                .get(path)
                .is_some_and(|i| i.kind == InodeKind::File);
            if is_file {
                if let Ok(bytes) = self.read_file(path) {
                    file_data.insert(path.clone(), bytes);
                }
            }
        }

        let snapshot = Snapshot {
            id: self.next_snapshot_id,
            name: name.to_string(),
            scope_root: scope_root.to_string(),
            created_at: SystemTime::now(),
            paths,
            file_data,
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
            .ok_or_else(|| {
                CoreFsError::NotFound(format!("snapshot {snapshot_id} not found"))
            })?;
        let snapshot = self.snapshots.remove(pos);
        self.journal.record(
            "delete_snapshot",
            "/",
            format!("id={snapshot_id} name={}", snapshot.name),
        );
        Ok(())
    }

    /// Restores the filesystem to the state recorded in snapshot `snapshot_id`.
    ///
    /// For each file in `snapshot.file_data`:
    /// - If the file still exists it is overwritten via `write_file`.
    /// - If the file was deleted after the snapshot it is recreated via `create_file`.
    ///
    /// Paths that cannot be written (e.g. quota exceeded) are reported in
    /// `SnapshotRestoreReport.skipped_paths` rather than aborting the whole restore.
    /// Directories are not recreated — only file content is restored.
    pub fn restore_snapshot(&mut self, snapshot_id: u64) -> CoreFsResult<SnapshotRestoreReport> {
        let snapshot = self
            .snapshots
            .iter()
            .find(|s| s.id == snapshot_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {snapshot_id} not found")))?
            .clone();

        let mut restored_files = 0usize;
        let mut skipped_paths: Vec<String> = Vec::new();

        for (path, bytes) in &snapshot.file_data {
            let is_file = self
                .catalog
                .get(path)
                .is_some_and(|i| i.kind == InodeKind::File);
            let missing = self.catalog.get(path).is_none();

            if is_file {
                match self.write_file(path, bytes) {
                    Ok(()) => restored_files += 1,
                    Err(e) => skipped_paths.push(format!("{path}: {e}")),
                }
            } else if missing {
                match self.create_file(path, bytes, &[]) {
                    Ok(()) => restored_files += 1,
                    Err(e) => skipped_paths.push(format!("{path}: {e}")),
                }
            } else {
                // Path exists but is not a regular file (directory, symlink) — skip.
                skipped_paths.push(format!("{path}: not a regular file"));
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

            let mut target =
                Inode::new(target_id, InodeKind::File, to.to_string(), source_meta);
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
        self.journal.record("expunge", path, "permanent_delete=true");
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
    pub fn run_dedup(
        &mut self,
    ) -> CoreFsResult<crate::storage::block_store::DedupePassReport> {
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
        // Write to a sibling temp file first, then rename atomically so a crash
        // during the write never leaves a partially-written image behind.
        let tmp_name = path
            .file_name()
            .map(|n| format!("{}.tmp", n.to_string_lossy()))
            .unwrap_or_else(|| "corefs.img.tmp".to_string());
        let tmp_path = path.with_file_name(tmp_name);
        volume_image::save_volume_image(&tmp_path, &state)?;
        std::fs::rename(&tmp_path, path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp_path);
            CoreFsError::State(format!("atomic rename of image failed: {error}"))
        })
    }

    pub fn load_image_from_path(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let path = path.as_ref();
        let state = volume_image::load_volume_image(path)?;
        let mut service = Self::from_persisted_state(state);
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
    pub fn version_bytes_at(&self, path: &str, at: SystemTime) -> Option<Vec<u8>> {
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
    pub fn file_version_ids(&self, path: &str) -> Vec<(u64, SystemTime)> {
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
                inode.modified_at = SystemTime::now();
                self.catalog.insert(inode);
            }
        }
        self.hot_paths.record_metadata(from);
        self.hot_paths.record_metadata(to);
        self.journal.record("rename", from, format!("to={to}"));
        Ok(())
    }

    fn persisted_state(&self) -> PersistedState {
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

    fn from_persisted_state(state: PersistedState) -> Self {
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
mod tests {
    use super::*;
    use crate::services::journal::{JournalRuntimeState, JournalTransaction};

    fn test_fs() -> CoreFsService {
        CoreFsService::format(CoreFsConfig::default())
    }

    #[test]
    fn format_initializes_enterprise_services() {
        let fs = test_fs();

        assert_eq!(fs.volume_name(), "corefs");
        assert_eq!(fs.journal_entries(), 1);
        assert_eq!(fs.synced_paths(), 0);
        assert!(fs.list_paths().is_empty());
    }

    #[test]
    fn create_and_read_file_round_trips_content() {
        let mut fs = test_fs();
        fs.create_file("/notes.txt", b"hello", &["docs".to_string()])
            .expect("file creation should succeed");

        assert_eq!(
            fs.read_file("/notes.txt").expect("file should exist"),
            b"hello".to_vec()
        );
        assert!(fs.inode_for_path("/notes.txt").is_some());
    }

    #[test]
    fn duplicate_paths_are_rejected_for_file_directory_and_symlink() {
        let mut fs = test_fs();
        fs.create_file("/dup", b"a", &[]).expect("first file");
        assert!(matches!(
            fs.create_file("/dup", b"b", &[]),
            Err(CoreFsError::AlreadyExists(_))
        ));

        fs.create_directory("/dir").expect("dir");
        assert!(matches!(
            fs.create_directory("/dir"),
            Err(CoreFsError::AlreadyExists(_))
        ));

        fs.create_symlink("/ln", "/dup").expect("symlink");
        assert!(matches!(
            fs.create_symlink("/ln", "/dup"),
            Err(CoreFsError::AlreadyExists(_))
        ));
    }

    #[test]
    fn write_file_updates_existing_file_and_rejects_non_files() {
        let mut fs = test_fs();
        fs.create_file("/file.txt", b"old", &[]).expect("file");
        fs.write_file("/file.txt", b"new")
            .expect("write should work");
        assert_eq!(fs.read_file("/file.txt").expect("file"), b"new".to_vec());

        fs.create_directory("/dir").expect("dir");
        assert!(matches!(
            fs.write_file("/dir", b"bad"),
            Err(CoreFsError::InvalidInput(_))
        ));
        assert!(matches!(
            fs.write_file("/missing", b"bad"),
            Err(CoreFsError::NotFound(_))
        ));
    }

    #[test]
    fn read_file_returns_errors_for_missing_paths() {
        let fs = test_fs();
        assert!(matches!(
            fs.read_file("/missing"),
            Err(CoreFsError::NotFound(_))
        ));
    }

    #[test]
    fn delete_restore_and_secure_delete_follow_policies() {
        let mut fs = test_fs();
        fs.create_file("/recover.txt", b"data", &[]).expect("file");
        fs.delete_file("/recover.txt", false).expect("soft delete");
        assert!(fs.read_file("/recover.txt").is_err());
        assert_eq!(fs.recoverable_paths(), vec!["/recover.txt".to_string()]);
        fs.restore_file("/recover.txt").expect("restore");
        assert_eq!(
            fs.read_file("/recover.txt").expect("restored"),
            b"data".to_vec()
        );

        fs.delete_file("/recover.txt", true).expect("secure delete");
        assert!(matches!(
            fs.restore_file("/recover.txt"),
            Err(CoreFsError::NotFound(_))
        ));
        assert!(matches!(
            fs.delete_file("/recover.txt", true),
            Err(CoreFsError::NotFound(_))
        ));
    }

    #[test]
    fn snapshot_scrub_sync_and_reporting_are_available() {
        let mut fs = test_fs();
        fs.create_directory("/etc").expect("dir");
        fs.create_file("/etc/config.txt", b"cfg", &["config".to_string()])
            .expect("file");

        let snapshot = fs.create_snapshot("baseline");
        assert_eq!(snapshot.id, 1);
        assert!(snapshot.paths.iter().any(|path| path == "/etc/config.txt"));
        assert_eq!(fs.snapshot_names(), vec!["baseline".to_string()]);

        let scrub = fs.scrub();
        assert_eq!(scrub.checked_paths, 1);
        assert_eq!(scrub.valid_blocks, 1);
        assert_eq!(scrub.invalid_blocks, 0);

        fs.mark_synced("/etc/config.txt", "node-a").expect("sync");
        assert_eq!(fs.synced_paths(), 1);
        assert!(matches!(
            fs.mark_synced("/missing", "node-a"),
            Err(CoreFsError::NotFound(_))
        ));

        let stats = fs.stats();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.deleted_files, 0);
        assert_eq!(stats.versions, 1);
        assert_eq!(stats.snapshots, 1);
        assert!(stats.journal_entries >= 5);

        let report = fs.admin_report();
        assert_eq!(report.volume.name, "corefs");
        assert!(
            report
                .runtime
                .compatibility_targets
                .iter()
                .any(|item| item == "native-os")
        );
        assert_eq!(report.tools.mkfs, "corefs mkfs");
    }

    #[test]
    fn state_can_be_saved_and_loaded_as_binary_image() {
        let path = std::env::temp_dir().join(format!(
            "corefs-image-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));

        let mut fs = test_fs();
        fs.create_directory("/var").expect("dir");
        fs.create_file("/var/log.bin", b"log", &["logs".to_string()])
            .expect("file");
        fs.create_snapshot("binary");
        fs.save_image_to_path(&path)
            .expect("image save should succeed");

        let loaded = CoreFsService::load_image_from_path(&path).expect("image load should succeed");

        assert!(
            loaded
                .list_paths()
                .iter()
                .any(|path| path == "/var/log.bin")
        );
        assert_eq!(loaded.snapshot_names(), vec!["binary".to_string()]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn journal_replay_reconciles_deleted_entries_on_load() {
        let path = std::env::temp_dir().join(format!(
            "corefs-journal-replay-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));

        let mut fs = test_fs();
        fs.create_file("/replay.txt", b"data", &[]).expect("file");
        fs.delete_file("/replay.txt", false).expect("delete");
        fs.save_image_to_path(&path).expect("save image");

        let loaded = CoreFsService::load_image_from_path(&path).expect("load image");

        assert!(!loaded.list_paths().iter().any(|path| path == "/replay.txt"));
        assert_eq!(loaded.recoverable_paths(), vec!["/replay.txt".to_string()]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn load_image_recovers_unclean_runtime_state_and_aborts_pending_transaction() {
        let path = std::env::temp_dir().join(format!(
            "corefs-journal-runtime-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ));

        let mut fs = test_fs();
        fs.create_file("/runtime.txt", b"stable", &[])
            .expect("file");
        let mut state = fs.export_state();
        state.clean_unmount = false;
        state.journal_runtime = JournalRuntimeState {
            next_transaction_id: 1,
            unclean_shutdown: true,
            pending_transaction: Some(JournalTransaction {
                id: 1,
                label: "rw-writeback".to_string(),
                started_at: SystemTime::now(),
                operations: vec![crate::services::journal::JournalEntry {
                    timestamp: SystemTime::now(),
                    operation: "write_file".to_string(),
                    target: "/runtime.txt".to_string(),
                    details: "bytes=7".to_string(),
                }],
            }),
            ..JournalRuntimeState::default()
        };
        volume_image::save_volume_image(&path, &state).expect("save image");

        let loaded = CoreFsService::load_image_from_path(&path).expect("load image");

        assert!(!loaded.had_unclean_shutdown());
        assert_eq!(
            loaded.read_file("/runtime.txt").expect("file"),
            b"stable".to_vec()
        );
        assert!(loaded.journal_entries() >= 4);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_image_to_path_is_atomic_and_leaves_no_tmp_file() {
        let path = std::env::temp_dir().join(format!(
            "corefs-atomic-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time should be after unix epoch")
                .as_nanos()
        ));
        let tmp_path = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_string_lossy()
        ));

        let mut fs = test_fs();
        fs.create_file("/atomic.txt", b"hello", &[]).expect("file");
        fs.save_image_to_path(&path).expect("save");

        // The final image exists and the tmp file is gone.
        assert!(path.exists(), "image should exist");
        assert!(!tmp_path.exists(), "tmp file should be cleaned up");

        // Reload to confirm the image is readable.
        let loaded = CoreFsService::load_image_from_path(&path).expect("load");
        assert_eq!(
            loaded.read_file("/atomic.txt").expect("read"),
            b"hello".to_vec()
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rename_entry_moves_file_to_new_path() {
        let mut fs = test_fs();
        fs.create_file("/old.txt", b"content", &[]).expect("file");
        fs.rename_entry("/old.txt", "/new.txt").expect("rename");

        assert!(fs.read_file("/old.txt").is_err());
        assert_eq!(
            fs.read_file("/new.txt").expect("new path"),
            b"content".to_vec()
        );
    }

    #[test]
    fn rename_entry_cascades_to_directory_children() {
        let mut fs = test_fs();
        fs.create_directory("/src").expect("dir");
        fs.create_file("/src/main.rs", b"fn main(){}", &[])
            .expect("file");
        fs.create_directory("/src/utils").expect("subdir");
        fs.create_file("/src/utils/helper.rs", b"// helper", &[])
            .expect("file");

        fs.rename_entry("/src", "/lib").expect("rename dir");

        assert!(!fs.list_paths().iter().any(|p| p.starts_with("/src")));
        assert_eq!(
            fs.read_file("/lib/main.rs").expect("main.rs"),
            b"fn main(){}".to_vec()
        );
        assert_eq!(
            fs.read_file("/lib/utils/helper.rs").expect("helper.rs"),
            b"// helper".to_vec()
        );
    }

    #[test]
    fn rename_entry_overwrites_existing_target() {
        let mut fs = test_fs();
        fs.create_file("/a.txt", b"aaa", &[]).expect("file a");
        fs.create_file("/b.txt", b"bbb", &[]).expect("file b");

        fs.rename_entry("/a.txt", "/b.txt").expect("rename over");

        assert!(fs.read_file("/a.txt").is_err());
        assert_eq!(fs.read_file("/b.txt").expect("b.txt"), b"aaa".to_vec());
        // overwritten entry is soft-deleted and recoverable
        assert!(fs.recoverable_paths().contains(&"/b.txt".to_string()));
    }

    #[test]
    fn rename_entry_fails_for_missing_source() {
        let mut fs = test_fs();
        assert!(matches!(
            fs.rename_entry("/missing.txt", "/target.txt"),
            Err(CoreFsError::NotFound(_))
        ));
    }

    #[test]
    fn validate_path_rejects_invalid_inputs() {
        assert!(matches!(
            validate_path(""),
            Err(CoreFsError::InvalidInput(_))
        ));
        assert!(matches!(
            validate_path("relative"),
            Err(CoreFsError::InvalidInput(_))
        ));
        assert!(validate_path("/valid").is_ok());
    }

    #[test]
    fn validate_path_rejects_excessively_long_paths() {
        let too_long = format!("/{}", "a".repeat(16_384));
        assert!(matches!(
            validate_path(&too_long),
            Err(CoreFsError::InvalidInput(_))
        ));
    }

    #[test]
    fn data_layout_for_inode_tracks_data_segment_offsets() {
        // Disable encryption and compression so stored bytes == raw bytes.
        let mut fs = CoreFsService::format(CoreFsConfig {
            security: crate::config::SecurityPolicy {
                encryption_at_rest: false,
                ..CoreFsConfig::default().security
            },
            performance: crate::config::PerformancePolicy {
                compression_enabled: false,
                ..CoreFsConfig::default().performance
            },
            ..CoreFsConfig::default()
        });
        fs.create_file("/a.txt", b"abc", &[]).expect("file");
        fs.create_file("/b.txt", b"hello", &[]).expect("file");

        let first = fs
            .inode_for_path("/a.txt")
            .and_then(|inode| fs.data_layout_for_inode(inode))
            .expect("first layout");
        let second = fs
            .inode_for_path("/b.txt")
            .and_then(|inode| fs.data_layout_for_inode(inode))
            .expect("second layout");

        assert_eq!(first.data_offset, 0);
        assert_eq!(first.length, 3);
        assert_eq!(second.data_offset, fs.block_size() as u64);
        assert_eq!(second.length, 5);
    }

    #[test]
    fn data_extents_for_inode_follow_block_boundaries() {
        // Disable encryption and compression so stored bytes == raw bytes.
        let mut fs = CoreFsService::format(CoreFsConfig {
            block_size: 4,
            security: crate::config::SecurityPolicy {
                encryption_at_rest: false,
                ..CoreFsConfig::default().security
            },
            performance: crate::config::PerformancePolicy {
                compression_enabled: false,
                ..CoreFsConfig::default().performance
            },
            ..CoreFsConfig::default()
        });
        fs.create_file("/payload.bin", b"abcdefghij", &[])
            .expect("file");

        let inode = fs.inode_for_path("/payload.bin").expect("inode");
        let extents = fs.data_extents_for_inode(inode);

        assert_eq!(extents.len(), 3);
        assert_eq!(extents[0].device_block, 0);
        assert_eq!(extents[0].inode_offset, 0);
        assert_eq!(extents[0].length, 4);
        assert_eq!(extents[1].device_block, 1);
        assert_eq!(extents[1].inode_offset, 4);
        assert_eq!(extents[1].length, 4);
        assert_eq!(extents[2].device_block, 2);
        assert_eq!(extents[2].inode_offset, 8);
        assert_eq!(extents[2].length, 2);
    }

    #[test]
    fn defragment_compacts_device_blocks_and_records_journal_entry() {
        let mut fs = CoreFsService::format(CoreFsConfig {
            block_size: 4,
            ..CoreFsConfig::default()
        });
        fs.create_file("/a", b"aaaa", &[]).expect("file");
        fs.create_file("/b", b"bbbb", &[]).expect("file");
        fs.create_file("/c", b"cccc", &[]).expect("file");
        fs.delete_file("/b", true).expect("delete");

        let before = fs
            .inode_for_path("/c")
            .and_then(|inode| fs.data_layout_for_inode(inode))
            .expect("layout")
            .data_offset;
        let report = fs.defragment();
        let after = fs
            .inode_for_path("/c")
            .and_then(|inode| fs.data_layout_for_inode(inode))
            .expect("layout")
            .data_offset;

        assert!(report.moved_entries >= 1);
        assert!(after < before);
        assert!(fs.journal_entries() >= 1);
    }

    #[test]
    fn optimize_storage_reports_fragmentation_and_compacts_when_needed() {
        let mut fs = CoreFsService::format(CoreFsConfig {
            block_size: 4,
            ..CoreFsConfig::default()
        });
        fs.set_allocator_policy(AllocatorPolicy {
            background_compaction_enabled: false,
            fragmentation_threshold_percent: 25,
            coalesce_on_release: false,
            ..AllocatorPolicy::default()
        });
        fs.create_file("/a", b"aaaa", &[]).expect("file");
        fs.create_file("/b", b"bbbb", &[]).expect("file");
        fs.create_file("/c", b"cccc", &[]).expect("file");
        fs.create_file("/d", b"dddd", &[]).expect("file");
        fs.create_file("/e", b"eeee", &[]).expect("file");
        fs.delete_file("/b", true).expect("delete");
        fs.delete_file("/d", true).expect("delete");

        let report = fs.optimize_storage();

        assert!(report.heat_reallocation.is_some());
        assert!(report.defragmentation.is_none());
        assert!(report.before.fragmentation_percent >= 25);
        assert_eq!(report.after.fragmentation_percent, 0);
    }

    #[test]
    fn auto_optimize_runs_when_policy_requests_background_compaction() {
        let mut fs = CoreFsService::format(CoreFsConfig {
            block_size: 4,
            ..CoreFsConfig::default()
        });
        fs.set_allocator_policy(AllocatorPolicy {
            background_compaction_enabled: true,
            fragmentation_threshold_percent: 25,
            coalesce_on_release: false,
            ..AllocatorPolicy::default()
        });
        fs.create_file("/a", b"aaaa", &[]).expect("file");
        fs.create_file("/b", b"bbbb", &[]).expect("file");
        fs.create_file("/c", b"cccc", &[]).expect("file");
        fs.delete_file("/b", true).expect("delete");
        fs.write_file("/c", b"ccccdddd").expect("write");

        let report = fs.fragmentation_report();

        assert_eq!(report.fragmentation_percent, 0);
        assert!(
            fs.journal_entries() >= 1,
            "auto optimization should leave a journal trail"
        );
    }

    #[test]
    fn persisted_state_round_trips_hot_path_records() {
        let mut fs = test_fs();
        fs.create_file("/hot.txt", b"hello", &[]).expect("file");
        fs.write_file("/hot.txt", b"hello-world").expect("write");

        let state = fs.export_state();

        assert!(
            state.hot_path_records.iter().any(|record| {
                record.path == "/hot.txt" && record.write_ops >= 2 && record.bytes_written >= 16
            }),
            "expected hot path telemetry to be exported"
        );
    }

    // ── Copy-on-Write ───────────────────────────────────────────────────────────

    #[test]
    fn snapshot_captures_file_data_at_creation_time() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/notes.txt", b"hello snapshot", &[])
            .expect("file");

        let snap = fs.create_snapshot("v1");

        assert!(
            snap.file_data.contains_key("/notes.txt"),
            "snapshot must include file_data for every regular file"
        );
        assert_eq!(
            snap.file_data["/notes.txt"],
            b"hello snapshot".to_vec(),
            "captured bytes must match what was written"
        );
    }

    #[test]
    fn snapshot_captures_uncompressed_bytes_for_compressed_files() {
        let config = CoreFsConfig {
            performance: crate::config::PerformancePolicy {
                compression_enabled: true,
                copy_on_write: true,
                journaling_enabled: true,
                deduplication_enabled: false,
                trim_enabled: true,
            },
            ..CoreFsConfig::default()
        };
        let payload = b"compressible content ".repeat(50);
        let mut fs = CoreFsService::format(config);
        fs.create_file("/big.txt", &payload, &[]).expect("file");

        let snap = fs.create_snapshot("compressed-snap");

        assert_eq!(
            snap.file_data.get("/big.txt").cloned().unwrap_or_default(),
            payload,
            "snapshot must store uncompressed bytes"
        );
    }

    #[test]
    fn snapshot_restore_reverts_file_to_captured_state() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/doc.txt", b"original", &[]).expect("file");
        let snap = fs.create_snapshot("before-change");

        // Modify the file after the snapshot.
        fs.write_file("/doc.txt", b"modified").expect("write");
        assert_eq!(fs.read_file("/doc.txt").expect("read"), b"modified".to_vec());

        // Restore the snapshot — file must revert.
        let report = fs
            .restore_snapshot(snap.id)
            .expect("restore should succeed");

        assert_eq!(report.restored_files, 1);
        assert!(report.skipped_paths.is_empty());
        assert_eq!(
            fs.read_file("/doc.txt").expect("read"),
            b"original".to_vec(),
            "file must be restored to snapshot state"
        );
    }

    #[test]
    fn snapshot_restore_recreates_deleted_files() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/gone.txt", b"was here", &[]).expect("file");
        let snap = fs.create_snapshot("before-delete");

        fs.delete_file("/gone.txt", false).expect("delete");

        let report = fs.restore_snapshot(snap.id).expect("restore");

        assert_eq!(report.restored_files, 1, "deleted file must be recreated");
        assert_eq!(
            fs.read_file("/gone.txt").expect("read"),
            b"was here".to_vec()
        );
    }

    #[test]
    fn delete_snapshot_removes_it_from_the_list() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        let snap = fs.create_snapshot("ephemeral");
        assert_eq!(fs.snapshots().len(), 1);

        fs.delete_snapshot(snap.id).expect("delete should succeed");
        assert!(fs.snapshots().is_empty());
    }

    #[test]
    fn delete_snapshot_returns_error_for_unknown_id() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        assert!(fs.delete_snapshot(999).is_err());
    }

    #[test]
    fn restore_snapshot_returns_error_for_unknown_id() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        assert!(fs.restore_snapshot(999).is_err());
    }

    #[test]
    fn clone_file_shares_blob_and_allows_independent_divergence() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/src.txt", b"shared data", &[]).expect("file");

        fs.clone_file("/src.txt", "/dst.txt").expect("clone");

        // Both files read the same content immediately after cloning.
        assert_eq!(
            fs.read_file("/src.txt").expect("read"),
            fs.read_file("/dst.txt").expect("read"),
            "clone must initially equal source"
        );

        // Overwrite source — clone must remain independent.
        fs.write_file("/src.txt", b"diverged").expect("write");
        assert_eq!(
            fs.read_file("/dst.txt").expect("read"),
            b"shared data".to_vec(),
            "clone must not be affected by source write"
        );
        assert_eq!(
            fs.read_file("/src.txt").expect("read"),
            b"diverged".to_vec()
        );
    }

    #[test]
    fn clone_file_fails_for_nonexistent_source() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        assert!(fs.clone_file("/ghost.txt", "/copy.txt").is_err());
    }

    #[test]
    fn clone_file_fails_when_target_already_exists() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/a.txt", b"a", &[]).expect("file");
        fs.create_file("/b.txt", b"b", &[]).expect("file");
        assert!(fs.clone_file("/a.txt", "/b.txt").is_err());
    }

    #[test]
    fn expunge_file_permanently_removes_soft_deleted_file() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/temp.txt", b"temporary", &[]).expect("file");
        fs.delete_file("/temp.txt", false).expect("soft delete");

        // File is in deleted catalog, not active.
        assert!(fs.get_inode("/temp.txt").is_none());
        assert!(fs.recoverable_paths().contains(&"/temp.txt".to_string()));

        fs.expunge_file("/temp.txt").expect("expunge");

        // File is now gone from both catalogs.
        assert!(!fs.recoverable_paths().contains(&"/temp.txt".to_string()));
        assert!(
            fs.restore_file("/temp.txt").is_err(),
            "cannot restore expunged file"
        );
    }

    #[test]
    fn expunge_file_returns_error_for_active_or_missing_path() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/active.txt", b"x", &[]).expect("file");
        // Active files cannot be expunged — only soft-deleted ones.
        assert!(fs.expunge_file("/active.txt").is_err());
        assert!(fs.expunge_file("/nonexistent.txt").is_err());
    }

    #[test]
    fn cow_report_reflects_sharing_and_config() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/a.txt", b"abc", &[]).expect("file");
        fs.clone_file("/a.txt", "/b.txt").expect("clone");
        fs.create_file("/c.txt", b"unique", &[]).expect("file");

        let report = fs.cow_report();

        assert!(report.copy_on_write_enabled);
        assert_eq!(report.snapshot_count, 0);
        // At least one shared blob must be detected (a.txt and b.txt share a blob).
        assert!(
            report.stats.shared_blobs >= 1,
            "shared blob must be reported"
        );
        assert!(
            report.stats.bytes_saved_by_sharing > 0,
            "savings must be nonzero for shared blobs"
        );
    }

    #[test]
    fn cow_report_snapshot_count_matches_live_snapshots() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_snapshot("snap1");
        fs.create_snapshot("snap2");

        let report = fs.cow_report();
        assert_eq!(report.snapshot_count, 2);
    }

    // ── Config enforcement ──────────────────────────────────────────────────────

    #[test]
    fn clone_file_with_cow_disabled_creates_independent_copy() {
        let config = CoreFsConfig {
            performance: crate::config::PerformancePolicy {
                copy_on_write: false,
                compression_enabled: false,
                journaling_enabled: true,
                deduplication_enabled: false,
                trim_enabled: true,
            },
            ..CoreFsConfig::default()
        };
        let mut fs = CoreFsService::format(config);
        fs.create_file("/a.txt", b"data", &[]).expect("file");

        fs.clone_file("/a.txt", "/b.txt").expect("clone");

        // Both files exist with the same content.
        assert_eq!(fs.read_file("/a.txt").unwrap(), b"data".to_vec());
        assert_eq!(fs.read_file("/b.txt").unwrap(), b"data".to_vec());

        // With CoW disabled there should be no shared blobs (eager full copy).
        let report = fs.cow_report();
        assert!(
            !report.copy_on_write_enabled,
            "CoW flag should be off"
        );
    }

    // ── Recursive directory cloning ─────────────────────────────────────────────

    #[test]
    fn clone_tree_copies_directory_recursively() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/src").expect("dir");
        fs.create_directory("/src/sub").expect("sub");
        fs.create_file("/src/a.txt", b"alpha", &[]).expect("file");
        fs.create_file("/src/sub/b.txt", b"beta", &[]).expect("file");

        let report = fs.clone_tree("/src", "/dst").expect("clone_tree");

        assert_eq!(report.cloned_directories, 2, "root + sub");
        assert_eq!(report.cloned_files, 2, "two regular files");
        assert!(report.skipped_paths.is_empty());

        assert_eq!(fs.read_file("/dst/a.txt").unwrap(), b"alpha".to_vec());
        assert_eq!(fs.read_file("/dst/sub/b.txt").unwrap(), b"beta".to_vec());
    }

    #[test]
    fn clone_tree_diverges_independently() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/orig").expect("dir");
        fs.create_file("/orig/data.bin", b"shared", &[]).expect("file");
        fs.clone_tree("/orig", "/copy").expect("clone_tree");

        // Modify the copy — original must not change.
        fs.write_file("/copy/data.bin", b"changed").expect("write");
        assert_eq!(fs.read_file("/orig/data.bin").unwrap(), b"shared".to_vec());
        assert_eq!(fs.read_file("/copy/data.bin").unwrap(), b"changed".to_vec());
    }

    #[test]
    fn clone_tree_rejects_non_directory_source() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/file.txt", b"x", &[]).expect("file");
        assert!(fs.clone_tree("/file.txt", "/copy").is_err());
    }

    #[test]
    fn clone_tree_rejects_existing_target() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/a").expect("dir");
        fs.create_directory("/b").expect("dir");
        assert!(fs.clone_tree("/a", "/b").is_err());
    }

    #[test]
    fn clone_tree_handles_symlinks() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/src").expect("dir");
        fs.create_symlink("/src/link", "/some/target").expect("symlink");

        let report = fs.clone_tree("/src", "/dst").expect("clone_tree");

        // Symlink counts as a cloned file.
        assert_eq!(report.cloned_files, 1);
        assert_eq!(report.cloned_directories, 1);
    }

    // ── Scoped snapshots ────────────────────────────────────────────────────────

    #[test]
    fn scoped_snapshot_captures_only_subtree() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/docs").expect("dir");
        fs.create_file("/docs/readme.md", b"hello", &[]).expect("file");
        fs.create_file("/root.txt", b"root", &[]).expect("file");

        let snap = fs.create_snapshot_scoped("docs-only", "/docs");

        assert!(
            snap.file_data.contains_key("/docs/readme.md"),
            "scoped file must be captured"
        );
        assert!(
            !snap.file_data.contains_key("/root.txt"),
            "out-of-scope file must not be captured"
        );
        assert_eq!(snap.scope_root, "/docs");
    }

    #[test]
    fn scoped_snapshot_restore_only_restores_scoped_files() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/proj").expect("dir");
        fs.create_file("/proj/code.rs", b"fn main() {}", &[]).expect("file");
        fs.create_file("/unrelated.txt", b"keep", &[]).expect("file");
        let snap = fs.create_snapshot_scoped("proj-snap", "/proj");

        // Modify both files.
        fs.write_file("/proj/code.rs", b"fn changed() {}").expect("write");
        fs.write_file("/unrelated.txt", b"also changed").expect("write");

        let report = fs.restore_snapshot(snap.id).expect("restore");

        assert_eq!(report.restored_files, 1);
        assert_eq!(
            fs.read_file("/proj/code.rs").unwrap(),
            b"fn main() {}".to_vec(),
            "scoped file must be restored"
        );
        assert_eq!(
            fs.read_file("/unrelated.txt").unwrap(),
            b"also changed".to_vec(),
            "out-of-scope file must remain unchanged"
        );
    }

    // ── Snapshot diff ───────────────────────────────────────────────────────────

    #[test]
    fn diff_snapshots_detects_all_change_categories() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/stable.txt", b"same", &[]).expect("file");
        fs.create_file("/changed.txt", b"old", &[]).expect("file");
        fs.create_file("/removed.txt", b"gone", &[]).expect("file");

        let snap_a = fs.create_snapshot("before");

        // Modify, delete, and add between snapshots.
        fs.write_file("/changed.txt", b"new").expect("write");
        fs.delete_file("/removed.txt", false).expect("delete");
        fs.create_file("/added.txt", b"fresh", &[]).expect("file");

        let snap_b = fs.create_snapshot("after");

        let diff = fs.diff_snapshots(snap_a.id, snap_b.id).expect("diff");

        assert_eq!(diff.added, vec!["/added.txt".to_string()]);
        assert_eq!(diff.removed, vec!["/removed.txt".to_string()]);
        assert_eq!(diff.modified, vec!["/changed.txt".to_string()]);
        assert_eq!(diff.unchanged, vec!["/stable.txt".to_string()]);
    }

    #[test]
    fn diff_snapshots_returns_error_for_unknown_snapshot() {
        let fs = CoreFsService::format(CoreFsConfig::default());
        assert!(fs.diff_snapshots(1, 2).is_err());
    }

    #[test]
    fn diff_snapshots_identical_snapshots_shows_all_unchanged() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/x.txt", b"data", &[]).expect("file");

        let a = fs.create_snapshot("a");
        let b = fs.create_snapshot("b");

        let diff = fs.diff_snapshots(a.id, b.id).expect("diff");

        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
        assert_eq!(diff.unchanged.len(), 1);
    }

    // ── Encryption at rest ──────────────────────────────────────────────────────

    #[test]
    fn encrypted_file_round_trips_through_read_write() {
        // Default config has encryption_at_rest: true.
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/secret.txt", b"classified", &[])
            .expect("create");

        let inode = fs.get_inode("/secret.txt").expect("inode");
        assert!(inode.metadata.encrypted, "file should be marked encrypted");

        assert_eq!(
            fs.read_file("/secret.txt").unwrap(),
            b"classified".to_vec(),
            "read_file must transparently decrypt"
        );
    }

    #[test]
    fn encrypted_file_write_updates_and_reads_back() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/doc.txt", b"v1", &[]).expect("create");
        fs.write_file("/doc.txt", b"v2-encrypted").expect("write");

        assert_eq!(
            fs.read_file("/doc.txt").unwrap(),
            b"v2-encrypted".to_vec()
        );
    }

    #[test]
    fn encryption_plus_compression_round_trips() {
        let payload = b"compress and encrypt me ".repeat(50);
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/both.bin", &payload, &[]).expect("create");

        let inode = fs.get_inode("/both.bin").expect("inode");
        assert!(inode.metadata.encrypted);
        assert!(inode.metadata.compressed);

        assert_eq!(fs.read_file("/both.bin").unwrap(), payload);
    }

    #[test]
    fn snapshot_captures_plaintext_bytes_even_when_encrypted() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/enc.txt", b"plaintext content", &[])
            .expect("create");

        let snap = fs.create_snapshot("encrypted-snap");

        assert_eq!(
            snap.file_data.get("/enc.txt").cloned().unwrap_or_default(),
            b"plaintext content".to_vec(),
            "snapshot must capture unencrypted content"
        );
    }

    #[test]
    fn encryption_disabled_stores_and_reads_plaintext() {
        let config = CoreFsConfig {
            security: crate::config::SecurityPolicy {
                encryption_at_rest: false,
                ..CoreFsConfig::default().security
            },
            ..CoreFsConfig::default()
        };
        let mut fs = CoreFsService::format(config);
        fs.create_file("/plain.txt", b"no encryption", &[])
            .expect("create");

        let inode = fs.get_inode("/plain.txt").expect("inode");
        assert!(!inode.metadata.encrypted);
        assert_eq!(fs.read_file("/plain.txt").unwrap(), b"no encryption".to_vec());
    }

    // ── Dedup pass ──────────────────────────────────────────────────────────────

    #[test]
    fn run_dedup_requires_config_flag() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        // Default has deduplication_enabled: false.
        assert!(
            fs.run_dedup().is_err(),
            "dedup should be rejected when disabled"
        );
    }

    #[test]
    fn run_dedup_reports_clean_state() {
        let config = CoreFsConfig {
            performance: crate::config::PerformancePolicy {
                deduplication_enabled: true,
                ..CoreFsConfig::default().performance
            },
            ..CoreFsConfig::default()
        };
        let mut fs = CoreFsService::format(config);
        fs.create_file("/a.txt", b"data", &[]).expect("file");
        fs.create_file("/b.txt", b"other", &[]).expect("file");

        let report = fs.run_dedup().expect("dedup should succeed");

        assert!(report.blobs_scanned >= 2);
        assert_eq!(report.hash_collisions, 0);
        assert_eq!(report.duplicates_consolidated, 0);
    }

    // ── Deep fsck ───────────────────────────────────────────────────────────────

    #[test]
    fn fsck_clean_filesystem_reports_no_errors() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/docs").expect("dir");
        fs.create_file("/docs/readme.md", b"hello", &[]).expect("file");
        fs.create_file("/data.bin", b"binary payload", &[]).expect("file");

        let report = fs.fsck();

        assert!(report.checked_inodes >= 3);
        assert_eq!(report.missing_blocks, Vec::<String>::new());
        assert_eq!(report.checksum_failures, Vec::<String>::new());
        assert_eq!(report.compression_errors, Vec::<String>::new());
        assert_eq!(report.encryption_errors, Vec::<String>::new());
        assert_eq!(report.size_mismatches, Vec::<(String, usize, usize)>::new());
        assert_eq!(report.orphaned_blocks, Vec::<crate::domain::inode::InodeId>::new());
    }

    #[test]
    fn fsck_with_encryption_and_compression_validates_all_layers() {
        let payload = b"fsck test payload ".repeat(10);
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/layered.bin", &payload, &[]).expect("file");

        let inode = fs.get_inode("/layered.bin").expect("inode");
        assert!(inode.metadata.encrypted);
        assert!(inode.metadata.compressed);

        let report = fs.fsck();

        // All layers valid: decrypt → decompress → size match.
        assert_eq!(report.encryption_errors, Vec::<String>::new());
        assert_eq!(report.compression_errors, Vec::<String>::new());
        assert_eq!(report.size_mismatches, Vec::<(String, usize, usize)>::new());
    }

    #[test]
    fn fsck_detects_missing_blocks() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/orphan.txt", b"data", &[]).expect("file");

        // Manually remove the block to simulate corruption.
        let inode_id = fs.inode_for_path("/orphan.txt").expect("inode");
        fs.blocks.remove(inode_id);

        let report = fs.fsck();

        assert!(
            report.missing_blocks.contains(&"/orphan.txt".to_string()),
            "fsck must detect missing blocks"
        );
    }
}
