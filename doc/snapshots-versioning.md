# Snapshots & Versionierung

## Snapshots

Implementierung: [src/domain/snapshot.rs](../src/domain/snapshot.rs), Service in [src/app/mod.rs](../src/app/mod.rs).

### Modell

```rust
pub struct Snapshot {
    pub id: u64,
    pub name: String,
    pub scope_root: String,
    pub created_at: SystemTime,
    pub paths: Vec<String>,
    pub file_data: BTreeMap<String, Vec<u8>>,
}
```

Snapshots sind **selbständig**: Sie enthalten die vollständigen Dateibytes zum Zeitpunkt der Erstellung. Dadurch sind sie unabhängig von späteren Block-Mutationen — auch nach Defrag oder Block-Reallocation bleibt der Snapshot konsistent.

### Erstellen und verwalten

```bash
cargo run -- snapshot                # Name "manual"
cargo run -- snapshot nightly
cargo run -- status                  # zeigt Snapshot-Count
```

### Scoped Snapshots

Über `scope_root` lassen sich Snapshots auf einen Verzeichnis-Teilbaum beschränken. API:

```rust
fs.create_scoped_snapshot("backup-docs", "/documents")?;
```

### Snapshot-Diff

Klassifiziert Pfade zwischen zwei Snapshots in `added / removed / modified / unchanged`.

### Zugriff über FUSE

Im RW-Mount erscheint automatisch `/mount/.snapshots/<id>-<name>/` als Read-only-Overlay (siehe [fuse-integration.md](fuse-integration.md)).

## Versionierung

Implementierung: [src/services/versioning.rs](../src/services/versioning.rs).

### Verhalten

- Bei jedem `write_file()` wird die vorherige Version in die Historie übernommen.
- Historie pro Datei mit monoton steigenden Versionsnummern.
- Pruning über ein globales Byte-Budget:

```rust
VersioningPolicy {
    keep_latest: 16,
    auto_prune: true,
    max_version_bytes: Some(64 * 1024 * 1024), // 64 MiB default
    ..
}
```

### Time-Travel

Über FUSE durch `@`-Syntax im Dateinamen (siehe [fuse-integration.md](fuse-integration.md)):

```
file.txt@2026-04-13       Version vom Tag
file.txt@2026-04-13T10:30 Version zum Zeitpunkt
file.txt@v2               Version Nr. 2
```

Aktivierbar über `VersioningPolicy::expose_time_travel` (Default an).

### Programmatische Nutzung

```rust
let versions = fs.list_versions("/a.txt")?;
let bytes = fs.read_version("/a.txt", 2)?;
let at = fs.read_file_at("/a.txt", timestamp)?;
```

## Restore

Soft-gelöschte Dateien können wiederhergestellt werden:

```bash
cargo run -- delete /a.txt
cargo run -- restore /a.txt
```

Secure-Delete (Nulling der Blöcke, kein Restore möglich):

```bash
cargo run -- delete /a.txt --secure
```
