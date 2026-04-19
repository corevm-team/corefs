# Snapshots & Versionierung

CoreFS bietet zwei komplementäre Zeitreise-Mechanismen:

- **Automatische Versionierung** — pro Datei, transparent bei jedem Write.
- **Snapshots** — punktgenaue, konsistente Zustandsaufnahmen des gesamten Baums oder eines Teilbaums.

Beide sind über die FUSE-Integration direkt bedienbar.

## Automatische Versionierung ✅

Implementierung: `corefs-core/src/services/versioning.rs`, Orchestrierung in `CoreFsService::write_file()`.

### Datenmodell

```rust
pub struct FileVersion {
    pub version_id: u64,
    pub path:       String,
    pub created_at: Timestamp,
    pub bytes:      Vec<u8>,     // unkomprimierter Snapshot des Inhalts
}

pub struct VersioningService {
    next_version: u64,
    versions: BTreeMap<String, Vec<FileVersion>>,
}
```

Vor jedem `write_file` wird die alte Version über `store_version_at(path, bytes, now)` abgelegt.

### Budget-Verwaltung

- Konfigurierbar via `config.persistence.max_version_bytes` (Default 64 MiB).
- `prune_to_budget(max_bytes)` entfernt älteste Versionen global, sobald das Budget überschritten ist.
- `prune(path, keep_latest)` entfernt pro Pfad alles bis auf die neuesten N Versionen.

### Abfragen

- `list_versions(path)` — komplette Historie einer Datei.
- `version_at_or_before(path, instant)` — historische Lesezugriffe.

## Snapshots ✅

Implementierung: `corefs-core/src/domain/snapshot.rs` + `CoreFsService::create_snapshot_scoped(...)`.

### Datenmodell

```rust
pub struct Snapshot {
    pub id:          u64,
    pub name:        String,
    pub scope_root:  String,                          // "/" für Voll-Snapshot
    pub created_at:  Timestamp,
    pub paths:       Vec<String>,
    pub file_data:   BTreeMap<String, Vec<u8>>,       // Inhalte (unkomprimiert)
    pub inodes:      BTreeMap<String, SnapshotInode>, // Metadaten-Snapshot
}

pub struct SnapshotInode {
    pub kind:             InodeKind,
    pub size:             usize,
    pub created_at, modified_at, changed_at: Timestamp,
    pub metadata:         FileMetadata,
    pub symlink_target:   Option<String>,
}
```

### Operationen

| Operation | Methode | Bemerkung |
|---|---|---|
| Voll-Snapshot | `create_snapshot(name)` | entspricht `create_snapshot_scoped(name, "/")` |
| Scoped | `create_snapshot_scoped(name, root)` | erfasst nur Teilbaum |
| Listen | `list_snapshots()` | |
| Restore | `restore_snapshot(id)` | liefert `SnapshotRestoreReport` mit Per-Pfad-Skip-Liste |
| Löschen | `delete_snapshot(id)` | |
| Diff | `diff_snapshots(a, b)` | added · removed · modified · unchanged |

### On-Disk

Snapshots liegen im `SNAP`-Segment des Volume-Images bzw. als Teil des `PersistedState`. Sie überleben Umounts und Neu-Mounts konsistent.

## Time-Travel via FUSE ✅

Im RW-Mount sind Snapshots und Versionen transparent zugänglich:

| Pfad | Bedeutung |
|---|---|
| `/.snapshots/` | Verzeichnislistung aller Snapshots (virtuell) |
| `/.snapshots/<id>-<name>/…` | Read-only Blick in einen bestimmten Snapshot |
| `/path/to/file@2026-04-01T12:00:00Z` | Datei zum angegebenen Zeitpunkt (via `VersioningService`) |
| `/path/to/file@v3` | Version mit explizitem `version_id` |

Schreibzugriffe auf Overlays liefern `EROFS`.

## CLI ✅

```bash
corefs snapshot <name>            # Voll-Snapshot
corefs snapshot-list
corefs snapshot-restore <id>
corefs snapshot-diff <a> <b>
corefs snapshot-delete <id>
```

## Tests

- ~10 Snapshot-Tests in `src/app/app_tests.rs` (Voll/Scoped, Restore, Diff, Delete).
- Versioning-Tests in `corefs-core/src/services/versioning_tests.rs`.
- Time-Travel-Overlays in `src/platform/linux_fuse_tests.rs`.

## Offene Punkte / Verbesserungsbedarf

- **Speichereffizienz**: Snapshots pinnen heute **unkomprimierte Bytes** pro Datei. Für grosse Dateien verdoppelt sich damit der Plattenbedarf bei jedem Snapshot. Zielbild: Content-Hash + Block-Pinning gegen den `BlockStore`, CoW-Reuse.
- **Automatische Retention-Policy**: Es existiert Byte-Budget-Pruning, aber **kein** Time-Based-Retention-Schema (z. B. "hourly=24, daily=7, weekly=4").
- **Crash-Konsistenz bei Snapshot-Erstellung**: aktuell wird der In-Memory-Catalog serialisiert. Bei riesigen Volumes könnte eine Copy-on-Write-Capture (ohne vollständige Bytes-Kopie) effizienter sein.
- **Snapshot-Verschlüsselung**: Snapshot-`file_data` wird wie normale Datei-Inhalte behandelt — eine eigene, pro-Snapshot verschlüsselte Ablage wäre für zusätzliche Isolation denkbar.
- **Write-Shadow während Restore**: Ein abgebrochener `restore_snapshot` hinterlässt potenziell teilrestaurierte Dateien; ein Two-Phase-Commit wäre härter.
