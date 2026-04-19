# CoreFS

**CoreFS** ist ein in **Rust** (Edition 2024) entwickeltes, plattformneutrales Dateisystem. Primäres Zielsystem ist das eigene Betriebssystem **AnyOS**; unter Linux steht eine produktive **FUSE-Integration** sowie direkte Blockgeräte-I/O bereit.

Das Projekt ist als Cargo-Workspace organisiert:

| Crate | Rolle |
|---|---|
| `corefs-core` | Kernbibliothek — Domain, Storage, Services, Security. `no_std + alloc`, kompiliert für `x86_64-anyos` |
| `corefs` (Root) | Komposition, Image-Persistenz, FUSE-Adapter, CLI (std, Linux-target-gated) |
| `corefs-cli` | Wrapper-CLI (Backup, Keys) |
| `corefs-tools` | Host-Tools für AnyOS |
| `corefs-std`, `corefs-fuse-proto`, `corefs-fuse-adapter` | Adapter / Protokoll-Stubs |

## Ziele

- **Plattformneutralität** — strikte Trennung Kern ↔ Plattformadapter.
- **Enterprise-Architektur** — `domain → storage → services → app → platform`.
- **Datenintegrität** — CRC32C, redundante Superblöcke, transaktionales Journal, WAL, mehrstufiger `fsck`/Repair.
- **Versionierung & Snapshots** — automatische Historie, scoped Snapshots, Time-Travel.
- **Verschlüsselung** — ChaCha20-Poly1305 AEAD, HKDF-SHA256 Per-File-Keys, Keystore-Format.
- **Performance** — CoW mit Ref-Counting, Extent-Trees, LZ4, Dedup, Hot-Path-Telemetrie.
- **SSD-Freundlichkeit** — TRIM/Discard, 4 KiB-Alignment, inkrementelle Persistenz.

Die fachliche Anforderungsbasis steht in [features_corefs.md](features_corefs.md), die vollständige Dokumentation unter [doc/](doc/).

## Projektstatus (Stand 2026-04)

| Aspekt | Status |
|---|---|
| Build | ✅ stabil (Workspace, Multi-Crate) |
| Tests | ✅ ~979 Tests grün (Unit, Integration, E2E, Stress, Fault-Injection, Concurrency) |
| On-Disk-Format (ODF v1) | ✅ `COREFSDF`-Magic, 4 KiB Superblock, redundant |
| Image-Format | ✅ `COREFS01` v7, mehrsegmentig, repairable |
| Linux-FUSE (RO/RW) | ✅ produktiv, Streaming, Snapshot-Overlays, Time-Travel |
| Blockgeräte-I/O | ✅ `probe` / `mkfs` / `fsck` / `verify` / `mount` |
| Verschlüsselung | ✅ Pipeline compress → encrypt → store |
| Snapshots & Versionierung | ✅ byte-budget-gepruned |
| Backup/Export | ✅ `COREFSBK`, full + incremental |
| Dedup / Defrag / Scrub | ✅ |
| AnyOS-Kernelintegration | 🔶 `corefs-core` no_std-ready, Treiber POC |
| Hardlinks | ❌ `link_count` reserviert, keine Semantik |
| Clustering / Sync | ⚠️ `SyncService` ist Stub |
| Windows-Adapter | ❌ nur Stub-Verzeichnis |
| ACL-Enforcement | 🔶 Speicherung vollständig, FUSE prüft nur POSIX-Mode |

Legende: ✅ produktiv · 🔶 teilweise · ⚠️ geplant / POC · ❌ nicht implementiert

Details: [PROJECT_PROGRESS.md](PROJECT_PROGRESS.md) · Feature-Matrix: [doc/features.md](doc/features.md).

## Voraussetzungen

Rust-Toolchain mit Edition-2024-Support. Linux-FUSE-Build benötigt zusätzlich `libfuse3`/`fuser 0.14`.

```bash
cargo --version
rustc --version
```

## Build und Tests

```bash
cargo check                                    # Syntax-/Type-Check
cargo build --release                          # optimiertes Binary
cargo test                                     # alle Tests
cargo test --release -p corefs --test fuse_handler_e2e   # E2E-FUSE
cargo build -p corefs-core --no-default-features         # AnyOS-Kompatibilitätscheck
```

Commit-Workflow: Tests müssen grün sein, bevor ein Commit erzeugt wird (siehe [CLAUDE.md](CLAUDE.md)).

## Schnellstart

```bash
cargo build --release
alias corefs=./target/release/corefs

# Image anlegen, mounten, nutzen
corefs mkfs-image ./corefs.img --demo
corefs mount-image ./corefs.img /tmp/corefs-mnt
echo "Hallo" > /tmp/corefs-mnt/hello.txt
umount /tmp/corefs-mnt

# Integrität prüfen
corefs fsck-image ./corefs.img
corefs inspect-image ./corefs.img

# Blockgerät (Linux, root)
sudo corefs probe-device        /dev/sdb1
sudo corefs mkfs-device         /dev/sdb1
sudo corefs mount-device-rw     /dev/sdb1 /mnt/usb

# Benchmark mit History
corefs benchmark --profile ci --log ./PERFORMANCE_LOG.md
```

Vollständige CLI-Referenz: [doc/cli.md](doc/cli.md) · weitere Beispiele: [doc/examples.md](doc/examples.md).

## Dokumentation

| Dokument | Inhalt |
|---|---|
| [doc/README.md](doc/README.md) | Dokumentations-Index |
| [doc/overview.md](doc/overview.md) | Projektübersicht |
| [doc/architecture.md](doc/architecture.md) | Workspace, Schichten, Fassade |
| [doc/features.md](doc/features.md) | Feature-Katalog mit Status |
| [doc/persistence-format.md](doc/persistence-format.md) | ODF v1 & `COREFS01`-Image |
| [doc/fuse-integration.md](doc/fuse-integration.md) | Linux-FUSE-Adapter |
| [doc/snapshots-versioning.md](doc/snapshots-versioning.md) | Snapshots, Versionen, Time-Travel |
| [doc/security.md](doc/security.md) | Verschlüsselung & Zugriff |
| [doc/key-management.md](doc/key-management.md) | Keystore, HKDF, Rotation |
| [doc/backup.md](doc/backup.md) | `COREFSBK`-Stream |
| [doc/deduplication.md](doc/deduplication.md) | CoW, Inline-Dedup, Pass |
| [doc/integrity-recovery.md](doc/integrity-recovery.md) | CRC32C, Journal, WAL, Repair |
| [doc/block-devices.md](doc/block-devices.md) | Blockgeräte, Fake-Stick-Schutz |
| [doc/performance.md](doc/performance.md) | Benchmark-Framework |
| [doc/testing.md](doc/testing.md) | Tests (Unit / E2E / Stress / Fault) |
| [doc/anyos-integration.md](doc/anyos-integration.md) | Einbindung in AnyOS |
| [doc/configuration.md](doc/configuration.md) | `CoreFsConfig` & Policies |
| [doc/development.md](doc/development.md) | Entwicklungsleitfaden |
| [doc/examples.md](doc/examples.md) | Beispiele |
| [doc/glossary.md](doc/glossary.md) | Begriffsverzeichnis |

## Noch offen / Verbesserungsbedarf

Kompakte Übersicht (ausführlich in den Themendokumenten):

- **Hardlinks** einführen (`link()`, `link_count`-Semantik).
- **ACL-Enforcement** im FUSE-Pfad (bisher nur POSIX-Mode).
- **xattr-Routing** End-to-End durch FUSE und Service-Fassade.
- **Snapshot-Speichereffizienz**: Content-Hash + Block-Pinning statt Byte-Kopien.
- **Argon2id** als produktive Passwort-KDF.
- **Produktiver Kernel-Treiber** für AnyOS (aktuell POC).
- **Cluster/Replikation** — aktuell nur `SyncService`-Stub.
- **Tiering** — Layout vorhanden, Migrations-Policies fehlen.
- **Semantic-Indexing** — POC, keine produktiven Queries.
- **Performance-Regressions-Gate** — Historie vorhanden, Schwellwert-Assertions fehlen.
- **Windows-Adapter** — nur Stub-Verzeichnis.

Siehe [PROJECT_PROGRESS.md](PROJECT_PROGRESS.md) für den aktuellen Phasen-Plan.

## Referenzen

- [features_corefs.md](features_corefs.md) — fachliche Zieldefinition
- [corefs_brainstorming.txt](corefs_brainstorming.txt) — Architektur-Ideen
- [PERFORMANCE_LOG.md](PERFORMANCE_LOG.md) — Benchmark-Historie
- [CLAUDE.md](CLAUDE.md) — Arbeitsanweisungen für Claude Code
