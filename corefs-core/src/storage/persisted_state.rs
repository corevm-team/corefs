// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutraler `PersistedState`-Aggregat-Typ.
//!
//! [`PersistedState`] aggregiert den vollständigen Zustand eines CoreFS-Volumes
//! in einer einzigen, serialisierbaren Struktur. Sie ist die Wire-Repräsentation,
//! die von `storage::ondisk` gelesen und geschrieben wird, und der Snapshot,
//! den der App-Layer beim Mount erzeugt bzw. beim Unmount persistiert.
//!
//! Der Typ liegt bewusst in `corefs-core`, damit `storage::ondisk::*`-Module
//! (`volume`, `native`, `grouped`, …) plattformneutral bleiben können, ohne
//! die `crate::app`-Schicht zu importieren.
//!
//! ## Verhältnis zu `crate::app::CoreFsService`
//!
//! Der `CoreFsService` im main `corefs` crate baut [`PersistedState`] aus
//! seinem internen Zustand auf bzw. hydriert sich aus einem geladenen
//! [`PersistedState`]. Diese Konstruktions- und Hydrations-Pfade selbst
//! bleiben im main crate, weil sie an die std-/FUSE-/Service-Schicht
//! gebunden sind.

use crate::config::CoreFsConfig;
use crate::domain::inode::Inode;
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::services::hot_paths::HotPathRecord;
use crate::services::journal::{JournalEntry, JournalRuntimeState};
use crate::services::sync::SyncStatus;
use crate::services::versioning::FileVersion;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord, FreeExtentRecord};
use crate::storage::volume_wal::VolumeWal;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Vollständiger, persistierbarer Zustand eines CoreFS-Volumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    /// Volume-Konfiguration.
    pub config: CoreFsConfig,
    /// Volume-Beschreibung (Name, Größe, Feature-Flags, Erstellungszeit).
    pub volume: VolumeDescriptor,
    /// `true`, wenn der letzte Unmount sauber war.
    pub clean_unmount: bool,
    /// Pending Write-Ahead-Log einer noch offenen Transaktion (falls vorhanden).
    pub pending_wal: Option<VolumeWal>,
    /// Aktive (nicht gelöschte) Inodes.
    pub active_inodes: Vec<Inode>,
    /// Soft-gelöschte Inodes (über Recovery-Fenster wiederherstellbar).
    pub deleted_inodes: Vec<Inode>,
    /// Persistierte Allocator-Policy.
    pub allocator_policy: AllocatorPolicy,
    /// Frei stehende Extents im Block-Store.
    pub free_extents: Vec<FreeExtentRecord>,
    /// Hot-Path-Telemetrie für gezielte Reallocation.
    pub hot_path_records: Vec<HotPathRecord>,
    /// In-memory Block-Records (Inhalte je Inode).
    pub block_records: Vec<BlockRecord>,
    /// Journal-Einträge (audit log).
    pub journal_entries: Vec<JournalEntry>,
    /// Journal-Laufzeitzustand (Counter, etc.).
    pub journal_runtime: JournalRuntimeState,
    /// Versionshistorie pro Datei.
    pub versions: Vec<FileVersion>,
    /// Sync-Status pro Datei.
    pub sync_statuses: Vec<SyncStatus>,
    /// Snapshots.
    pub snapshots: Vec<Snapshot>,
    /// Nächste zu vergebende Snapshot-ID.
    pub next_snapshot_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::journal::JournalRuntimeState;

    #[test]
    fn persisted_state_serde_round_trips() {
        let state = PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config_at(
                &CoreFsConfig::default(),
                crate::platform::Timestamp::EPOCH,
            ),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: Vec::new(),
            deleted_inodes: Vec::new(),
            allocator_policy: AllocatorPolicy::default(),
            free_extents: Vec::new(),
            hot_path_records: Vec::new(),
            block_records: Vec::new(),
            journal_entries: Vec::new(),
            journal_runtime: JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        };
        let bytes = bincode::serialize(&state).expect("serialize ok");
        let decoded: PersistedState = bincode::deserialize(&bytes).expect("deserialize ok");
        assert_eq!(state, decoded);
    }
}
