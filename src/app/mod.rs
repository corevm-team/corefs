use crate::config::CoreFsConfig;
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
use crate::services::recovery::RecoveryService;
use crate::services::security::SecurityService;
use crate::services::sync::SyncService;
use crate::services::versioning::VersioningService;
use crate::storage::allocator::InodeAllocator;
use crate::storage::block_store::BlockStore;
use crate::storage::catalog::Catalog;
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
            security: SecurityService,
            sync: SyncService::default(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        }
    }

    pub fn create_file(&mut self, path: &str, bytes: &[u8], tags: &[String]) -> CoreFsResult<()> {
        validate_path(path)?;
        if self.catalog.get(path).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {path}"
            )));
        }

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
        if self.catalog.get(path).is_some() {
            return Err(CoreFsError::AlreadyExists(format!(
                "path already exists: {path}"
            )));
        }

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
        self.journal
            .record("create_symlink", path, format!("target={target}"));
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
        self.versioning.store_version(path, bytes.to_vec());
        self.versioning
            .prune(path, self.config.versioning.keep_latest);
        self.journal
            .record("write_file", path, format!("bytes={}", bytes.len()));
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
            self.journal.record("secure_delete", path, "blocks_zeroed");
        } else {
            self.recovery.remember(inode.clone());
            self.catalog.move_to_deleted(inode.clone());
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
        self.journal.record("restore", path, "");
        Ok(())
    }

    pub fn create_snapshot(&mut self, name: &str) -> Snapshot {
        self.next_snapshot_id += 1;
        let snapshot = Snapshot {
            id: self.next_snapshot_id,
            name: name.to_string(),
            created_at: SystemTime::now(),
            paths: self.catalog.list_paths(),
        };
        self.journal.record("snapshot", "/", format!("name={name}"));
        self.snapshots.push(snapshot.clone());
        snapshot
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
