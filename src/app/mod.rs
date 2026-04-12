use crate::config::{CoreFsConfig, StorageTier};
use crate::domain::acl::{AclEntry, Principal};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::runtime::RuntimeIntegrationBlueprint;
use crate::platform::tools::ToolRegistry;
use crate::services::indexing::IndexingService;
use crate::services::integrity::{IntegrityReport, IntegrityService};
use crate::services::journal::JournalService;
use crate::services::metadata::MetadataService;
use crate::services::quota::{QuotaReport, QuotaService};
use crate::services::recovery::RecoveryService;
use crate::services::security::SecurityService;
use crate::services::sync::SyncService;
use crate::services::versioning::VersioningService;
use crate::storage::allocator::InodeAllocator;
use crate::storage::block_store::BlockStore;
use crate::storage::catalog::Catalog;
use crate::storage::persistence;
use crate::storage::volume_image;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub inode: InodeId,
    pub kind: InodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataView {
    pub path: String,
    pub tags: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub storage_tier: StorageTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub config: CoreFsConfig,
    pub volume: VolumeDescriptor,
    pub active_inodes: Vec<Inode>,
    pub deleted_inodes: Vec<Inode>,
    pub block_records: Vec<crate::storage::block_store::BlockRecord>,
    pub journal_entries: Vec<crate::services::journal::JournalEntry>,
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
    metadata: MetadataService,
    quota: QuotaService,
    security: SecurityService,
    sync: SyncService,
    snapshots: Vec<Snapshot>,
    next_snapshot_id: u64,
}

impl CoreFsService {
    pub fn format(config: CoreFsConfig) -> Self {
        let volume = VolumeDescriptor::from_config(&config);
        let mut journal = JournalService::default();
        journal.record("format", "/", format!("volume={}", volume.name));

        Self {
            config,
            volume,
            allocator: InodeAllocator::default(),
            catalog: Catalog::default(),
            blocks: BlockStore::default(),
            journal,
            versioning: VersioningService::default(),
            recovery: RecoveryService::default(),
            integrity: IntegrityService,
            indexing: IndexingService,
            metadata: MetadataService,
            quota: QuotaService,
            security: SecurityService,
            sync: SyncService::default(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        }
    }

    pub fn create_file(&mut self, path: &str, bytes: &[u8], tags: &[String]) -> CoreFsResult<()> {
        validate_path(path)?;
        self.ensure_path_is_absent(path)?;
        self.ensure_parent_directory(path)?;

        let inode_id = self.allocator.allocate();
        let mut metadata = FileMetadata::default();
        metadata.tags = tags.to_vec();
        metadata.content_class = self.indexing.classify_path(path);
        metadata.encrypted = self
            .security
            .mark_encrypted(self.config.security.encryption_at_rest);
        metadata.compressed = self.config.performance.compression_enabled;
        metadata.acl = vec![AclEntry::full_access(Principal::Role("system".to_string()))];

        let mut inode = Inode::new(inode_id, InodeKind::File, path.to_string(), metadata);
        self.ensure_quota_allows(1, bytes.len() as isize)?;
        inode.size = self.blocks.write(inode_id, bytes.to_vec());

        if self.config.versioning.keep_latest > 0 {
            self.versioning.store_version(path, bytes.to_vec());
            self.versioning
                .prune(path, self.config.versioning.keep_latest);
        }

        self.catalog.insert(inode);
        self.journal
            .record("create_file", path, format!("bytes={}", bytes.len()));
        Ok(())
    }

    pub fn create_directory(&mut self, path: &str) -> CoreFsResult<()> {
        validate_path(path)?;
        if path == "/" {
            return Ok(());
        }
        self.ensure_path_is_absent(path)?;
        self.ensure_parent_directory(path)?;

        let inode_id = self.allocator.allocate();
        let inode = Inode::new(
            inode_id,
            InodeKind::Directory,
            path.to_string(),
            FileMetadata::default(),
        );
        self.catalog.insert(inode);
        self.journal.record("create_directory", path, "");
        Ok(())
    }

    pub fn create_symlink(&mut self, path: &str, target: &str) -> CoreFsResult<()> {
        validate_path(path)?;
        self.ensure_path_is_absent(path)?;
        self.ensure_parent_directory(path)?;

        let inode_id = self.allocator.allocate();
        let mut inode = Inode::new(
            inode_id,
            InodeKind::Symlink,
            path.to_string(),
            FileMetadata::default(),
        );
        self.ensure_quota_allows(1, target.len() as isize)?;
        inode.size = self.blocks.write(inode_id, target.as_bytes().to_vec());
        self.catalog.insert(inode);
        self.journal
            .record("create_symlink", path, format!("target={target}"));
        Ok(())
    }

    pub fn write_file(&mut self, path: &str, bytes: &[u8]) -> CoreFsResult<()> {
        self.write_file_range(path, 0, bytes)?;
        Ok(())
    }

    pub fn write_file_range(
        &mut self,
        path: &str,
        offset: usize,
        bytes: &[u8],
    ) -> CoreFsResult<usize> {
        let existing = self.read_file(path)?;
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;

        if inode.kind != InodeKind::File {
            return Err(CoreFsError::InvalidInput(format!(
                "writes are only supported for files: {path}"
            )));
        }

        let mut merged = existing;
        if merged.len() < offset {
            merged.resize(offset, 0);
        }
        let required_len = offset.saturating_add(bytes.len());
        if merged.len() < required_len {
            merged.resize(required_len, 0);
        }
        merged[offset..offset + bytes.len()].copy_from_slice(bytes);
        let byte_delta = merged.len() as isize - inode.size as isize;
        self.ensure_quota_allows(0, byte_delta)?;

        inode.modified_at = SystemTime::now();
        inode.size = self.blocks.write(inode.id, merged.clone());
        self.versioning.store_version(path, merged);
        self.versioning
            .prune(path, self.config.versioning.keep_latest);
        self.journal.record(
            "write_file",
            path,
            format!("offset={offset} bytes={}", bytes.len()),
        );
        Ok(bytes.len())
    }

    pub fn read_file(&self, path: &str) -> CoreFsResult<Vec<u8>> {
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        if inode.kind == InodeKind::Directory {
            return Err(CoreFsError::InvalidInput(format!(
                "directories cannot be read as files: {path}"
            )));
        }
        let record = self
            .blocks
            .read(inode.id)
            .ok_or_else(|| CoreFsError::State(format!("missing data blocks for {path}")))?;
        Ok(record.bytes.clone())
    }

    pub fn read_file_range(&self, path: &str, offset: usize, size: usize) -> CoreFsResult<Vec<u8>> {
        let bytes = self.read_file(path)?;
        if offset >= bytes.len() {
            return Ok(Vec::new());
        }
        let end = bytes.len().min(offset.saturating_add(size));
        Ok(bytes[offset..end].to_vec())
    }

    pub fn delete_file(&mut self, path: &str, secure: bool) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        if inode.kind == InodeKind::Directory {
            return Err(CoreFsError::InvalidInput(format!(
                "delete_file only supports regular files and symlinks: {path}"
            )));
        }
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
            self.journal.record("secure_delete", path, "blocks_zeroed");
        } else {
            self.recovery.remember(inode.clone());
            self.catalog.move_to_deleted(inode.clone());
            self.journal.record("delete", path, "recoverable=true");
        }

        Ok(())
    }

    pub fn remove_directory(&mut self, path: &str) -> CoreFsResult<()> {
        validate_path(path)?;
        if path == "/" {
            return Err(CoreFsError::InvalidInput(
                "root directory cannot be removed".to_string(),
            ));
        }

        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        if inode.kind != InodeKind::Directory {
            return Err(CoreFsError::InvalidInput(format!(
                "remove_directory only supports directories: {path}"
            )));
        }
        if !self.list_directory(path)?.is_empty() {
            return Err(CoreFsError::PolicyViolation(format!(
                "directory is not empty: {path}"
            )));
        }

        let inode = self
            .catalog
            .remove(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        self.recovery.remember(inode.clone());
        self.catalog.move_to_deleted(inode);
        self.journal
            .record("delete_directory", path, "recoverable=true");
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
        self.journal.record("restore", path, "");
        Ok(())
    }

    pub fn create_snapshot(&mut self, name: &str) -> Snapshot {
        self.create_snapshot_for_subtree(name, "/")
            .expect("root snapshot should always be valid")
    }

    pub fn create_snapshot_for_subtree(
        &mut self,
        name: &str,
        root_path: &str,
    ) -> CoreFsResult<Snapshot> {
        validate_path(root_path)?;
        if root_path != "/" {
            let inode = self
                .catalog
                .get(root_path)
                .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {root_path}")))?;
            if inode.kind != InodeKind::Directory {
                return Err(CoreFsError::InvalidInput(format!(
                    "snapshot roots must be directories: {root_path}"
                )));
            }
        }

        self.next_snapshot_id += 1;
        let snapshot = Snapshot {
            id: self.next_snapshot_id,
            name: name.to_string(),
            scope_root: root_path.to_string(),
            created_at: SystemTime::now(),
            paths: self.paths_in_subtree(root_path),
        };
        self.journal
            .record("snapshot", root_path, format!("name={name}"));
        self.snapshots.push(snapshot.clone());
        Ok(snapshot)
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> CoreFsResult<()> {
        let state = self.persisted_state();
        persistence::save_state(path, &state)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let state = persistence::load_state(path)?;
        Ok(Self::from_persisted_state(state))
    }

    pub fn save_image_to_path(&self, path: impl AsRef<Path>) -> CoreFsResult<()> {
        let state = self.persisted_state();
        volume_image::save_volume_image(path, &state)
    }

    pub fn load_image_from_path(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let state = volume_image::load_volume_image(path)?;
        Ok(Self::from_persisted_state(state))
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

    pub fn truncate_file(&mut self, path: &str, size: usize) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        self.ensure_quota_allows(0, size as isize - inode.size as isize)?;
        let mut bytes = self.read_file(path)?;
        bytes.resize(size, 0);
        self.write_file(path, &bytes)
    }

    pub fn rename_path(&mut self, old_path: &str, new_path: &str) -> CoreFsResult<()> {
        validate_path(old_path)?;
        validate_path(new_path)?;
        if old_path == "/" {
            return Err(CoreFsError::InvalidInput(
                "root directory cannot be renamed".to_string(),
            ));
        }
        if old_path == new_path {
            return Ok(());
        }

        let source = self
            .catalog
            .get(old_path)
            .cloned()
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {old_path}")))?;
        self.ensure_parent_directory(new_path)?;
        if self.catalog.get(new_path).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {new_path}"
            )));
        }
        if source.kind == InodeKind::Directory && is_descendant_path(new_path, old_path) {
            return Err(CoreFsError::InvalidInput(format!(
                "cannot move directory into its own subtree: {old_path} -> {new_path}"
            )));
        }

        let mut active = self.catalog.active_entries();
        for inode in &mut active {
            if inode.path == old_path {
                inode.path = new_path.to_string();
                inode.modified_at = SystemTime::now();
            } else if is_descendant_path(&inode.path, old_path) {
                inode.path = rebase_path(&inode.path, old_path, new_path);
                inode.modified_at = SystemTime::now();
            }
        }
        self.catalog.replace_active_entries(active);
        self.versioning.remap_prefix(old_path, new_path);
        self.journal
            .record("rename", old_path, format!("new_path={new_path}"));
        Ok(())
    }

    pub fn read_symlink(&self, path: &str) -> CoreFsResult<String> {
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        if inode.kind != InodeKind::Symlink {
            return Err(CoreFsError::InvalidInput(format!(
                "path is not a symlink: {path}"
            )));
        }
        Ok(String::from_utf8_lossy(&self.read_file(path)?).into_owned())
    }

    pub fn get_inode(&self, path: &str) -> Option<Inode> {
        if path == "/" {
            Some(self.root_inode())
        } else {
            self.catalog.get(path).cloned()
        }
    }

    pub fn get_inode_by_id(&self, inode_id: InodeId) -> Option<Inode> {
        self.catalog.inode_by_id(inode_id).cloned()
    }

    pub fn path_for_inode(&self, inode_id: InodeId) -> Option<String> {
        self.catalog
            .inode_by_id(inode_id)
            .map(|inode| inode.path.clone())
    }

    pub fn list_directory(&self, path: &str) -> CoreFsResult<Vec<DirectoryEntry>> {
        validate_path(path)?;
        if path != "/" {
            let inode = self
                .catalog
                .get(path)
                .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
            if inode.kind != InodeKind::Directory {
                return Err(CoreFsError::InvalidInput(format!(
                    "path is not a directory: {path}"
                )));
            }
        }

        let mut entries = Vec::new();
        for inode in self.catalog.active_entries() {
            if let Some(name) = direct_child_name(path, &inode.path) {
                entries.push(DirectoryEntry {
                    name,
                    path: inode.path.clone(),
                    inode: inode.id,
                    kind: inode.kind,
                });
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    pub fn quota_report(&self) -> QuotaReport {
        self.quota
            .report(&self.config.quotas, &self.catalog.active_entries())
    }

    pub fn list_versions_for_path(
        &self,
        path: &str,
    ) -> CoreFsResult<Vec<crate::services::versioning::FileVersion>> {
        validate_path(path)?;
        Ok(self.versioning.list_versions(path).to_vec())
    }

    pub fn read_version_selector(&self, selector: &str) -> CoreFsResult<Vec<u8>> {
        let (path, query) = parse_version_selector(selector)?;
        let version = match query {
            VersionQuery::Latest => self.versioning.latest_version(path),
            VersionQuery::VersionId(version_id) => self.versioning.version_by_id(path, version_id),
            VersionQuery::Timestamp(instant) => self.versioning.version_at_or_before(path, instant),
        }
        .ok_or_else(|| {
            CoreFsError::NotFound(format!("version not found for selector: {selector}"))
        })?;

        Ok(version.bytes.clone())
    }

    pub fn metadata_for_path(&self, path: &str) -> CoreFsResult<MetadataView> {
        validate_path(path)?;
        let inode = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        Ok(MetadataView {
            path: path.to_string(),
            tags: inode.metadata.tags.clone(),
            attributes: inode.metadata.attributes.clone(),
            storage_tier: inode.metadata.storage_tier.clone(),
        })
    }

    pub fn add_tag(&mut self, path: &str, tag: &str) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        self.metadata.add_tag(inode, tag);
        inode.modified_at = SystemTime::now();
        self.journal.record("tag_add", path, format!("tag={tag}"));
        Ok(())
    }

    pub fn remove_tag(&mut self, path: &str, tag: &str) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        self.metadata.remove_tag(inode, tag);
        inode.modified_at = SystemTime::now();
        self.journal
            .record("tag_remove", path, format!("tag={tag}"));
        Ok(())
    }

    pub fn set_attribute(&mut self, path: &str, key: &str, value: &str) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        self.metadata.set_attribute(inode, key, value);
        inode.modified_at = SystemTime::now();
        self.journal
            .record("attribute_set", path, format!("key={key} value={value}"));
        Ok(())
    }

    pub fn set_storage_tier(&mut self, path: &str, tier: StorageTier) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        self.metadata.set_storage_tier(inode, tier.clone());
        inode.modified_at = SystemTime::now();
        self.journal
            .record("storage_tier", path, format!("tier={}", tier_name(&tier)));
        Ok(())
    }

    pub fn find_by_tag(&self, tag: &str) -> Vec<String> {
        let mut matches = self
            .catalog
            .active_entries()
            .into_iter()
            .filter(|inode| inode.metadata.tags.iter().any(|candidate| candidate == tag))
            .map(|inode| inode.path)
            .collect::<Vec<_>>();
        matches.sort();
        matches
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
        if path == "/" {
            Some(InodeId(0))
        } else {
            self.catalog.get(path).map(|inode| inode.id)
        }
    }

    fn persisted_state(&self) -> PersistedState {
        PersistedState {
            config: self.config.clone(),
            volume: self.volume.clone(),
            active_inodes: self.catalog.active_entries(),
            deleted_inodes: self.catalog.deleted_entries(),
            block_records: self.blocks.records(),
            journal_entries: self.journal.entries().to_vec(),
            versions: self.versioning.all_versions(),
            sync_statuses: self.sync.statuses().to_vec(),
            snapshots: self.snapshots.clone(),
            next_snapshot_id: self.next_snapshot_id,
        }
    }

    fn from_persisted_state(state: PersistedState) -> Self {
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
            blocks: BlockStore::from_records(state.block_records),
            journal: JournalService::from_entries(state.journal_entries),
            versioning: VersioningService::from_versions(state.versions),
            recovery,
            integrity: IntegrityService,
            indexing: IndexingService,
            metadata: MetadataService,
            quota: QuotaService,
            security: SecurityService,
            sync: SyncService::from_statuses(state.sync_statuses),
            snapshots: state.snapshots,
            next_snapshot_id: state.next_snapshot_id,
        };
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

    fn ensure_path_is_absent(&self, path: &str) -> CoreFsResult<()> {
        if self.catalog.get(path).is_some() || path == "/" {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {path}"
            )));
        }
        Ok(())
    }

    fn ensure_parent_directory(&self, path: &str) -> CoreFsResult<()> {
        let parent = parent_path(path);
        if parent == "/" {
            return Ok(());
        }
        let inode = self.catalog.get(parent).ok_or_else(|| {
            CoreFsError::NotFound(format!("parent directory not found: {parent}"))
        })?;
        if inode.kind != InodeKind::Directory {
            return Err(CoreFsError::InvalidInput(format!(
                "parent is not a directory: {parent}"
            )));
        }
        Ok(())
    }

    fn ensure_quota_allows(&self, file_delta: isize, byte_delta: isize) -> CoreFsResult<()> {
        self.quota.enforce_delta(
            &self.config.quotas,
            &self.catalog.active_entries(),
            file_delta,
            byte_delta,
        )
    }

    fn root_inode(&self) -> Inode {
        Inode::new(
            InodeId(0),
            InodeKind::Directory,
            "/".to_string(),
            FileMetadata::default(),
        )
    }

    fn paths_in_subtree(&self, root_path: &str) -> Vec<String> {
        let mut paths = self
            .catalog
            .list_paths()
            .into_iter()
            .filter(|candidate| {
                candidate == root_path
                    || root_path == "/"
                    || is_descendant_path(candidate, root_path)
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
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
    if path != "/" && path.ends_with('/') {
        return Err(CoreFsError::InvalidInput(format!(
            "paths must not end with '/': {path}"
        )));
    }
    Ok(())
}

fn parent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

fn direct_child_name(parent: &str, path: &str) -> Option<String> {
    if parent == "/" {
        let remainder = path.strip_prefix('/')?;
        if remainder.is_empty() || remainder.contains('/') {
            return None;
        }
        return Some(remainder.to_string());
    }

    let prefix = format!("{parent}/");
    let remainder = path.strip_prefix(&prefix)?;
    if remainder.is_empty() || remainder.contains('/') {
        return None;
    }
    Some(remainder.to_string())
}

fn is_descendant_path(path: &str, prefix: &str) -> bool {
    path.len() > prefix.len()
        && path.starts_with(prefix)
        && path.as_bytes().get(prefix.len()) == Some(&b'/')
}

fn rebase_path(path: &str, old_prefix: &str, new_prefix: &str) -> String {
    if path == old_prefix {
        new_prefix.to_string()
    } else {
        format!("{new_prefix}/{}", &path[old_prefix.len() + 1..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionQuery {
    Latest,
    VersionId(u64),
    Timestamp(SystemTime),
}

fn parse_version_selector(selector: &str) -> CoreFsResult<(&str, VersionQuery)> {
    let (path, suffix) = selector.rsplit_once('@').ok_or_else(|| {
        CoreFsError::InvalidInput(format!("version selector must contain '@': {selector}"))
    })?;
    validate_path(path)?;

    if suffix == "latest" {
        return Ok((path, VersionQuery::Latest));
    }
    if let Some(raw) = suffix.strip_prefix('v') {
        let version_id = raw.parse::<u64>().map_err(|error| {
            CoreFsError::InvalidInput(format!(
                "invalid version id in selector {selector}: {error}"
            ))
        })?;
        return Ok((path, VersionQuery::VersionId(version_id)));
    }

    Ok((
        path,
        VersionQuery::Timestamp(parse_timestamp_selector(suffix)?),
    ))
}

fn parse_timestamp_selector(value: &str) -> CoreFsResult<SystemTime> {
    let normalized = value.replace('T', "-").replace(':', "-");
    let parts = normalized
        .split('-')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(CoreFsError::InvalidInput(format!(
            "invalid timestamp selector: {value}"
        )));
    }

    let year = parse_i64(parts[0], "year", value)?;
    let month = parse_i64(parts[1], "month", value)?;
    let day = parse_i64(parts[2], "day", value)?;
    let hour = parse_i64(parts[3], "hour", value)?;
    let minute = parse_i64(parts[4], "minute", value)?;
    let second = parse_i64(parts[5], "second", value)?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return Err(CoreFsError::InvalidInput(format!(
            "timestamp selector out of range: {value}"
        )));
    }

    let days = days_from_civil(year, month, day)?;
    let total_seconds = days
        .checked_mul(86_400)
        .and_then(|base| base.checked_add(hour * 3_600 + minute * 60 + second))
        .ok_or_else(|| {
            CoreFsError::InvalidInput(format!("timestamp selector overflow: {value}"))
        })?;

    if total_seconds < 0 {
        return Err(CoreFsError::InvalidInput(format!(
            "timestamp selector predates unix epoch: {value}"
        )));
    }

    Ok(UNIX_EPOCH + Duration::from_secs(total_seconds as u64))
}

fn parse_i64(value: &str, label: &str, original: &str) -> CoreFsResult<i64> {
    value.parse::<i64>().map_err(|error| {
        CoreFsError::InvalidInput(format!(
            "invalid {label} in timestamp selector {original}: {error}"
        ))
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> CoreFsResult<i64> {
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let yoe = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let max_day = match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 31,
    };
    if day > max_day {
        return Err(CoreFsError::InvalidInput(format!(
            "invalid day {day} for month {month} in timestamp selector"
        )));
    }
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Ok(era * 146_097 + doe - 719_468)
}

fn tier_name(tier: &StorageTier) -> &'static str {
    match tier {
        StorageTier::Hot => "hot",
        StorageTier::Warm => "warm",
        StorageTier::Cold => "cold",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn list_directory_and_rename_support_nested_paths() {
        let mut fs = test_fs();
        fs.create_directory("/srv").expect("srv");
        fs.create_directory("/srv/corefs").expect("corefs");
        fs.create_file("/srv/corefs/a.txt", b"alpha", &[])
            .expect("file");

        let entries = fs.list_directory("/srv/corefs").expect("directory listing");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");

        fs.rename_path("/srv/corefs", "/srv/platform")
            .expect("rename");
        assert!(fs.get_inode("/srv/platform").is_some());
        assert!(fs.get_inode("/srv/platform/a.txt").is_some());
        assert_eq!(
            fs.read_file("/srv/platform/a.txt").expect("renamed file"),
            b"alpha".to_vec()
        );
    }

    #[test]
    fn remove_directory_requires_empty_directory() {
        let mut fs = test_fs();
        fs.create_directory("/data").expect("data");
        fs.create_file("/data/file.txt", b"payload", &[])
            .expect("file");
        assert!(matches!(
            fs.remove_directory("/data"),
            Err(CoreFsError::PolicyViolation(_))
        ));

        fs.delete_file("/data/file.txt", false).expect("delete");
        fs.remove_directory("/data").expect("remove");
        assert!(fs.get_inode("/data").is_none());
    }

    #[test]
    fn state_can_be_saved_and_loaded_again() {
        let path = std::env::temp_dir().join(format!(
            "corefs-service-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));

        let mut fs = test_fs();
        fs.create_directory("/etc").expect("dir");
        fs.create_file("/etc/config.txt", b"cfg", &["config".to_string()])
            .expect("file");
        fs.create_snapshot("baseline");
        fs.mark_synced("/etc/config.txt", "node-a").expect("sync");
        fs.delete_file("/etc/config.txt", false).expect("delete");
        fs.save_to_path(&path).expect("save should succeed");

        let loaded = CoreFsService::load_from_path(&path).expect("load should succeed");

        assert_eq!(loaded.volume_name(), "corefs");
        assert!(loaded.list_paths().iter().any(|path| path == "/etc"));
        assert_eq!(
            loaded.recoverable_paths(),
            vec!["/etc/config.txt".to_string()]
        );
        assert_eq!(loaded.snapshot_names(), vec!["baseline".to_string()]);
        assert_eq!(loaded.synced_paths(), 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn loading_invalid_state_returns_error() {
        let path = std::env::temp_dir().join(format!(
            "corefs-invalid-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));
        std::fs::write(&path, b"not-json").expect("test file should be written");

        let result = CoreFsService::load_from_path(&path);
        assert!(matches!(result, Err(CoreFsError::State(_))));

        let _ = std::fs::remove_file(path);
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
}
