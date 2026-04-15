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
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::services::hot_paths::HotPathRecord;
use crate::services::journal::{JournalEntry, JournalRuntimeState};
use crate::services::sync::SyncStatus;
use crate::services::versioning::FileVersion;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord, FreeExtentRecord};
use crate::storage::volume_wal::VolumeWal;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Report einer [`PersistedState::restore_snapshot_at`]-Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// ID des wiederhergestellten Snapshots.
    pub snapshot_id: u64,
    /// Snapshot-Name (zur Bestätigung).
    pub snapshot_name: String,
    /// Anzahl regulärer Dateien, deren Inhalt oder Metadaten zurückgespielt wurden.
    pub restored_files: usize,
    /// Anzahl Verzeichnisse, deren Metadaten zurückgespielt wurden.
    pub restored_dirs: usize,
    /// Pfade, die nicht wiederhergestellt werden konnten (mit Begründung).
    pub skipped_paths: Vec<String>,
}

impl PersistedState {
    /// Konstruiert einen leeren `PersistedState` aus einer Konfiguration und
    /// einem Erstellungszeitstempel.
    ///
    /// no_std-fähig: der Aufrufer liefert die Zeit explizit. Alle Listen
    /// (Inodes, Block-Records, Journal-Entries, Snapshots, …) starten leer;
    /// `next_snapshot_id` ist `0`, `clean_unmount` ist `true`,
    /// `pending_wal` ist `None`.
    ///
    /// Diese Konstruktion ist die Grundlage für den NATIVE-Mode-Initialisierungs-
    /// Pfad: nach [`crate::storage::ondisk::volume::format_device`] wird
    /// `save_state_native(device, &PersistedState::empty_at(...))` aufgerufen,
    /// um das Volume in das Native-Layout zu überführen.
    #[must_use]
    pub fn empty_at(config: CoreFsConfig, created_at: crate::platform::Timestamp) -> Self {
        let volume = VolumeDescriptor::from_config_at(&config, created_at);
        Self {
            config,
            volume,
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
        }
    }

    /// Konstruiert einen leeren `PersistedState` mit der aktuellen Systemzeit.
    ///
    /// Bequemere Variante von [`PersistedState::empty_at`] für std-basierte
    /// Umgebungen.
    #[cfg(feature = "std")]
    #[must_use]
    pub fn empty(config: CoreFsConfig) -> Self {
        Self::empty_at(config, crate::platform::Timestamp::now())
    }

    /// Defragmentiert den Block-Store-Anteil dieses `PersistedState` in-place.
    ///
    /// Konstruiert temporär einen [`crate::storage::block_store::BlockStore`]
    /// aus den aktuellen `block_records`, `allocator_policy` und
    /// `free_extents`, ruft [`crate::storage::block_store::BlockStore::defragment`]
    /// auf und schreibt das defragmentierte Ergebnis zurück in den Zustand.
    /// Liefert den Defragmentation-Report.
    ///
    /// no_std-fähig — der Aufrufer muss anschließend persistieren (z. B. via
    /// [`crate::storage::ondisk::session::OdfDeviceSession::flush`]).
    /// Liefert die nächste freie Inode-ID: `max(existing) + 1`, minimum `1`.
    ///
    /// Scannt sowohl `active_inodes` als auch `deleted_inodes`, damit ein
    /// frisch vergebener `InodeId` garantiert nicht mit einem soft-gelöschten
    /// Eintrag kollidiert.
    #[must_use]
    pub fn next_inode_id(&self) -> InodeId {
        let mut max_id = 0u64;
        for inode in &self.active_inodes {
            if inode.id.0 > max_id {
                max_id = inode.id.0;
            }
        }
        for inode in &self.deleted_inodes {
            if inode.id.0 > max_id {
                max_id = inode.id.0;
            }
        }
        InodeId(max_id + 1)
    }

    /// Wenn der gegebene Pfad unter `deleted_inodes` steht, wird er nach
    /// `active_inodes` zurückverschoben. Kein-op andernfalls. Liefert
    /// `true`, wenn eine Resurrection stattgefunden hat.
    pub fn resurrect_inode_if_soft_deleted(&mut self, path: &str) -> bool {
        if let Some(pos) = self.deleted_inodes.iter().position(|i| i.path == path) {
            let inode = self.deleted_inodes.remove(pos);
            self.active_inodes.push(inode);
            return true;
        }
        false
    }

    /// Stellt den Snapshot mit der gegebenen ID wieder her.
    ///
    /// Semantik:
    /// * Für jeden Pfad im Snapshot werden kind, size, timestamps und
    ///   metadata auf die Snapshot-Werte gesetzt.
    /// * Existiert der Pfad bereits als aktiver Inode, wird dieser überschrieben
    ///   (sofern die Kinds zueinander passen — andernfalls: skip).
    /// * Fehlt der Pfad, wird ein neuer Inode angelegt (neue InodeId via
    ///   [`Self::next_inode_id`]).
    /// * Enthält `snapshot.file_data` Bytes für den Pfad, wird der zugehörige
    ///   [`BlockRecord`] ersetzt bzw. neu angelegt. Metadata-only Snapshots
    ///   (leeres `file_data`) lassen die Block-Records unberührt.
    ///
    /// `now` ist der Zeitstempel, den die Operation intern für neue Inodes
    /// verwendet — die Snapshot-eigenen created/modified/changed-Zeitstempel
    /// werden anschließend darüber gelegt. `now` dient daher primär als Fallback
    /// für Audit-Spuren.
    ///
    /// Liefert einen [`RestoreReport`] mit Restore-Statistiken.
    pub fn restore_snapshot_at(
        &mut self,
        snapshot_id: u64,
        now: crate::platform::Timestamp,
    ) -> CoreFsResult<RestoreReport> {
        let snapshot = self
            .snapshots
            .iter()
            .find(|s| s.id == snapshot_id)
            .ok_or_else(|| CoreFsError::NotFound(format!("snapshot {snapshot_id} not found")))?
            .clone();

        let mut restored_files = 0usize;
        let mut restored_dirs = 0usize;
        let mut skipped_paths: Vec<String> = Vec::new();

        // Ordered iteration (BTreeMap) -> parent directories come first.
        for (path, snap_inode) in &snapshot.inodes {
            // Soft-deleted? resurrect so we can overwrite instead of create.
            self.resurrect_inode_if_soft_deleted(path);

            let existing_kind = self
                .active_inodes
                .iter()
                .find(|i| &i.path == path)
                .map(|i| i.kind);

            match existing_kind {
                Some(existing) if existing != snap_inode.kind => {
                    skipped_paths.push(format!(
                        "{path}: kind mismatch (current {:?} vs snapshot {:?})",
                        existing, snap_inode.kind
                    ));
                    continue;
                }
                Some(_) => {
                    if let Some(inode) =
                        self.active_inodes.iter_mut().find(|i| &i.path == path)
                    {
                        inode.kind = snap_inode.kind;
                        inode.size = snap_inode.size;
                        inode.created_at = snap_inode.created_at;
                        inode.modified_at = snap_inode.modified_at;
                        inode.changed_at = snap_inode.changed_at;
                        inode.metadata = snap_inode.metadata.clone();
                    }
                }
                None => {
                    let new_id = self.next_inode_id();
                    let mut inode = Inode::new_at(
                        new_id,
                        snap_inode.kind,
                        path.clone(),
                        snap_inode.metadata.clone(),
                        snap_inode.created_at,
                    );
                    inode.size = snap_inode.size;
                    inode.modified_at = snap_inode.modified_at;
                    inode.changed_at = snap_inode.changed_at;
                    self.active_inodes.push(inode);
                }
            }

            // Replace/add block record content when file_data contains this path.
            if snap_inode.kind == InodeKind::File {
                if let Some(bytes) = snapshot.file_data.get(path) {
                    let inode_id = self
                        .active_inodes
                        .iter()
                        .find(|i| &i.path == path)
                        .map(|i| i.id)
                        .expect("inode just inserted/updated");
                    // Drop any existing records for this inode, then reinsert.
                    self.block_records.retain(|r| r.inode != inode_id);
                    self.block_records.push(BlockRecord {
                        inode: inode_id,
                        bytes: bytes.clone(),
                        checksum: 0,
                        device_block: 0,
                        allocated_blocks: 0,
                    });
                }
            }

            match snap_inode.kind {
                InodeKind::Directory => restored_dirs += 1,
                InodeKind::File => restored_files += 1,
                InodeKind::Symlink => {}
            }
        }

        // Audit-Eintrag im Journal — dokumentiert die Restore-Operation
        // im persistierten Audit-Log. Verwendet `now` (Aufrufer-Timestamp)
        // und liefert damit deterministische Replay-Fähigkeit.
        self.journal_entries.push(JournalEntry {
            timestamp: now,
            operation: alloc::string::ToString::to_string("restore_snapshot"),
            target: format!("/.snapshots/{}", snapshot.name),
            details: format!(
                "id={snapshot_id} restored_files={restored_files} restored_dirs={restored_dirs} skipped={}",
                skipped_paths.len()
            ),
        });

        Ok(RestoreReport {
            snapshot_id,
            snapshot_name: snapshot.name,
            restored_files,
            restored_dirs,
            skipped_paths,
        })
    }

    pub fn defragment_in_place(
        &mut self,
    ) -> crate::storage::block_store::DefragmentationReport {
        use crate::storage::block_store::BlockStore;
        let block_size = self.volume.block_size;
        let mut store = BlockStore::from_records_with_allocator(
            core::mem::take(&mut self.block_records),
            block_size,
            self.allocator_policy.clone(),
            core::mem::take(&mut self.free_extents),
        );
        let report = store.defragment();
        self.block_records = store.records();
        self.allocator_policy = store.allocator_policy().clone();
        self.free_extents = store.free_extents();
        report
    }
}

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
        let bytes = crate::bincode_compat::serialize(&state).expect("serialize ok");
        let decoded: PersistedState =
            crate::bincode_compat::deserialize(&bytes).expect("deserialize ok");
        assert_eq!(state, decoded);
    }

    #[test]
    fn empty_at_yields_clean_unmount_and_no_inodes() {
        let state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        assert!(state.clean_unmount);
        assert!(state.pending_wal.is_none());
        assert!(state.active_inodes.is_empty());
        assert!(state.deleted_inodes.is_empty());
        assert!(state.block_records.is_empty());
        assert!(state.journal_entries.is_empty());
        assert!(state.snapshots.is_empty());
        assert_eq!(state.next_snapshot_id, 0);
        assert_eq!(state.volume.name, "corefs");
        assert_eq!(state.volume.created_at, crate::platform::Timestamp::EPOCH);
    }

    #[test]
    fn defragment_in_place_on_empty_state_is_noop() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        let report = state.defragment_in_place();
        assert_eq!(report.moved_entries, 0);
        assert_eq!(report.reclaimed_gaps, 0);
        // `final_device_blocks` ist die Block-Belegung nach Defrag — auf einem
        // leeren Volume entweder 0 oder konstant. Wir prüfen nur, dass
        // anschließend kein Datenverlust (records leer) eingetreten ist.
        assert!(state.block_records.is_empty());
    }

    // ---------------------------------------------------------------------
    // restore_snapshot_at
    // ---------------------------------------------------------------------

    use crate::domain::metadata::FileMetadata;
    use crate::domain::snapshot::{Snapshot, SnapshotInode};
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;
    use alloc::vec;

    fn snap_inode_file(size: usize) -> SnapshotInode {
        SnapshotInode {
            kind: InodeKind::File,
            size,
            created_at: crate::platform::Timestamp::EPOCH,
            modified_at: crate::platform::Timestamp::EPOCH,
            changed_at: crate::platform::Timestamp::EPOCH,
            metadata: FileMetadata::default(),
            symlink_target: None,
        }
    }

    fn snap_inode_dir() -> SnapshotInode {
        SnapshotInode {
            kind: InodeKind::Directory,
            size: 0,
            created_at: crate::platform::Timestamp::EPOCH,
            modified_at: crate::platform::Timestamp::EPOCH,
            changed_at: crate::platform::Timestamp::EPOCH,
            metadata: FileMetadata {
                mode: 0o755,
                ..FileMetadata::default()
            },
            symlink_target: None,
        }
    }

    fn push_snapshot(state: &mut PersistedState, id: u64, inodes: Vec<(String, SnapshotInode)>, data: Vec<(String, Vec<u8>)>) {
        let mut inode_map: BTreeMap<String, SnapshotInode> = BTreeMap::new();
        for (p, si) in &inodes {
            inode_map.insert(p.clone(), si.clone());
        }
        let mut file_data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (p, bytes) in data {
            file_data.insert(p, bytes);
        }
        let paths: Vec<String> = inodes.into_iter().map(|(p, _)| p).collect();
        state.snapshots.push(Snapshot {
            id,
            name: format!("snap{id}"),
            scope_root: "/".to_string(),
            created_at: crate::platform::Timestamp::EPOCH,
            paths,
            file_data,
            inodes: inode_map,
        });
    }

    #[test]
    fn restore_unknown_snapshot_returns_not_found() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        let err = state
            .restore_snapshot_at(42, crate::platform::Timestamp::EPOCH)
            .expect_err("must fail");
        assert!(matches!(err, CoreFsError::NotFound(_)));
    }

    #[test]
    fn restore_on_empty_state_creates_new_inodes() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        push_snapshot(
            &mut state,
            1,
            vec![
                ("/dir".to_string(), snap_inode_dir()),
                ("/dir/a.txt".to_string(), snap_inode_file(3)),
            ],
            vec![("/dir/a.txt".to_string(), b"abc".to_vec())],
        );

        let report = state
            .restore_snapshot_at(1, crate::platform::Timestamp::EPOCH)
            .expect("restore ok");
        assert_eq!(report.snapshot_id, 1);
        assert_eq!(report.restored_dirs, 1);
        assert_eq!(report.restored_files, 1);
        assert!(report.skipped_paths.is_empty());
        assert_eq!(state.active_inodes.len(), 2);
        // Block record was inserted for the file.
        let file_inode = state
            .active_inodes
            .iter()
            .find(|i| i.path == "/dir/a.txt")
            .unwrap();
        let br = state
            .block_records
            .iter()
            .find(|r| r.inode == file_inode.id)
            .expect("record present");
        assert_eq!(br.bytes, b"abc");
    }

    #[test]
    fn restore_overwrites_existing_active_inode_metadata() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        // Seed one active file with mode 0o600 and some later snapshot carrying 0o644.
        let mut md = FileMetadata::default();
        md.mode = 0o600;
        let existing = Inode::new_at(
            InodeId(7),
            InodeKind::File,
            "/f".to_string(),
            md,
            crate::platform::Timestamp::EPOCH,
        );
        state.active_inodes.push(existing);

        let mut snap_md = FileMetadata::default();
        snap_md.mode = 0o644;
        let si = SnapshotInode {
            kind: InodeKind::File,
            size: 2,
            created_at: crate::platform::Timestamp::EPOCH,
            modified_at: crate::platform::Timestamp::EPOCH,
            changed_at: crate::platform::Timestamp::EPOCH,
            metadata: snap_md,
            symlink_target: None,
        };
        push_snapshot(
            &mut state,
            9,
            vec![("/f".to_string(), si)],
            vec![("/f".to_string(), b"hi".to_vec())],
        );

        let report = state
            .restore_snapshot_at(9, crate::platform::Timestamp::EPOCH)
            .expect("restore ok");
        assert_eq!(report.restored_files, 1);
        assert_eq!(state.active_inodes.len(), 1);
        let f = &state.active_inodes[0];
        assert_eq!(f.metadata.mode, 0o644);
        assert_eq!(f.size, 2);
        // InodeId unchanged (overwrote the existing entry).
        assert_eq!(f.id, InodeId(7));
        let br = state.block_records.iter().find(|r| r.inode == f.id).unwrap();
        assert_eq!(br.bytes, b"hi");
    }

    #[test]
    fn restore_metadata_only_snapshot_leaves_block_records_untouched() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        let existing = Inode::new_at(
            InodeId(3),
            InodeKind::File,
            "/x".to_string(),
            FileMetadata::default(),
            crate::platform::Timestamp::EPOCH,
        );
        state.active_inodes.push(existing);
        state.block_records.push(BlockRecord {
            inode: InodeId(3),
            bytes: b"old-body".to_vec(),
            checksum: 0,
            device_block: 0,
            allocated_blocks: 0,
        });

        push_snapshot(
            &mut state,
            2,
            vec![("/x".to_string(), snap_inode_file(8))],
            vec![], // metadata-only
        );
        let report = state
            .restore_snapshot_at(2, crate::platform::Timestamp::EPOCH)
            .expect("restore ok");
        assert_eq!(report.restored_files, 1);
        // block_records for /x preserved.
        let br = state
            .block_records
            .iter()
            .find(|r| r.inode == InodeId(3))
            .expect("record still there");
        assert_eq!(br.bytes, b"old-body");
    }

    #[test]
    fn restore_skips_paths_with_kind_conflict() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        // /p exists as a Directory but snapshot has it as a File.
        let existing = Inode::new_at(
            InodeId(1),
            InodeKind::Directory,
            "/p".to_string(),
            FileMetadata::default(),
            crate::platform::Timestamp::EPOCH,
        );
        state.active_inodes.push(existing);

        push_snapshot(
            &mut state,
            5,
            vec![("/p".to_string(), snap_inode_file(1))],
            vec![],
        );
        let report = state
            .restore_snapshot_at(5, crate::platform::Timestamp::EPOCH)
            .expect("restore ok");
        assert_eq!(report.restored_files, 0);
        assert_eq!(report.skipped_paths.len(), 1);
        assert!(report.skipped_paths[0].starts_with("/p:"));
    }

    #[test]
    fn next_inode_id_respects_deleted_entries() {
        let mut state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        assert_eq!(state.next_inode_id(), InodeId(1));
        state.active_inodes.push(Inode::new_at(
            InodeId(5),
            InodeKind::File,
            "/a".to_string(),
            FileMetadata::default(),
            crate::platform::Timestamp::EPOCH,
        ));
        state.deleted_inodes.push(Inode::new_at(
            InodeId(9),
            InodeKind::File,
            "/b".to_string(),
            FileMetadata::default(),
            crate::platform::Timestamp::EPOCH,
        ));
        assert_eq!(state.next_inode_id(), InodeId(10));
    }

    #[test]
    fn empty_at_round_trips_via_bincode_compat() {
        let state =
            PersistedState::empty_at(CoreFsConfig::default(), crate::platform::Timestamp::EPOCH);
        let bytes = crate::bincode_compat::serialize(&state).expect("serialize ok");
        let decoded: PersistedState =
            crate::bincode_compat::deserialize(&bytes).expect("deserialize ok");
        assert_eq!(state, decoded);
    }
}
