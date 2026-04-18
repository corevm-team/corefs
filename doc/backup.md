# Backup/Export-Schnittstelle

CoreFS exportiert Volumes in ein stream-basiertes Backup-Format, das sowohl
full-Dumps als auch inkrementelle Dumps gegen einen Basis-Snapshot abdeckt.

## Architektur

```
PersistedState ──▶  stream_dump  ──▶  <frames>  ──▶  stream_restore ──▶ PersistedState
```

- **Kern:** `corefs_core::storage::backup` (`no_std + alloc`)
- **Host-Tools:** `corefs_tools::backup` (`std`, arbeitet auf Image-Dateien)
- **CLI:** `corefs-cli backup dump|restore`

Der Kern kennt weder `std::io` noch Pfade; er ruft nur `BackupWriter::write_all`
und `BackupReader::read_exact` auf Adapter-Traits auf, die der Aufrufer bereitstellt.
Das macht das Modul direkt im AnyOS-Kernel und in Userspace-Tools nutzbar.

## Wire-Format

```
+--------+-------------------+ ... +------------+
| Header |  Frame[0]         |     |  Trailer   |
+--------+-------------------+     +------------+
```

Jedes Frame: `u32`-Länge (LE) + `bincode`-legacy-Payload.

### Header

| Feld              | Typ         | Bedeutung |
|-------------------|-------------|-----------|
| `magic`           | `u64`       | ASCII `"COREFSBK"` als LE-`u64` |
| `version`         | `u16`       | Aktuell `1` |
| `volume_id`       | `[u8; 16]`  | Stabile ID aus Volume-Name + created_at |
| `base_snapshot_id`| `Option<u64>` | `None` = Full, `Some(id)` = Inkrement |
| `created_at`      | `Timestamp` | Dump-Zeitpunkt |
| `entry_count`     | `u32`       | Anzahl folgender Entry-Frames (ohne Trailer) |

### Entries

| Variante          | Zweck |
|-------------------|-------|
| `InodeRecord`     | Kompletter Inode (id, path, kind, size, alle Timestamps, metadata) |
| `Blob`            | Rohbytes für einen Inode (nur wenn `BlobProvider` sie liefert) |
| `Delete`          | Incremental-Marker: Pfad wurde gelöscht |
| `SnapshotRecord`  | Eingebetteter Snapshot (inkl. `file_data`, `inodes`) |
| `VersionRecord`   | Eintrag aus der Versionshistorie |
| `End`             | Trailer, trägt `entries_crc32c` (CRC32C über alle Entry-Payloads) |

## Full vs. Incremental

- **Full** (`since = None`):
  - alle aktiven Inodes
  - alle Snapshots (inkl. `file_data` und `inodes`)
  - alle Versions
  - optional Blobs für aktive Datei-Inhalte (via `BlobProvider`)
- **Incremental** (`since = Some(snapshot_id)`):
  - Inodes mit `changed_at > base_snapshot.created_at`
  - `Delete`-Einträge für Pfade im Basis-Snapshot, die aktuell fehlen
  - Snapshots mit `id > base_snapshot.id`
  - Versions mit `created_at > base_snapshot.created_at`

## Integrität

- **Magic-Check** beim Restore (Ablehnung fremder Streams)
- **Version-Check** (aktuelle Version: `1`; andere werden abgewiesen)
- **CRC32C** über alle Entry-Payloads im Trailer — wird beim Restore
  rollend neu berechnet und gegen den Trailer-Wert verifiziert
- **Frame-Größen-Cap:** 1 GiB pro Frame (schützt vor Fehlstreams)

## CLI

```bash
# Full-Dump in Datei
corefs-cli backup dump /path/to/vol.img --output /tmp/vol.bkp --json

# Inkremental gegen Snapshot-ID 3
corefs-cli backup dump /path/to/vol.img \
    --output /tmp/vol.bkp.inc --since 3 --json

# Restore auf ein frisches oder existierendes Image
corefs-cli backup restore /path/to/target.img --input /tmp/vol.bkp --json

# Stdout/Stdin (ohne --output / --input)
corefs-cli backup dump    vol.img       | gzip > vol.bkp.gz
corefs-cli backup restore target.img < <(gunzip < vol.bkp.gz)
```

## Programmatic Use

```rust
use corefs_core::storage::backup::{stream_dump, stream_restore, SliceReader};
use corefs_core::platform::Timestamp;

// Dump
let mut sink: Vec<u8> = Vec::new();
let report = stream_dump(&state, None, &mut sink, Timestamp::now())?;
println!("wrote {} entries", report.entries_written);

// Restore
let mut reader = SliceReader::new(&sink);
let r = stream_restore(&mut target, &mut reader)?;
```

## Blob-Rekonstruktion

Aktive Datei-Inhalte liegen auf dem Block-Device (via `BlockRecord`-Extents).
Der Kern ist block-device-agnostisch; der Aufrufer liefert bei Bedarf einen
`BlobProvider`:

```rust
pub trait BlobProvider {
    fn read_inode(&mut self, inode_id: InodeId) -> Option<Vec<u8>>;
}
```

Snapshot-`file_data` wird automatisch als `SnapshotRecord` mitgeführt —
für die meisten Use-Cases reicht das, da Produktiv-Backups ohnehin einen
Snapshot als konsistenten Zustand nutzen sollten.

## Restore-Semantik

- **Inode-Kollision per Pfad:** überschreiben
- **Snapshot-Kollision per ID:** überschreiben; `next_snapshot_id` wird
  ggf. hochgezogen
- **Blob-Einträge:** werden in einen ad-hoc `restore-blobs`-Snapshot
  installiert, damit die Daten über die Snapshot-API abrufbar bleiben.
  Dieser Hook wird in späteren Integrationsschritten durch direkte
  Block-Device-Writes der App-Schicht ersetzt.

## Tests

Kern: `corefs-core/src/storage/backup_tests.rs` — 14 Tests
(full/inkremental Roundtrip, Magic/Version-Mismatch, CRC-Detection,
truncated-stream, volume-id-stability, delete-marker-Semantik,
Provider-Beitrag, Wire-Format-Regression).

Host: `corefs-tools/src/backup_tests.rs` — 5 Tests
(dump-full-roundtrip, restore-auf-fresh, inkrement-mit-bad-base,
JSON-Rendering, missing-input).

## Bekannte Limits

- Aktiver Datei-Inhalt (nicht gepinnt in einem Snapshot) landet nur
  im Stream, wenn der Aufrufer einen `BlobProvider` übergibt.
  Der Host-Pfad (`corefs-tools`) nutzt aktuell keinen —
  produktive Backups sollten zuerst einen Snapshot erzeugen.
- Keine Stream-Kompression: das Output ist `bincode`-Rohformat.
  Komprimieren per externer Pipeline (`gzip`, `zstd`, …).
- Keine Verschlüsselung im Backup-Stream selbst. Geheime Inhalte
  sollten nur in verschlüsselten Containern (z. B. `age`, GPG) transportiert
  werden.
