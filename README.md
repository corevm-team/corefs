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
- Test-Status: `129/129` Tests erfolgreich
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
- `linux_fuse.rs` — Linux-FUSE-Adapter (read-only und read-write) mit `.img`-Dateien als Backend, inkl. virtuellen Read-only-Overlays für Snapshots (`.snapshots/`) und Time-Travel (`file@<spec>`)
- `performance.rs` / `tools.rs` — synthetisches Benchmark-Framework mit konfigurierbaren Profilen

### `src/cli.rs`

Einfacher Einstieg für Demo- und Verwaltungsoperationen.

## Aktuell implementierte Funktionen

Der Prototyp deckt bereits folgende Bereiche ab:

- Formatierung eines CoreFS-Volumes im In-Memory-Modell
- Speichern und Laden eines mehrsegmentigen binären CoreFS-Volume-Images mit Segmenttabelle, redundanten Superblocks (`SUPR` und `SUP2`), Generation Countern, Prüfsummen, Clean/Unclean-Markierung und binären Segment-Frames für Fachsegmente wie `AINO`, `DINO`, `JOUR`, `TXNJ`, `HOTP`, `VERS`, `SNAP`, `BLKD` und `DATA`
- spezialisierte Binärlayouts für Inode-, Journal- und Snapshot-Segmente statt allgemeiner Serde-Serialisierung
- Dateien, Verzeichnisse und symbolische Links
- Lesen und Schreiben von Inhalten
- Linux-FUSE-Adapter mit `.img`-Dateien als Mount-Backend (read-only und read-write mit Writeback)
- direkte Block-Device-Nutzung (USB-Stick, Partition, Raw Device): `probe-device`, `mkfs-device`, `fsck-device`, `verify-device`, `mount-device-rw` mit sektorausgerichtetem I/O, Permission-Checks und Fake-Stick-Erkennung (Sanity-Check an 6 verteilten Offsets automatisch bei `mkfs-device`; vollständiger destruktiver Scan via `verify-device`)
- Linux-FUSE-Read-/Write-Caching auf File-Handle-Ebene mit Write-Back-Flush ueber `flush`/`fsync`/`release`
- Streaming-Writes: sequentielle Schreibzugriffe ab 32 MiB werden als Zwischenflushes delegiert, Peak-RAM bleibt auf O(32 MiB) begrenzt statt O(Dateigrösse)
- FUSE-Durchsatz-Optimierungen: `FUSE_WRITEBACK_CACHE` (Kernel-seitiges Schreib-Batching) und `max_write = 1 MiB` für weniger Roundtrips
- backing-store-aware `statfs`-Kapazitaetsmeldung fuer Linux-FUSE und `ENOSPC`-Rueckgabe bei Platzmangel im `.img`-Persistenzpfad
- treibernahe Linux-FUSE-Tests fuer Handle-Open, Truncate, Read-Cache, Write-Back-Flush, Release, Persistenzpfade, Snapshot-Overlays und Time-Travel-Parsing
- Fix fuer neu angelegte Dateien im Linux-FUSE-RW-Pfad: `create` liefert jetzt sofort einen gueltigen Write-Back-Handle fuer anschliessende `write()`-Aufrufe
- Linux-End-to-End-Testskript fuer `mkfs-image`, RW-Mount, Shell-Dateioperationen, optionalen `unzip`-Workload, Umount und Read-only-Revalidierung
- Journaling von Operationen
- transaktionales Journal mit `tx_begin`/`tx_commit`/`tx_abort`, Pending-Transaktionen und Recovery-Markern
- persistente physische Volume-Allokation pro Dateiinhalt mit stabilen `device_block`-/`allocated_blocks`-Metadaten im Volume-Image
- freie Extent-Wiederverwendung im Storage-Layer mit Gap-Reuse, Shrink-Freigabe und Tail-Trim fuer physische Blockallokationen
- persistentes `FREE`-Segment im Volume-Image mit Free-List-Metadaten und persistenter Allocator-Policy
- aktive Defragmentierung/Compaction fuer belegte Extents inklusive Service-API und CLI-Kommandos `defrag` und `defrag-image`
- Fragmentierungsmetriken, persistente Auto-Compaction-Policy und Optimierungspfade ueber `optimize` und `optimize-image`
- gezielte Heat-aware Extent-Reallocation mit persistierter Hot-Path-Telemetrie fuer priorisierte Platzierung haeufig genutzter Inodes
- integriertes Pending-WAL im Volume-Image fuer den RW-Mount
- extent- und device-blockadressierte WAL-Records ueber `inode + device_block + block_offset + inode_offset` fuer partielle File-Patches und Truncates statt nur grober Vollwrites
- Basis-Versionierung mit automatischer Versionshistorie pro Datei
- Snapshots mit Erfassung aller aktiven Pfade zum Aufnahmezeitpunkt
- Snapshot-Browsing über `.snapshots/<id>-<name>/` im gemounteten RW-Dateisystem (virtuelle Read-only-Ordnerstruktur)
- Time-Travel-Zugriff auf historische Dateiversionen über `file@YYYY-MM-DD`, `file@YYYY-MM-DDTHH:MM` und `file@vN` im gemounteten RW-Dateisystem
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

Linux-FUSE-End-to-End-Testskript:

```bash
./scripts/corefs-e2e-linux-rw.sh
```

Optional mit ZIP-Workload:

```bash
./scripts/corefs-e2e-linux-rw.sh ./scripts/corefs-e2e.img ./scripts/mnt/corefs-e2e /pfad/zu/test.zip
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

Blockgerät (USB-Stick, Partition) analysieren, formatieren, prüfen und mounten:

```bash
sudo cargo run --release -- probe-device /dev/sdb1
sudo cargo run --release -- mkfs-device /dev/sdb1
sudo cargo run --release -- fsck-device /dev/sdb1
sudo cargo run --release -- mount-device-rw /dev/sdb1 /mnt/usb
sudo cargo run --release -- verify-device /dev/sdb1 --destructive
```

Details und vollständiger Workflow unter [Block-Device-Nutzung](#block-device-nutzung-usb-stick-partition-raw-device).

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

### Snapshot-Browsing über `.snapshots/`

Im RW-Mount erscheint ein virtuelles Verzeichnis `.snapshots/` direkt im Root des Dateisystems. Es enthält für jeden vorhandenen Snapshot ein Read-only-Unterverzeichnis mit der Baumstruktur zum Aufnahmezeitpunkt:

```bash
# Snapshot anlegen (CLI oder programmatisch)
cargo run -- snapshot nightly

# Im gemounteten RW-Mount
ls /tmp/corefs-mnt/.snapshots/
# → 1-nightly/

ls /tmp/corefs-mnt/.snapshots/1-nightly/etc/
# → corefs.conf  (Inhalt wie zum Snapshot-Zeitpunkt)

cat /tmp/corefs-mnt/.snapshots/1-nightly/etc/corefs.conf
# → historischer Inhalt, auch wenn die Datei live überschrieben wurde
```

Schreibversuche in `.snapshots/` geben `EROFS` zurück.

### Time-Travel-Zugriff über `@`-Syntax

Im RW-Mount kann jede Datei mit einem `@`-Suffix versehen werden, um eine historische Version direkt zu lesen, ohne den Snapshot-Ordner aufzurufen:

```bash
# Version vom bestimmten Datum
cat /tmp/corefs-mnt/etc/corefs.conf@2026-04-13

# Version mit Uhrzeit
cat /tmp/corefs-mnt/etc/corefs.conf@2026-04-13T10:30

# Bestimmte Versionsnummer
cat /tmp/corefs-mnt/etc/corefs.conf@v2
```

Diese Time-Travel-Dateien sind ebenfalls Read-only (`EROFS` bei Schreibversuchen). Die Versionshistorie wird automatisch bei jedem Schreibzugriff auf eine Datei aufgezeichnet.

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
| `.snapshots/<id>-<name>/...` lesen | historischer Snapshot-Inhalt (Read-only) |
| `datei@<spec>` lesen | historische Dateiversion per Datum oder ID (Read-only) |

> **Hinweis:** Der FUSE-Adapter ist ein Integrations- und Testpfad, kein produktionsreifes Dateisystem. Er steht nur auf Linux-Builds zur Verfügung.

## Block-Device-Nutzung (USB-Stick, Partition, Raw Device)

CoreFS kann direkt auf einem Linux-Blockgerät (`/dev/sdX1`, `/dev/nvmeXnYpZ`, …) formatiert und gemountet werden — ohne Umweg über eine `.img`-Datei auf einem Fremdsystem.

Alle Device-Kommandos erfordern Root-Rechte (Schreibzugriff auf den Device-Node).

### Gerät analysieren

Bevor du etwas zerstörst, prüfe den Zustand:

```bash
sudo ./target/release/corefs probe-device /dev/sdb1
```

Ausgabe umfasst:
- Kapazität in Bytes
- logische und physische Sektorgrösse
- Read-only-Status
- Mount-Status (`/proc/mounts`-Abgleich)
- Ganz-Disk-Erkennung (verweigert Formatierung ohne Partitionstabelle)
- `safe_to_format`-Verdict mit Liste der Blocker

### Formatieren

```bash
sudo ./target/release/corefs mkfs-device /dev/sdb1
```

Das Kommando:
1. Prüft Permissions (`sudo`/Root erforderlich)
2. Ruft `probe-device`-Sicherheits-Check auf — bricht ab bei gemountetem Gerät, Ganz-Disk ohne Partitionstabelle oder Read-only-Status
3. Schreibt ein leeres CoreFS-Volume sektorausgerichtet auf das Gerät
4. **Führt automatisch einen Fake-Stick-Sanity-Check durch** — probiert an 6 verteilten Offsets (10/25/50/75/90/99% der Kapazität) Schreibzugriffe, liest sie zurück und verifiziert. Bricht ab mit klarer Diagnose wenn das Gerät betrügerisch mehr Kapazität vortäuscht als tatsächlich vorhanden.

Mit `--skip-check` lässt sich der Sanity-Check überspringen (nicht empfohlen).

### Fake-/Counterfeit-Stick-Erkennung

Billige USB-Sticks aus unseriösen Quellen melden oft eine höhere Kapazität (z.B. 64 GB) als tatsächlich beschreibbar ist (oft nur wenige MB echter Flash). Writes über das echte Limit hinaus werden entweder stillschweigend verworfen (Datenverlust!) oder mit SCSI Sense Key "Data Protect" abgelehnt.

Für einen gründlichen Test:

```bash
# Schnell-Scan (200 Chunks × 64 KiB, wenige Sekunden)
sudo ./target/release/corefs verify-device /dev/sdb1 --destructive

# Gründlicher Scan (1000 Chunks)
sudo ./target/release/corefs verify-device /dev/sdb1 --destructive --chunks 1000

# Feinkörniger Scan mit grösseren Chunks
sudo ./target/release/corefs verify-device /dev/sdb1 --destructive \
    --chunks 500 --chunk-size 1048576
```

> **Warnung:** `verify-device --destructive` überschreibt alle Daten auf dem Gerät. Das Flag `--destructive` ist zwingend, ohne es verweigert das Kommando die Ausführung.

Ausgabe bei einem ehrlichen Stick:

```
verdict: ok — device appears to be honest
```

Ausgabe bei einem Fake-Stick:

```
verdict: FAKE — roughly 98% of advertised capacity is unusable
```

### Mounten (Read-Write)

```bash
sudo mkdir -p /mnt/usb-corefs
sudo ./target/release/corefs mount-device-rw /dev/sdb1 /mnt/usb-corefs &
```

Der Mount läuft im Vordergrund (FUSE-typisch). Danach stehen die üblichen Linux-Dateisystemoperationen zur Verfügung:

```bash
echo "Hallo CoreFS" | sudo tee /mnt/usb-corefs/hello.txt
sudo mkdir /mnt/usb-corefs/docs
sudo cp /etc/os-release /mnt/usb-corefs/docs/
ls -R /mnt/usb-corefs
```

### Unmounten

```bash
sudo fusermount -u /mnt/usb-corefs
```

Beim Unmount wird der Volume-Zustand sauber auf das Gerät zurückgeschrieben. Der Stick kann danach ausgesteckt und später wieder gemountet werden — die Daten sind persistent.

### Dateisystemprüfung (fsck) auf Blockgeräten

```bash
sudo ./target/release/corefs fsck-device /dev/sdb1
```

Prüft ohne Schreibzugriff:
- CoreFS-Magic (`COREFS01`) und Format-Version
- Redundanz beider Superblock-Kopien (`SUPR` + `SUP2`)
- Directory-Checksumme (FNV1a-Hash über die Segmenttabelle)
- Payload-Checksumme über alle Segment-Frames
- Vollständigkeit der 15 Pflichtsegmente (`CNFG`, `VOLM`, `AINO`, `DINO`, `JOUR`, `VERS`, `SYNC`, `HOTP`, `SNAP`, `TXNJ`, `FREE`, `BLKD`, `DATA`)
- Block-Deskriptor-Konsistenz

Bei Fehlschlag liefert das Kommando Exit-Code ≠ 0 — geeignet für Scripts und Monitoring.

### Vollständiger USB-Stick-Workflow

```bash
# 1. Gerät analysieren
sudo ./target/release/corefs probe-device /dev/sdb1

# 2. Optional: Fake-Stick-Check
sudo ./target/release/corefs verify-device /dev/sdb1 --destructive

# 3. Formatieren (mit automatischem Sanity-Check)
sudo ./target/release/corefs mkfs-device /dev/sdb1

# 4. Integrität prüfen
sudo ./target/release/corefs fsck-device /dev/sdb1

# 5. Mounten
sudo mkdir -p /mnt/usb-corefs
sudo ./target/release/corefs mount-device-rw /dev/sdb1 /mnt/usb-corefs &

# 6. Benutzen
ls /mnt/usb-corefs
echo "test" | sudo tee /mnt/usb-corefs/file.txt
cat /mnt/usb-corefs/file.txt

# 7. Unmounten
sudo fusermount -u /mnt/usb-corefs

# 8. Persistenz verifizieren
sudo ./target/release/corefs fsck-device /dev/sdb1
sudo ./target/release/corefs mount-device-rw /dev/sdb1 /mnt/usb-corefs &
cat /mnt/usb-corefs/file.txt
sudo fusermount -u /mnt/usb-corefs
```

### Architektur-Hinweise und Grenzen

- **Das Binärformat** auf dem Gerät ist identisch zum `.img`-Datei-Format — mehrsegmentiges Layout mit 64-Byte-Alignment, redundanten Superblocks, Generation-Counter-basierter Crash-Recovery und Checksummen.
- **Aktueller Mount-Pfad** lädt das komplette Volume-Image in den RAM und schreibt es bei jedem FUSE-Flush vollständig zurück. Für kleine Datenmengen (< einige GB) ist das funktional ausreichend.
- **Für grosse Volumes** existiert bereits die `DeviceVolume`-Abstraktion ([src/storage/device_volume.rs](src/storage/device_volume.rs)) mit On-Demand-Segment-I/O und Read-Cache/Write-Buffer. Die Integration in den FUSE-Mount ist vorgesehen, aber noch nicht produktiv aktiviert.
- **Device-Journal**: `DeviceJournal` verwaltet eine 256-KiB-Region nach dem Volume-Image für barrier-safe WAL-Einträge mit `fdatasync()`.
- **TRIM/Discard** wird beim Freigeben von Extents protokolliert und (optional, hinter Config-Flag) an den RawBlockDevice via `ioctl(BLKDISCARD)` weitergereicht.

> **Hinweis:** CoreFS auf Blockgeräten ist funktionsfähig, aber weiterhin ein Prototyp. Für produktive Daten sollte zusätzlich extern gesichert werden.

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
