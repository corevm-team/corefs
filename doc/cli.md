# CLI-Referenz

Implementation: [src/cli.rs](../src/cli.rs). Start über `cargo run -- <kommando>` oder nach `cargo build --release` über das Binary:

```bash
./target/release/corefs <kommando> [argumente]
```

Status: ✅ ~30 Kommandos produktiv. Offene Lücken siehe Ende.

## Grundoperationen

| Kommando | Argumente | Zweck |
|---|---|---|
| `mkfs` | — | Formatiert Volume (In-Memory-Modell) |
| `status` | — | Volume-Info, File-Count, Snapshot-Count, Fragmentierung |
| `ls` | — | Listet alle Dateipfade |
| `scrub` | — | CRC32C-Scrubbing über aktive Blöcke |

## Dateioperationen

| Kommando | Argumente | Zweck |
|---|---|---|
| `write <path> <payload>` | Pfad, Payload-String | Anlegen oder überschreiben |
| `read <path>` | Pfad | Inhalt ausgeben (transparent Decrypt/Decompress) |
| `delete <path> [--secure]` | Pfad, optional `--secure` | Soft-Delete oder Secure-Erase |
| `restore <path>` | Pfad | Soft-gelöschte Datei wiederherstellen |

## Snapshots & Versionierung

| Kommando | Argumente | Zweck |
|---|---|---|
| `snapshot [name]` | opt. Name | Voll-Snapshot |
| `snapshot-list` | — | Alle Snapshots auflisten |
| `snapshot-restore <id>` | Snapshot-ID | Restore mit Per-Pfad-Skip-Report |
| `snapshot-diff <a_id> <b_id>` | zwei IDs | added/removed/modified/unchanged |
| `snapshot-delete <id>` | ID | Snapshot entfernen |

## Speicheroptimierung

| Kommando | Argumente | Zweck |
|---|---|---|
| `defrag` | — | Defragmentierung (live) |
| `optimize` | — | Defrag + Heat-Reallocation |
| `dedup` | — | Expliziter Dedup-Pass |
| `defrag-image <path>` | Image | Offline-Defrag |
| `optimize-image <path>` | Image | Offline-Optimize |

## Image-Persistenz

| Kommando | Argumente | Zweck |
|---|---|---|
| `mkfs-image <path> [--demo]` | Image, opt. `--demo` | Neues Image anlegen |
| `save-image <path>` | Image | Aktuelles Volume speichern |
| `load-image <path>` | Image | Laden + Recovery, Stats |
| `fsck-image <path>` | Image | Read-only Integritätscheck |
| `repair-image <path> [--aggressive]` | Image | Mehrstufige Reparatur |
| `inspect-image <path>` | Image | Detailliertes Segment-/Superblock-Dump |

## FUSE-Mount (Linux)

| Kommando | Argumente | Zweck |
|---|---|---|
| `mount-image <img> <mnt>` | Image, Mount | Read-Write-Mount |
| `mount-image-ro <img> <mnt>` | Image, Mount | Read-Only-Mount |
| `umount <mnt>` | Mount | Sauberer Unmount |
| `diagnose-mount <img> <mnt> [--create]` | Image, Mount | Voraussetzungscheck |

Overlays (`.snapshots/`, `file@timestamp`) siehe [fuse-integration.md](fuse-integration.md).

## Blockgeräte (Linux, root)

| Kommando | Argumente | Zweck |
|---|---|---|
| `probe-device <dev>` | Device | Kapazität / Mount-Status / R/O |
| `mkfs-device <dev> [--skip-check]` | Device | Formatieren (mit Fake-Stick-Sanity-Check) |
| `fsck-device <dev>` | Device | Read-only-Prüfung |
| `mount-device-rw <dev> <mnt>` | Device, Mount | Direkter FUSE-Mount mit On-Demand-I/O |
| `verify-device <dev> --destructive [--chunks N] [--chunk-size B]` | Device, `--destructive` verpflichtend | Kapazitäts-Verifikations-Scan |

Details: [block-devices.md](block-devices.md).

## Performance & Diagnose

| Kommando | Argumente | Zweck |
|---|---|---|
| `benchmark [--profile P] [--files N] [--payload B] [--snapshots N] [--saves N] [--log FILE]` | alle optional | Vollsuite / Single-Run inkl. Markdown-Log |
| `benchmark-once <suite>` | Suite | Einmaliger Run eines benannten Profils |

Verfügbare Profile: `dev`, `ci`, `regression`, `storage-heavy` sowie `balanced`, `small-files`, `metadata-heavy`, `snapshot-heavy`, `persist-heavy` (siehe [performance.md](performance.md)).

## Backup (via `corefs-cli`)

| Kommando | Zweck |
|---|---|
| `corefs-cli backup dump <img> --output <bkp>` | Full-Backup |
| `corefs-cli backup dump <img> --since <snapshot_id> --output <bkp>` | Incremental |
| `corefs-cli backup restore <img> --input <bkp>` | Restore |

Details: [backup.md](backup.md).

## Key-Management (via `corefs-cli`)

| Kommando | Zweck |
|---|---|
| `corefs-cli keys init <keystore>` | Neuer Keystore |
| `corefs-cli keys rotate <keystore>` | Master-Key rotieren (ohne Re-Encrypt) |
| `corefs-cli keys verify <keystore>` | Magic/Version + Probe-Unwrap |

Details: [key-management.md](key-management.md).

## Exitcodes

| Code | Bedeutung |
|---|---|
| 0 | Erfolg |
| ≠ 0 | Fehlermeldung; typische `CoreFsError`-Varianten: `NotFound`, `PolicyViolation`, `InvalidInput`, `Integrity`, `QuotaExceeded`, `State` |

## Beispielsitzung

```bash
corefs mkfs-image ./demo.img --demo
corefs mount-image ./demo.img /tmp/mnt &
echo "Hallo Welt" > /tmp/mnt/hello.txt
corefs snapshot initial
corefs snapshot-list
corefs fsck-image ./demo.img
umount /tmp/mnt
```

Siehe auch [examples.md](examples.md) für vollständige End-to-End-Beispiele.

## Offene Punkte / Verbesserungsbedarf

- Explizite Top-Level-Kommandos für `mkdir`, `rename`, `clone` fehlen in `corefs` (sind über FUSE bedienbar).
- `cluster`-Subkommandos sind geplant, solange `SyncService` Stub ist, noch nicht exponiert.
- `mount-image-rw` ist historisches Alias für `mount-image`; vereinheitlichen.
