mod pathing;
mod selectors;
#[cfg(test)]
mod tests;
mod types;

pub use types::{AdminReport, DirectoryEntry, FsStats, MetadataView, PersistedState};

use crate::config::{CoreFsConfig, StorageTier};
use crate::domain::acl::{AclEntry, Principal};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::runtime::RuntimeIntegrationBlueprint;
use crate::platform::tools::ToolRegistry;
use crate::services::hot_paths::{HotPathEntry, HotPathService};
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
use pathing::{direct_child_name, is_descendant_path, parent_path, rebase_path, validate_path};
use selectors::{VersionQuery, parse_version_selector, tier_name};
use std::cell::RefCell;
use std::path::Path;
use std::time::SystemTime;

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
    hot_paths: RefCell<HotPathService>,
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
            hot_paths: RefCell::new(HotPathService::default()),
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
        self.hot_paths.borrow_mut().record_write(path, bytes.len());
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
        self.hot_paths.borrow_mut().record_write(path, target.len());
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
        let current_size = self
            .catalog
            .get(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?
            .size;
        let mut merged = existing;
        if merged.len() < offset {
            merged.resize(offset, 0);
        }
        let required_len = offset.saturating_add(bytes.len());
        if merged.len() < required_len {
            merged.resize(required_len, 0);
        }
        merged[offset..offset + bytes.len()].copy_from_slice(bytes);
        let byte_delta = merged.len() as isize - current_size as isize;
        self.ensure_quota_allows(0, byte_delta)?;

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
        inode.size = self.blocks.write(inode.id, merged.clone());
        self.versioning.store_version(path, merged);
        self.versioning
            .prune(path, self.config.versioning.keep_latest);
        self.hot_paths.borrow_mut().record_write(path, bytes.len());
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
        self.hot_paths
            .borrow_mut()
            .record_read(path, record.bytes.len());
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

    pub fn hot_paths(&self, limit: usize) -> Vec<HotPathEntry> {
        self.hot_paths.borrow().hottest_paths(limit)
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
            attributes: MetadataService::resolve_attributes(inode, |target_path| {
                self.read_file(target_path)
            }),
            storage_tier: inode.metadata.storage_tier.clone(),
        })
    }

    pub fn add_tag(&mut self, path: &str, tag: &str) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        MetadataService::add_tag(inode, tag);
        inode.modified_at = SystemTime::now();
        self.journal.record("tag_add", path, format!("tag={tag}"));
        Ok(())
    }

    pub fn remove_tag(&mut self, path: &str, tag: &str) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        MetadataService::remove_tag(inode, tag);
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
        MetadataService::set_attribute(inode, key, value);
        inode.modified_at = SystemTime::now();
        self.journal
            .record("attribute_set", path, format!("key={key} value={value}"));
        Ok(())
    }

    pub fn set_content_pointer_attribute(
        &mut self,
        path: &str,
        key: &str,
        target_path: &str,
        extractor: &str,
    ) -> CoreFsResult<()> {
        validate_path(target_path)?;
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        MetadataService::set_content_pointer(inode, key, target_path, extractor);
        inode.modified_at = SystemTime::now();
        self.journal.record(
            "attribute_pointer_set",
            path,
            format!("key={key} target={target_path} extractor={extractor}"),
        );
        Ok(())
    }

    pub fn set_storage_tier(&mut self, path: &str, tier: StorageTier) -> CoreFsResult<()> {
        let inode = self
            .catalog
            .get_mut(path)
            .ok_or_else(|| CoreFsError::NotFound(format!("path not found: {path}")))?;
        MetadataService::set_storage_tier(inode, tier.clone());
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

    pub fn find_by_attribute_term(&self, term: &str) -> Vec<String> {
        let mut matches = self
            .catalog
            .active_entries()
            .into_iter()
            .filter(|inode| {
                MetadataService::resolve_attributes(inode, |target_path| {
                    self.read_file(target_path)
                })
                .into_iter()
                .any(|(_, value)| value.contains(term))
            })
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
            hot_path_records: self.hot_paths.borrow().records(),
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
            hot_paths: RefCell::new(HotPathService::from_records(state.hot_path_records)),
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
