// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Journal-Service: nimmt gerichtete Operationen und Transaktionen entgegen.
//!
//! Der Service ist plattformneutral. Alle Zeitstempel werden explizit vom
//! Aufrufer übergeben (`*_at(now: Timestamp)`-APIs). Für std-basierte Hosts
//! existieren `cfg(feature = "std")`-Wrapper, die automatisch
//! [`Timestamp::now`] nutzen.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::platform::Timestamp;

/// Einzelner Journal-Eintrag: eine Operation auf einem Zielpfad.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Zeitpunkt des Eintrags.
    pub timestamp: Timestamp,
    /// Operationstyp (z. B. `create_file`, `delete`, `snapshot`).
    pub operation: String,
    /// Zielpfad oder Scope der Operation.
    pub target: String,
    /// Zusätzliche Diagnose-Details.
    pub details: String,
}

/// Offene Journal-Transaktion mit gepufferten Operationen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalTransaction {
    /// Fortlaufende Transaktions-ID.
    pub id: u64,
    /// Label/Name der Transaktion (z. B. `rw-writeback`).
    pub label: String,
    /// Startzeitpunkt.
    pub started_at: Timestamp,
    /// Bislang gepufferte Operationen (noch nicht committed).
    pub operations: Vec<JournalEntry>,
}

/// Laufzeitzustand des Journals (abseits der Einträge selbst).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JournalRuntimeState {
    /// Nächste zu vergebende Transaktions-ID.
    pub next_transaction_id: u64,
    /// Letzte erfolgreich committete Transaktions-ID.
    pub last_committed_transaction_id: Option<u64>,
    /// Letzte abgebrochene Transaktions-ID.
    pub last_aborted_transaction_id: Option<u64>,
    /// Letzte per Recovery-Replay wiederhergestellte Transaktions-ID.
    pub last_replayed_transaction_id: Option<u64>,
    /// Kennzeichen für unsauberen Shutdown (wird beim Recovery zurückgesetzt).
    pub unclean_shutdown: bool,
    /// Ggf. noch offene Transaktion, die beim nächsten Laden wiederholt wird.
    pub pending_transaction: Option<JournalTransaction>,
}

/// Journal-Service.
#[derive(Debug, Default)]
pub struct JournalService {
    entries: Vec<JournalEntry>,
    runtime: JournalRuntimeState,
}

/// Ergebnis eines Journal-Replays: aggregierter Stand aktiver und gelöschter Pfade.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalReplayState {
    /// Aktuell aktive Pfade laut Journal-Replay.
    pub active_paths: BTreeSet<String>,
    /// Aktuell als gelöscht markierte Pfade laut Journal-Replay.
    pub deleted_paths: BTreeSet<String>,
    /// Anzahl aufgezeichneter Snapshots laut Journal.
    pub snapshot_count: usize,
}

/// Zusammenfassung einer Journal-Reparatur gegen einen persistierten Zustand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalRepairSummary {
    /// Anzahl Inodes, die von "aktiv" nach "gelöscht" verschoben wurden.
    pub moved_to_deleted: usize,
    /// Anzahl Inodes, die von "gelöscht" nach "aktiv" wiederhergestellt wurden.
    pub restored_to_active: usize,
    /// Anzahl endgültig entsorgter Tombstones.
    pub purged_deleted: usize,
    /// Anzahl entfernter Waisen-Block-Records.
    pub removed_orphan_blocks: usize,
    /// Anzahl Inodes, deren `size` an die tatsächliche Block-Länge angeglichen wurde.
    pub resized_inodes: usize,
    /// Ob die Snapshot-ID-Zähler hochgezogen werden mussten.
    pub snapshot_id_adjusted: bool,
}

/// Zusammenfassung einer Journal-Recovery beim Laden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalRecoverySummary {
    /// Ob eine schwebende Transaktion abgebrochen wurde.
    pub aborted_pending_transaction: bool,
    /// Ob das Unclean-Shutdown-Flag zurückgesetzt wurde.
    pub cleared_unclean_shutdown: bool,
    /// Ob ein Replay-Eintrag ins Journal geschrieben wurde.
    pub replay_recorded: bool,
}

impl JournalService {
    /// Nimmt einen Journal-Eintrag mit explizitem Zeitstempel entgegen.
    pub fn record_at(
        &mut self,
        operation: &str,
        target: &str,
        details: impl Into<String>,
        now: Timestamp,
    ) {
        let entry = JournalEntry {
            timestamp: now,
            operation: operation.to_string(),
            target: target.to_string(),
            details: details.into(),
        };

        if let Some(transaction) = self.runtime.pending_transaction.as_mut() {
            transaction.operations.push(entry);
        } else {
            self.entries.push(entry);
        }
    }

    /// std-Komfortvariante von [`Self::record_at`].
    #[cfg(feature = "std")]
    pub fn record(&mut self, operation: &str, target: &str, details: impl Into<String>) {
        self.record_at(operation, target, details, Timestamp::now());
    }

    /// Liefert alle committeten Einträge.
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Liefert den Runtime-Zustand des Journals.
    pub fn runtime_state(&self) -> &JournalRuntimeState {
        &self.runtime
    }

    /// `true`, wenn gerade eine offene Transaktion existiert.
    pub fn has_pending_transaction(&self) -> bool {
        self.runtime.pending_transaction.is_some()
    }

    /// Startet eine Transaktion mit explizitem Startzeitpunkt.
    pub fn begin_transaction_at(&mut self, label: impl Into<String>, now: Timestamp) -> u64 {
        if let Some(transaction) = self.runtime.pending_transaction.as_ref() {
            return transaction.id;
        }

        self.runtime.next_transaction_id += 1;
        let id = self.runtime.next_transaction_id;
        self.runtime.pending_transaction = Some(JournalTransaction {
            id,
            label: label.into(),
            started_at: now,
            operations: Vec::new(),
        });
        id
    }

    /// std-Komfortvariante von [`Self::begin_transaction_at`].
    #[cfg(feature = "std")]
    pub fn begin_transaction(&mut self, label: impl Into<String>) -> u64 {
        self.begin_transaction_at(label, Timestamp::now())
    }

    /// Committet die offene Transaktion mit explizitem Zeitstempel.
    pub fn commit_transaction_at(&mut self, now: Timestamp) -> Option<u64> {
        let transaction = self.runtime.pending_transaction.take()?;
        let id = transaction.id;
        let operation_count = transaction.operations.len();
        self.entries.push(JournalEntry {
            timestamp: now,
            operation: "tx_begin".to_string(),
            target: transaction.label.clone(),
            details: format!("id={id} ops={operation_count}"),
        });
        self.entries.extend(transaction.operations);
        self.entries.push(JournalEntry {
            timestamp: now,
            operation: "tx_commit".to_string(),
            target: transaction.label,
            details: format!("id={id} ops={operation_count}"),
        });
        self.runtime.last_committed_transaction_id = Some(id);
        Some(id)
    }

    /// std-Komfortvariante von [`Self::commit_transaction_at`].
    #[cfg(feature = "std")]
    pub fn commit_transaction(&mut self) -> Option<u64> {
        self.commit_transaction_at(Timestamp::now())
    }

    /// Bricht die offene Transaktion mit explizitem Zeitstempel ab.
    pub fn abort_transaction_at(&mut self, reason: &str, now: Timestamp) -> Option<u64> {
        let transaction = self.runtime.pending_transaction.take()?;
        let id = transaction.id;
        self.entries.push(JournalEntry {
            timestamp: now,
            operation: "tx_abort".to_string(),
            target: transaction.label,
            details: format!("id={id} reason={reason}"),
        });
        self.runtime.last_aborted_transaction_id = Some(id);
        Some(id)
    }

    /// std-Komfortvariante von [`Self::abort_transaction_at`].
    #[cfg(feature = "std")]
    pub fn abort_transaction(&mut self, reason: &str) -> Option<u64> {
        self.abort_transaction_at(reason, Timestamp::now())
    }

    /// Markiert den Journal-Zustand als unsauber heruntergefahren.
    pub fn mark_unclean_shutdown(&mut self) {
        self.runtime.unclean_shutdown = true;
    }

    /// Markiert den Journal-Zustand als sauber heruntergefahren.
    pub fn mark_clean_shutdown(&mut self) {
        self.runtime.unclean_shutdown = false;
    }

    /// Führt Recovery-Schritte beim Laden mit explizitem Zeitstempel aus.
    pub fn recover_on_load_at(&mut self, now: Timestamp) -> JournalRecoverySummary {
        let mut summary = JournalRecoverySummary::default();

        if let Some(transaction_id) = self.abort_transaction_at("recovery_replay", now) {
            self.runtime.last_replayed_transaction_id = Some(transaction_id);
            summary.aborted_pending_transaction = true;
        }

        if self.runtime.unclean_shutdown {
            self.runtime.unclean_shutdown = false;
            summary.cleared_unclean_shutdown = true;
        }

        if summary.aborted_pending_transaction || summary.cleared_unclean_shutdown {
            self.entries.push(JournalEntry {
                timestamp: now,
                operation: "recovery_replay".to_string(),
                target: "/".to_string(),
                details: format!(
                    "aborted_pending={} cleared_unclean={}",
                    summary.aborted_pending_transaction, summary.cleared_unclean_shutdown
                ),
            });
            summary.replay_recorded = true;
        }

        summary
    }

    /// std-Komfortvariante von [`Self::recover_on_load_at`].
    #[cfg(feature = "std")]
    pub fn recover_on_load(&mut self) -> JournalRecoverySummary {
        self.recover_on_load_at(Timestamp::now())
    }

    /// Konstruiert einen Service aus bestehenden Einträgen (Runtime leer).
    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self {
            entries,
            runtime: JournalRuntimeState::default(),
        }
    }

    /// Konstruiert einen Service aus Einträgen und Runtime-Zustand.
    pub fn from_entries_with_runtime(
        entries: Vec<JournalEntry>,
        runtime: JournalRuntimeState,
    ) -> Self {
        Self { entries, runtime }
    }

    /// Rekonstruiert aktive und gelöschte Pfade aus dem Journal-Verlauf.
    pub fn replay(&self) -> JournalReplayState {
        let mut state = JournalReplayState::default();

        for entry in &self.entries {
            match entry.operation.as_str() {
                "create_file" | "create_directory" | "create_symlink" | "write_file"
                | "restore" => {
                    state.active_paths.insert(entry.target.clone());
                    state.deleted_paths.remove(&entry.target);
                }
                "delete" => {
                    state.active_paths.remove(&entry.target);
                    state.deleted_paths.insert(entry.target.clone());
                }
                "secure_delete" => {
                    state.active_paths.remove(&entry.target);
                    state.deleted_paths.remove(&entry.target);
                }
                "snapshot" => {
                    state.snapshot_count += 1;
                }
                "format" | "sync" | "rename" | "tx_begin" | "tx_commit" | "tx_abort"
                | "recovery_replay" => {}
                _ => {}
            }
        }

        state
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
