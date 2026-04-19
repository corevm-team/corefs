# Projektübersicht

## Was ist CoreFS?

CoreFS ist ein in **Rust** (Edition 2024) entwickeltes, natives Dateisystem. Es ist primär als Standard-Dateisystem des eigenen Betriebssystems **AnyOS** konzipiert, bewusst **plattformneutral** gehalten und verfügt unter Linux über einen optionalen **FUSE-Adapter** sowie direkte Blockgeräte-I/O.

Der Quellcode verteilt sich auf mehrere Crates eines Cargo-Workspace:

- `corefs-core` — Kernbibliothek, **vollständig `no_std + alloc`** (kompiliert für `x86_64-anyos`)
- `corefs` (Root-Crate) — `std`-Komposition, Image-Persistenz, FUSE, CLI
- `corefs-cli`, `corefs-tools`, `corefs-std` — Werkzeuge und Wrapper
- `corefs-fuse-proto`, `corefs-fuse-adapter` — Protokoll-Stubs für einen zukünftigen Kernel-Daemon

## Kernziele

- **Plattformneutralität** — strikte Trennung Kern ↔ Plattformadapter
- **Enterprise-Architektur** — klare Schichten (`domain → storage → services → app → platform`)
- **Datenintegrität** — CRC32C-Checksummen, transaktionales Journal, mehrstufige Reparatur
- **Versionierung & Snapshots** — automatische Dateihistorie, scoped Snapshots, Time-Travel
- **Verschlüsselung** — ChaCha20-Poly1305 (AEAD), HKDF-SHA256 Per-File-Keys, Keystore
- **Performance** — CoW mit Ref-Counting, Extent-Trees, LZ4-Kompression, Dedup, Hot-Path-Telemetrie
- **SSD-Freundlichkeit** — TRIM/Discard, blockaligned Layout, inkrementelle Persistierung

## Status (Stand 2026-04)

| Aspekt | Reifegrad |
|---|---|
| Build | stabil (Workspace, mehrere Crates) |
| Tests | ~979 Tests (Unit / Integration / E2E / Stress / Fault-Injection / Concurrency) |
| On-Disk-Format (ODF v1) | produktiv, `COREFSDF`-Magic, redundante Superblöcke |
| Image-Format | produktiv, `COREFS01` v7, mehrsegmentig, repairable |
| Linux-FUSE (RO/RW) | produktiv, Streaming, Snapshots-Overlay, Time-Travel |
| Blockgeräte-I/O | produktiv (probe / mkfs / mount / fsck / verify) |
| Verschlüsselung | produktiv (Pipeline compress→encrypt→store) |
| Snapshots & Versionierung | produktiv, Byte-Budget-Pruning |
| Backup/Export | produktiv (`COREFSBK`, voll und inkrementell) |
| Dedup / Defrag / Scrub | produktiv |
| AnyOS-Kernelintegration | **teilweise** (`corefs-core` no_std-ready, Treiber POC) |
| Clustering / Replikation | **geplant** (Service-Stub vorhanden) |
| Hardlinks | **nicht implementiert** (`link_count`-Feld reserviert) |
| Windows-Support | **nicht implementiert** (Stub-Verzeichnis) |

Details: [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md), [features.md](features.md).

## Einsatzszenarien

- **Testbench** für Dateisystem-Konzepte (CoW, Snapshots, Time-Travel, Repair)
- **Embedded-FS** auf Linux via FUSE-Image-Mount
- **USB-Stick / Partition** via direktem Block-Device-Mount (inkl. Fake-Stick-Detection)
- **Zielplattform AnyOS** — als natives Dateisystem (in Vorbereitung)

## Abgrenzung

CoreFS ist **kein** Produktions-Ersatz für ZFS, btrfs oder ext4. Es ist ein architektonisch durchdachter, umfangreich getesteter Prototyp mit produktiv nutzbaren Kernfunktionen. Für den breiten Produktiveinsatz fehlen insbesondere:

- Skalierungs-Nachweis jenseits von ~2000 Dateien / wenigen GiB (Stress-Tests existieren, aber keine Multi-Node-Workloads).
- Hardlink-Semantik (nicht implementiert).
- Vollständiges ACL-Enforcement im FUSE-Pfad (ACLs werden gespeichert, aber nur POSIX-Mode wird erzwungen).
- Produktiver Kernel-Treiber für AnyOS (aktuell POC-Stadium).
