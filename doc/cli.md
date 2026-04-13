# CLI-Referenz

Alle CLI-Kommandos sind in [src/cli.rs](../src/cli.rs) implementiert. Start immer über `cargo run -- <kommando>` oder über das Binary nach `cargo build --release`:

```bash
./target/release/corefs <kommando> [argumente]
```

## Grundoperationen

| Kommando | Argumente | Zweck |
|---|---|---|
| `mkfs` | — | Formatiert ein Volume (In-Memory-Modell) |
| `status` | — | Zeigt Volume-Info, File-Count, Snapshot-Count, Fragmentierung |
| `ls` | — | Listet alle Dateipfade |
| `snapshot [name]` | optional: Name (Default `manual`) | Erstellt einen Snapshot |
| `scrub` | — | Checksummen-Scrubbing über alle aktiven Blöcke |

## Dateioperationen

| Kommando | Argumente | Zweck |
|---|---|---|
| `write <path> <payload>` | Pfad, Payload-String | Schreibt Datei (neu oder überschreiben) |
| `read <path>` | Pfad | Gibt Dateiinhalt aus |
| `delete <path> [--secure]` | Pfad, optional `--secure` | Soft-Delete oder Secure-Delete mit Nulling |
| `restore <path>` | Pfad | Stellt gelöschte Datei wieder her |

## Speicheroptimierung

| Kommando | Argumente | Zweck |
|---|---|---|
| `defrag` | — | Defragmentierung auf Live-Instance |
| `optimize` | — | Combined: Defrag + Heat-Reallocation |
| `defrag-image <path>` | Image-Pfad | Defragmentierung auf gespeichertem Image |
| `optimize-image <path>` | Image-Pfad | Combined Optimization auf Image |

## Image-Persistenz

| Kommando | Argumente | Zweck |
|---|---|---|
| `save-image <path>` | Image-Pfad | Speichert Volume im mehrsegmentigen Format |
| `load-image <path>` | Image-Pfad | Lädt Image von Disk, zeigt Stats |
| `mkfs-image <path> [--demo]` | Image-Pfad, optional `--demo` | Erstellt neues Image (mit Demo-Inhalt, wenn `--demo`) |
| `fsck-image <path> [--repair]` | Image-Pfad, optional `--repair` | Prüft oder repariert Image (mehrstufig) |

## FUSE-Mount (Linux)

| Kommando | Argumente | Zweck |
|---|---|---|
| `mount-image <img> <mnt>` | Image, Mount-Point | Read-only-Mount |
| `mount-image-rw <img> <mnt>` | Image, Mount-Point | Read-write-Mount mit Writeback |
| `diagnose-mount <img> <mnt> [--create]` | Image, Mount-Point, optional `--create` | Prüft Mount-Voraussetzungen |

Details zu Mount-Modi und Virtual Overlays (Snapshots, Time-Travel) in [fuse-integration.md](fuse-integration.md).

## Blockgeräte (Linux)

| Kommando | Argumente | Zweck |
|---|---|---|
| `probe-device <dev>` | Device-Pfad (z.B. `/dev/sdb1`) | Analysiert Kapazität, Sektorgröße, R/O-Status, Mount-Status |
| `mkfs-device <dev> [--skip-check]` | Device, opt. `--skip-check` | Formatiert Device; automatischer Fake-Stick-Sanity-Check (außer `--skip-check`) |
| `fsck-device <dev>` | Device | Read-only-Prüfung |
| `mount-device-rw <dev> <mnt>` | Device, Mount-Point | FUSE-Mount direkt vom Blockgerät |
| `verify-device <dev> --destructive [--chunks N] [--chunk-size B]` | Device, zwingend `--destructive`, optional `--chunks`, `--chunk-size` | Vollständiger Kapazitäts-Verifikations-Scan (Fake-Stick-Detection) |

Details zum Block-Device-Workflow in [block-devices.md](block-devices.md).

## Performance & Diagnose

| Kommando | Argumente | Zweck |
|---|---|---|
| `benchmark [--profile P] [--files N] [--payload B] [--snapshots N] [--saves N]` | Alle optional | Führt Benchmark aus, zeigt Durchsatz und Ops/s |
| `benchmark-log <path> [--profile P] [...]` | Log-Pfad + Profile/Parameter | Wie `benchmark`, appendet Ergebnis an Markdown-Log |

**Verfügbare Profile** (siehe [performance.md](performance.md)):
- `balanced`
- `small-files`
- `metadata-heavy`
- `snapshot-heavy`
- `persist-heavy`

## Exitcodes

| Code | Bedeutung |
|---|---|
| 0 | Erfolg |
| ≠ 0 | Siehe Fehlermeldung; typisch: `CoreFsError`-Varianten (NotFound, PolicyViolation, InvalidInput …) |

## Beispielsitzung

```bash
# Image erstellen, Datei schreiben, Snapshot, Read
cargo run -- mkfs-image ./demo.img
cargo run -- write /hello.txt "Hallo Welt"
cargo run -- snapshot initial
cargo run -- ls
cargo run -- read /hello.txt
cargo run -- save-image ./demo.img

# Integrität prüfen
cargo run -- fsck-image ./demo.img
```

Siehe auch [examples.md](examples.md) für vollständige End-to-End-Beispiele.
