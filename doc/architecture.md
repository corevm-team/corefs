# Architektur

CoreFS ist als **Cargo-Workspace** mit klarer Schichten- und Crate-Trennung aufgebaut. Plattformneutralität und `no_std`-Fähigkeit des Kerns sind leitend.

## Workspace-Struktur

| Crate | Rolle | Umgebung |
|---|---|---|
| `corefs-core` | Kernbibliothek: Domain, Storage, Services, Security | `no_std + alloc`, Feature-Gates `std`, `crypto`, `compression` |
| `corefs` (Root) | Komposition, Image-Persistenz, FUSE, CLI | `std`, Linux-Dependencies target-gated |
| `corefs-cli` | Externe CLI-Oberfläche / Skripte | `std` |
| `corefs-tools` | Host-Tools für AnyOS (Backup, Keys, Mount) | `std` |
| `corefs-std` | `std`-Wrapper-Crate | `std` |
| `corefs-fuse-proto` | Protokoll-Definitionen für Kernel-Daemon-IPC | `no_std`, POC |
| `corefs-fuse-adapter` | Adapter zwischen FUSE und IPC | `std`, POC |

`corefs-core` kompiliert gegen `x86_64-anyos` (Custom Target, siehe `vendor/`). Plattformcode (FUSE, `libc`, `fuser`) ist ausschliesslich über `[target.'cfg(target_os = "linux")'.dependencies]` eingebunden.

## Schichtenmodell

```
┌─────────────────────────────────────────────────┐
│ platform/   FUSE, Runtime, Performance,          │
│             Diagnostics, Online-CTL              │  (optional, OS-spezifisch)
├─────────────────────────────────────────────────┤
│ app/        CoreFsService (Fassade)              │
│             Orchestriert alle Services           │
├─────────────────────────────────────────────────┤
│ services/   Journal, Versioning, Integrity,      │
│             Compression, Encryption, Recovery,   │
│             Quota, Hot-Paths, Indexing …         │
├─────────────────────────────────────────────────┤
│ storage/    Volume-Image, Block-Device,          │
│             BlockStore (CoW, Dedup), Catalog,    │
│             Allocator, WAL, ondisk/ (ODF v1)     │
├─────────────────────────────────────────────────┤
│ domain/     Inode, Metadata, Snapshot,           │
│             Volume, ACL, Timestamp               │
└─────────────────────────────────────────────────┘
```

Abhängigkeiten fliessen nur von aussen nach innen. Der `domain`-Layer kennt keine Plattform- oder Storage-Abhängigkeiten.

## Modulbaum (wesentliche Dateien)

### `corefs-core`

```
corefs-core/src/
├── lib.rs, platform.rs, config.rs, error.rs, bincode_compat.rs
├── domain/
│   ├── inode.rs         InodeId, InodeKind, Inode
│   ├── metadata.rs      FileMetadata (uid/gid/mode/tags/xattrs/encrypted/compressed)
│   ├── snapshot.rs      Snapshot, SnapshotInode
│   ├── volume.rs        VolumeDescriptor
│   └── acl.rs           AclEntry, Principal
├── security/            (Feature-Gate `crypto`)
│   ├── sha256.rs        Pure-Rust SHA-256 (FIPS-180-4)
│   ├── hkdf.rs          HKDF-SHA256 (RFC-5869)
│   └── keystore.rs      KeystoreFile (Magic "COREFSKS")
├── services/
│   ├── journal.rs, versioning.rs, recovery.rs,
│   ├── encryption.rs, compression.rs,
│   ├── quota.rs, metadata.rs, indexing.rs,
│   ├── security.rs, sync.rs, semantic.rs, hot_paths.rs
└── storage/
    ├── allocator.rs, block_device.rs, block_store.rs,
    ├── catalog.rs, backup.rs (Magic "COREFSBK"),
    ├── persisted_state.rs,
    ├── volume_image.rs, device_volume.rs,
    ├── volume_session.rs, volume_wal.rs
    └── ondisk/          ODF v1 (39 Module)
        ├── layout.rs, superblock.rs, inode.rs, extent_tree.rs,
        ├── journal.rs, fsck.rs, fsck_repair.rs, scrub.rs,
        ├── grouped.rs, journaled.rs, native.rs, tiering.rs,
        ├── refcount.rs, checksum.rs, xattr.rs, attr_block.rs,
        ├── dir_entry.rs, reader.rs, session.rs,
        ├── multi_group_allocator.rs, allocated.rs, resize.rs …
```

### Root-Crate `corefs`

```
src/
├── lib.rs, main.rs, cli.rs, config.rs, error.rs
├── domain/             Re-Exports + std-Erweiterungen
├── storage/
│   ├── block_device.rs (FileImageDevice, RawBlockDevice mit libc-ioctls)
│   ├── device_volume.rs, volume_image.rs,
│   ├── volume_session.rs, volume_wal.rs
├── services/
│   ├── encryption.rs, compression.rs, integrity.rs
├── app/
│   ├── mod.rs          CoreFsService (zentrale Fassade)
│   ├── types.rs, pathing.rs, selectors.rs
└── platform/
    ├── linux_fuse.rs   FUSE v31 (lookup/getattr/read/write/…)
    ├── diagnostics.rs, performance.rs, tools.rs,
    ├── runtime.rs, online_ctl.rs
    └── windows/        (leerer Stub)
```

## Verantwortlichkeiten pro Schicht

| Schicht | Aufgaben | Tabu |
|---|---|---|
| `domain/` | Typen, Invarianten, reine Logik | `std`, FS-I/O, Plattform-APIs |
| `storage/` | Persistenz, Blockallokation, ODF, WAL, Image | Fachregeln, Plattform-APIs |
| `services/` | Querschnittsdienste | Direktes Image-I/O (delegiert) |
| `app/` | Orchestrierung, Transaktionsgrenzen, Fassade | Plattform-I/O-Primitive |
| `platform/` | OS-Adapter | Fachlogik |

## Service-Fassade

`CoreFsService` (`src/app/mod.rs`) hält alle Services und exponiert etwa 100 Operationen:

- **Dateien**: `create_file`, `read_file`, `write_file`, `truncate`, `delete_file`
- **Verzeichnisse**: `create_directory`, `delete_directory`, `list_paths`, `list_directory`
- **Symlinks**: `create_symlink`, `resolve_symlink`
- **Metadaten**: `set_owner`, `set_mode`, `get_inode`, `touch`
- **Snapshots**: `create_snapshot(_scoped)`, `list_snapshots`, `restore_snapshot`, `delete_snapshot`, `diff_snapshots`
- **CoW**: `clone_file`, `clone_tree`, `expunge_file`
- **Wartung**: `defragment`, `optimize_storage`, `run_dedup`, `scrub`
- **Persistenz**: `format`, `load_from_image`, `save_image_to_path`, `checkpoint`
- **Reporting**: `admin_report`, `fragmentation_report`

Jede Write-Operation durchläuft die Pipeline **Compression → Encryption → Store** und aktualisiert Quota-, Journal-, Versioning- und Hot-Path-Services konsistent.

## Bekannte Abweichungen vom Idealbild

Gegenüber den Regeln in `CLAUDE.md`:

- Der Top-Level `src/domain/` ist **Re-Export und std-Erweiterung** der `corefs-core::domain`-Typen — keine vollständig eigenständige Schicht. Pragmatisch, aber nicht strikt.
- `storage/ondisk/` enthält an wenigen Stellen domain-Referenzen; die Inversion ist nicht überall streng.
- `services/compression` ist feature-gated und zieht via `lz4_flex::frame` indirekt `std` nach. Reine `no_std`-Builds müssen es weglassen.

Siehe auch [persistence-format.md](persistence-format.md), [fuse-integration.md](fuse-integration.md), [anyos-integration.md](anyos-integration.md).
