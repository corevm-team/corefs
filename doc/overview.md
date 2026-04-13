# Projektübersicht

## Was ist CoreFS?

CoreFS ist ein in **Rust** entwickeltes, natives Dateisystem. Es ist primär als Standard-Dateisystem des Betriebssystems **AnyOS** konzipiert, aber bewusst **plattformneutral** gehalten — Linux-spezifische Konzepte gehören nicht in den Kern, sondern ausschließlich in optionale Plattformadapter (`platform/`).

Der Code liegt unter [src/](../src/) (~18'600 Zeilen Rust, Edition 2024) und wird von einer umfangreichen Unit- und Integrationstest-Suite begleitet (aktuell ~283 Tests erfolgreich).

## Kernziele

- **Plattformneutralität** — saubere Trennung Kern ↔ Plattformadapter
- **Enterprise-Architektur** — klare Schichten (`domain → storage → services → app → platform`)
- **Hohe Parallel-Performance** — optimiert für viele gleichzeitige Zugriffe
- **SSD-Optimierung** — TRIM/Discard, blockfreundliches Layout
- **Datenintegrität** — Checksummen, transaktionales Journal, mehrstufige fsck-Reparatur
- **Versionierung & Snapshots** — automatische Dateihistorie, Time-Travel
- **Semantische Metadaten** — Tags, ACLs, erweiterte Attribute
- **Sicherheit** — ChaCha20-Poly1305-Verschlüsselung, ACLs, sicheres Löschen

## Status

| Aspekt | Reifegrad |
|---|---|
| Build | stabil |
| Tests | 283/283 grün |
| Kernfunktionen | produktiv nutzbar |
| Linux-FUSE | produktiv, read-only und read-write |
| Blockgeräte-I/O | produktiv (`mkfs-device`, `fsck-device`, `mount-device-rw`, `verify-device`) |
| Nebenläufigkeit | Basis vorhanden, Concurrency-Tests fehlen (Ausbaupunkt) |
| Cluster-Sync | konzeptionell vorhanden, nicht aktiviert |

Details zum aktuellen Stand: [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md).

## Einsatzszenarien

- **Testbench** für Dateisystem-Konzepte (CoW, Snapshots, Time-Travel)
- **Embedded-FS** auf Linux via FUSE
- **USB-Stick / Partition** via direkter Block-Device-Nutzung (inkl. Fake-Stick-Detection)
- **Zielplattform AnyOS** — als natives Dateisystem (in Vorbereitung)

## Abgrenzung

CoreFS ist **kein** Ersatz für produktive Dateisysteme wie ZFS oder btrfs. Es ist ein architektonisch durchdachter Prototyp mit produktiv nutzbaren Kernfunktionen, dem für den Enterprise-Einsatz noch Tests für Concurrency, Fault-Injection und Skalierung fehlen.
