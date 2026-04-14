// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für Snapshot-Lifecycle.
//!
//! Wrapper um die Snapshot-APIs von [`corefs::app::CoreFsService`]:
//!
//! - [`list`]    — alle vorhandenen Snapshots anzeigen
//! - [`create`]  — neuen Snapshot erzeugen (optional gescoped auf einen Pfad)
//! - [`delete`]  — Snapshot anhand seiner ID löschen
//! - [`restore`] — Snapshot-Inhalt zurückspielen
//!
//! Alle Operationen agieren auf einem [`OdfFileSession`], d. h. sie öffnen
//! die Image-Datei, mutieren den Service-State, persistieren via
//! `save_state_native` und schließen die Session am Ende.

use crate::error::ToolsResult;
use crate::report::{Report, to_pretty_json};
use corefs::storage::ondisk::session::OdfFileSession;
use serde::{Deserialize, Serialize};
use std::path::Path;

// =====================================================================
// list
// =====================================================================

/// Snapshot-Eintrag im [`SnapshotListReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Snapshot-ID (eindeutig pro Volume).
    pub id: u64,
    /// Vom Aufrufer vergebener Snapshot-Name.
    pub name: String,
    /// Wurzelpfad des Snapshot-Scopes (`/` für volle Volumes, sonst Subtree).
    pub scope_root: String,
    /// Erstellungszeitpunkt in Sekunden seit Epoche.
    pub created_at_secs: u64,
    /// Anzahl Pfade, die der Snapshot enthält.
    pub path_count: usize,
    /// Anzahl regulärer Dateien, deren Inhalt im Snapshot eingebettet ist.
    pub file_data_count: usize,
}

/// Strukturierter Report einer `snapshot list`-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotListReport {
    /// Pfad des Volumes.
    pub image_path: String,
    /// Snapshots, sortiert nach `id` aufsteigend.
    pub snapshots: Vec<SnapshotEntry>,
}

impl Report for SnapshotListReport {
    fn summary(&self) -> String {
        if self.snapshots.is_empty() {
            format!("snapshots: none ({})", self.image_path)
        } else {
            format!("snapshots: {} ({})", self.snapshots.len(), self.image_path)
        }
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("snapshots in {}\n", self.image_path));
        out.push_str("──────────────\n");
        if self.snapshots.is_empty() {
            out.push_str("(none)\n");
        } else {
            for s in &self.snapshots {
                out.push_str(&format!(
                    "  id={:<4} name={:<20} scope={:<20} files={:<6} paths={}\n",
                    s.id, s.name, s.scope_root, s.file_data_count, s.path_count
                ));
            }
        }
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Listet alle Snapshots der Image-Datei `path` auf.
pub fn list(path: impl AsRef<Path>) -> ToolsResult<SnapshotListReport> {
    let path = path.as_ref();
    let session = OdfFileSession::open(path)?;
    let snapshots = session
        .service()
        .snapshots()
        .iter()
        .map(|s| SnapshotEntry {
            id: s.id,
            name: s.name.clone(),
            scope_root: s.scope_root.clone(),
            created_at_secs: s.created_at.as_secs(),
            path_count: s.paths.len(),
            file_data_count: s.file_data.len(),
        })
        .collect();
    Ok(SnapshotListReport {
        image_path: path.display().to_string(),
        snapshots,
    })
}

// =====================================================================
// create
// =====================================================================

/// Optionen für [`create`].
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Snapshot-Name. Muss innerhalb des Volumes nicht eindeutig sein —
    /// Eindeutigkeit wird über die ID hergestellt.
    pub name: String,
    /// Wurzelpfad des Scopes. `None` ⇒ ganzer Volume (`/`).
    pub scope_root: Option<String>,
}

/// Strukturierter Report einer `snapshot create`-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCreateReport {
    /// Pfad des Volumes.
    pub image_path: String,
    /// Frisch vergebene Snapshot-ID.
    pub id: u64,
    /// Snapshot-Name.
    pub name: String,
    /// Effektiver Scope-Root (nach Default-Auflösung).
    pub scope_root: String,
    /// Anzahl Pfade im Snapshot.
    pub path_count: usize,
    /// Anzahl regulärer Dateien, deren Inhalt eingebettet wurde.
    pub file_data_count: usize,
}

impl Report for SnapshotCreateReport {
    fn summary(&self) -> String {
        format!(
            "snapshot created: id={} name={} files={}",
            self.id, self.name, self.file_data_count
        )
    }

    fn render_text(&self) -> String {
        format!(
            "snapshot created\n────────────────\n\
             image path        : {}\n\
             id                : {}\n\
             name              : {}\n\
             scope root        : {}\n\
             paths             : {}\n\
             embedded files    : {}\n",
            self.image_path,
            self.id,
            self.name,
            self.scope_root,
            self.path_count,
            self.file_data_count,
        )
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Erzeugt einen Snapshot der Image-Datei `path`.
pub fn create(path: impl AsRef<Path>, options: &CreateOptions) -> ToolsResult<SnapshotCreateReport> {
    let path = path.as_ref();
    let mut session = OdfFileSession::open(path)?;
    let (snap, _flush) = session.mutate(|svc| {
        let snap = match &options.scope_root {
            Some(root) => svc.create_snapshot_scoped(&options.name, root),
            None => svc.create_snapshot(&options.name),
        };
        Ok(snap)
    })?;
    Ok(SnapshotCreateReport {
        image_path: path.display().to_string(),
        id: snap.id,
        name: snap.name,
        scope_root: snap.scope_root,
        path_count: snap.paths.len(),
        file_data_count: snap.file_data.len(),
    })
}

// =====================================================================
// delete
// =====================================================================

/// Strukturierter Report einer `snapshot delete`-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDeleteReport {
    /// Pfad des Volumes.
    pub image_path: String,
    /// Gelöschte Snapshot-ID.
    pub id: u64,
}

impl Report for SnapshotDeleteReport {
    fn summary(&self) -> String {
        format!("snapshot deleted: id={}", self.id)
    }

    fn render_text(&self) -> String {
        format!(
            "snapshot deleted\n────────────────\nimage path : {}\nid         : {}\n",
            self.image_path, self.id
        )
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Löscht den Snapshot mit der ID `snapshot_id` aus der Image-Datei `path`.
pub fn delete(path: impl AsRef<Path>, snapshot_id: u64) -> ToolsResult<SnapshotDeleteReport> {
    let path = path.as_ref();
    let mut session = OdfFileSession::open(path)?;
    session.mutate(|svc| svc.delete_snapshot(snapshot_id))?;
    Ok(SnapshotDeleteReport {
        image_path: path.display().to_string(),
        id: snapshot_id,
    })
}

// =====================================================================
// restore
// =====================================================================

/// Strukturierter Report einer `snapshot restore`-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRestoreReport {
    /// Pfad des Volumes.
    pub image_path: String,
    /// ID des wiederhergestellten Snapshots.
    pub snapshot_id: u64,
    /// Snapshot-Name (zur Bestätigung).
    pub snapshot_name: String,
    /// Anzahl Dateien, die erfolgreich aus den Snapshot-Daten zurückgeschrieben wurden.
    pub restored_files: usize,
    /// Pfade, die nicht wiederhergestellt werden konnten (mit Begründung).
    pub skipped_paths: Vec<String>,
}

impl Report for SnapshotRestoreReport {
    fn summary(&self) -> String {
        if self.skipped_paths.is_empty() {
            format!(
                "snapshot {} restored ({} files)",
                self.snapshot_id, self.restored_files
            )
        } else {
            format!(
                "snapshot {} restored partially ({} files, {} skipped)",
                self.snapshot_id,
                self.restored_files,
                self.skipped_paths.len()
            )
        }
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("snapshot restore\n────────────────\n");
        out.push_str(&format!("image path     : {}\n", self.image_path));
        out.push_str(&format!("snapshot id    : {}\n", self.snapshot_id));
        out.push_str(&format!("snapshot name  : {}\n", self.snapshot_name));
        out.push_str(&format!("restored files : {}\n", self.restored_files));
        out.push_str(&format!("skipped paths  : {}\n", self.skipped_paths.len()));
        for p in &self.skipped_paths {
            out.push_str(&format!("  {}\n", p));
        }
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Stellt den Snapshot mit der ID `snapshot_id` in der Image-Datei `path` wieder her.
pub fn restore(
    path: impl AsRef<Path>,
    snapshot_id: u64,
) -> ToolsResult<SnapshotRestoreReport> {
    let path = path.as_ref();
    let mut session = OdfFileSession::open(path)?;
    let (inner, _flush) = session.mutate(|svc| svc.restore_snapshot(snapshot_id))?;
    Ok(SnapshotRestoreReport {
        image_path: path.display().to_string(),
        snapshot_id: inner.snapshot_id,
        snapshot_name: inner.snapshot_name,
        restored_files: inner.restored_files,
        skipped_paths: inner.skipped_paths,
    })
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
