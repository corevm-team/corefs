# CoreFS — Dokumentation

CoreFS ist ein natives, plattformneutrales Dateisystem, entwickelt in **Rust** (Edition 2024), primär als Standard-Dateisystem für das Betriebssystem **AnyOS**. Unter Linux steht eine optionale **FUSE-Integration** bereit, mit der Volumes aus Image-Dateien oder direkt von Blockgeräten gemountet werden können.

## Inhaltsverzeichnis

| Dokument | Inhalt |
|---|---|
| [overview.md](overview.md) | Projektübersicht, Ziele, Status |
| [architecture.md](architecture.md) | Schichtenmodell, Modulbaum, Verantwortlichkeiten |
| [cli.md](cli.md) | Vollständige CLI-Referenz aller Subkommandos |
| [features.md](features.md) | Feature-Katalog mit Implementierungsstand |
| [configuration.md](configuration.md) | `CoreFsConfig` und Policies |
| [persistence-format.md](persistence-format.md) | Mehrsegmentiges On-Disk-Format |
| [block-devices.md](block-devices.md) | Block-Device-Workflow und Fake-Stick-Schutz |
| [fuse-integration.md](fuse-integration.md) | Linux-FUSE-Adapter, Mount-Modi |
| [snapshots-versioning.md](snapshots-versioning.md) | Snapshots, Versionierung, Time-Travel |
| [deduplication.md](deduplication.md) | Inline-Dedup, expliziter Dedup-Pass, Ref-Counting |
| [integrity-recovery.md](integrity-recovery.md) | Checksummen, Journal, fsck, Recovery |
| [security.md](security.md) | Verschlüsselung, ACL, Quotas, Secure-Delete |
| [performance.md](performance.md) | Benchmarking-Framework und Profile |
| [testing.md](testing.md) | Unit-, Integrations-, POSIX-, Stress-Tests |
| [examples.md](examples.md) | End-to-End-Beispiele |
| [development.md](development.md) | Build, Commit-Workflow, Entwicklungsregeln |
| [glossary.md](glossary.md) | Begriffsverzeichnis |

## Schnelleinstieg

```bash
# Projekt bauen
cargo build --release

# Image erstellen und mounten (Linux)
cargo run --release -- mkfs-image ./corefs.img --demo
cargo run --release -- mount-image-rw ./corefs.img /tmp/corefs-mnt

# Blockgerät formatieren und mounten (Linux, root)
sudo cargo run --release -- probe-device /dev/sdb1
sudo cargo run --release -- mkfs-device /dev/sdb1
sudo cargo run --release -- mount-device-rw /dev/sdb1 /mnt/usb
```

Weitergehende Beispiele sind in [examples.md](examples.md) dokumentiert.

## Projektdokumente im Repository-Root

- [README.md](../README.md) — Einstieg, Schnellüberblick
- [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md) — aktueller Implementierungsstand
- [features_corefs.md](../features_corefs.md) — Feature-Anforderungen
- [corefs_brainstorming.txt](../corefs_brainstorming.txt) — Architekturideen
- [PERFORMANCE_LOG.md](../PERFORMANCE_LOG.md) — Benchmark-Historie
- [CLAUDE.md](../CLAUDE.md) — Arbeitsanweisungen für Claude Code
