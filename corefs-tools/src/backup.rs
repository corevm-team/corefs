// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für das Stream-Backup-Format.
//!
//! Wrapper um [`corefs_core::storage::backup`] — operiert auf Image-Dateien:
//!
//! - [`dump`]    — erzeugt einen Backup-Stream aus einem ODF-Volume
//! - [`restore`] — applied einen Backup-Stream auf ein existierendes
//!                 (leeres oder gefülltes) Volume
//!
//! Unterstützt full-Dumps (`since = None`) und Inkremental-Dumps gegen
//! eine bekannte Basis-Snapshot-ID (`since = Some(snapshot_id)`).

use crate::error::{ToolsError, ToolsResult};
use crate::report::{Report, to_pretty_json};
use corefs::storage::ondisk::session::OdfFileSession;
use corefs_core::error::CoreFsError;
use corefs_core::platform::Timestamp;
use corefs_core::storage::backup::{
    BackupReader, BackupWriter, DumpReport as CoreDumpReport,
    RestoreReport as CoreRestoreReport, SliceReader, stream_dump, stream_restore,
};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Writer/Reader-Adapter für std::io
// ---------------------------------------------------------------------------

struct IoWriter<W: Write> {
    inner: W,
}

impl<W: Write> BackupWriter for IoWriter<W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), CoreFsError> {
        self.inner
            .write_all(bytes)
            .map_err(|e| CoreFsError::State(format!("backup write failed: {e}")))
    }
}

#[allow(dead_code)]
struct IoReader<R: Read> {
    inner: R,
}

#[allow(dead_code)]
impl<R: Read> BackupReader for IoReader<R> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), CoreFsError> {
        self.inner
            .read_exact(buf)
            .map_err(|e| CoreFsError::InvalidInput(format!("backup read failed: {e}")))
    }
}

fn te(msg: String) -> ToolsError {
    ToolsError::Core(CoreFsError::State(msg))
}

fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| Timestamp::from_secs_nanos(d.as_secs(), d.subsec_nanos()))
        .unwrap_or(Timestamp::EPOCH)
}

// ---------------------------------------------------------------------------
// dump
// ---------------------------------------------------------------------------

/// Strukturierter Report für [`dump`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupDumpReport {
    /// Pfad des Quell-Images.
    pub image_path: String,
    /// Pfad der Output-Datei (oder `"-"` für stdout).
    pub output_path: String,
    /// Bytes, die in den Output geschrieben wurden.
    pub bytes_written: u64,
    /// `true`, wenn der Dump inkrementell war.
    pub incremental: bool,
    /// Basis-Snapshot-ID (nur bei Inkremental).
    pub base_snapshot_id: Option<u64>,
    /// Anzahl inode-Einträge im Stream.
    pub inode_records: u32,
    /// Anzahl Blob-Einträge.
    pub blob_records: u32,
    /// Anzahl Delete-Marker (nur bei Inkremental relevant).
    pub delete_markers: u32,
    /// Anzahl mitgeschriebener Snapshots.
    pub snapshot_records: u32,
    /// Anzahl mitgeschriebener Versions.
    pub version_records: u32,
    /// Gesamtanzahl Entry-Frames.
    pub entries_written: u32,
}

impl Report for BackupDumpReport {
    fn summary(&self) -> String {
        let mode = if self.incremental {
            format!("incremental (base={})", self.base_snapshot_id.unwrap_or(0))
        } else {
            "full".to_string()
        };
        format!(
            "backup dump {} → {} ({} entries, {} bytes, {})",
            self.image_path, self.output_path, self.entries_written, self.bytes_written, mode
        )
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("backup dump {}\n", self.image_path));
        out.push_str("──────────────\n");
        out.push_str(&format!("  output          : {}\n", self.output_path));
        out.push_str(&format!(
            "  mode            : {}\n",
            if self.incremental { "incremental" } else { "full" }
        ));
        if let Some(base) = self.base_snapshot_id {
            out.push_str(&format!("  base snapshot   : {}\n", base));
        }
        out.push_str(&format!("  entries         : {}\n", self.entries_written));
        out.push_str(&format!("    inodes        : {}\n", self.inode_records));
        out.push_str(&format!("    blobs         : {}\n", self.blob_records));
        out.push_str(&format!("    deletes       : {}\n", self.delete_markers));
        out.push_str(&format!("    snapshots     : {}\n", self.snapshot_records));
        out.push_str(&format!("    versions      : {}\n", self.version_records));
        out.push_str(&format!("  bytes written   : {}\n", self.bytes_written));
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Dump ein Volume-Image in eine Backup-Stream-Datei.
///
/// Wenn `output` `None` ist, wird nach stdout geschrieben.
pub fn dump(
    image: &Path,
    output: Option<&Path>,
    since: Option<u64>,
) -> ToolsResult<BackupDumpReport> {
    let sess = OdfFileSession::open(image)
        .map_err(|e| te(format!("open image {}: {e}", image.display())))?;
    let state = sess.service().persisted_state();

    // Output-Ziel: Datei oder stdout
    let (output_path_str, bytes_written, core_report): (String, u64, CoreDumpReport) =
        match output {
            Some(path) => {
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(path)
                    .map_err(|e| {
                        te(format!("open output {}: {e}", path.display()))
                    })?;
                let mut counting = CountingWriter::new(BufWriter::new(file));
                let core_report = {
                    let mut adapter = IoWriter {
                        inner: &mut counting,
                    };
                    stream_dump(&state, since, &mut adapter, now())
                        .map_err(|e| te(format!("backup dump: {e}")))?
                };
                // Flush BufWriter
                counting
                    .inner
                    .flush()
                    .map_err(|e| te(format!("flush output: {e}")))?;
                (path.display().to_string(), counting.count, core_report)
            }
            None => {
                let stdout = std::io::stdout();
                let mut lock = stdout.lock();
                let mut counting = CountingWriter::new(&mut lock);
                let core_report = {
                    let mut adapter = IoWriter {
                        inner: &mut counting,
                    };
                    stream_dump(&state, since, &mut adapter, now())
                        .map_err(|e| te(format!("backup dump: {e}")))?
                };
                ("-".to_string(), counting.count, core_report)
            }
        };

    Ok(BackupDumpReport {
        image_path: image.display().to_string(),
        output_path: output_path_str,
        bytes_written,
        incremental: core_report.incremental,
        base_snapshot_id: since,
        inode_records: core_report.inode_records,
        blob_records: core_report.blob_records,
        delete_markers: core_report.delete_markers,
        snapshot_records: core_report.snapshot_records,
        version_records: core_report.version_records,
        entries_written: core_report.entries_written,
    })
}

// ---------------------------------------------------------------------------
// restore
// ---------------------------------------------------------------------------

/// Strukturierter Report für [`restore`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRestoreReport {
    /// Pfad des Ziel-Images.
    pub image_path: String,
    /// Pfad der Input-Datei (oder `"-"` für stdin).
    pub input_path: String,
    /// Bytes, die aus dem Input gelesen wurden.
    pub bytes_read: u64,
    /// `true`, wenn das Input als inkrementell markiert war.
    pub incremental: bool,
    /// Eingelesene Entry-Frames.
    pub entries_read: u32,
    /// Applizierte Inodes.
    pub inodes_applied: u32,
    /// Applizierte Blobs.
    pub blobs_applied: u32,
    /// Verarbeitete Delete-Marker.
    pub deletes_applied: u32,
    /// Applizierte Snapshots.
    pub snapshots_applied: u32,
    /// Applizierte Versions.
    pub versions_applied: u32,
}

impl Report for BackupRestoreReport {
    fn summary(&self) -> String {
        format!(
            "backup restore {} → {} ({} entries, {} bytes)",
            self.input_path, self.image_path, self.entries_read, self.bytes_read
        )
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("backup restore {}\n", self.image_path));
        out.push_str("──────────────\n");
        out.push_str(&format!("  input           : {}\n", self.input_path));
        out.push_str(&format!(
            "  mode            : {}\n",
            if self.incremental { "incremental" } else { "full" }
        ));
        out.push_str(&format!("  entries read    : {}\n", self.entries_read));
        out.push_str(&format!("    inodes        : {}\n", self.inodes_applied));
        out.push_str(&format!("    blobs         : {}\n", self.blobs_applied));
        out.push_str(&format!("    deletes       : {}\n", self.deletes_applied));
        out.push_str(&format!("    snapshots     : {}\n", self.snapshots_applied));
        out.push_str(&format!("    versions      : {}\n", self.versions_applied));
        out.push_str(&format!("  bytes read      : {}\n", self.bytes_read));
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Applied einen Backup-Stream auf ein Volume-Image.
///
/// Das Image wird mit [`OdfFileSession::open`] geöffnet, gemutated und
/// per [`OdfFileSession::flush`] persistiert.
pub fn restore(image: &Path, input: Option<&Path>) -> ToolsResult<BackupRestoreReport> {
    // Lese vollständiges Input in den Speicher: einerseits um Byte-Zähler zu
    // haben, andererseits weil wir den Stream über einen SliceReader
    // abarbeiten (single-pass, kein Seek nötig).
    let (input_path_str, payload) = match input {
        Some(path) => {
            let mut f = File::open(path).map_err(|e| {
                te(format!("open input {}: {e}", path.display()))
            })?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)
                .map_err(|e| te(format!("read input: {e}")))?;
            (path.display().to_string(), buf)
        }
        None => {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut buf = Vec::new();
            lock.read_to_end(&mut buf)
                .map_err(|e| te(format!("read stdin: {e}")))?;
            ("-".to_string(), buf)
        }
    };
    let bytes_read = payload.len() as u64;

    let mut sess = OdfFileSession::open(image)
        .map_err(|e| te(format!("open image {}: {e}", image.display())))?;

    let (report, _flush) = sess
        .mutate(|service| {
            let mut state = service.persisted_state();
            let mut reader = SliceReader::new(&payload);
            let r = stream_restore(&mut state, &mut reader)
                .map_err(|e| CoreFsError::State(format!("backup restore: {e}")))?;
            // Service aus neuem State neu bauen.
            *service = corefs::app::CoreFsService::from_persisted_state(state);
            Ok::<_, CoreFsError>(r)
        })
        .map_err(|e| te(format!("mutate: {e}")))?;

    Ok(BackupRestoreReport {
        image_path: image.display().to_string(),
        input_path: input_path_str,
        bytes_read,
        incremental: report.incremental,
        entries_read: report.entries_read,
        inodes_applied: report.inodes_applied,
        blobs_applied: report.blobs_applied,
        deletes_applied: report.deletes_applied,
        snapshots_applied: report.snapshots_applied,
        versions_applied: report.versions_applied,
    })
}

// ---------------------------------------------------------------------------
// Hilfstyp: Writer mit Byte-Zähler
// ---------------------------------------------------------------------------

struct CountingWriter<W: Write> {
    inner: W,
    count: u64,
}

impl<W: Write> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, count: 0 }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// Internal helpers exported for test-adjacency
#[allow(dead_code)]
pub(crate) fn _touch_path(_p: PathBuf) {}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
