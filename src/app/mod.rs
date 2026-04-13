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
use crate::services::recovery::RecoveryService;
use crate::services::security::SecurityService;
use crate::services::sync::SyncService;
use crate::services::versioning::VersioningService;
use crate::storage::allocator::InodeAllocator;
use crate::storage::block_store::{
    AllocatorPolicy, BlockStore, DefragmentationReport, FragmentationReport, FreeExtentRecord,
    HeatReallocationReport, OptimizationReport,
};
use crate::storage::catalog::Catalog;
use crate::storage::volume_image;
use crate::storage::volume_wal::{self, VolumeWal};
use serde::{Deserialize, Serialize};
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

        Self {
            config,
            volume,
            allocator: InodeAllocator::default(),
            catalog: Catalog::default(),
            blocks: BlockStore::with_block_size(block_size),
            journal,
            versioning: VersioningService::default(),
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

        self.allocator.allocate_specific(inode_id);
        let mut metadata = FileMetadata::default();
        metadata.tags = tags.to_vec();
        metadata.content_class = self.indexing.classify_path(path);
        metadata.encrypted = self
            .security
            .mark_encrypted(self.config.security.encryption_at_rest);
        metadata.compressed = self.config.performance.compression_enabled;
        metadata.acl = vec![AclEntry::full_access(Principal::Role("system".to_string()))];

        let mut inode = Inode::new(inode_id, InodeKind::File, path.to_string(), metadata);
        inode.size = self.blocks.write(inode_id, bytes.to_vec());

        if self.config.versioning.keep_latest > 0 {
            self.versioning.store_version(path, bytes.to_vec());
            self.versioning
                .prune(path, self.config.versioning.keep_latest);
        }

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

        inode.modified_at = SystemTime::now();
        inode.size = self.blocks.write(inode.id, bytes.to_vec());
        self.hot_paths.record_write(path, bytes.len());
        self.versioning.store_version(path, bytes.to_vec());
        self.versioning
            .prune(path, self.config.versioning.keep_latest);
        self.journal
            .record("write_file", path, format!("bytes={}", bytes.len()));
        self.auto_optimize_storage("write_file");
        Ok(())
    }

    pub fn read_file(&self, path: &str) -> CoreFsResult<Vec<u8>> {
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        let record = self
            .blocks
            .read(inode.id)
            .ok_or_else(|| CoreFsError::State(format!("missing data blocks for {path}")))?;
        Ok(record.bytes.clone())
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

    pub fn create_snapshot(&mut self, name: &str) -> Snapshot {
        self.next_snapshot_id += 1;
        let snapshot = Snapshot {
            id: self.next_snapshot_id,
            name: name.to_string(),
            scope_root: "/".to_string(),
            created_at: SystemTime::now(),
            paths: self.catalog.list_paths(),
        };
        self.journal.record("snapshot", "/", format!("name={name}"));
        self.snapshots.push(snapshot.clone());
        snapshot
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
        let mut fs = CoreFsService::format(CoreFsConfig::default());
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
        let mut fs = CoreFsService::format(CoreFsConfig {
            block_size: 4,
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
}
