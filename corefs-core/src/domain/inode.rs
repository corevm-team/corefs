// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Inode-Domänentypen.
//!
//! Inodes sind plattformneutrale Grund-Datensätze einer Datei, eines Verzeichnisses
//! oder eines Symlinks. Zeitstempel liegen als [`Timestamp`] vor (nicht als
//! `std::time::SystemTime`), damit der Kern auch im no_std-Kontext verwendet
//! werden kann. Die Wire-Kompatibilität mit bestehenden `SystemTime`-Serialisaten
//! ist in [`crate::platform`] dokumentiert und durch einen Regressionstest
//! abgesichert.

use crate::domain::metadata::FileMetadata;
use crate::platform::Timestamp;
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InodeId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InodeKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inode {
    pub id: InodeId,
    pub kind: InodeKind,
    pub path: String,
    pub size: usize,
    /// POSIX `birth time` — set on creation, never updated afterwards.
    pub created_at: Timestamp,
    /// POSIX `mtime` — updated when file **content** changes
    /// (write, truncate).  Not updated by chown/chmod/rename.
    pub modified_at: Timestamp,
    /// POSIX `ctime` — updated on any inode change: content changes,
    /// metadata changes (chown/chmod), or rename.
    pub changed_at: Timestamp,
    /// POSIX `atime` — updated on read access (relatime-semantic is
    /// left to higher layers). Kept separately so setattr/read paths
    /// can mutate access time without touching mtime/ctime.
    ///
    /// Wire-Compat: alte bincode-Persisted-States kennen dieses Feld
    /// nicht — `#[serde(default)]` liefert für sie `Timestamp::EPOCH`,
    /// damit Volumes aus vorigen Versionen weiterhin lesbar sind.
    #[serde(default)]
    pub accessed_at: Timestamp,
    pub metadata: FileMetadata,
}

impl Inode {
    /// Konstruiert einen neuen Inode mit dem gegebenen Zeitstempel für alle
    /// drei POSIX-Zeitstempelfelder (crtime, mtime, ctime).
    ///
    /// Diese Variante ist no_std-fähig — der Aufrufer liefert die Zeit explizit,
    /// etwa aus einer [`crate::platform::Clock`]-Implementierung oder aus einem
    /// bekannten Zeitpunkt (Tests, Replay).
    pub fn new_at(
        id: InodeId,
        kind: InodeKind,
        path: String,
        metadata: FileMetadata,
        now: Timestamp,
    ) -> Self {
        Self {
            id,
            kind,
            path,
            size: 0,
            created_at: now,
            modified_at: now,
            changed_at: now,
            accessed_at: now,
            metadata,
        }
    }

    /// Konstruiert einen neuen Inode mit der aktuellen Systemzeit.
    ///
    /// Bequemere Variante von [`Inode::new_at`] für std-basierte Umgebungen.
    #[cfg(feature = "std")]
    pub fn new(id: InodeId, kind: InodeKind, path: String, metadata: FileMetadata) -> Self {
        Self::new_at(id, kind, path, metadata, Timestamp::now())
    }

    /// Markiert den Inode als inhaltlich modifiziert (setzt sowohl
    /// `modified_at` als auch `changed_at` auf `now`).
    pub fn touch_modified_at(&mut self, now: Timestamp) {
        self.modified_at = now;
        self.changed_at = now;
    }

    /// Markiert den Inode als inhaltlich modifiziert, mit aktueller Systemzeit.
    #[cfg(feature = "std")]
    pub fn touch_modified(&mut self) {
        self.touch_modified_at(Timestamp::now());
    }

    /// Markiert den Inode als metadata-seitig verändert (setzt nur `changed_at`).
    pub fn touch_changed_at(&mut self, now: Timestamp) {
        self.changed_at = now;
    }

    /// Markiert den Inode als metadata-seitig verändert, mit aktueller Systemzeit.
    #[cfg(feature = "std")]
    pub fn touch_changed(&mut self) {
        self.touch_changed_at(Timestamp::now());
    }

    /// Setzt den Access-Zeitstempel (`atime`). Ändert weder `mtime`
    /// noch `ctime` — Access ist ein reiner Lese-Touch.
    pub fn touch_accessed_at(&mut self, now: Timestamp) {
        self.accessed_at = now;
    }

    /// Setzt den Access-Zeitstempel auf die aktuelle Systemzeit.
    #[cfg(feature = "std")]
    pub fn touch_accessed(&mut self) {
        self.touch_accessed_at(Timestamp::now());
    }
}

#[cfg(test)]
#[path = "inode_tests.rs"]
mod tests;
