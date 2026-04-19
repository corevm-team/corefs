# Linux-FUSE-Integration

Implementierung: [src/platform/linux_fuse.rs](../src/platform/linux_fuse.rs). Baut auf `fuser 0.14` auf (FUSE v31). Verfügbar nur auf Linux (`[target.'cfg(target_os = "linux")'.dependencies]`).

## Rolle im Schichtenmodell

FUSE ist ein optionaler **Plattformadapter**. Er übersetzt Kernel-FUSE-Requests in `CoreFsService`-Aufrufe und verwaltet:

- Inode-Nummern-Mapping (Kernel-ino ↔ interner Pfad / InodeId),
- Per-Handle Read-/Write-Caches,
- Snapshot-Overlays und Time-Travel-Addressing,
- POSIX-Timestamp-Semantik.

## Unterstützte Operationen

| Operation | Status | Bemerkung |
|---|---|---|
| `lookup` | ✅ | Pfad → Inode |
| `getattr` | ✅ | 3 Timestamps, Mode, Owner, Grösse |
| `setattr` | ✅ | chown / chmod / truncate / utime |
| `open` | ✅ | Handle-Allokation |
| `release` | ✅ | Flush + Cleanup |
| `create` | ✅ | Anlegen inkl. sofortigem Write-Back-Handle |
| `read` | ✅ | lazy gecachte Reads, transparent Decrypt/Decompress |
| `write` | ✅ | Write-Back, Streaming ≥ 32 MiB |
| `mkdir` / `rmdir` | ✅ | |
| `unlink` | ✅ | Soft-Delete (Tombstone) |
| `rename` | ✅ | inkl. Parent-Wechsel |
| `readdir` | ✅ | volle Listung |
| `symlink` / `readlink` | ✅ | |
| `statfs` | ✅ | Kapazität aus Backing-Store |
| `copy_file_range` | ✅ | serverseitige Kopie zwischen Handles |
| `fsync` / `flush` | ✅ | per Handle |
| `link` (Hardlink) | ❌ | nicht implementiert |
| `getxattr` / `setxattr` / `listxattr` / `removexattr` | 🔶 | xattrs im Domain-Modell vorhanden, FUSE-Routing ist minimal |
| `flock` / `setlk` | ⚠️ | nicht implementiert |

## Performance-Features

- **`FUSE_WRITEBACK_CACHE`** — Kernel puffert Schreibvorgänge, reduziert Syscalls.
- **`max_write = 1 MiB`** — weniger Kernel↔Daemon-Roundtrips.
- **Streaming-Writes**: Bei Schreiben > 32 MiB werden Zwischenflushes erzwungen, sodass Peak-RAM auf O(32 MiB) statt O(File-Size) bleibt.
- **Per-Handle Read-Cache**: Erster Read lädt Blob, folgende lesen aus dem Handle-Buffer.
- **Per-Handle Write-Buffer**: Flush on Release oder expliziter `fsync`.

## Snapshot- & Time-Travel-Overlays

- Virtueller Pfad `/.snapshots/<id>-<name>/…` — Read-only Blick in einen Snapshot.
- **Time-Travel-Syntax**: `file@2026-04-01T12:00:00Z` oder `file@v2` — löst über `VersioningService::version_at_or_before()` auf.
- Schreiben auf Overlays liefert `EROFS`.

## Mount-Modi

| Modus | CLI | Bemerkung |
|---|---|---|
| Image RW | `mount-image <img> <mnt>` | erstellt Arbeitskopie, Änderungen werden beim `umount` persistiert |
| Image RO | `mount-image-ro <img> <mnt>` | read-only, ideal für Forensik |
| Device RW | `mount-device-rw <dev> <mnt>` | direkt gegen Blockgerät inkl. Device-Journal |

## Unterstützende Werkzeuge

- `diagnostics.rs` — Mount-Punkt-Analyse (via `/proc/mounts`), Backend-Erkennung, Space-Stats.
- `online_ctl.rs` — IPC-Stub für laufende Mounts (Hot-Paths hinzufügen, Checkpoint auslösen, Defrag / Dedup triggern). Transport-Layer ist POC.

## Tests

`src/platform/linux_fuse_tests.rs` (~27 Tests) plus die E2E-Suite in [tests/fuse_handler_e2e.rs](../tests/fuse_handler_e2e.rs) — mount + Shell-Operationen + unzip-Workload + revalidiertes unmount.

## Offene Punkte

- **Hardlinks** (`link`-Operation) — erfordert neue Inode-Semantik.
- **Datei-Locks** (`flock`, POSIX-Byte-Range-Locks) — derzeit nicht umgesetzt.
- **xattr-Routing** — End-to-End-Weiterreichung aller vier xattr-Ops.
- **ACL-Enforcement** — siehe [security.md](security.md).
- **Online-CTL-Transport** — produktiver Unix-Socket statt Stub.
