// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Volume-WAL-Datentypen (no_std + alloc).
//!
//! Beschreibt eine pending Write-Ahead-Log-Transaktion mit ihren Operationen.
//! Die `apply_operation`-Funktion, die einen `VolumeWal`-Eintrag gegen einen
//! laufenden `CoreFsService` zurückspielt, lebt weiterhin im main `corefs`
//! crate, da sie an die High-Level-Service-API gebunden ist. Hier liegen
//! ausschließlich die plattformneutralen Datenstrukturen.

use crate::domain::inode::InodeId;
use crate::platform::Timestamp;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Eine einzelne Operation innerhalb einer Volume-WAL-Transaktion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOperation {
    /// Datei mit gegebener Inode-ID und Pfad anlegen.
    CreateFile {
        /// Absoluter Pfad.
        path: String,
        /// Inode-ID der neuen Datei.
        inode: InodeId,
    },
    /// Verzeichnis anlegen.
    CreateDirectory {
        /// Absoluter Pfad.
        path: String,
        /// Inode-ID des neuen Verzeichnisses.
        inode: InodeId,
    },
    /// Patch eines Extents an einer konkreten Stelle.
    PatchExtent {
        /// Inode-ID, deren Extent gepatcht wird.
        inode: InodeId,
        /// Physikalische Block-Adresse des Extents.
        device_block: u64,
        /// Byte-Offset innerhalb des Blocks.
        block_offset: usize,
        /// Byte-Offset innerhalb der Datei (zum Fallback, wenn der Extent nicht
        /// mehr existiert).
        inode_offset: usize,
        /// Zu schreibende Bytes.
        bytes: Vec<u8>,
        /// Länge der Datei nach Anwendung des Patches.
        final_len: usize,
    },
    /// Inode auf eine bestimmte Größe trunkieren.
    TruncateInode {
        /// Inode-ID.
        inode: InodeId,
        /// Neue Dateigröße in Bytes.
        size: usize,
    },
    /// Pfad löschen.
    DeletePath {
        /// Absoluter Pfad.
        path: String,
    },
    /// Pfad umbenennen.
    RenamePath {
        /// Alter Pfad.
        from: String,
        /// Neuer Pfad.
        to: String,
    },
}

/// Pending Write-Ahead-Log-Transaktion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeWal {
    /// Eindeutige Transaktions-ID.
    pub transaction_id: u64,
    /// Frei vergebenes Label (für Diagnose und Debugging).
    pub label: String,
    /// Zeitpunkt der WAL-Erzeugung.
    pub created_at: Timestamp,
    /// Operationen in Anwendungsreihenfolge.
    pub operations: Vec<WalOperation>,
}

impl VolumeWal {
    /// Konstruiert eine WAL-Transaktion mit einem expliziten Zeitstempel.
    ///
    /// Diese no_std-fähige Variante nimmt den Zeitstempel als Parameter.
    pub fn new_at(transaction_id: u64, label: impl Into<String>, created_at: Timestamp) -> Self {
        Self {
            transaction_id,
            label: label.into(),
            created_at,
            operations: Vec::new(),
        }
    }

    /// Konstruiert eine WAL-Transaktion mit der aktuellen Systemzeit.
    ///
    /// Bequemere Variante von [`VolumeWal::new_at`] für std-basierte
    /// Umgebungen.
    #[cfg(feature = "std")]
    pub fn new(transaction_id: u64, label: impl Into<String>) -> Self {
        Self::new_at(transaction_id, label, Timestamp::now())
    }

    /// Hängt eine Operation an die Transaktion an.
    pub fn push(&mut self, operation: WalOperation) {
        self.operations.push(operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn new_at_sets_fields() {
        let wal = VolumeWal::new_at(42, "demo", Timestamp::from_secs(1_700_000_000));
        assert_eq!(wal.transaction_id, 42);
        assert_eq!(wal.label, "demo");
        assert_eq!(wal.created_at.as_secs(), 1_700_000_000);
        assert!(wal.operations.is_empty());
    }

    #[test]
    fn push_appends_operations_in_order() {
        let mut wal = VolumeWal::new_at(1, "x", Timestamp::EPOCH);
        wal.push(WalOperation::CreateFile {
            path: "/a".to_string(),
            inode: InodeId(1),
        });
        wal.push(WalOperation::DeletePath {
            path: "/a".to_string(),
        });
        assert_eq!(wal.operations.len(), 2);
        assert!(matches!(wal.operations[0], WalOperation::CreateFile { .. }));
        assert!(matches!(wal.operations[1], WalOperation::DeletePath { .. }));
    }
}
