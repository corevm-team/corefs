// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tier-Status-Helfer für Single-Device-Volumes.
//!
//! Echte Tier-Migration (hot ↔ cold) benötigt zwei physische Devices und
//! ist Aufgabe der [`super::tiering`]-Infrastruktur. Für Tool-Aufrufer auf
//! einem einzelnen Volume liefert dieses Modul eine ehrliche Status-Sicht:
//! welche Pfade sind laut Hot-Path-Telemetrie aktuell am heißesten, und wie
//! verteilt sich der konfigurierte Storage-Tier über die aktiven Inodes.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::config::StorageTier;
use crate::domain::inode::InodeKind;
use crate::services::hot_paths::HotPathRecord;
use crate::storage::persisted_state::PersistedState;

/// Report der [`tier_status_from_state`]-Funktion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierStatusReport {
    /// Gesamtanzahl aktiver Inodes (alle Kinds).
    pub total_inodes: usize,
    /// Anzahl aktiver Inodes mit `StorageTier::Hot`.
    pub hot_inodes: usize,
    /// Anzahl aktiver Inodes mit `StorageTier::Warm`.
    pub warm_inodes: usize,
    /// Anzahl aktiver Inodes mit `StorageTier::Cold`.
    pub cold_inodes: usize,
    /// Top-N Pfade nach Hot-Path-Score, absteigend sortiert.
    ///
    /// Jeder Eintrag ist `(path, score)`, wobei `score` aus dem gleichen
    /// Heuristik-Modell stammt wie
    /// [`crate::services::hot_paths::HotPathService::hottest_paths`].
    pub hottest: Vec<(String, u64)>,
}

/// Berechnet den Status-Report aus einem hydrierten [`PersistedState`].
///
/// `top_n` begrenzt die Anzahl zurückgelieferter Hottest-Einträge.
#[must_use]
pub fn tier_status_from_state(state: &PersistedState, top_n: usize) -> TierStatusReport {
    let mut hot = 0usize;
    let mut warm = 0usize;
    let mut cold = 0usize;
    for inode in &state.active_inodes {
        if inode.kind != InodeKind::File {
            continue;
        }
        match inode.metadata.storage_tier {
            StorageTier::Hot => hot += 1,
            StorageTier::Warm => warm += 1,
            StorageTier::Cold => cold += 1,
        }
    }

    let mut scored: Vec<(String, u64)> = state
        .hot_path_records
        .iter()
        .map(|r| (r.path.clone(), score(r)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(top_n);

    TierStatusReport {
        total_inodes: state.active_inodes.len(),
        hot_inodes: hot,
        warm_inodes: warm,
        cold_inodes: cold,
        hottest: scored,
    }
}

fn score(record: &HotPathRecord) -> u64 {
    // Spiegelt die Gewichtung aus `services::hot_paths::score` wider.
    record.read_ops
        + record.write_ops.saturating_mul(2)
        + record.metadata_ops
        + (record.bytes_read / 4096)
        + (record.bytes_written / 4096).saturating_mul(2)
}

// Discourage dead-code warnings on the imported String alias in no_std builds
// where the alias is only used via impl paths.
#[allow(dead_code)]
fn _noop_to_string(s: &str) -> String {
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CoreFsConfig;
    use crate::domain::inode::{Inode, InodeId};
    use crate::domain::metadata::FileMetadata;
    use crate::platform::Timestamp;

    fn seed_state() -> PersistedState {
        PersistedState::empty_at(CoreFsConfig::default(), Timestamp::EPOCH)
    }

    fn inode_with_tier(id: u64, path: &str, tier: StorageTier) -> Inode {
        let mut md = FileMetadata::default();
        md.storage_tier = tier;
        Inode::new_at(
            InodeId(id),
            InodeKind::File,
            path.to_string(),
            md,
            Timestamp::EPOCH,
        )
    }

    #[test]
    fn status_on_empty_state() {
        let state = seed_state();
        let report = tier_status_from_state(&state, 5);
        assert_eq!(report.total_inodes, 0);
        assert_eq!(report.hot_inodes, 0);
        assert_eq!(report.warm_inodes, 0);
        assert_eq!(report.cold_inodes, 0);
        assert!(report.hottest.is_empty());
    }

    #[test]
    fn status_counts_storage_tiers() {
        let mut state = seed_state();
        state
            .active_inodes
            .push(inode_with_tier(1, "/h1", StorageTier::Hot));
        state
            .active_inodes
            .push(inode_with_tier(2, "/h2", StorageTier::Hot));
        state
            .active_inodes
            .push(inode_with_tier(3, "/w", StorageTier::Warm));
        state
            .active_inodes
            .push(inode_with_tier(4, "/c", StorageTier::Cold));
        let report = tier_status_from_state(&state, 5);
        assert_eq!(report.total_inodes, 4);
        assert_eq!(report.hot_inodes, 2);
        assert_eq!(report.warm_inodes, 1);
        assert_eq!(report.cold_inodes, 1);
    }

    #[test]
    fn hottest_is_sorted_by_score_desc_and_respects_top_n() {
        let mut state = seed_state();
        state.hot_path_records.push(HotPathRecord {
            path: "/a".to_string(),
            read_ops: 10,
            write_ops: 0,
            metadata_ops: 0,
            bytes_read: 0,
            bytes_written: 0,
        });
        state.hot_path_records.push(HotPathRecord {
            path: "/b".to_string(),
            read_ops: 0,
            write_ops: 50, // heavy write doubles
            metadata_ops: 0,
            bytes_read: 0,
            bytes_written: 0,
        });
        state.hot_path_records.push(HotPathRecord {
            path: "/c".to_string(),
            read_ops: 0,
            write_ops: 0,
            metadata_ops: 5,
            bytes_read: 0,
            bytes_written: 0,
        });
        let report = tier_status_from_state(&state, 2);
        assert_eq!(report.hottest.len(), 2);
        assert_eq!(report.hottest[0].0, "/b");
        assert_eq!(report.hottest[1].0, "/a");
    }

    #[test]
    fn directories_do_not_contribute_to_tier_counts() {
        let mut state = seed_state();
        let mut md = FileMetadata::default();
        md.storage_tier = StorageTier::Hot;
        let dir = Inode::new_at(
            InodeId(1),
            InodeKind::Directory,
            "/d".to_string(),
            md,
            Timestamp::EPOCH,
        );
        state.active_inodes.push(dir);
        let report = tier_status_from_state(&state, 5);
        assert_eq!(report.total_inodes, 1);
        // Files-only counters stay zero:
        assert_eq!(report.hot_inodes, 0);
    }
}
