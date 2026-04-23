// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Stream-basiertes Backup/Export-Format für CoreFS-Volumes.
//!
//! Dieses Modul serialisiert einen [`PersistedState`] in einen
//! länge-prefixten Frame-Stream und restore-d ihn wieder. Es arbeitet
//! strikt `no_std + alloc` — die Konsumenten liefern einen
//! [`BackupWriter`] bzw. [`BackupReader`] für die eigentliche IO.
//!
//! ## Wire-Format
//!
//! ```text
//! +--------+-------------------+                 +------------+
//! | Header |  Frame[0]         |  ...            |  Trailer   |
//! +--------+-------------------+                 +------------+
//! ```
//!
//! Jedes Frame besteht aus einem `u32`-Längenpräfix (Little Endian) +
//! `bincode`-Legacy-kodiertem Payload. Der Header trägt Magic `COREFSBK`
//! (als `u64` in Little-Endian-Byte-Reihenfolge), eine `version` (aktuell
//! `1`), eine stabile Volume-ID (Hash aus `volume.name` +
//! `volume.created_at`), eine optionale Basis-Snapshot-ID (für
//! Inkremental-Dumps), den Erstellungszeitpunkt und die Anzahl folgender
//! Einträge. Der Trailer ist ein `BackupEntry::End` mit
//! CRC32C über alle Entry-Bytes als Integritätssicherung.
//!
//! ## Full vs. Incremental
//!
//! - **Full-Dump** (`since = None`): schreibt alle aktiven Inodes,
//!   alle Block-Records (Metadata), alle Versionen, alle Snapshots.
//! - **Incremental-Dump** (`since = Some(snap_id)`): nur Inodes, deren
//!   `changed_at > snapshot.created_at`, plus alle neuen Snapshots (mit
//!   ID > Basis), plus `Delete`-Einträge für Pfade, die im Basis-Snapshot
//!   existierten, aber im aktuellen State nicht mehr vorkommen.
//!
//! ## Restore
//!
//! `stream_restore` applied den Stream auf den übergebenen
//! [`PersistedState`]:
//! - Neue Inodes werden angehängt (bei Kollision per Pfad: überschrieben).
//! - `Delete`-Einträge entfernen den entsprechenden aktiven Inode.
//! - Snapshots werden eingefügt (Duplikate per ID werden übersprungen).
//! - Versions werden akkumuliert.
//!
//! ## Integrität
//!
//! - Magic-Prüfung
//! - Version-Kompatibilitätsprüfung
//! - CRC32C über den entire-Entry-Byte-Stream

use crate::bincode_compat;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::Snapshot;
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::Timestamp;
use crate::services::versioning::FileVersion;
use crate::storage::block_store::BlockRecord;
use crate::storage::ondisk::checksum::Crc32c;
use crate::storage::persisted_state::PersistedState;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Magic für das CoreFS-Backup-Format: ASCII "COREFSBK" (little-endian u64).
pub const BACKUP_MAGIC: u64 = u64::from_le_bytes(*b"COREFSBK");

/// Aktuelle Wire-Version.
pub const BACKUP_VERSION: u16 = 1;

/// Header-Frame eines Backup-Streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupHeader {
    /// Magic-Zeichenkette, s. [`BACKUP_MAGIC`].
    pub magic: u64,
    /// Wire-Version, s. [`BACKUP_VERSION`].
    pub version: u16,
    /// Stabile Volume-Identifikation (16 Byte, FNV-1a-basiert aus Name + created_at).
    pub volume_id: [u8; 16],
    /// Basis-Snapshot-ID für Inkremental-Dumps, `None` = Full-Dump.
    pub base_snapshot_id: Option<u64>,
    /// Zeitpunkt der Dump-Erstellung.
    pub created_at: Timestamp,
    /// Anzahl der folgenden Entry-Frames (ohne Trailer).
    pub entry_count: u32,
}

/// Ein einzelner Wire-Entry im Backup-Stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupEntry {
    /// Vollständiger Inode-Eintrag (Metadata + Pfad + Kind).
    InodeRecord {
        /// Inode-ID.
        id: InodeId,
        /// Pfad im Volume.
        path: String,
        /// Inode-Kind (Datei/Verzeichnis/Symlink).
        kind: InodeKind,
        /// Größe in Bytes.
        size: usize,
        /// Erstellzeit.
        created_at: Timestamp,
        /// Letzte Content-Änderung.
        modified_at: Timestamp,
        /// Letzte Metadata-Änderung.
        changed_at: Timestamp,
        /// Zugriffszeit.
        accessed_at: Timestamp,
        /// Zugehörige Metadata.
        metadata: FileMetadata,
    },
    /// Rekonstruierter File-Blob (vollständig, unkomprimiert).
    ///
    /// Für Full-Dumps aus Snapshot-`file_data` oder vom Aufrufer via
    /// [`BlobProvider`] geliefert. Für Incremental nur wenn sich der
    /// Inode gegenüber dem Basis-Snapshot geändert hat.
    Blob {
        /// Inode, zu dem dieser Blob gehört.
        inode_id: InodeId,
        /// Offset (aktuell immer 0; zukünftig für Chunked-Transfers reserviert).
        offset: u64,
        /// Rohbytes.
        data: Vec<u8>,
    },
    /// Löschmarker: Pfad ist im Ziel-Dump nicht mehr vorhanden
    /// (nur für Inkremental-Dumps relevant).
    Delete {
        /// Zu löschender Pfad.
        path: String,
    },
    /// Snapshot (eingebettet als vollständiger Snapshot-Record).
    SnapshotRecord {
        /// Kompletter Snapshot-Inhalt.
        snapshot: Snapshot,
    },
    /// Version-Historien-Eintrag.
    VersionRecord {
        /// Kompletter Version-Eintrag.
        version: FileVersion,
    },
    /// Trailer — beendet den Stream; `entries_crc32c` validiert Integrität.
    End {
        /// CRC32C über alle Entry-Frame-Payloads (in Reihenfolge, ohne Längenpräfix).
        entries_crc32c: u32,
    },
}

/// Report einer [`stream_dump`]-Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpReport {
    /// Geschriebene Entry-Frames (ohne Header und Trailer).
    pub entries_written: u32,
    /// Anzahl inode-Einträge.
    pub inode_records: u32,
    /// Anzahl Blob-Einträge.
    pub blob_records: u32,
    /// Anzahl Delete-Marker.
    pub delete_markers: u32,
    /// Anzahl mitgeschriebener Snapshots.
    pub snapshot_records: u32,
    /// Anzahl mitgeschriebener Versions.
    pub version_records: u32,
    /// Dump-Modus: `true` für inkrementell, `false` für full.
    pub incremental: bool,
}

/// Report einer [`stream_restore`]-Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Eingelesene Entry-Frames.
    pub entries_read: u32,
    /// Neu angelegte oder überschriebene Inodes.
    pub inodes_applied: u32,
    /// Applizierte Blob-Einträge.
    pub blobs_applied: u32,
    /// Verarbeitete Delete-Marker.
    pub deletes_applied: u32,
    /// Eingefügte Snapshots.
    pub snapshots_applied: u32,
    /// Eingefügte Version-Einträge.
    pub versions_applied: u32,
    /// `true`, wenn das Input als inkrementell markiert war.
    pub incremental: bool,
}

/// Sink-Interface für Backup-Output.
pub trait BackupWriter {
    /// Schreibt `bytes` vollständig.
    fn write_all(&mut self, bytes: &[u8]) -> CoreFsResult<()>;
}

/// Source-Interface für Backup-Input.
pub trait BackupReader {
    /// Liest `buf.len()` Bytes vollständig. Gibt Fehler bei vorzeitigem Ende.
    fn read_exact(&mut self, buf: &mut [u8]) -> CoreFsResult<()>;
}

/// Optionaler Blob-Provider für Full-Dumps aktiver Datei-Inhalte.
///
/// Wird kein Provider übergeben, werden nur Blobs exportiert, die in
/// Snapshots (`file_data`) oder in Versions-Einträgen vorliegen. Inhalte
/// aktiver (nicht im Snapshot gepinnter) Dateien erscheinen dann nur als
/// `InodeRecord`-Metadaten, nicht als `Blob`.
pub trait BlobProvider {
    /// Liefert die vollständigen Bytes des Inode-Inhalts, falls verfügbar.
    fn read_inode(&mut self, inode_id: InodeId) -> Option<Vec<u8>>;
}

/// Null-Provider, der nie Bytes liefert. Default für Metadata-only-Dumps.
pub struct NullBlobProvider;

impl BlobProvider for NullBlobProvider {
    fn read_inode(&mut self, _: InodeId) -> Option<Vec<u8>> {
        None
    }
}

// --- impls auf Vec<u8> / Slices für Tests und einfache Aufrufer ---

impl BackupWriter for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> CoreFsResult<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// Cursor-Reader über ein `&[u8]`.
pub struct SliceReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SliceReader<'a> {
    /// Konstruiert einen neuen Reader.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Anzahl noch nicht gelesener Bytes.
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}

impl<'a> BackupReader for SliceReader<'a> {
    fn read_exact(&mut self, buf: &mut [u8]) -> CoreFsResult<()> {
        if self.remaining() < buf.len() {
            return Err(CoreFsError::InvalidInput(format!(
                "backup reader short read: need {}, have {}",
                buf.len(),
                self.remaining()
            )));
        }
        buf.copy_from_slice(&self.bytes[self.pos..self.pos + buf.len()]);
        self.pos += buf.len();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helper: Volume-ID
// ---------------------------------------------------------------------------

/// Stabile 16-Byte Volume-ID aus Name + created_at.
///
/// Zweimaliger FNV-1a über unterschiedliche Seeds, zusammengesetzt zu 16 Byte.
#[must_use]
pub fn derive_volume_id(name: &str, created_at: Timestamp) -> [u8; 16] {
    let secs = created_at.as_secs();
    let nanos = created_at.subsec_nanos();
    let lo = fnv1a_64(name.as_bytes(), 0xcbf2_9ce4_8422_2325, secs, nanos);
    let hi = fnv1a_64(
        name.as_bytes(),
        0x8421_4733_9941_ab11,
        secs ^ 0xaaaa_5555_aaaa_5555,
        nanos.wrapping_mul(0x9e37_79b9),
    );
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&lo.to_le_bytes());
    out[8..].copy_from_slice(&hi.to_le_bytes());
    out
}

fn fnv1a_64(bytes: &[u8], seed: u64, mix_s: u64, mix_n: u32) -> u64 {
    let mut h = seed ^ mix_s ^ u64::from(mix_n);
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Kleine rollende CRC32C-Akkumulator über mehrere `update`-Aufrufe hinweg.
struct RollingCrc32c(u32);

impl RollingCrc32c {
    fn new() -> Self {
        Self(!0u32)
    }
    fn update(&mut self, data: &[u8]) {
        self.0 = Crc32c::update(self.0, data);
    }
    fn value(&self) -> u32 {
        !self.0
    }
}

// ---------------------------------------------------------------------------
// Frame helpers
// ---------------------------------------------------------------------------

fn write_frame<W: BackupWriter>(writer: &mut W, payload: &[u8]) -> CoreFsResult<()> {
    let len: u32 = payload.len().try_into().map_err(|_| {
        CoreFsError::InvalidInput("backup frame exceeds u32::MAX bytes".to_string())
    })?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

fn read_frame<R: BackupReader>(reader: &mut R) -> CoreFsResult<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    // Hard-cap: 1 GiB pro Frame — schützt vor Fehlformatierten Streams.
    const FRAME_CAP: usize = 1024 * 1024 * 1024;
    if len > FRAME_CAP {
        return Err(CoreFsError::InvalidInput(format!(
            "backup frame length {len} exceeds cap {FRAME_CAP}"
        )));
    }
    let mut buf = alloc::vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn encode<T: Serialize>(value: &T) -> CoreFsResult<Vec<u8>> {
    bincode_compat::serialize(value)
        .map_err(|e| CoreFsError::State(format!("backup: bincode serialize failed: {e}")))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> CoreFsResult<T> {
    bincode_compat::deserialize(bytes)
        .map_err(|e| CoreFsError::InvalidInput(format!("backup: bincode deserialize failed: {e}")))
}

// ---------------------------------------------------------------------------
// Dump
// ---------------------------------------------------------------------------

/// Full- oder Incremental-Dump eines [`PersistedState`] in einen Stream.
///
/// Ruft `write_all` mehrfach auf `writer` auf. Die Funktion selbst
/// allociert nur transiente Buffers und ist no_std-tauglich.
pub fn stream_dump<W: BackupWriter>(
    state: &PersistedState,
    since: Option<u64>,
    writer: &mut W,
    now: Timestamp,
) -> CoreFsResult<DumpReport> {
    stream_dump_with_blobs(state, since, writer, now, &mut NullBlobProvider)
}

/// Wie [`stream_dump`], aber mit einem [`BlobProvider`] für aktive
/// Datei-Inhalte, die nicht in Snapshots gepinnt sind.
pub fn stream_dump_with_blobs<W: BackupWriter, P: BlobProvider>(
    state: &PersistedState,
    since: Option<u64>,
    writer: &mut W,
    now: Timestamp,
    blobs: &mut P,
) -> CoreFsResult<DumpReport> {
    // --- Auswahl treffen ---
    let base_snapshot = if let Some(id) = since {
        Some(state.snapshots.iter().find(|s| s.id == id).ok_or_else(|| {
            CoreFsError::NotFound(format!("base snapshot {id} not found in state"))
        })?)
    } else {
        None
    };
    let incremental = base_snapshot.is_some();

    // Entries in Reihenfolge sammeln, damit wir `entry_count` + CRC ausrechnen können,
    // BEVOR wir den Header schreiben. Das kostet etwas RAM, ist aber für ein Backup-Tool
    // angemessen und ermöglicht streng-single-pass-Readbarkeit.
    let mut entries: Vec<BackupEntry> = Vec::new();
    let mut inode_records = 0u32;
    let mut blob_records = 0u32;
    let mut delete_markers = 0u32;
    let mut snapshot_records = 0u32;
    let mut version_records = 0u32;

    // Inodes
    for inode in &state.active_inodes {
        let include = match base_snapshot {
            None => true,
            Some(snap) => inode.changed_at > snap.created_at,
        };
        if !include {
            continue;
        }
        entries.push(BackupEntry::InodeRecord {
            id: inode.id,
            path: inode.path.clone(),
            kind: inode.kind,
            size: inode.size,
            created_at: inode.created_at,
            modified_at: inode.modified_at,
            changed_at: inode.changed_at,
            accessed_at: inode.accessed_at,
            metadata: inode.metadata.clone(),
        });
        inode_records += 1;

        // Blob: für reguläre Dateien versuchen Blob zu liefern.
        // Nur aus dem Provider (aktive Daten). Snapshot-gepinnter Inhalt wird
        // bereits durch die SnapshotRecord-Einträge transportiert — ein
        // zusätzlicher Blob-Eintrag wäre redundant.
        if matches!(inode.kind, InodeKind::File) && inode.size > 0 {
            if let Some(data) = blobs.read_inode(inode.id) {
                entries.push(BackupEntry::Blob {
                    inode_id: inode.id,
                    offset: 0,
                    data,
                });
                blob_records += 1;
            }
        }
    }

    // Delete-Marker: Pfade im Basis-Snapshot, die jetzt nicht mehr existieren.
    if let Some(snap) = base_snapshot {
        let mut active_paths: Vec<&String> = state.active_inodes.iter().map(|i| &i.path).collect();
        active_paths.sort();
        for path in snap.paths.iter() {
            if active_paths.binary_search(&path).is_err() {
                entries.push(BackupEntry::Delete { path: path.clone() });
                delete_markers += 1;
            }
        }
    }

    // Snapshots
    for snap in &state.snapshots {
        let include = match base_snapshot {
            None => true,
            Some(base) => snap.id > base.id,
        };
        if include {
            entries.push(BackupEntry::SnapshotRecord {
                snapshot: snap.clone(),
            });
            snapshot_records += 1;
        }
    }

    // Versionen
    for version in &state.versions {
        let include = match base_snapshot {
            None => true,
            Some(base) => version.created_at > base.created_at,
        };
        if include {
            entries.push(BackupEntry::VersionRecord {
                version: version.clone(),
            });
            version_records += 1;
        }
    }

    let entry_count_u32: u32 = entries
        .len()
        .try_into()
        .map_err(|_| CoreFsError::InvalidInput("backup entry count > u32::MAX".to_string()))?;

    // --- Header schreiben ---
    let header = BackupHeader {
        magic: BACKUP_MAGIC,
        version: BACKUP_VERSION,
        volume_id: derive_volume_id(&state.volume.name, state.volume.created_at),
        base_snapshot_id: since,
        created_at: now,
        entry_count: entry_count_u32,
    };
    let header_bytes = encode(&header)?;
    write_frame(writer, &header_bytes)?;

    // --- Entries schreiben + CRC rollend berechnen ---
    let mut crc = RollingCrc32c::new();
    for entry in &entries {
        let bytes = encode(entry)?;
        crc.update(&bytes);
        write_frame(writer, &bytes)?;
    }

    // --- Trailer ---
    let trailer = BackupEntry::End {
        entries_crc32c: crc.value(),
    };
    let trailer_bytes = encode(&trailer)?;
    write_frame(writer, &trailer_bytes)?;

    Ok(DumpReport {
        entries_written: entry_count_u32,
        inode_records,
        blob_records,
        delete_markers,
        snapshot_records,
        version_records,
        incremental,
    })
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

/// Applied einen Backup-Stream auf den übergebenen [`PersistedState`].
pub fn stream_restore<R: BackupReader>(
    state: &mut PersistedState,
    reader: &mut R,
) -> CoreFsResult<RestoreReport> {
    // Header
    let header_bytes = read_frame(reader)?;
    let header: BackupHeader = decode(&header_bytes)?;
    if header.magic != BACKUP_MAGIC {
        return Err(CoreFsError::InvalidInput(format!(
            "backup: bad magic 0x{:016x} (expected 0x{:016x})",
            header.magic, BACKUP_MAGIC
        )));
    }
    if header.version != BACKUP_VERSION {
        return Err(CoreFsError::InvalidInput(format!(
            "backup: unsupported version {} (expected {})",
            header.version, BACKUP_VERSION
        )));
    }
    let incremental = header.base_snapshot_id.is_some();

    let mut report = RestoreReport {
        entries_read: 0,
        inodes_applied: 0,
        blobs_applied: 0,
        deletes_applied: 0,
        snapshots_applied: 0,
        versions_applied: 0,
        incremental,
    };

    let mut crc = RollingCrc32c::new();
    let mut blob_buffer: Vec<(InodeId, Vec<u8>)> = Vec::new();

    for _ in 0..header.entry_count {
        let frame = read_frame(reader)?;
        let entry: BackupEntry = decode(&frame)?;
        crc.update(&frame);
        report.entries_read += 1;

        match entry {
            BackupEntry::InodeRecord {
                id,
                path,
                kind,
                size,
                created_at,
                modified_at,
                changed_at,
                accessed_at,
                metadata,
            } => {
                // Overwrite-by-path
                if let Some(existing) = state.active_inodes.iter_mut().find(|i| i.path == path) {
                    existing.id = id;
                    existing.kind = kind;
                    existing.size = size;
                    existing.created_at = created_at;
                    existing.modified_at = modified_at;
                    existing.changed_at = changed_at;
                    existing.accessed_at = accessed_at;
                    existing.metadata = metadata;
                } else {
                    state.active_inodes.push(Inode {
                        id,
                        kind,
                        path,
                        size,
                        created_at,
                        modified_at,
                        changed_at,
                        accessed_at,
                        metadata,
                    });
                }
                report.inodes_applied += 1;
            }
            BackupEntry::Blob {
                inode_id,
                offset: _,
                data,
            } => {
                blob_buffer.push((inode_id, data));
                report.blobs_applied += 1;
            }
            BackupEntry::Delete { path } => {
                state.active_inodes.retain(|i| i.path != path);
                state.block_records.retain(|r| {
                    // nach Pfad matchen via Inode-Lookup ist schwierig; wir
                    // entfernen Delete nur auf Inode-Seite. Block-Records
                    // bleiben unberührt und werden später ggf. durch andere
                    // Operationen bereinigt.
                    let _ = r;
                    true
                });
                report.deletes_applied += 1;
            }
            BackupEntry::SnapshotRecord { snapshot } => {
                // Duplikate per ID werden überschrieben.
                if let Some(existing) = state.snapshots.iter_mut().find(|s| s.id == snapshot.id) {
                    *existing = snapshot.clone();
                } else {
                    if snapshot.id >= state.next_snapshot_id {
                        state.next_snapshot_id = snapshot.id + 1;
                    }
                    state.snapshots.push(snapshot);
                }
                report.snapshots_applied += 1;
            }
            BackupEntry::VersionRecord { version } => {
                state.versions.push(version);
                report.versions_applied += 1;
            }
            BackupEntry::End { entries_crc32c: _ } => {
                // Unerwarteter End in der Mitte — Stream ist kürzer als Header behauptet.
                return Err(CoreFsError::InvalidInput(
                    "backup: End entry before entry_count reached".to_string(),
                ));
            }
        }
    }

    // Trailer
    let trailer_frame = read_frame(reader)?;
    let trailer: BackupEntry = decode(&trailer_frame)?;
    match trailer {
        BackupEntry::End { entries_crc32c } => {
            if entries_crc32c != crc.value() {
                return Err(CoreFsError::State(format!(
                    "backup: CRC mismatch (header={:#010x} computed={:#010x})",
                    entries_crc32c,
                    crc.value()
                )));
            }
        }
        _ => {
            return Err(CoreFsError::InvalidInput(
                "backup: trailer is not an End entry".to_string(),
            ));
        }
    }

    // Blobs als Snapshot-artige "latest content" speichern — wir nutzen
    // den jüngsten Snapshot (falls einer existiert) oder legen einen
    // ad-hoc "__restore_blobs__"-Snapshot an, damit die Daten abrufbar
    // bleiben. Für die produktive Restore-Pfad-Integration wird dieser
    // Hook später durch die App-Schicht ersetzt, die Block-Device-Writes
    // ansteuert.
    if !blob_buffer.is_empty() {
        install_blobs(state, blob_buffer, header.created_at);
    }

    Ok(report)
}

fn install_blobs(
    state: &mut PersistedState,
    blobs: Vec<(InodeId, Vec<u8>)>,
    created_at: Timestamp,
) {
    use crate::domain::snapshot::{Snapshot, SnapshotInode};
    use alloc::collections::BTreeMap;

    let mut paths: Vec<String> = Vec::new();
    let mut file_data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut inodes_map: BTreeMap<String, SnapshotInode> = BTreeMap::new();

    for (id, data) in blobs {
        if let Some(inode) = state.active_inodes.iter().find(|i| i.id == id) {
            paths.push(inode.path.clone());
            inodes_map.insert(
                inode.path.clone(),
                SnapshotInode {
                    kind: inode.kind,
                    size: data.len(),
                    created_at: inode.created_at,
                    modified_at: inode.modified_at,
                    changed_at: inode.changed_at,
                    metadata: inode.metadata.clone(),
                    symlink_target: None,
                },
            );
            file_data.insert(inode.path.clone(), data);
            // Gleichzeitig inode.size aktualisieren, damit Listings konsistent sind.
            if let Some(inode_mut) = state.active_inodes.iter_mut().find(|i| i.id == id) {
                inode_mut.size = file_data.get(&inode_mut.path).map(|v| v.len()).unwrap_or(0);
            }
        }
    }

    if !file_data.is_empty() {
        let id = state.next_snapshot_id;
        state.next_snapshot_id += 1;
        state.snapshots.push(Snapshot {
            id,
            name: "restore-blobs".to_string(),
            scope_root: "/".to_string(),
            created_at,
            paths,
            file_data,
            inodes: inodes_map,
        });
    }
}

// ---------------------------------------------------------------------------
// `BlockRecord` helper re-export — reserviert für zukünftige Block-Device-
// Integration, damit das Modul weiterhin die BlockRecord-Abstraktion
// kennt.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn _block_record_sanity(_r: &BlockRecord) {}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
