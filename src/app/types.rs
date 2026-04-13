use crate::config::{CoreFsConfig, StorageTier};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::platform::runtime::RuntimeIntegrationBlueprint;
use crate::platform::tools::ToolRegistry;
use crate::storage::block_store::{AllocatorPolicy, FreeExtentRecord};
use serde::{Deserialize, Serialize};

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
    pub allocator_policy: AllocatorPolicy,
    pub free_extents: Vec<FreeExtentRecord>,
    pub block_records: Vec<crate::storage::block_store::BlockRecord>,
    pub journal_entries: Vec<crate::services::journal::JournalEntry>,
    pub versions: Vec<crate::services::versioning::FileVersion>,
    pub sync_statuses: Vec<crate::services::sync::SyncStatus>,
    pub hot_path_records: Vec<crate::services::hot_paths::HotPathRecord>,
    pub snapshots: Vec<Snapshot>,
    pub next_snapshot_id: u64,
}
