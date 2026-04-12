# CoreFS

`CoreFS` ist ein in Rust entwickeltes, plattformneutrales Dateisystemprojekt mit dem Ziel, als natives Standard-Dateisystem eines eigenen Betriebssystems eingesetzt zu werden. Das Repository enthält aktuell einen strukturierten, getesteten Kern-, Persistenz-, Volume-Layout-, Replay-, Integritäts-, Linux-FUSE- und Performance-Prototyp mit klarer Modultrennung, CLI-Einstiegspunkt und einer dokumentierten Zielarchitektur.

## Ziel

CoreFS soll langfristig ein modernes, performantes und fehlertolerantes Dateisystem werden mit Schwerpunkt auf:

- hoher Parallel-Performance
- SSD-Optimierung
- Versionierung und Snapshots
- Datenintegrität und Recovery
- semantischen Metadaten
- plattformneutraler Systemintegration
- optionalen Kompatibilitätsadaptern für Fremdsysteme

Die fachliche Zieldefinition liegt in [features_corefs.md](/daten1/development/brian/corefs/features_corefs.md).

## Projektstatus

Der aktuelle Stand ist ein Architektur-, Kern-, Persistenz-, Volume-Layout- und Performance-Prototyp im Userspace-Modell.

- Build-Status: stabil
- Test-Status: `66/66` Tests erfolgreich
- Git-Status: initialisiert
- Plattformausrichtung: plattformneutral, nicht Linux-zentriert

Der detaillierte Fortschritt wird in [PROJECT_PROGRESS.md](/daten1/development/brian/corefs/PROJECT_PROGRESS.md) gepflegt.

## Repository-Struktur

### `src/app`

Zentrale Orchestrierung und Fassade über `CoreFsService`.

### `src/domain`

Fachliche Grundtypen wie:

- `Inode`
- `Snapshot`
- `FileMetadata`
- `AclEntry`
- `VolumeDescriptor`

### `src/storage`

Speichernahe Basiskomponenten:

- Inode-Allokation
- Blockspeicher
- Katalog für aktive und gelöschte Einträge

### `src/services`

Anwendungsnahe Funktionsmodule:

- Journaling
- Versionierung
- Recovery
- Integrität
- Indexierung
- Sicherheit
- Synchronisationsstatus

### `src/platform`

Plattformneutrale Runtime- und Tooling-Blueprints.

### `src/cli.rs`

Einfacher Einstieg für Demo- und Verwaltungsoperationen.

## Aktuell implementierte Funktionen

Der Prototyp deckt bereits folgende Bereiche ab:

- Formatierung eines CoreFS-Volumes im In-Memory-Modell
- Speichern und Laden eines vollständigen CoreFS-Zustands in ein JSON-basiertes Persistenzformat
- Speichern und Laden eines mehrsegmentigen binären CoreFS-Volume-Images mit Segmenttabelle, redundanten Superblocks (`SUPR` und `SUP2`), Generation Countern, Prüfsummen und getrennten Fachsegmenten wie `AINO`, `DINO`, `JOUR`, `VERS`, `SNAP`, `BLKD` und `DATA`
- Dateien, Verzeichnisse und symbolische Links
- Lesen und Schreiben von Inhalten
- Linux-Testadapter über FUSE mit `.img`-Dateien als Mount-Backend
- Journaling von Operationen
- Basis-Versionierung
- Snapshots
- Recoverable Delete und Secure Delete
- Checksummenbasierte Integritätsprüfung
- Scrubbing über vorhandene Datenblöcke
- `fsck-image` zur strukturellen Prüfung persistierter Volume-Images
- mehrstufige `fsck-image --repair`-Reparatur mit Wiederaufbau redundanter Superblocks, Rekonstruktion beschädigter Segmentverzeichnisse aus validierbaren Payloads, Rekonstruktion defekter `BLKD`-Deskriptoren aus Inode-/DATA-Informationen, Header-/Segmenttabellen-Fallback, Journal-Abgleich und Bereinigung verwaister Block-Deskriptoren
- Sync-Status-Verfolgung
- semantische Inhaltsklassifikation
- ACL-, Tag- und Metadaten-Grundmodell
- Journal-Replay zur Zustandsabstimmung beim Laden persistierter Images
- Auswahl der besten gültigen Superblock-Kopie über Generation Counter
- Journal-basierte Kanonisierung aktiver und gelöschter Inodes beim Image-Repair
- best-effort-Recovery über Header und Segmenttabelle, auch wenn keine gültige Superblock-Kopie mehr vorhanden ist
- Rekonstruktion einzelner Segmenteinträge aus bekannter Segmentreihenfolge und JSON-validierbaren Payload-Grenzen
- plattformneutrales Runtime-Blueprint-Modell
- Linux-spezifischer FUSE-Adapter als optionaler Test- und Integrationspfad
- synthetisches Performance-Tool
- Markdown-basierte Performance-Historie
- konfigurierbare Benchmark-Profile fuer verschiedene Lastbilder

## Noch nicht vollständig implementiert

Diese Punkte sind vorgesehen, aber aktuell noch nicht als echte produktionsnahe Laufzeitimplementierung vorhanden:

- vollständig produktionsnahes On-Disk-Format
- echter Blockdevice-Zugriff
- vollständiges Copy-on-Write auf Datenträgerebene
- vollständig operationsbasiertes replay-fähiges Journal
- Deduplizierung
- Self-Healing mit Redundanzquellen
- Clusterfähigkeit
- Hot/Cold-Storage
- echte Kompression und Verschlüsselung
- Quotas mit Durchsetzung
- Time-Travel-Zugriff
- native Kernel-/VFS-Integration

## Voraussetzungen

Benötigt wird eine aktuelle Rust-Toolchain mit `cargo` und `rustc`.

Beispiel:

```bash
cargo --version
rustc --version
```

## Build und Tests

Projekt prüfen:

```bash
cargo check
```

Tests ausführen:

```bash
cargo test
```

Formatierung anwenden:

```bash
cargo fmt
```

## CLI-Nutzung

Das Projekt enthält ein einfaches CLI für Demo- und Entwicklungszwecke.

Status anzeigen:

```bash
cargo run -- status
```

Volume modellhaft formatieren:

```bash
cargo run -- mkfs
```

Snapshot erzeugen:

```bash
cargo run -- snapshot nightly
```

Integritätsprüfung ausführen:

```bash
cargo run -- scrub
```

Datei lesen:

```bash
cargo run -- read /etc/corefs.conf
```

Zustand speichern:

```bash
cargo run -- save ./corefs-state.json
```

Zustand laden:

```bash
cargo run -- load ./corefs-state.json
```

Volume-Image speichern:

```bash
cargo run -- save-image ./corefs-volume.img
```

Linux-Test-Image erzeugen:

```bash
cargo run -- mkfs-image ./corefs-linux.img --demo
```

Volume-Image laden:

```bash
cargo run -- load-image ./corefs-volume.img
```

Benchmark ausführen:

```bash
cargo run -- benchmark
```

Profilierter Benchmark:

```bash
cargo run -- benchmark --profile snapshot-heavy --files 100 --payload 512 --snapshots 5 --saves 2
```

Benchmark protokollieren:

```bash
cargo run -- benchmark-log ./PERFORMANCE_LOG.md
```

Volume-Image prüfen:

```bash
cargo run -- fsck-image ./corefs-volume.img
```

Volume-Image prüfen und redundante Superblocks reparieren:

```bash
cargo run -- fsck-image ./corefs-volume.img --repair
```

Image unter Linux per FUSE mounten:

```bash
mkdir -p /tmp/corefs-mnt
cargo run -- mount-image ./corefs-linux.img /tmp/corefs-mnt
```

Verfuegbare Profile:

- `balanced`
- `small-files`
- `metadata-heavy`
- `snapshot-heavy`
- `persist-heavy`

Datei schreiben:

```bash
cargo run -- write /etc/corefs.conf updated
```

Datei löschen:

```bash
cargo run -- delete /var/readme.txt
```

Sicher löschen:

```bash
cargo run -- delete /var/readme.txt --secure
```

## Dokumentationsquellen im Repository

- [features_corefs.md](/daten1/development/brian/corefs/features_corefs.md): fachliche Zielanforderungen
- [PROJECT_PROGRESS.md](/daten1/development/brian/corefs/PROJECT_PROGRESS.md): aktueller Umsetzungsstand
- [PERFORMANCE_LOG.md](/daten1/development/brian/corefs/PERFORMANCE_LOG.md): protokollierte Benchmark-Historie
- [corefs_brainstorming.txt](/daten1/development/brian/corefs/corefs_brainstorming.txt): ursprüngliche Ideensammlung
- [/.codex](/daten1/development/brian/corefs/.codex): projektinterne Arbeitsvorgaben

## Empfohlene nächste Schritte

Die nächste sinnvolle Ausbaufolge ist:

1. Das aktuelle JSON-Zwischenformat in ein echtes blockorientiertes On-Disk-Format überführen.
2. Journal und Metadaten dauerhaft und crash-konsistent speichern.
3. VFS- und Kernel-Integrationsschnittstelle für das eigene Betriebssystem entwerfen.
4. Sicherheits-, Integritäts- und Recovery-Funktionen auf reale Laufzeitmechanismen anheben.
5. Erweiterte Features wie Cluster, Deduplizierung und semantische Tiefenanalyse ergänzen.

## Einordnung

CoreFS ist aktuell ein sauber strukturierter und getesteter Kern-, Persistenz-, Volume-Layout-, Replay- und Performance-Prototyp, aber noch kein produktionsreifes Dateisystem. Das Repository ist bewusst so aufgebaut, dass aus dem vorhandenen Architekturkern schrittweise ein echtes Dateisystem mit segmentierter dauerhafter Speicherung, Replay-Mechanismen und nativer Systemintegration entstehen kann.
