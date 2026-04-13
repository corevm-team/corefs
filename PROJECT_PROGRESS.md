# CoreFS Projektfortschritt

## Überblick

`CoreFS` ist ein in Rust entwickeltes, plattformneutrales Dateisystemprojekt mit Fokus auf den nativen Einsatz als Standard-Dateisystem des eigenen Betriebssystems. Das Repository enthält aktuell ein strukturiertes, kompilierbares Grundsystem mit klarer Modultrennung, CLI-Einstiegspunkt, Service-Schichten, Volume-Persistenzpfad, Integritätswerkzeugen, Linux-FUSE-Testadapter, Performance-Tooling und einer breiten Testsuite.

## Aktueller Status

**Projektphase:** Architektur-, Kern-, Persistenz-, Volume-Layout-, Replay-, Integritäts-, Linux-FUSE- und Performance-Prototyp  
**Build-Status:** stabil  
**Test-Status:** `165/165` Tests erfolgreich  
**Ausrichtung:** plattformneutral, nicht Linux-zentriert

## Bereits umgesetzt

### Projektstruktur

- Rust-Projekt mit `lib`- und `bin`-Einstieg
- klare Schichtung in `app`, `domain`, `storage`, `services`, `platform`
- zentrale Fassade über `CoreFsService`
- CLI-Kommandos für Grundfunktionen
- CLI-Kommandos für Image-Persistenz und Wartung
- CLI-Kommandos für Benchmarking und Performance-Logging

### Domänenmodell

- Inodes
- ACL-Einträge und Principals
- Dateimetadaten
- Snapshots
- Volume-Deskriptoren

### Kernfunktionen im Prototyp

- Formatierung eines CoreFS-Volumes im Userspace-Modell
- Persistenz eines mehrsegmentigen binären CoreFS-Volume-Images mit Segmenttabelle, redundanten Superblocks, Generation Countern, Clean/Unclean-Markierung und binären Segment-Frames für Fachsegmente
- spezialisierte Binärlayouts für Inode-, Journal- und Snapshot-Segmente innerhalb des Volume-Images
- Linux-Testadapter über FUSE zum read-only und read-write Mounten von `.img`-Dateien
- Linux-FUSE-Read-/Write-Caching auf File-Handle-Ebene mit Write-Back-Flush und Read-Serving aus dem Open-Handle-Cache
- backing-store-aware `statfs`-Kapazitaetsmeldung fuer Linux-FUSE und sauberere `ENOSPC`-Rueckgabe bei Platzmangel im `.img`-Persistenzpfad
- treibernahe Linux-FUSE-Tests fuer Open/Truncate, Read-Cache, Write-Back-Flush, Release, Persistenzverhalten, Snapshot-Overlays und Time-Travel-Parsing
- Fix fuer neu angelegte Dateien im Linux-FUSE-RW-Pfad: `create` liefert jetzt sofort einen gueltigen Write-Back-Handle fuer nachfolgende Schreibzugriffe
- Linux-End-to-End-Testskript fuer `mkfs-image`, RW-Mount, Shell-Dateioperationen, optionalen `unzip`-Workload, Umount und Read-only-Revalidierung
- Erzeugen von Dateien, Verzeichnissen und symbolischen Links
- Lesen und Schreiben von Dateiinhalten
- Journaling von Operationen
- transaktionales Journal mit Pending-Transaktionen, Commit-/Abort-Markern und Recovery-Einträgen
- persistente physische Volume-Allokation pro Dateiinhalt mit stabilen `device_block`-/`allocated_blocks`-Metadaten
- freie Extent-Wiederverwendung mit Gap-Reuse, Freigabe überschüssiger Blöcke bei Shrinks und Tail-Trim im Storage-Layer
- persistentes `FREE`-Segment mit Free-List-Metadaten und persistenter Allocator-Policy
- aktive Defragmentierung/Compaction fuer belegte Extents inklusive Service- und CLI-Pfad
- Fragmentierungsmetriken, persistente Auto-Compaction-Policy und explizite Optimierungspfade fuer Live-Instanzen und Volume-Images
- gezielte Heat-aware Extent-Reallocation auf Basis persistierter Hot-Path-Telemetrie fuer priorisierte Platzierung haeufig genutzter Inodes
- integriertes Pending-WAL im Volume-Image fuer RW-Sessions
- extent- und device-blockadressierte WAL-Records ueber `inode + device_block + block_offset + inode_offset` fuer partielle File-Patches und Truncates
- automatische Versionierung im Basismodell
- Snapshot-Erzeugung
- Recoverable Delete und Secure Delete
- einfache Integritätsprüfung per Checksummen
- Scrubbing über vorhandene Datenblöcke
- `fsck-image` für strukturelle Prüfungen von Volume-Images
- erste mehrstufige Image-Reparatur aus verbliebener gültiger Kopie oder per Header-/Segmenttabellen-Fallback mit Superblock-Wiederaufbau, Rekonstruktion beschädigter Segmentverzeichnisse, Rekonstruktion defekter Blockdeskriptoren, Journal-Abgleich und Bereinigung verwaister Blockdaten
- Sync-Status-Verfolgung
- semantische Inhaltsklassifikation nach Dateiendung
- Metadaten-, Tag- und ACL-Grundmodell
- Journal-Replay zur Zustandsabstimmung geladener Images
- Recovery eines unclean beendeten RW-Mounts mit automatischem Abbruch offener Pending-Transaktionen beim Laden
- WAL-Recovery vor FUSE-Mount und `VolumeSession::open`, damit persistierte Pending-Operationen direkt aus dem Volume-Image ins Haupt-Image zurueckgespielt werden
- synthetischer Performance-Benchmark für Datei-, Snapshot- und Persistenzpfade
- Markdown-Protokollierung von Benchmark-Ergebnissen
- konfigurierbare Benchmark-Profile für unterschiedliche Lastbilder
- Streaming-Writes im Linux-FUSE-RW-Mount: sequenzielle Schreibzugriffe ab 32 MiB werden als Zwischenflushes an den Service delegiert statt vollständig im RAM gebuffert, wodurch der Peak-Speicherbedarf auf `O(Threshold)` statt `O(Dateigrösse)` begrenzt wird
- FUSE-Schreibdurchsatz-Optimierungen: `FUSE_WRITEBACK_CACHE` (Kernel-seitiges Schreib-Batching) und `max_write = 1 MiB` (weniger Kernel↔Daemon-Roundtrips), O(n²)-Klon-Bug in `write_to_handle` behoben (vorher: `node.data.clone()` bei jedem Write-Call)
- WAL-Vereinfachung für Dateidaten: `PatchExtent`-Records werden nicht mehr pro Flush geschrieben, da der atomare Image-Save (write→rename) hinreichende Crash-Safety bietet; strukturelle Ops (CreateFile, TruncateInode etc.) bleiben vollständig WAL-geschützt
- inkrementelle Prüfsummenfortschreibung in `BlockStore::append_to_inode`: `O(extra.len())` statt `O(gesamt)`, Rekey der Blob-Map nach Checksum-Änderung
- transparente LZ4-Kompression für Dateiinhalte: `CompressionService` (lz4_flex frame format) komprimiert Payloads ≥ 64 B bei Schreibzugriffen automatisch; `read_file` dekomprimiert transparent; `inode.size` enthält immer die logische (unkomprimierte) Größe; Versionen werden vor der Kompression gespeichert, sodass die Versionshistorie stets vollständig lesbar bleibt
- Quota-Enforcement in `create_file` und `write_file`: schnelle Überprüfung über `Catalog::quota_stats()` (liefert `(file_count, total_bytes)` ohne Klon aller Inodes) und `QuotaService::check_stats()`; bei Überschreitung wird `CoreFsError::QuotaExceeded` zurückgegeben, bevor Daten geschrieben werden
- automatische Versionsbereinigung unter Platzdruck: `VersioningService::prune_to_budget(max_bytes)` entfernt global die ältesten Versionen solange das Gesamtvolumen der versionierten Bytes den konfigurierbaren `max_version_bytes`-Wert (Standard: 64 MiB) überschreitet
- Copy-on-Write auf Storage-Ebene: `BlockStore` implementiert vollständiges CoW mit Referenz-Zählung auf Blob-Ebene; `clone_for_inode()` teilt einen Blob zwischen zwei Inodes (ref_count++); der nächste Schreibzugriff auf einen der beiden Inodes materialisiert eine unabhängige Kopie; `cow_stats()` liefert ein Sharing-Report; `CowStats` (shared_blobs, exclusive_blobs, bytes_saved_by_sharing, max_ref_count)
- Snapshot-Block-Pinning: `Snapshot.file_data` speichert die unkomprimierten Bytes aller regulären Dateien zum Snapshot-Zeitpunkt; Snapshots sind dadurch vollständig selbständig und unabhängig von späteren Block-Mutationen
- Snapshot-Lifecycle-Management: `delete_snapshot(id)` entfernt Snapshots; `restore_snapshot(id)` schreibt alle Dateien aus `file_data` zurück (überschreibt vorhandene, legt gelöschte neu an, meldet Fehler pro Pfad statt abzubrechen)
- CoW-Klon-Semantik: `clone_file(from, to)` erstellt einen CoW-Klon — teilt sofort den Blob, divergiert erst beim nächsten Schreibzugriff; `expunge_file(path)` löscht soft-gelöschte Dateien permanent und dekrementiert den Blob-ref_count korrekt
- Bug-Fix in `BlockStore::append_to_inode` (shared path): doppeltes ref_count-Dekrement verhindert — `write()` dekrementiert bereits beim BlockEntry-Remove; das manuelle Dekrement davor würde den Blob auf 0 setzen während andere Inodes ihn noch referenzieren
- Config-Enforcement: `clone_file()` prüft `config.performance.copy_on_write`; bei deaktiviertem CoW wird eine vollständige Kopie (read+create) statt Blob-Sharing durchgeführt
- rekursives Verzeichnis-Klonen: `clone_tree(from, to)` klont einen Teilbaum mit CoW für Dateien, Verzeichnis-Erzeugung und Symlink-Neuanlage; liefert `CloneTreeReport` mit Zählern und Fehler-pro-Pfad
- Scoped Snapshots: `create_snapshot_scoped(name, scope_root)` erfasst nur Pfade unter `scope_root`; `create_snapshot(name)` delegiert auf `scope_root="/"`
- Snapshot-Diff: `diff_snapshots(a_id, b_id)` klassifiziert Dateien als added/removed/modified/unchanged zwischen zwei Snapshots
- FUSE `copy_file_range`: serverseitige Kopie zwischen zwei offenen File-Handles ohne Kernel↔Userspace-Roundtrips; liest aus Source-Handle (oder virtuellem Snapshot-Node), schreibt in Destination-Handle; EROFS für virtuelle Destinations

### Plattform- und Integrationsmodell

- native Runtime-Integration als generisches Blueprint-Modell
- optionale Kompatibilitätsziele als Adapter-Konzept
- Tool-Registry für `mkfs`, `fsck` und Administration
- Tool-Registry für Benchmarking
- Linux-FUSE-Mountpfad für Image-basierte Integrationstests inklusive RW-Writeback und Dirty/Clean-Session-Markierung
- virtuelle Read-only-Overlays im Linux-FUSE-RW-Mount: `.snapshots/<id>-<name>/` für Snapshot-Browsing und `file@<spec>` für Time-Travel-Adressierung

### Qualitätssicherung

- breite Unit-Test-Abdeckung über App-, CLI-, Service-, Storage-, Platform- und Domain-Schichten
- Regression im Recovery-/Delete-Pfad bereits gefunden und behoben
- Persistenz-Roundtrip und Ladefehler sind testseitig abgesichert
- Benchmark-Ausführung und Markdown-Logging sind testseitig abgesichert
- redundante Superblock-Fallbacks, Generation-Counter-Selektion, `fsck-image`, Image-Reparatur, Header-Directory-Recovery, Rekonstruktion beschädigter Segmentverzeichnisse, Rekonstruktion defekter Blockdeskriptoren, Journal-Replay, Dirty/Clean-Recovery und Bereinigung verwaister Blockdaten sind testseitig abgesichert
- `cargo test` aktuell vollständig erfolgreich

## Noch nicht umgesetzt

Diese Punkte sind konzeptionell vorgesehen oder im Anforderungskatalog enthalten, aber noch nicht als vollständige reale Implementierung vorhanden:

- produktionsnahes blockorientiertes On-Disk-Format
- echter Blockdevice-Zugriff
- persistentes physisch device-blockadressiertes Write-Ahead-Log direkt im Volume statt des aktuellen extent-orientierten Pending-WAL
- Deduplizierung
- Self-Healing mit Redundanzquellen
- Cluster-Synchronisation
- Hot/Cold-Storage und Tiering-Strategien
- echte Verschlüsselung
- echtes Copy-on-Write auf Datenträgerebene (physische Block-Sharing auf persistierten Medien; aktuell: logisches CoW im In-Memory-Modell)
- Time-Travel-Adressierung im FUSE-RW-Mount über `@`-Syntax ist für Lookup und Read umgesetzt; fehlt noch: Adressierung im Read-only-Mount, persistente Zugriffspfade als reale Symlinks
- fsck als weiter auszubauendes Reparatur- und Korrekturwerkzeug für stärker beschädigte Segmenttabellen, tiefere Blockdeskriptor-Rekonstruktion, Datensegment-Validierung und echte Datenheilung
- native Kernel-/VFS-Integration für das eigene Betriebssystem
- Fremdsystem-Adapter als reale Laufzeitkomponenten

## Architekturüberblick

### `src/app`

- Orchestrierung der Hauptlogik
- zentrale Fassade für Dateioperationen, Snapshots, Recovery, Scrubbing und Reports

### `src/domain`

- fachliche Grundtypen wie `Inode`, `Snapshot`, `FileMetadata`, `AclEntry`, `VolumeDescriptor`

### `src/storage`

- Inode-Allokation
- Blockspeicher im In-Memory-Modell
- Katalog für aktive und gelöschte Einträge
- mehrsegmentiges binäres Volume-Image-Format mit Segmenttabelle, Alignment-Regeln, redundanten Superblocks, Generation Countern, binären Segment-Frames und Prüflogik als weiterer Persistenzpfad

### `src/services`

- Journaling
- Versionierung mit konfigurierbarer Byte-Budget-Bereinigung (`prune_to_budget`)
- Recovery
- Integrität
- Indexierung
- Sicherheit
- Synchronisationsstatus
- Kompression (LZ4 frame via `lz4_flex`)
- Quota-Enforcement
- Copy-on-Write mit Blob-Referenz-Zählung, CoW-Klons und Snapshot-Pinning

### `src/platform`

- plattformneutrales Runtime-Integrationsmodell
- Blueprint für Verwaltungswerkzeuge
- optionaler Linux-FUSE-Adapter für `.img`-basierte Dateisystemtests
- Performance-Benchmarking und Protokollierung

### `src/cli.rs`

- einfacher administrativer Einstieg für Demo- und Testoperationen

## Abgleich mit den Anforderungen

Die Datei [features_corefs.md](/daten1/development/brian/corefs/features_corefs.md) bleibt die fachliche Zieldefinition. Der aktuelle Implementierungsstand deckt bereits Teile der folgenden Bereiche ab:

- grundlegende Dateisystem-Funktionen
- Metadaten- und ACL-Grundmodell
- Versionierung in Basisform
- Löschen und Wiederherstellung
- Integritätsprüfung
- Plattformneutralität und Integrationsmodell
- Verwaltungs- und Tooling-Grundstruktur
- Persistenz eines vollständigen CoreFS-Zustands
- Performance-Messung und Ergebnisprotokollierung
- strukturelle Prüfung persistierter Volume-Images
- Linux-Nutzung und Testbarkeit über gemountete `.img`-Dateien
- profilbasierte Performance-Messung mit variablen Parametern
- Snapshot-Browsing und Time-Travel im Linux-FUSE-RW-Mount (`.snapshots/` und `@`-Syntax)

Nur teilweise oder noch konzeptionell abgebildet sind aktuell:

- Speicherverwaltung auf echter Datenträgerebene
- fortgeschrittene Integritäts- und Redundanzmechanismen
- semantische Tiefenanalyse
- vollständige Runtime- und Betriebssystemintegration

## Empfohlene nächste Schritte

### Phase 1: Persistenz

- blockorientiertes On-Disk-Format definieren
- Metadaten-Layout festlegen
- die aktuellen binären Segment-Frames schrittweise in ein noch stärker blockorientiertes und spezialisierteres On-Disk-Format überführen
- die Defragmentierungs- und Allocator-Schicht um intelligentere Reallocation-Policies, Hintergrund-Compaction und spaeter Copy-on-Write-orientierte Extent-Moves weiterentwickeln
- Performance-Baseline für zukünftige Persistenzumstellungen fortlaufend protokollieren

### Phase 2: Systemkern

- VFS-Schnittstelle für das eigene Betriebssystem definieren
- Kernel- und Userland-Grenzen trennen
- Mount-, Unmount- und Recovery-Lebenszyklus implementieren

### Phase 3: Integrität und Sicherheit

- blocknahes Write-Ahead-Log und Replay auf Delta-Ebene direkt im Volume
- echte Kompression
- echte Verschlüsselung
- Quotas
- Scrubbing und Self-Healing mit Redundanzmodell

### Phase 4: Erweiterte Funktionen

- Time Travel
- Deduplizierung
- Clusterfähigkeit
- Hot/Cold-Storage
- semantische Inhaltsindexierung

## Wichtige Hinweise

- Das Projekt ist aktuell ein strukturierter, getesteter Kern-, Persistenz- und Volume-Layout-Prototyp und noch kein produktionsreifes Dateisystem.
- Der Linux-Mountpfad unterstützt read-only und read-write; der RW-Pfad nutzt Dirty/Clean-Markierung, transaktionales Journal-Writeback, persistente physische Volume-Allokation, freie Extent-Wiederverwendung, ein persistentes `FREE`-Segment mit Allocator-Policy, aktive Defragmentierung, ein integriertes extent- und device-blockadressiertes Pending-WAL im Volume sowie virtuelle Read-only-Overlays für Snapshot-Browsing (`.snapshots/`) und Time-Travel (`file@<spec>`), ist aber noch kein vollständiges produktionsnahes Device-WAL mit Hintergrund-Compaction oder Copy-on-Write-Moves.
- Die virtuellen FUSE-Overlays belegen INO-Bereiche im oberen `u64`-Raum (ab `u64::MAX/4` abwärts für dynamische Knoten, `u64::MAX/2+1_000_000` für Snapshot-Root-Dirs, `u64::MAX-1` für `.snapshots/`) und sind vollständig write-protected (EROFS bei jeder Mutation).
- Performance-Messungen werden jetzt über `benchmark` und `benchmark-log` reproduzierbar ausführbar.
- Die vorhandene Testsuite ist stark für die aktuelle In-Memory-Implementierung, aber keine Garantie für `100%` messbare Coverage, da in der Umgebung keine Coverage-Tools installiert sind.
- `.codex` ist inzwischen als projektinterne Vorgabedatei befüllt.
