# Architektur

## Schichtenmodell

CoreFS folgt strikt einem Schichtenmodell — Abhängigkeiten verlaufen **ausschließlich von außen nach innen**.

```
┌────────────────────────────────────────────────────────┐
│  cli.rs / main.rs                  (Einstiegspunkte)   │
├────────────────────────────────────────────────────────┤
│  app/                              (Orchestrierung)    │
│     CoreFsService (zentrale Fassade)                   │
├────────────────────────────────────────────────────────┤
│  platform/      (OPTIONAL, Plattformadapter)           │
│     linux_fuse, diagnostics, performance, tools        │
├────────────────────────────────────────────────────────┤
│  services/                         (Fachlogik)         │
│     journal, versioning, integrity, recovery,          │
│     compression, encryption, quota, security,          │
│     hot_paths, indexing, metadata, sync                │
├────────────────────────────────────────────────────────┤
│  storage/                          (Persistenz)        │
│     block_device, block_store, catalog, allocator,     │
│     volume_image, device_volume, volume_session,       │
│     volume_wal                                         │
├────────────────────────────────────────────────────────┤
│  domain/                           (reine Typen)       │
│     inode, metadata, snapshot, volume, acl             │
└────────────────────────────────────────────────────────┘
  querschnitt: config.rs, error.rs
```

Plattformspezifische Abhängigkeiten (`fuser`, `libc`) sind ausschließlich über `[target.'cfg(target_os = "linux")'.dependencies]` in [Cargo.toml](../Cargo.toml) eingebunden.

## Modulbaum

```
src/
├── lib.rs                    Crate-Root, Re-exports
├── main.rs                   Binary-Einstieg (delegiert an cli::run)
├── cli.rs                    CLI-Dispatcher (~30 Subkommandos)
├── config.rs                 CoreFsConfig und Policies
├── error.rs                  CoreFsError enum, CoreFsResult<T>
│
├── domain/
│   ├── mod.rs
│   ├── inode.rs              Inode, InodeId, InodeKind
│   ├── metadata.rs           FileMetadata (Permissions, Tags, Classification)
│   ├── snapshot.rs           Snapshot (id, name, paths, file_data)
│   ├── volume.rs             VolumeDescriptor
│   └── acl.rs                AclEntry, Principal
│
├── storage/
│   ├── mod.rs
│   ├── allocator.rs          InodeAllocator (Bitset-basiert)
│   ├── block_device.rs       BlockDevice-Trait + File/Raw/Memory-Devices
│   ├── block_store.rs        Extent-Mgmt, CoW mit ref_count, Defrag
│   ├── catalog.rs            Active/Deleted Inode-Maps, Quota-Stats
│   ├── device_volume.rs      On-Demand Segment-I/O + Cache
│   ├── volume_image.rs       Mehrsegmentiges binäres Format
│   ├── volume_session.rs     Session-Lifecycle (format, open, flush)
│   └── volume_wal.rs         Extent-/Device-Block-adressierte WAL
│
├── services/
│   ├── mod.rs
│   ├── journal.rs            Transaktionales Journal
│   ├── versioning.rs         Auto-Versionierung mit Byte-Budget
│   ├── integrity.rs          Scrubbing, fsck, Image-Reparatur
│   ├── recovery.rs           Crash-Recovery, WAL-Replay
│   ├── compression.rs        LZ4-Kompression (frame format)
│   ├── encryption.rs         ChaCha20-Poly1305 AEAD
│   ├── quota.rs              max_files / max_bytes Enforcement
│   ├── security.rs           Tamper-Detection
│   ├── hot_paths.rs          Zugriffszähler für Heat-Reallocation
│   ├── indexing.rs           Datei-Indexierung
│   ├── metadata.rs           Tag-/Attribute-Verwaltung
│   └── sync.rs               Sync-Status-Tracking
│
├── platform/
│   ├── mod.rs
│   ├── runtime.rs            RuntimeIntegrationBlueprint (generisch)
│   ├── tools.rs              ToolRegistry
│   ├── performance.rs        BenchmarkConfig, Profile
│   ├── diagnostics.rs        FUSE-Mount-Diagnose
│   └── linux_fuse.rs         Linux-FUSE-Adapter
│
└── app/
    ├── mod.rs                CoreFsService (Fassade)
    ├── types.rs              FsStats, AdminReport, SnapshotRestoreReport
    ├── pathing.rs            Pfad-Validierung
    ├── selectors.rs          Inode-Selektoren
    └── tests.rs              App-Layer-Integrationstests
```

## Verantwortlichkeiten der Schichten

### `domain/` — reine Domänenobjekte
Keine I/O, keine Geschäftslogik. Nur fachliche Typen und Invarianten. Ziel: vollständig unabhängig von Plattform und Persistenz.

### `storage/` — Persistenz
Blockallokation, Catalog, Volume-Image, WAL. Kennt das On-Disk-Format. Siehe [persistence-format.md](persistence-format.md).

Wichtige Komponenten:
- **BlockDevice-Trait**: Abstrahiert sektoraligned I/O über `FileImageDevice`, `RawBlockDevice`, `MemoryDevice`.
- **BlockStore**: Extent-Management mit Copy-on-Write über Blob-Referenzzählung.
- **VolumeImage**: Mehrsegmentiges Format mit redundanten Superblocks.
- **DeviceVolume**: On-Demand Segment-I/O direkt von Blockgeräten (mit Read-Cache und Write-Buffer).

### `services/` — fachliche Services
Funktionsmodule ohne direkte Persistenz-Kenntnisse. Operieren über `domain/` und `storage/`.

### `platform/` — optionale Plattformadapter
Linux-FUSE, Performance-Tooling, Diagnose-Hilfen. Nicht zwingend für Kernfunktionen.

### `app/` — Orchestrierung
`CoreFsService` ist die zentrale Fassade, die Services koordiniert und eine stabile API für CLI und Plattformadapter bereitstellt.

## Wichtige Querschnittstypen

- [`CoreFsConfig`](../src/config.rs) — Konfiguration und Policies (Versioning, Security, Performance, Quota). Siehe [configuration.md](configuration.md).
- [`CoreFsError`](../src/error.rs) — gemeinsames Error-Enum mit Varianten `AlreadyExists`, `InvalidCommand`, `InvalidInput`, `NotFound`, `PolicyViolation`, `State`.
- `CoreFsResult<T>` — Typalias `Result<T, CoreFsError>`.

## Prinzipien

1. **Keine plattformspezifischen Annahmen** in `domain/` oder `storage/`.
2. **Keine Abstraktion ohne konkreten Bedarf** — keine spekulativen Generalisierungen.
3. **Testbarkeit und Wartbarkeit** haben Vorrang vor Feature-Vollständigkeit.
4. **Fremdsysteme** (FUSE, Kernel-VFS) nur über Plattformadapter — niemals im Kern.
