# CoreFS

`CoreFS` ist ein in Rust entwickeltes, plattformneutrales Dateisystemprojekt mit dem Ziel, als natives Standard-Dateisystem eines eigenen Betriebssystems eingesetzt zu werden. Das Repository enthält aktuell einen strukturierten, getesteten Kern-, Volume-Layout-, Replay-, Integritäts-, Linux-FUSE- und Performance-Prototyp mit klarer Modultrennung, CLI-Einstiegspunkt und einer dokumentierten Zielarchitektur.

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
- Test-Status: `112/112` Tests erfolgreich
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

Plattformadapter und optionale Integrationspfade:

- `runtime.rs` — plattformneutrales Blueprint-Modell für VFS-Integration
- `linux_fuse.rs` — Linux-FUSE-Adapter (read-only und read-write) mit `.img`-Dateien als Backend
- `performance.rs` / `tools.rs` — synthetisches Benchmark-Framework mit konfigurierbaren Profilen

### `src/cli.rs`

Einfacher Einstieg für Demo- und Verwaltungsoperationen.

## Aktuell implementierte Funktionen

Der Prototyp deckt bereits folgende Bereiche ab:

- Formatierung eines CoreFS-Volumes im In-Memory-Modell
- Speichern und Laden eines mehrsegmentigen binären CoreFS-Volume-Images mit Segmenttabelle, redundanten Superblocks (`SUPR` und `SUP2`), Generation Countern, Prüfsummen, Clean/Unclean-Markierung und binären Segment-Frames für Fachsegmente wie `AINO`, `DINO`, `JOUR`, `TXNJ`, `VERS`, `SNAP`, `BLKD` und `DATA`
- spezialisierte Binärlayouts für Inode-, Journal- und Snapshot-Segmente statt allgemeiner Serde-Serialisierung
- Dateien, Verzeichnisse und symbolische Links
- Lesen und Schreiben von Inhalten
- Linux-FUSE-Adapter mit `.img`-Dateien als Mount-Backend (read-only und read-write mit Writeback)
- Journaling von Operationen
- transaktionales Journal mit `tx_begin`/`tx_commit`/`tx_abort`, Pending-Transaktionen und Recovery-Markern
- persistente physische Volume-Allokation pro Dateiinhalt mit stabilen `device_block`-/`allocated_blocks`-Metadaten im Volume-Image
- freie Extent-Wiederverwendung im Storage-Layer mit Gap-Reuse, Shrink-Freigabe und Tail-Trim fuer physische Blockallokationen
- persistentes `FREE`-Segment im Volume-Image mit Free-List-Metadaten und persistenter Allocator-Policy
- aktive Defragmentierung/Compaction fuer belegte Extents inklusive Service-API und CLI-Kommandos `defrag` und `defrag-image`
- Fragmentierungsmetriken, persistente Auto-Compaction-Policy und Optimierungspfade ueber `optimize` und `optimize-image`
- integriertes Pending-WAL im Volume-Image fuer den RW-Mount
- extent- und device-blockadressierte WAL-Records ueber `inode + device_block + block_offset + inode_offset` fuer partielle File-Patches und Truncates statt nur grober Vollwrites
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
- Recovery eines unclean beendeten RW-Mounts mit Abbruch offener Pending-Transaktionen beim Laden
- WAL-Recovery, das persistierte Pending-Operationen direkt aus dem Volume-Image vor dem naechsten Mount oder Session-Open ins Haupt-Image zurueckspielt
- Auswahl der besten gültigen Superblock-Kopie über Generation Counter
- Journal-basierte Kanonisierung aktiver und gelöschter Inodes beim Image-Repair
- best-effort-Recovery über Header und Segmenttabelle, auch wenn keine gültige Superblock-Kopie mehr vorhanden ist
- Rekonstruktion einzelner Segmenteinträge aus bekannter Segmentreihenfolge und validierbaren binären Segment-Frames
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
- blocknahes Write-Ahead-Log direkt im Volume statt des aktuellen segmentbasierten Pending-WAL
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

Verfuegbare Profile:

- `balanced`
- `small-files`
- `metadata-heavy`
- `snapshot-heavy`
- `persist-heavy`

Volume-Image prüfen:

```bash
cargo run -- fsck-image ./corefs-volume.img
```

Volume-Image prüfen und redundante Superblocks reparieren:

```bash
cargo run -- fsck-image ./corefs-volume.img --repair
```

Image schreibgeschützt mounten:

```bash
cargo run -- mount-image ./corefs-linux.img /tmp/corefs-mnt
```

Image beschreibbar mounten (Writeback in .img):

```bash
cargo run -- mount-image-rw ./corefs-linux.img /tmp/corefs-mnt
```

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

## Linux-FUSE-Mount

Der Linux-FUSE-Adapter ermöglicht es, ein CoreFS-Volume-Image direkt als Dateisystem einzuhängen. Voraussetzung ist ein Linux-System mit installiertem FUSE-Subsystem (`libfuse3` bzw. `fuse`-Kernelmodul).

### Read-only-Mount

Hängt das Image schreibgeschützt ein. Nützlich zur Inspektion, ohne den Image-Inhalt zu verändern.

```bash
# Image mit Demo-Inhalt erzeugen
cargo run -- mkfs-image ./corefs-linux.img --demo

# Mountpoint anlegen und Image einbinden
mkdir -p /tmp/corefs-mnt
cargo run -- mount-image ./corefs-linux.img /tmp/corefs-mnt

# Inhalt ansehen
ls /tmp/corefs-mnt
cat /tmp/corefs-mnt/etc/corefs.conf

# Aushängen
fusermount -u /tmp/corefs-mnt
```

Das Dateisystem erscheint unter Linux als `corefs:<volume-name>`, z. B. `corefs:corefs`.

### Read-write-Mount mit Writeback

Hängt das Image beschreibbar ein. Alle Änderungen im gemounteten Verzeichnis werden bei `close` bzw. `sync` automatisch in die `.img`-Datei zurückgeschrieben. Der RW-Pfad markiert das Image beim Öffnen bewusst als `unclean`, bündelt Änderungen in Journal-Transaktionen, persistiert Pending-Operationen direkt im `TXNJ`-Segment des Volume-Images und nutzt dabei delta-orientierte Records für File-Patches und Truncates, bevor der Zustand nach erfolgreichem Persist wieder auf `clean` gesetzt wird.

```bash
# Image erzeugen (falls noch nicht vorhanden)
cargo run -- mkfs-image ./corefs-linux.img --demo

# Mountpoint anlegen und Image beschreibbar einbinden
mkdir -p /tmp/corefs-mnt
cargo run -- mount-image-rw ./corefs-linux.img /tmp/corefs-mnt

# Dateien lesen, schreiben, anlegen, löschen
cat /tmp/corefs-mnt/etc/corefs.conf
echo "updated" > /tmp/corefs-mnt/etc/corefs.conf
mkdir /tmp/corefs-mnt/data
cp /etc/hostname /tmp/corefs-mnt/data/hostname.txt
rm /tmp/corefs-mnt/var/readme.txt

# Aushängen — ausstehende Writes werden dabei persistiert
fusermount -u /tmp/corefs-mnt

# Inhalt nach dem Aushängen prüfen
cargo run -- load-image ./corefs-linux.img
cargo run -- read /etc/corefs.conf
```

### Unterstützte Operationen im RW-Modus

| Operation | Verhalten |
|---|---|
| Lesen (`cat`, `cp`, ...) | liest aus dem In-Memory-Cache |
| Schreiben (`echo >`, `cp`, ...) | Read-modify-write, Writeback bei `flush`/`fsync` |
| Truncate (`truncate`, `> file`) | wird über `setattr` mit neuer Größe abgebildet |
| Neue Datei anlegen (`touch`, `cp`) | erzeugt neuen Inode via `create` |
| Verzeichnis anlegen (`mkdir`) | erzeugt Verzeichnis-Inode via `mkdir` |
| Datei löschen (`rm`) | Soft-delete, Inode bleibt als gelöscht markiert |
| Leeres Verzeichnis löschen (`rmdir`) | entfernt Verzeichnis-Inode; nicht-leere Dirs werden abgelehnt |
| Schreiben bei geschlossenem Handle | persistiert das Image automatisch (`flush`) |

> **Hinweis:** Der FUSE-Adapter ist ein Integrations- und Testpfad, kein produktionsreifes Dateisystem. Er steht nur auf Linux-Builds zur Verfügung.

## Dokumentationsquellen im Repository

- [features_corefs.md](/daten1/development/brian/corefs/features_corefs.md): fachliche Zielanforderungen
- [PROJECT_PROGRESS.md](/daten1/development/brian/corefs/PROJECT_PROGRESS.md): aktueller Umsetzungsstand
- [PERFORMANCE_LOG.md](/daten1/development/brian/corefs/PERFORMANCE_LOG.md): protokollierte Benchmark-Historie
- [corefs_brainstorming.txt](/daten1/development/brian/corefs/corefs_brainstorming.txt): ursprüngliche Ideensammlung
- [/.codex](/daten1/development/brian/corefs/.codex): projektinterne Arbeitsvorgaben

## Empfohlene nächste Schritte

Die nächste sinnvolle Ausbaufolge ist:

1. Das aktuelle segmentierte Binärformat weiter in Richtung eines echten blockorientierten On-Disk-Layouts mit stärker spezialisierter Segmentcodierung weiterentwickeln.
2. Die Defragmentierungs- und Allocator-Schicht um intelligentere Reallocation-Policies, Hintergrund-Compaction und spaeter Copy-on-Write-orientierte Extent-Moves erweitern.
3. VFS- und Kernel-Integrationsschnittstelle für das eigene Betriebssystem entwerfen.
4. Sicherheits-, Integritäts- und Recovery-Funktionen auf reale Laufzeitmechanismen anheben.
5. Erweiterte Features wie Cluster, Deduplizierung und semantische Tiefenanalyse ergänzen.

## Einordnung

CoreFS ist aktuell ein sauber strukturierter und getesteter Kern-, Persistenz-, Volume-Layout-, Replay- und Performance-Prototyp, aber noch kein produktionsreifes Dateisystem. Das Repository ist bewusst so aufgebaut, dass aus dem vorhandenen Architekturkern schrittweise ein echtes Dateisystem mit segmentierter dauerhafter Speicherung, Replay-Mechanismen und nativer Systemintegration entstehen kann.
