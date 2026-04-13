# CoreFS Projektfortschritt

## Überblick

`CoreFS` ist ein in Rust entwickeltes, plattformneutrales Dateisystemprojekt mit Fokus auf den nativen Einsatz als Standard-Dateisystem des eigenen Betriebssystems. Das Repository enthält aktuell ein strukturiertes, kompilierbares Grundsystem mit klarer Modultrennung, CLI-Einstiegspunkt, Service-Schichten, Volume-Persistenzpfad, Integritätswerkzeugen, Linux-FUSE-Testadapter, Performance-Tooling und einer breiten Testsuite.

## Aktueller Status

**Projektphase:** Architektur-, Kern-, Persistenz-, Volume-Layout-, Replay-, Integritäts-, Linux-FUSE- und Performance-Prototyp  
**Build-Status:** stabil  
**Test-Status:** `294/294` Tests erfolgreich  
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
- Verschlüsselung ruhender Daten: `EncryptionService` mit ChaCha20-Poly1305 (AEAD), 256-Bit-Schlüssel, zufällige 12-Byte-Nonce pro Verschlüsselung; Pipeline: compress → encrypt → store; read → decrypt → decompress; `inode.metadata.encrypted` Flag pro Datei; Schlüssel-Ableitung für Tests via `derive_key_from()`; FUSE-Read-only-Mount unterstützt transparente Entschlüsselung
- expliziter Deduplizierungs-Pass: `BlockStore::dedup_pass()` mit 3-Phasen-Scan (ref_count-Audit, Hash-Kollisions-Erkennung, byte-identische Konsolidierung); `CoreFsService::run_dedup()` hinter `config.performance.deduplication_enabled` konfigurierbar
- erweiterte In-Memory-Konsistenzprüfung: `IntegrityService::deep_fsck()` validiert Katalog↔Block-Konsistenz, Checksum-Integrität, Entschlüsselungs- und Dekomprimierungs-Pipeline, `inode.size`-Abgleich, verwaiste Blöcke; `FsckReport` mit Detailkategorien (orphaned_blocks, missing_blocks, size_mismatches, compression_errors, encryption_errors, checksum_failures)
- Block-Device I/O Layer: `BlockDevice`-Trait mit sektorausgerichtetem `read_at`/`write_at`/`sync`/`trim`, Alignment-Enforcement, Bounds-Checking und Read-only-Protection; drei Implementierungen: `FileImageDevice` (dateibasiert), `RawBlockDevice` (Linux `/dev/sdX` mit `ioctl(BLKGETSIZE64)`/`BLKDISCARD`, sysfs-Probing), `MemoryDevice` (Test-Referenzimplementierung)
- Device-Safety: `probe_device()` mit `DeviceInfo`-Struct (Mount-Erkennung, Ganz-Disk-Erkennung, Read-only-Status, NVMe-Partitionserkennung); `is_safe_to_format()` und `format_blockers()` als Sicherheitsabfrage vor destruktiven Operationen
- Volume-Image-Persistenz auf Block-Devices: `save_to_device()` und `load_from_device()` serialisieren/deserialisieren den vollständigen CoreFS-Zustand sektorausgerichtet auf beliebige `BlockDevice`-Implementierungen; `build_volume_image_bytes()` für In-Memory-Serialisierung
- `DeviceVolumeSession`: Block-Device-basierte Volume-Session mit `format_new()`, `open()`, `flush()` und `mutate()` analog zur dateibasierten `VolumeSession`
- TRIM/Discard-Tracking im BlockStore: `FreedExtent`-Akkumulation bei `release_inode()` und Extent-Shrink; `drain_freed_extents()` für Weiterleitung an `BlockDevice::trim()`
- CLI-Kommandos für Block-Devices: `probe-device` (Sicherheitsanalyse), `mkfs-device` (Formatierung mit Safety-Checks), `mount-device-rw` (FUSE-RW-Mount von `/dev/sdX`)
- FUSE-Mount von Block-Devices: `mount_device_rw()` lädt Volume vom Device, dient über bestehende FUSE-RW-Infrastruktur, schreibt bei Unmount zurück auf das Device; `format_device()` formatiert ein Device mit leerem CoreFS-Volume
- On-Demand Sektor-I/O: `DeviceVolume` liest beim Öffnen nur Header und Segment-Directory (~400 Bytes) vom Device; individuelle Segmente werden bei Bedarf sektorausgerichtet geladen und im Read-Cache gehalten; Write-Buffer akkumuliert Änderungen pro Segment; `flush()` schreibt nur geänderte Segmente; `invalidate_cache()` erzwingt Device-Reads
- Device-Journal: `DeviceJournal` verwaltet eine reservierte 256-KiB-Region nach dem Volume-Image; `commit()` serialisiert `VolumeWal` mit Checksumme und `fdatasync()`-Barrier (Header → Payload → Sync); `clear()` markiert Journal als leer nach erfolgreichem Image-Update; Generation-Counter für Crash-Ordering; korrupte Journals werden bei `open()` erkannt und verworfen
- Fake-Stick-Erkennung: `sanity_check_writable()` probiert 6 verteilte Offsets (10/25/50/75/90/99% der Kapazität) mit deterministischen Testmustern, liest zurück und zero-fillt — läuft automatisch nach `mkfs-device` (überspringbar mit `--skip-check`); `verify_device_capacity()` führt destruktiven Vollscan mit konfigurierbarer Chunk-Anzahl durch, exponiert als `verify-device --destructive` CLI-Kommando mit `fake_ratio_percent`-Verdict
- Permission-Checks: `check_device_permissions()` prüft Root/Write-Access vor Device-Zugriff mit hilfreicher `sudo`-Fehlermeldung; eingebaut in `mkfs-device`, `mount-device-rw`, `verify-device`
- CLI-Integritätsprüfung auf Blockgeräten: `fsck-device <path>` via `inspect_device()` ohne Schreibzugriff (Magic, Format-Version, Superblock-Redundanz, Checksummen, Segmentvollständigkeit, Block-Deskriptoren)

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

### Testauswertung — Enterprise-Readiness (Stand 2026-04-13)

#### Testverteilung nach Modul (~257 Tests)

| Schicht | Modul | Tests | Schwerpunkte |
|---------|-------|------:|-------------|
| Storage | `block_device.rs` | ~60 | Sektoralignment, Memory-/File-Devices, TRIM, Read-only |
| Storage | `block_store.rs` | ~21 | CoW, Dedup, Defragmentierung, Hot-Path-Allokation |
| Storage | `volume_image.rs` | ~12 | Persistenzformat, Superblock, Segmenttabellen, Reparatur |
| Storage | `allocator.rs`, `catalog.rs`, `volume_wal.rs`, `volume_session.rs` | ~10 | WAL-Ops, Session-Lifecycle, Allokation |
| App | `mod.rs`, `tests.rs` | ~75 | Dateioperationen, Snapshots, Klonen, Verschlüsselung |
| Platform | `linux_fuse.rs` | ~27 | Read-/Write-Caching, Snapshot-Overlays, Time-Travel |
| Platform | `performance.rs`, `diagnostics.rs`, `runtime.rs`, `tools.rs` | ~11 | Benchmark-Profile, Diagnostik |
| Services | `encryption.rs`, `compression.rs`, `security.rs` | ~10 | ChaCha20, LZ4, Tamper-Detection |
| Services | `integrity.rs`, `recovery.rs`, `journal.rs` | ~12 | Scrubbing, fsck, Journal-Replay, Crash-Recovery |
| Services | `versioning.rs`, `metadata.rs`, `quota.rs` | ~7 | Versionsbereinigung, Quota-Enforcement |
| Services | `hot_paths.rs`, `sync.rs`, `semantic.rs`, `indexing.rs` | ~4 | je 1 Basistest |
| Domain | `inode.rs`, `acl.rs`, `metadata.rs`, `volume.rs` | ~4 | je 1 Basistest |
| CLI/Config | `cli.rs`, `config.rs`, `error.rs` | ~7 | Kommandozeile, Konfiguration |

#### Gut abgedeckte Bereiche

- **Copy-on-Write & Blob-Sharing** (~9 Tests): Ref-Counting, Klonen, Materialisierung, Sharing-Statistiken
- **Snapshot-Lifecycle** (~15 Tests): Capture, Restore, Scoped Snapshots, Diff, Pinning, Encryption-Transparenz
- **Encryption-Pipeline** (~6 Tests): Roundtrip, Wrong-Key-Rejection, Tamper-Detection
- **Integritäts- & Recovery-Pfade** (~12 Tests): Scrubbing, fsck, Image-Reparatur, Journal-Reconciliation, Unclean-Recovery
- **Block-Device-Abstraktion** (~60 Tests): Alignment, Boundary-Checks, Memory-/File-Devices, TRIM
- **FUSE-Integration** (~27 Tests): Caching, Handle-Lifecycle, Virtual Overlays, Time-Travel

#### Identifizierte Lücken für Enterprise-Level

**P0 — Concurrency & Thread-Safety (0 Tests)**
- Kein einziger Multi-Thread-Test vorhanden
- Fehlend: parallele Schreibzugriffe, gleichzeitige Snapshot-Erstellung während Writes, CoW-Materialisierung unter Contention, Ref-Count-Races, Lock-Ordering / Deadlock-Erkennung
- Begründung: für ein Dateisystem die kritischste Lücke — Race Conditions können Datenverlust verursachen

**P0 — Fault Injection (0 Tests)**
- Fehlend: ENOSPC-Recovery (Platte voll während Write/Journal-Commit/Snapshot), partielle I/O-Fehler, Bit-Rot/Silent-Corruption-Erkennung, Journal-Korruption (abgebrochener WAL-Eintrag), Superblock-Verlust mit Fallback-Validierung, Power-Loss-Simulation (Write-Abbruch an zufälligen Stellen)
- Begründung: Enterprise bedeutet Überleben defekter Hardware und voller Platten

**P0 — Stress & Skalierung (0 Tests)**
- Fehlend: 10'000+ Dateien pro Verzeichnis (Katalog-Performance), 100+ MB Writes (Extent-Allokation), tiefe Verzeichnisbäume (500+ Ebenen), Langläufer (Sustained Writes über Minuten, Memory-Leak-Erkennung), 100+ Snapshot-Akkumulation, Clone-Kaskaden (Datei → Clone → Clone → Write)
- Begründung: aktuelle Tests arbeiten ausschliesslich mit kleinen Datenmengen

**P1 — Performance-Regression-Gate (Infrastruktur vorhanden, keine Assertions)**
- Benchmark-Profile existieren (Balanced, SmallFiles, MetadataHeavy, SnapshotHeavy, PersistHeavy), aber keine automatische Regressionserkennung
- Fehlend: Baseline-Capture pro Profil, Threshold-Assertions (Fail bei >15% Regression), Latenz-Histogramme (P50/P95/P99 statt nur Durchschnitt), CI-Integration
- Begründung: Tail-Latency ist Enterprise-kritisch; Regressionen müssen automatisch erkannt werden

**P1 — Crash-Recovery-Roundtrips (teilweise abgedeckt)**
- Fehlend: vollständige Roundtrips (Daten schreiben → Image-Save abrupt abbrechen → neu laden → fsck → Konsistenzprüfung), automatischer `deep_check` nach jedem Stress-Test, Orphaned-Block-Audit nach vielen Deletes/Clones/Snapshots, Ref-Count-Konsistenzprüfung (Σ ref_counts == tatsächliche Blob-Referenzen)
- Begründung: Crash-Recovery ist in Isolation getestet, aber nicht als End-to-End-Szenario unter Last

**P2 — Encryption + Compression unter Last**
- Fehlend: 1000+ Dateien verschlüsselt+komprimiert schreiben/lesen/verifizieren, Schlüsselwechsel (Re-Encryption aller Blöcke), gezielte Tamper-Detection unter Scale
- Begründung: Pipeline-Korrektheit unter Volumen sicherstellen

**P2 — Property-Based Testing / Deterministische Reproduzierbarkeit**
- Fehlend: Seed-basierte Randomisierung für Stress-Tests, Property-Based Tests (proptest/quickcheck) z.B. „beliebige Sequenz von create/write/delete/snapshot → fsck ist immer clean"
- Begründung: systematische Abdeckung von Zustandskombinationen, die manuell geschriebene Tests nicht erfassen

#### Empfohlene Test-Roadmap

| Prio | Kategorie | Umfang | Ziel |
|------|-----------|--------|------|
| P0 | Concurrency-Tests | ~15–20 Tests | Thread-Safety aller mutierbaren Pfade validieren |
| P0 | Fault-Injection-Framework | ~15 Tests | I/O-Fehler, Disk-Full, Korruption überleben |
| P0 | Stress- & Skalierungstests | ~10 Tests | Verhalten bei Enterprise-typischen Datenmengen |
| P1 | Performance-Regression-Gate | ~5 Tests + CI | Automatische Erkennung von Latenz-/Throughput-Regressionen |
| P1 | Crash-Recovery-Roundtrips | ~8 Tests | End-to-End-Konsistenz nach simulierten Abstürzen |
| P2 | Encryption-Pipeline-Stress | ~5 Tests | Korrektheit der Encrypt+Compress-Pipeline unter Volumen |
| P2 | Property-Based Tests | ~5 Generatoren | Zustandsinvarianten über zufällige Op-Sequenzen |

## Noch nicht umgesetzt

Diese Punkte sind konzeptionell vorgesehen oder im Anforderungskatalog enthalten, aber noch nicht als vollständige reale Implementierung vorhanden:

- produktionsnahes blockorientiertes On-Disk-Format
- vollständig segmentgranulares On-Demand I/O für FUSE-Mount (aktuell: `DeviceVolume` für Segment-Level-Zugriff vorhanden; FUSE-Mount lädt noch komplett in RAM und schreibt bei Unmount zurück)
- persistentes physisch device-blockadressiertes Write-Ahead-Log direkt im Volume statt des aktuellen extent-orientierten Pending-WAL
- Self-Healing mit Redundanzquellen
- Cluster-Synchronisation
- Hot/Cold-Storage und Tiering-Strategien
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
- Blockspeicher im In-Memory-Modell mit TRIM/Discard-Tracking (`FreedExtent`)
- Katalog für aktive und gelöschte Einträge
- mehrsegmentiges binäres Volume-Image-Format mit Segmenttabelle, Alignment-Regeln, redundanten Superblocks, Generation Countern, binären Segment-Frames und Prüflogik als weiterer Persistenzpfad
- `BlockDevice`-Abstraktion mit `FileImageDevice`, `RawBlockDevice` (Linux) und `MemoryDevice`
- `DeviceVolumeSession` für Block-Device-basierte Volume-Sitzungen
- `DeviceVolume` für On-Demand-Segment-I/O mit Read-Cache und Write-Buffer
- `DeviceJournal` für geräteresidentes Write-Ahead-Log mit Barrier-Semantik

### `src/services`

- Journaling
- Versionierung mit konfigurierbarer Byte-Budget-Bereinigung (`prune_to_budget`)
- Recovery
- Integrität
- Indexierung
- Sicherheit
- Synchronisationsstatus
- Kompression (LZ4 frame via `lz4_flex`)
- Verschlüsselung (ChaCha20-Poly1305 via `chacha20poly1305`)
- Quota-Enforcement
- Copy-on-Write mit Blob-Referenz-Zählung, CoW-Klons und Snapshot-Pinning
- Deduplizierung (aktiver Scan-Pass mit ref_count-Audit und Konsolidierung)

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

### Phase 1b: Block-Device I/O (USB-Stick, Partition, Raw Device)

Ziel: CoreFS direkt auf einem Blockgerät (`/dev/sdX1`) formatieren und via FUSE mounten — ohne Umweg über eine `.img`-Datei auf einem Fremddateisystem.

- ✅ **`BlockDevice`-Trait**: `read_at`/`write_at`/`sync`/`trim` mit Alignment-Enforcement, Bounds-Checking und Read-only-Protection; drei Implementierungen: `FileImageDevice`, `RawBlockDevice` (Linux), `MemoryDevice` (Test)
- ✅ **`mkfs-device /dev/sdX1`**: CLI-Kommando mit `probe_device()`-Safety-Checks (Mount-Erkennung, Ganz-Disk-Warnung, Read-only-Prüfung)
- ✅ **FUSE-Mount auf Blockgerät**: `mount-device-rw /dev/sdX1 /mnt` mit Load-from-Device, FUSE-RW-Session, Write-back-to-Device bei Unmount
- ✅ **TRIM/Discard-Tracking**: `FreedExtent`-Akkumulation im BlockStore bei `release_inode()` und Extent-Shrink; `drain_freed_extents()` für `BlockDevice::trim()`-Weiterleitung; `RawBlockDevice` implementiert `ioctl(BLKDISCARD)` mit automatischem Fallback bei `EOPNOTSUPP`
- ✅ **Device-Safety**: `probe_device()` → `DeviceInfo` (sysfs-basiert: Sektorgrössen, R/O-Status, NVMe-Erkennung, `/proc/mounts`-Abgleich); `is_safe_to_format()` / `format_blockers()`
- ✅ **`DeviceVolumeSession`**: Block-Device-basierte Session mit `format_new()`, `open()`, `flush()`, `mutate()` — analog zur dateibasierten `VolumeSession`
- ✅ **On-Demand Sektor-I/O**: `DeviceVolume` liest nur Header+Directory (~400 Bytes) beim Öffnen; individuelle Segmente werden bei Bedarf vom Device geladen und im Read-Cache gehalten; Write-Buffer puffert Änderungen pro Segment; `flush()` schreibt nur geänderte Segmente zurück
- ✅ **Device-Journal**: `DeviceJournal` reserviert 256 KiB nach dem Volume-Image für WAL-Entries; `commit()` schreibt Header + serialisierte `VolumeWal` mit Checksumme und `fdatasync()`-Barrier; `clear()` nach erfolgreichem Image-Update; Generation-Counter für Ordering; Crash-Recovery liest Journal und replayed committed Ops

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
