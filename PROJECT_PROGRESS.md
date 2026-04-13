# CoreFS Projektfortschritt

## Überblick

`CoreFS` ist ein in Rust entwickeltes, plattformneutrales Dateisystemprojekt mit Fokus auf den nativen Einsatz als Standard-Dateisystem des eigenen Betriebssystems. Das Repository enthält aktuell ein strukturiertes, kompilierbares Grundsystem mit klarer Modultrennung, CLI-Einstiegspunkt, Service-Schichten, Volume-Persistenzpfad, Integritätswerkzeugen, Linux-FUSE-Testadapter, Performance-Tooling und einer breiten Testsuite.

## Aktueller Status

**Projektphase:** Architektur-, Kern-, Persistenz-, Volume-Layout-, Replay-, Integritäts-, Linux-FUSE- und Performance-Prototyp  
**Build-Status:** stabil  
**Test-Status:** `106/106` Tests erfolgreich  
**Ausrichtung:** plattformneutral, nicht Linux-zentriert

## Bereits umgesetzt

### Projektstruktur

- Rust-Projekt mit `lib`- und `bin`-Einstieg
- klare Schichtung in `app`, `domain`, `storage`, `services`, `platform`
- zentrale Fassade über `CoreFsService`
- CLI-Kommandos für Grundfunktionen
- CLI-Kommandos für Persistenz (`save` und `load`)
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
- Erzeugen von Dateien, Verzeichnissen und symbolischen Links
- Lesen und Schreiben von Dateiinhalten
- Journaling von Operationen
- transaktionales Journal mit Pending-Transaktionen, Commit-/Abort-Markern und Recovery-Einträgen
- persistente physische Volume-Allokation pro Dateiinhalt mit stabilen `device_block`-/`allocated_blocks`-Metadaten
- freie Extent-Wiederverwendung mit Gap-Reuse, Freigabe überschüssiger Blöcke bei Shrinks und Tail-Trim im Storage-Layer
- persistentes `FREE`-Segment mit Free-List-Metadaten und persistenter Allocator-Policy
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

### Plattform- und Integrationsmodell

- native Runtime-Integration als generisches Blueprint-Modell
- optionale Kompatibilitätsziele als Adapter-Konzept
- Tool-Registry für `mkfs`, `fsck` und Administration
- Tool-Registry für Benchmarking
- Linux-FUSE-Mountpfad für Image-basierte Integrationstests inklusive RW-Writeback und Dirty/Clean-Session-Markierung

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
- vollständige Copy-on-Write-Implementierung auf Datenträgerebene
- persistentes physisch device-blockadressiertes Write-Ahead-Log direkt im Volume statt des aktuellen extent-orientierten Pending-WAL
- Deduplizierung
- Self-Healing mit Redundanzquellen
- Cluster-Synchronisation
- Hot/Cold-Storage und Tiering-Strategien
- echte Kompression und echte Verschlüsselung
- Quota-Durchsetzung
- Time-Travel-Adressierung
- automatische Versionenbereinigung bei Platzdruck
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
- Versionierung
- Recovery
- Integrität
- Indexierung
- Sicherheit
- Synchronisationsstatus

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
- die persistente Free-List/Allocator-Schicht um staerkere Fragmentierungssteuerung, Reuse-Policies und spaeter echte Device-Segmentadressierung weiterentwickeln
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
- Der Linux-Mountpfad unterstützt inzwischen read-only und read-write; der RW-Pfad nutzt Dirty/Clean-Markierung, transaktionales Journal-Writeback, persistente physische Volume-Allokation, freie Extent-Wiederverwendung, ein persistentes `FREE`-Segment mit Allocator-Policy und ein integriertes extent- und device-blockadressiertes Pending-WAL im Volume, ist aber noch kein vollständiges produktionsnahes Device-WAL mit fortgeschrittener Fragmentierungssteuerung.
- Performance-Messungen werden jetzt über `benchmark` und `benchmark-log` reproduzierbar ausführbar.
- Die vorhandene Testsuite ist stark für die aktuelle In-Memory-Implementierung, aber keine Garantie für `100%` messbare Coverage, da in der Umgebung keine Coverage-Tools installiert sind.
- `.codex` ist inzwischen als projektinterne Vorgabedatei befüllt.
