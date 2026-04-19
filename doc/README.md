# CoreFS — Dokumentation

CoreFS ist ein natives, plattformneutrales Dateisystem, entwickelt in **Rust** (Edition 2024). Primäres Zielsystem ist das eigene Betriebssystem **AnyOS**; unter Linux steht eine optionale **FUSE-Integration** sowie direkter Blockgeräte-Betrieb bereit.

## Inhaltsverzeichnis

| Dokument | Inhalt |
|---|---|
| [overview.md](overview.md) | Projektübersicht, Ziele, aktueller Reifegrad |
| [architecture.md](architecture.md) | Workspace, Schichtenmodell, Modulbaum, Fassade |
| [features.md](features.md) | Feature-Katalog mit Status und offenen Punkten |
| [configuration.md](configuration.md) | `CoreFsConfig`, Policies (Versioning, Security, Performance, Quota) |
| [persistence-format.md](persistence-format.md) | ODF v1 und `COREFS01`-Image-Format |
| [block-devices.md](block-devices.md) | Block-Device-Workflow und Fake-Stick-Schutz |
| [fuse-integration.md](fuse-integration.md) | Linux-FUSE-Adapter, Mount-Modi, Overlays |
| [anyos-integration.md](anyos-integration.md) | Einbindung in AnyOS, Kernel-API-Oberfläche |
| [snapshots-versioning.md](snapshots-versioning.md) | Snapshots, Auto-Versionierung, Time-Travel |
| [deduplication.md](deduplication.md) | Inline-Dedup, expliziter Dedup-Pass, Ref-Counting |
| [integrity-recovery.md](integrity-recovery.md) | CRC32C, Journal, WAL, fsck, Repair-Stufen |
| [security.md](security.md) | Verschlüsselung, ACLs, Quotas, Secure-Delete |
| [key-management.md](key-management.md) | Master-Key, Keystore, HKDF, Rotation |
| [backup.md](backup.md) | Backup-/Export-Stream (`COREFSBK`), Full+Incremental |
| [performance.md](performance.md) | Benchmark-Framework, Profile, History |
| [cli.md](cli.md) | Vollständige CLI-Referenz |
| [testing.md](testing.md) | Unit-, Integration-, Stress-, Fault-Injection-Tests |
| [examples.md](examples.md) | End-to-End-Beispiele |
| [development.md](development.md) | Build, Commit-Workflow, Entwicklungsregeln |
| [glossary.md](glossary.md) | Begriffsverzeichnis |

## Schnelleinstieg

```bash
cargo build --release
alias corefs=./target/release/corefs

# Image erstellen und mounten (Linux)
corefs mkfs-image ./corefs.img --demo
corefs mount-image ./corefs.img /tmp/corefs-mnt

# Blockgerät formatieren und mounten (Linux, root)
sudo corefs probe-device /dev/sdb1
sudo corefs mkfs-device  /dev/sdb1
sudo corefs mount-device-rw /dev/sdb1 /mnt/usb
```

Weiterführende Beispiele: [examples.md](examples.md).

## Repository-Root-Dokumente

- [README.md](../README.md) — Einstieg, Schnellüberblick
- [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md) — aktueller Implementierungsstand
- [features_corefs.md](../features_corefs.md) — Feature-Anforderungen
- [corefs_brainstorming.txt](../corefs_brainstorming.txt) — Architektur-Ideen
- [PERFORMANCE_LOG.md](../PERFORMANCE_LOG.md) — Benchmark-Historie
- [CLAUDE.md](../CLAUDE.md) — Arbeitsanweisungen für Claude Code

## Legende der Status-Marker

| Symbol | Bedeutung |
|---|---|
| ✅ | produktiv / voll implementiert und getestet |
| 🔶 | teilweise / Basis vorhanden, Verbesserungen offen |
| ⚠️ | geplant / POC / Stub |
| ❌ | nicht implementiert |
