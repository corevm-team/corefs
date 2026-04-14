# CoreFS Projektfortschritt

## Überblick

`CoreFS` ist ein in Rust entwickeltes, plattformneutrales Dateisystemprojekt mit Fokus auf den nativen Einsatz als Standard-Dateisystem des eigenen Betriebssystems. Das Repository enthält aktuell ein strukturiertes, kompilierbares Grundsystem mit klarer Modultrennung, CLI-Einstiegspunkt, Service-Schichten, Volume-Persistenzpfad, Integritätswerkzeugen, Linux-FUSE-Testadapter, Performance-Tooling und einer breiten Testsuite.

## Aktueller Status

**Projektphase:** Architektur-, Kern-, Persistenz-, Volume-Layout-, Replay-, Integritäts-, Linux-FUSE- und Performance-Prototyp  
**Build-Status:** stabil  
**Test-Status:** `636/636` Tests erfolgreich  
**Ausrichtung:** plattformneutral, nicht Linux-zentriert

## Bereits umgesetzt

**Legende für Status-Markierungen (dokumentweit)**

- `- [x]` abgeschlossen
- `- [~]` in Arbeit / teilweise umgesetzt
- `- [ ]` offen / nicht umgesetzt

In beschreibenden Abschnitten (z. B. Architekturüberblick) markieren Top-Level-Bullets den Umsetzungsstand der Komponente; eingerückte Sub-Bullets beschreiben die Struktur und tragen keine eigene Statusmarkierung.

### Projektstruktur

- [x] Rust-Projekt mit `lib`- und `bin`-Einstieg
- [x] klare Schichtung in `app`, `domain`, `storage`, `services`, `platform`
- [x] zentrale Fassade über `CoreFsService`
- [x] CLI-Kommandos für Grundfunktionen
- [x] CLI-Kommandos für Image-Persistenz und Wartung
- [x] CLI-Kommandos für Benchmarking und Performance-Logging

### Domänenmodell

- [x] Inodes
- [x] ACL-Einträge und Principals
- [x] Dateimetadaten
- [x] Snapshots
- [x] Volume-Deskriptoren

### Kernfunktionen im Prototyp

- [x] Formatierung eines CoreFS-Volumes im Userspace-Modell
- [x] Persistenz eines mehrsegmentigen binären CoreFS-Volume-Images mit Segmenttabelle, redundanten Superblocks, Generation Countern, Clean/Unclean-Markierung und binären Segment-Frames für Fachsegmente
- [x] spezialisierte Binärlayouts für Inode-, Journal- und Snapshot-Segmente innerhalb des Volume-Images
- [x] Linux-Testadapter über FUSE zum read-only und read-write Mounten von `.img`-Dateien
- [x] Linux-FUSE-Read-/Write-Caching auf File-Handle-Ebene mit Write-Back-Flush und Read-Serving aus dem Open-Handle-Cache
- [x] backing-store-aware `statfs`-Kapazitaetsmeldung fuer Linux-FUSE und sauberere `ENOSPC`-Rueckgabe bei Platzmangel im `.img`-Persistenzpfad
- [x] treibernahe Linux-FUSE-Tests fuer Open/Truncate, Read-Cache, Write-Back-Flush, Release, Persistenzverhalten, Snapshot-Overlays und Time-Travel-Parsing
- [x] Fix fuer neu angelegte Dateien im Linux-FUSE-RW-Pfad: `create` liefert jetzt sofort einen gueltigen Write-Back-Handle fuer nachfolgende Schreibzugriffe
- [x] Linux-End-to-End-Testskript fuer `mkfs-image`, RW-Mount, Shell-Dateioperationen, optionalen `unzip`-Workload, Umount und Read-only-Revalidierung
- [x] Erzeugen von Dateien, Verzeichnissen und symbolischen Links
- [x] Lesen und Schreiben von Dateiinhalten
- [x] Journaling von Operationen
- [x] transaktionales Journal mit Pending-Transaktionen, Commit-/Abort-Markern und Recovery-Einträgen
- [x] persistente physische Volume-Allokation pro Dateiinhalt mit stabilen `device_block`-/`allocated_blocks`-Metadaten
- [x] freie Extent-Wiederverwendung mit Gap-Reuse, Freigabe überschüssiger Blöcke bei Shrinks und Tail-Trim im Storage-Layer
- [x] persistentes `FREE`-Segment mit Free-List-Metadaten und persistenter Allocator-Policy
- [x] aktive Defragmentierung/Compaction fuer belegte Extents inklusive Service- und CLI-Pfad
- [x] Fragmentierungsmetriken, persistente Auto-Compaction-Policy und explizite Optimierungspfade fuer Live-Instanzen und Volume-Images
- [x] gezielte Heat-aware Extent-Reallocation auf Basis persistierter Hot-Path-Telemetrie fuer priorisierte Platzierung haeufig genutzter Inodes
- [x] integriertes Pending-WAL im Volume-Image fuer RW-Sessions
- [x] extent- und device-blockadressierte WAL-Records ueber `inode + device_block + block_offset + inode_offset` fuer partielle File-Patches und Truncates
- [x] automatische Versionierung im Basismodell
- [x] Snapshot-Erzeugung mit historischer Metadaten-Erfassung (Kind, Groesse, Timestamps, Mode/UID/GID, ACLs, Tags, xattrs, Symlink-Ziele) pro Pfad, Accessor `snapshot_inode` und Metadaten-getreuer `restore_snapshot` (inkl. Wiederherstellung fehlender Verzeichnisse und Symlinks)
- [x] Recoverable Delete und Secure Delete
- [x] einfache Integritätsprüfung per Checksummen
- [x] Scrubbing über vorhandene Datenblöcke
- [x] `fsck-image` für strukturelle Prüfungen von Volume-Images
- [x] erste mehrstufige Image-Reparatur aus verbliebener gültiger Kopie oder per Header-/Segmenttabellen-Fallback mit Superblock-Wiederaufbau, Rekonstruktion beschädigter Segmentverzeichnisse, Rekonstruktion defekter Blockdeskriptoren, Journal-Abgleich und Bereinigung verwaister Blockdaten
- [x] Sync-Status-Verfolgung
- [x] semantische Inhaltsklassifikation nach Dateiendung
- [x] Metadaten-, Tag- und ACL-Grundmodell
- [x] Journal-Replay zur Zustandsabstimmung geladener Images
- [x] Recovery eines unclean beendeten RW-Mounts mit automatischem Abbruch offener Pending-Transaktionen beim Laden
- [x] WAL-Recovery vor FUSE-Mount und `VolumeSession::open`, damit persistierte Pending-Operationen direkt aus dem Volume-Image ins Haupt-Image zurueckgespielt werden
- [x] synthetischer Performance-Benchmark für Datei-, Snapshot- und Persistenzpfade
- [x] Markdown-Protokollierung von Benchmark-Ergebnissen
- [x] konfigurierbare Benchmark-Profile für unterschiedliche Lastbilder
- [x] Streaming-Writes im Linux-FUSE-RW-Mount: sequenzielle Schreibzugriffe ab 32 MiB werden als Zwischenflushes an den Service delegiert statt vollständig im RAM gebuffert, wodurch der Peak-Speicherbedarf auf `O(Threshold)` statt `O(Dateigrösse)` begrenzt wird
- [x] FUSE-Schreibdurchsatz-Optimierungen: `FUSE_WRITEBACK_CACHE` (Kernel-seitiges Schreib-Batching) und `max_write = 1 MiB` (weniger Kernel↔Daemon-Roundtrips), O(n²)-Klon-Bug in `write_to_handle` behoben (vorher: `node.data.clone()` bei jedem Write-Call)
- [x] WAL-Vereinfachung für Dateidaten: `PatchExtent`-Records werden nicht mehr pro Flush geschrieben, da der atomare Image-Save (write→rename) hinreichende Crash-Safety bietet; strukturelle Ops (CreateFile, TruncateInode etc.) bleiben vollständig WAL-geschützt
- [x] inkrementelle Prüfsummenfortschreibung in `BlockStore::append_to_inode`: `O(extra.len())` statt `O(gesamt)`, Rekey der Blob-Map nach Checksum-Änderung
- [x] transparente LZ4-Kompression für Dateiinhalte: `CompressionService` (lz4_flex frame format) komprimiert Payloads ≥ 64 B bei Schreibzugriffen automatisch; `read_file` dekomprimiert transparent; `inode.size` enthält immer die logische (unkomprimierte) Größe; Versionen werden vor der Kompression gespeichert, sodass die Versionshistorie stets vollständig lesbar bleibt
- [x] Quota-Enforcement in `create_file` und `write_file`: schnelle Überprüfung über `Catalog::quota_stats()` (liefert `(file_count, total_bytes)` ohne Klon aller Inodes) und `QuotaService::check_stats()`; bei Überschreitung wird `CoreFsError::QuotaExceeded` zurückgegeben, bevor Daten geschrieben werden
- [x] automatische Versionsbereinigung unter Platzdruck: `VersioningService::prune_to_budget(max_bytes)` entfernt global die ältesten Versionen solange das Gesamtvolumen der versionierten Bytes den konfigurierbaren `max_version_bytes`-Wert (Standard: 64 MiB) überschreitet
- [x] Copy-on-Write auf Storage-Ebene: `BlockStore` implementiert vollständiges CoW mit Referenz-Zählung auf Blob-Ebene; `clone_for_inode()` teilt einen Blob zwischen zwei Inodes (ref_count++); der nächste Schreibzugriff auf einen der beiden Inodes materialisiert eine unabhängige Kopie; `cow_stats()` liefert ein Sharing-Report; `CowStats` (shared_blobs, exclusive_blobs, bytes_saved_by_sharing, max_ref_count)
- [x] Snapshot-Block-Pinning: `Snapshot.file_data` speichert die unkomprimierten Bytes aller regulären Dateien zum Snapshot-Zeitpunkt; Snapshots sind dadurch vollständig selbständig und unabhängig von späteren Block-Mutationen
- [x] Snapshot-Lifecycle-Management: `delete_snapshot(id)` entfernt Snapshots; `restore_snapshot(id)` schreibt alle Dateien aus `file_data` zurück (überschreibt vorhandene, legt gelöschte neu an, meldet Fehler pro Pfad statt abzubrechen)
- [x] CoW-Klon-Semantik: `clone_file(from, to)` erstellt einen CoW-Klon — teilt sofort den Blob, divergiert erst beim nächsten Schreibzugriff; `expunge_file(path)` löscht soft-gelöschte Dateien permanent und dekrementiert den Blob-ref_count korrekt
- [x] Bug-Fix in `BlockStore::append_to_inode` (shared path): doppeltes ref_count-Dekrement verhindert — `write()` dekrementiert bereits beim BlockEntry-Remove; das manuelle Dekrement davor würde den Blob auf 0 setzen während andere Inodes ihn noch referenzieren
- [x] Config-Enforcement: `clone_file()` prüft `config.performance.copy_on_write`; bei deaktiviertem CoW wird eine vollständige Kopie (read+create) statt Blob-Sharing durchgeführt
- [x] rekursives Verzeichnis-Klonen: `clone_tree(from, to)` klont einen Teilbaum mit CoW für Dateien, Verzeichnis-Erzeugung und Symlink-Neuanlage; liefert `CloneTreeReport` mit Zählern und Fehler-pro-Pfad
- [x] Scoped Snapshots: `create_snapshot_scoped(name, scope_root)` erfasst nur Pfade unter `scope_root`; `create_snapshot(name)` delegiert auf `scope_root="/"`
- [x] Snapshot-Diff: `diff_snapshots(a_id, b_id)` klassifiziert Dateien als added/removed/modified/unchanged zwischen zwei Snapshots
- [x] FUSE `copy_file_range`: serverseitige Kopie zwischen zwei offenen File-Handles ohne Kernel↔Userspace-Roundtrips; liest aus Source-Handle (oder virtuellem Snapshot-Node), schreibt in Destination-Handle; EROFS für virtuelle Destinations
- [x] Verschlüsselung ruhender Daten: `EncryptionService` mit ChaCha20-Poly1305 (AEAD), 256-Bit-Schlüssel, zufällige 12-Byte-Nonce pro Verschlüsselung; Pipeline: compress → encrypt → store; read → decrypt → decompress; `inode.metadata.encrypted` Flag pro Datei; Schlüssel-Ableitung für Tests via `derive_key_from()`; FUSE-Read-only-Mount unterstützt transparente Entschlüsselung
- [x] expliziter Deduplizierungs-Pass: `BlockStore::dedup_pass()` mit 3-Phasen-Scan (ref_count-Audit, Hash-Kollisions-Erkennung, byte-identische Konsolidierung); `CoreFsService::run_dedup()` hinter `config.performance.deduplication_enabled` konfigurierbar
- [x] erweiterte In-Memory-Konsistenzprüfung: `IntegrityService::deep_fsck()` validiert Katalog↔Block-Konsistenz, Checksum-Integrität, Entschlüsselungs- und Dekomprimierungs-Pipeline, `inode.size`-Abgleich, verwaiste Blöcke; `FsckReport` mit Detailkategorien (orphaned_blocks, missing_blocks, size_mismatches, compression_errors, encryption_errors, checksum_failures)
- [x] Block-Device I/O Layer: `BlockDevice`-Trait mit sektorausgerichtetem `read_at`/`write_at`/`sync`/`trim`, Alignment-Enforcement, Bounds-Checking und Read-only-Protection; drei Implementierungen: `FileImageDevice` (dateibasiert), `RawBlockDevice` (Linux `/dev/sdX` mit `ioctl(BLKGETSIZE64)`/`BLKDISCARD`, sysfs-Probing), `MemoryDevice` (Test-Referenzimplementierung)
- [x] Device-Safety: `probe_device()` mit `DeviceInfo`-Struct (Mount-Erkennung, Ganz-Disk-Erkennung, Read-only-Status, NVMe-Partitionserkennung); `is_safe_to_format()` und `format_blockers()` als Sicherheitsabfrage vor destruktiven Operationen
- [x] Volume-Image-Persistenz auf Block-Devices: `save_to_device()` und `load_from_device()` serialisieren/deserialisieren den vollständigen CoreFS-Zustand sektorausgerichtet auf beliebige `BlockDevice`-Implementierungen; `build_volume_image_bytes()` für In-Memory-Serialisierung
- [x] `DeviceVolumeSession`: Block-Device-basierte Volume-Session mit `format_new()`, `open()`, `flush()` und `mutate()` analog zur dateibasierten `VolumeSession`
- [x] TRIM/Discard-Tracking im BlockStore: `FreedExtent`-Akkumulation bei `release_inode()` und Extent-Shrink; `drain_freed_extents()` für Weiterleitung an `BlockDevice::trim()`
- [x] CLI-Kommandos für Block-Devices: `probe-device` (Sicherheitsanalyse), `mkfs-device` (Formatierung mit Safety-Checks), `mount-device-rw` (FUSE-RW-Mount von `/dev/sdX`)
- [x] FUSE-Mount von Block-Devices: `mount_device_rw()` lädt Volume vom Device, dient über bestehende FUSE-RW-Infrastruktur, schreibt bei Unmount zurück auf das Device; `format_device()` formatiert ein Device mit leerem CoreFS-Volume
- [x] On-Demand Sektor-I/O: `DeviceVolume` liest beim Öffnen nur Header und Segment-Directory (~400 Bytes) vom Device; individuelle Segmente werden bei Bedarf sektorausgerichtet geladen und im Read-Cache gehalten; Write-Buffer akkumuliert Änderungen pro Segment; `flush()` schreibt nur geänderte Segmente; `invalidate_cache()` erzwingt Device-Reads
- [x] Device-Journal: `DeviceJournal` verwaltet eine reservierte 256-KiB-Region nach dem Volume-Image; `commit()` serialisiert `VolumeWal` mit Checksumme und `fdatasync()`-Barrier (Header → Payload → Sync); `clear()` markiert Journal als leer nach erfolgreichem Image-Update; Generation-Counter für Crash-Ordering; korrupte Journals werden bei `open()` erkannt und verworfen
- [x] Fake-Stick-Erkennung: `sanity_check_writable()` probiert 6 verteilte Offsets (10/25/50/75/90/99% der Kapazität) mit deterministischen Testmustern, liest zurück und zero-fillt — läuft automatisch nach `mkfs-device` (überspringbar mit `--skip-check`); `verify_device_capacity()` führt destruktiven Vollscan mit konfigurierbarer Chunk-Anzahl durch, exponiert als `verify-device --destructive` CLI-Kommando mit `fake_ratio_percent`-Verdict
- [x] Permission-Checks: `check_device_permissions()` prüft Root/Write-Access vor Device-Zugriff mit hilfreicher `sudo`-Fehlermeldung; eingebaut in `mkfs-device`, `mount-device-rw`, `verify-device`
- [x] CLI-Integritätsprüfung auf Blockgeräten: `fsck-device <path>` via `inspect_device()` ohne Schreibzugriff (Magic, Format-Version, Superblock-Redundanz, Checksummen, Segmentvollständigkeit, Block-Deskriptoren)
- [x] POSIX-Besitzer und -Berechtigungen: `FileMetadata.uid`, `.gid`, `.mode` pro Inode persistent gespeichert; `CoreFsService::set_owner()` und `set_mode()` API; FUSE `setattr` handled `chown`/`chmod` korrekt für Dateien und Verzeichnisse; `create`/`mkdir` übernehmen UID/GID/Mode aus der FUSE-Request (umask-respektierend)
- [x] Dreistufige POSIX-Zeitstempel pro Inode: `created_at` (crtime, immutable), `modified_at` (mtime, bei Inhalts-Änderungen), `changed_at` (ctime, bei beliebigen Inode-Änderungen); `Inode::touch_modified()` bumpt mtime+ctime, `touch_changed()` bumpt nur ctime; FUSE `attr()` liefert alle drei Zeitstempel korrekt; Format-Version auf 6 gebumpt
- [x] Inkrementelle Device-Persistenz: `persist_to_device_incremental()` mit `DeviceImageCache` schreibt nur geänderte Segmente, wenn das Image-Layout stabil bleibt (Segment-Grössen unverändert); Fallback auf Full-Rewrite bei Size-Changes; Read-Modify-Write für partial-sector Segmente; `PersistReport` liefert `incremental`/`segments_written`/`bytes_written` für Telemetrie; FUSE RW-Device-Mount nutzt diesen Pfad automatisch (`persist()`), was `chown`/`chmod`-Kaskaden von Sekunden auf Millisekunden reduziert (nur AINO + Superblocks statt komplettes Image)

### Plattform- und Integrationsmodell

- [x] native Runtime-Integration als generisches Blueprint-Modell
- [x] optionale Kompatibilitätsziele als Adapter-Konzept
- [x] Tool-Registry für `mkfs`, `fsck` und Administration
- [x] Tool-Registry für Benchmarking
- [x] Linux-FUSE-Mountpfad für Image-basierte Integrationstests inklusive RW-Writeback und Dirty/Clean-Session-Markierung
- [x] virtuelle Read-only-Overlays im Linux-FUSE-RW-Mount: `.snapshots/<id>-<name>/` für Snapshot-Browsing und `file@<spec>` für Time-Travel-Adressierung

### Qualitätssicherung

- [x] breite Unit-Test-Abdeckung über App-, CLI-, Service-, Storage-, Platform- und Domain-Schichten
- [x] Regression im Recovery-/Delete-Pfad bereits gefunden und behoben
- [x] Persistenz-Roundtrip und Ladefehler sind testseitig abgesichert
- [x] Benchmark-Ausführung und Markdown-Logging sind testseitig abgesichert
- [x] redundante Superblock-Fallbacks, Generation-Counter-Selektion, `fsck-image`, Image-Reparatur, Header-Directory-Recovery, Rekonstruktion beschädigter Segmentverzeichnisse, Rekonstruktion defekter Blockdeskriptoren, Journal-Replay, Dirty/Clean-Recovery und Bereinigung verwaister Blockdaten sind testseitig abgesichert
- [x] `cargo test` aktuell vollständig erfolgreich

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

Diese Punkte sind konzeptionell vorgesehen oder im Anforderungskatalog enthalten, aber noch nicht als vollständige reale Implementierung vorhanden. Statusmarkierungen: `[ ]` offen · `[~]` teilweise · `[x]` inzwischen erledigt (Eintrag verbleibt aus historischen Gründen).

- [~] vollständig segmentgranulares On-Demand I/O für FUSE-Mount (aktuell: `DeviceVolume` für Segment-Level-Zugriff vorhanden; FUSE-Mount lädt noch komplett in RAM und schreibt bei Unmount zurück)
- [ ] persistentes physisch device-blockadressiertes Write-Ahead-Log direkt im Volume statt des aktuellen extent-orientierten Pending-WAL
- [ ] Self-Healing mit Redundanzquellen (Self-Healing-Scrubber in ODF vorhanden, aber ohne echte Daten-Redundanz in ODF v1)
- [ ] Cluster-Synchronisation
- [x] Hot/Cold-Storage und Tiering-Strategien (via `storage::ondisk::tiering` mit `TieredDevice`, `HotnessTracker`, `Migrator::rebalance`)
- [x] echtes Copy-on-Write auf Datenträgerebene (physisches CoW via `storage::ondisk::refcount::RefCountTable` + `BlockSharing`, gated durch `FEATURE_INCOMPAT_PHYSICAL_COW`)
- [~] Time-Travel-Adressierung im FUSE-RW-Mount über `@`-Syntax ist für Lookup und Read umgesetzt; fehlt noch: Adressierung im Read-only-Mount, persistente Zugriffspfade als reale Symlinks
- [~] fsck als weiter auszubauendes Reparatur- und Korrekturwerkzeug für stärker beschädigte Segmenttabellen, tiefere Blockdeskriptor-Rekonstruktion, Datensegment-Validierung und echte Datenheilung
- [ ] native Kernel-/VFS-Integration für das eigene Betriebssystem (Gegenstand von Phase 5: AnyOS-Integration)
- [ ] Fremdsystem-Adapter als reale Laufzeitkomponenten

## Architekturüberblick

### `src/app`

- [x] Orchestrierung der Hauptlogik
- [x] zentrale Fassade für Dateioperationen, Snapshots, Recovery, Scrubbing und Reports

### `src/domain`

- [x] fachliche Grundtypen wie `Inode`, `Snapshot`, `FileMetadata`, `AclEntry`, `VolumeDescriptor`

### `src/storage`

- [x] Inode-Allokation
- [x] Blockspeicher im In-Memory-Modell mit TRIM/Discard-Tracking (`FreedExtent`)
- [x] Katalog für aktive und gelöschte Einträge
- [x] mehrsegmentiges binäres Volume-Image-Format mit Segmenttabelle, Alignment-Regeln, redundanten Superblocks, Generation Countern, binären Segment-Frames und Prüflogik als weiterer Persistenzpfad
- [x] `BlockDevice`-Abstraktion mit `FileImageDevice`, `RawBlockDevice` (Linux) und `MemoryDevice`
- [x] `DeviceVolumeSession` für Block-Device-basierte Volume-Sitzungen
- [x] `DeviceVolume` für On-Demand-Segment-I/O mit Read-Cache und Write-Buffer
- [x] `DeviceJournal` für geräteresidentes Write-Ahead-Log mit Barrier-Semantik
- [x] `storage::ondisk` — produktionsreifes blockorientiertes On-Disk-Format (ODF v1) als vollständige Enterprise-Architektur:
  - **Basisschicht**: fixe 4-KiB-Blöcke, strukturierter Superblock (Magic, UUID, Label, Versions- und Feature-Flags, Generation-Counter, Clean/Dirty-State, Block-/Inode-Bitmap-CRCs, Layout-Mode, Root-Inode-Pointer), dreifach-redundante Superblock-Kopien bei Block 1, N/2 und N-1, dedizierter Block- und Inode-Bitmap mit CRC32C-Schutz im Superblock, CRC32C-Checksummen auf jedem Control-Block, automatischer Fallback auf Backup-Superblocks bei Korruption
  - **Allocator** (`allocator.rs`): echter First-Fit- und Best-Fit-Extent-Allocator über der Bitmap, kontinuierliche und fragmentierte Extent-Allokation, Inode-Slot-Allokation mit reserviertem System-Slot-Floor, Roll-Back bei Fehlschlägen
  - **Journal** (`journal.rs`): transaktionales Write-Ahead-Log mit eigenem Header-Block, Record-Format (magic, kind, txn_id, seq, CRC32C je Record), Op-Record-Typen (BlockWrite, InodeUpdate), commit/abort/replay-Semantik, partielle Transaktionen werden beim Replay verworfen, Checkpoint bereinigt Head/Tail, synchronisiertes Commit zwischen Op-Records und Commit-Record
  - **Indirekte Extents** (`extent_tree.rs`): 4-KiB-Index-Blöcke mit bis zu 254 Extents plus next-Pointer, verkettete Chains für Dateien mit mehr als 8 Extents, Loop-Detection beim Walker
  - **Dir-Entry-Blöcke** (`dir_entry.rs`): 4-KiB-Directory-Blöcke mit magic, entry_count, next_dir_block-Pointer, 8-Byte-ausgerichtete variable Einträge (inode+kind+UTF-8-Name), `DirBlock::pack` teilt Listings auf verkettete Blöcke auf
  - **Attr-Blöcke** (`attr_block.rs`): CRC-geschützter 4-KiB-Block mit bincode-Payload (bis 4076 Bytes) je Inode
  - **Strukturierte xattr+ACL** (`xattr.rs`): 4-KiB-Block mit typisierten Key/Value-xattr-Einträgen und ACL-Records (User/Group/Everyone mit RWX-Permissions), format-neutral ohne bincode
  - **Native Per-Inode-Layout** (`native.rs`): jeder Domain-Inode bekommt einen eigenen ODF-Slot, Datei-Content in echten Extents, Per-Inode-Metadata im Sibling-AttrBlock, Ancillary-State (Snapshots, Versions, Sync, Hot-Paths, Journal-Entries, Allocator-Policy, Free-Extents) im dedizierten System-Slot #1, Soft-Delete über `FLAG_DELETED`, Propagierung von `FLAG_ENCRYPTED`/`FLAG_COMPRESSED`/`FLAG_HAS_XATTRS`, Per-Inode-`data_crc` als CRC32C über den Plain-Text-Inhalt
  - **fsck-Walker** (`fsck.rs`): vollständiger Read-Only-Konsistenz-Check mit Issue-Codes (ODF.SB.*, ODF.BBM.CRC, ODF.IBM.CRC, ODF.INODE.*, ODF.BLOCK.DOUBLE_ALLOCATED, ODF.JOURNAL.*), prüft Superblock-Redundanz+Generation, Bitmap-CRCs, free_blocks/free_inodes-Konsistenz, Inode-Record-Decode, Extent-Grenzen, Double-Allocation-Detection, Attr-Block-Pointer, Journal-Header
  - **Benchmark** (`benchmark.rs`): Micro-Benchmark `run_odf_bench()` misst format+save+load-Pipeline für Blob- und Native-Modus mit synthetischem State
  - **TRIM-Propagation**: freigegebene Payload-Extents werden beim `save_state` per `BlockDevice::trim` an das Gerät gemeldet
  - **Inkrementelle Native-Saves** (`native::save_state_native_incremental`): liest die aktuelle Inode-Bitmap und jedes belegte Slot (Domain-ID, data_crc, FLAG_DELETED, Extents, Attr-Block); klassifiziert jedes Inode als Created/Updated/Removed/Unchanged; freigegebene Extents+Attr-Blöcke der entfernten Inodes gehen zuerst zurück in den Allocator (Pass 1), Updates rewritten am gleichen Slot, Creates greifen auf die freie Slack zu, Unchanged bleiben unangetastet; Ancillary-Slot wird immer rewritten, Generation und Bitmap-CRCs werden gebumpt; Erstaufruf fällt automatisch auf vollen `save_state_native` zurück
  - **Block-Groups** (`block_group.rs`, `multi_group_allocator.rs`): On-Disk-`BlockGroupDescriptor` (data_start, data_blocks, bitmap_block, inode_range_start/count, free_blocks, bitmap_crc — 48 Bytes), CRC-geschützter `BlockGroupTable`-Block mit bis zu 84 Descriptors, Overlap-Detection beim Konstruieren; `MultiGroupAllocator` mit `allocate_near(count, inode_slot)` der das Home-Group des Inode-Slots bevorzugt und nur bei vollem Home auf Round-Robin durch die anderen Groups ausweicht, Free-Extent geht zurück in die korrekte Group, `refresh_descriptors()` aktualisiert Free-Counter und Bitmap-CRCs für persistente Tabelle; vorbereitet für `FEATURE_INCOMPAT_BLOCK_GROUPS`-Aktivierung in einer ODF-v2-Layout-Variante (single-group-Modus von ODF v1 bleibt Default)
  - **Journal-integrierter Save-Pfad** (`journaled.rs`): `JournaledSaveSession` stageroutet Metadata-Writes (Bitmap-Blöcke, Inode-Records, Superblock-Kopien) in eine einzelne Journal-Transaktion und appliziert sie via Replay erst nach einem durablen Commit-Record — "ordered mode" analog zu ext4/JBD2; Data-Writes (Payload-Content, Attr-Blöcke, Ancillary-Payload) gehen direkt aufs Device; Crash-Windows vollständig in der Modul-Doku dokumentiert; `recover_pending_transactions()` als Mount-Zeit-Helper der idempotent repliziert und checkpointet; im Test mit echter Crash-Simulation (`commit_without_apply` → Reopen → Replay) als korrekt verifiziert
  - **Aktivierte Block-Groups** (`grouped.rs`): `format_device_grouped` / `save_state_native_grouped` / `load_state_native_grouped` hinter `FEATURE_INCOMPAT_BLOCK_GROUPS`-Flag; Descriptor-Table an Block 2, Per-Group-Bitmap als erster Block jeder Gruppe im Data-Bereich, MultiGroupAllocator mit Home-Group-Lokalität plus Round-Robin-Spill, Per-Group-Bitmap-CRCs werden vor dem Persistieren aktualisiert und beim Laden cross-geprüft; Single-Group-Volumes (default) bleiben unberührt und koexistieren
  - **fsck::repair** (`fsck_repair.rs`): konsumiert `FsckReport` und repariert jedes auto-fixbare Issue als einzige journaled Transaktion — Rewrite veralteter/unlesbarer tertiärer + sekundärer Superblocks aus dem primären, Recompute korrupter Bitmap-CRC-Felder, Korrektur falscher `free_blocks`/`free_inodes`-Counter, Clear von Inode-Bitmap-Bits die auf Unused-Slots zeigen, Nachträgliche Bitmap-Allokation für verwaiste Extents/Attr-Blöcke; nicht-fixbare Issues (Double-Allocation, Out-of-Range-Extents, Record-Decode-Fehler) werden im `RepairReport.unfixable` zurückgegeben statt riskant angefasst; Idempotenz-Test verifiziert dass ein zweiter Repair-Lauf nichts mehr schreibt
  - **CLI-Integration**: `mkfs-odf`, `fsck-odf`, `inspect-odf`, `migrate-to-odf` (liest legacy `volume_image` und schreibt Native-ODF), `mount-odf` (Linux), `odf-session-demo`
  - **Session-Layer** (`session.rs`): `OdfFileSession` (file-backed) + `OdfDeviceSession` (BlockDevice-backed) als ODF-native Äquivalente von `VolumeSession`/`DeviceVolumeSession` — `format_new`/`open`/`open_or_format`/`flush`/`mutate`; Flush geht immer durch `save_state_native_incremental`, Open ruft `recover_pending_transactions` auf damit Crashes transparent repliziert werden; `FlushReport` exponiert created/updated/removed/unchanged je Mutation
  - **On-Demand-Reader** (`reader.rs`): `OdfReader` lädt beim `open` nur Superblock + Inode-Bitmap; `list_inodes()` walkt Slots mit einem Block-Read pro Eintrag; `slot_for_domain_id`/`slot_for_path` mit Memoisierung; `read_on_disk_inode`/`read_inode_metadata`/`read_file_content`/`read_file_by_path` für punktuellen Zugriff; `data_crc` wird beim Lesen cross-geprüft, tamper-detection on-the-fly
  - **Fault-Injection** (`fault_injection.rs`): `FaultyDevice<D>` wrapper mit konfigurierbaren Fehlerquellen — `fail_after_writes(N)`, `fail_after_bytes_written` (ENOSPC-Sim), `silent_corrupt_writes` (Bit-Rot), `fail_writes_in_range` (Bad-Sector), `fail_sync` (Power-Loss zwischen Write und Barrier), `stop_after_n_writes` (stiller Write-Drop); `FaultStats` als beobachtbare Counter
  - **Resilienz-Szenarien** (`resilience.rs`): 10 End-to-End-Invarianten gegen den ODF-Save/Load-Stack — Crash-nach-Commit/vor-Apply wird durch Replay recovered, Sync-Failure beim Commit sichtbar, silenter Data-Block-Flip durch `data_crc` gefangen, silenter Bitmap-Flip durch Bitmap-CRC gefangen, Primary-SB-Wipe fällt auf Redundanz zurück, ENOSPC-Abbruch lässt vorherige Generation loadbar, fsck meldet strukturellen Schaden, Power-Loss-Simulation panict nie, Journaled-Save survives full Crash-Roundtrip
  - **Stress- und Skalierungstests** (`stress.rs`): 1000 kleine Files Roundtrip, 150 tiefe Directory-Ebenen, 1-MB-File als kontinuierliche Extent-Allocation, 200 inkrementelle Save-Zyklen mit O(1)-Churn, 30 Delete/Recreate-Zyklen wo der Allocator Blöcke zurückrecycelt, fsck-Walk über 500 Inodes
  - **Physisches Copy-on-Write** (`refcount.rs`): `RefCountTable` — Per-Data-Block-u16-Refcounter mit Encode/Decode in 4-KiB-Blöcken (2044 Counter/Block), CRC-geschützt, Overflow/Underflow-Detection; `BlockSharing` mit `register_fresh`/`clone_extent`/`cow_write` (InPlace/MustCopy-Outcome)/`release`; Diagnostics `shared_blocks()`/`bytes_saved()`; gated via `FEATURE_INCOMPAT_PHYSICAL_COW` flag
  - **Self-Healing-Scrubber** (`scrub.rs`): `scrub::run(device, &ScrubPlan)` kombiniert fsck, optional Auto-Repair via fsck_repair, und Per-File-data_crc-Verifikation in einem End-to-End-Call; drei Presets (`full`/`structural_only`/`read_only`); `ScrubReport` mit `extents_verified`/`blocks_verified`/`data_corruptions`/`residual_issues`/`repair_ops_committed`; unrecoverable-bit-rot wird gemeldet aber nicht auto-repariert (keine Daten-Redundanz in ODF v1)
  - **Hot/Cold-Tiering** (`tiering.rs`): `TieredDevice<H, C>` routet jedes I/O zu hot- oder cold-Device basierend auf `TierMap`-Block-Assignment; `HotnessTracker` zählt reads/writes pro Block; `TierPolicy` mit Promote-/Demote-Thresholds; `Migrator::rebalance` promoviert/demotiert Blöcke gecapped durch `max_migrations_per_pass`; `migrate_block` kopiert Daten mit deliberate Crash-ordering (destination durable before map flip)
  - **Property-Based-Tests** (`property.rs`): eigene deterministische xorshift64-PRNG ohne externe Deps, `generate_sequence(seed, len)` produziert reproduzierbare Op-Sequenzen aus `Op::{CreateFile, DeleteFile, OverwriteFile, CreateDirectory, CreateSnapshot}`; `run_and_check` appliziert gegen frische Session und prüft nach JEDEM Op: fsck clean, load_state_native roundtrippt; 8 Seeds × 15 Ops = 120 zufällige Mutate+Check-Zyklen
  - **Concurrency-Tests** (`concurrency.rs`): demonstriert was sicher ist und was nicht — CRC32C über N Threads (stateless), disjoint Bitmaps parallel mutiert und gemerged, N Reader unter `Arc<Mutex>` auf geteiltem Device, Single-Writer-Multi-Reader-Snapshot-Pattern, Session-Move zwischen Threads (Send), Snapshot-Reader sehen Mutations nach dem Snapshot nicht, `Send`-Bounds compile-time verifiziert
  - **FUSE-ODF-Mount** (`mount_odf_image`): Read-only-FUSE-Mount eines ODF-Images, lädt state via `FileImageDevice::open(..., read_only=true)` + `load_state_native`, verwendet das bestehende `CoreFsFuseView`-Gerüst — kein Duplikat der 2500-Zeilen-FUSE-Mount-Infrastruktur nötig
  - **FUSE-ODF-RW-Mount** (`mount_odf_image_rw`): voll beschreibbarer FUSE-Mount eines ODF-Images — `FuseBacking::Odf`-Variante im bestehenden `CoreFsFuseMountRw`, beim `open_odf_session` wird die FileImageDevice read-write geöffnet, `recover_pending_transactions` spielt halbfertige Journal-Transaktionen aus vorherigen Mounts zurück, Dirty-Marker wird via `save_state_native_incremental` gesetzt; jeder `persist()` während der Mount-Lifetime ist inkremental (nur geänderte Inodes werden rewritten) und crash-consistent (Metadata durch Journal, Data direkt); CLI-Kommando `mount-odf-rw <image> <mount-point>` (Linux-only)
  - Top-Level-APIs `format_device`/`save_state`/`load_state`/`inspect` (Blob-Modus) + `save_state_native`/`load_state_native` (Native-Modus) auf beliebigen `BlockDevice`-Implementierungen
  - Getestet mit **303 Unit-Tests** (Layout-Planer, CRC32C-Testvektoren, Superblock-Roundtrip+Korruptionsprüfung, Bitmap-Allokation, Inode-Record-Roundtrip, Extent-Chain-Roundtrip+Loop-Detection, Dir-Block-Roundtrip+Pack, Attr-Block-Roundtrip, Xattr-Block-Roundtrip+Principal-Reject, Allocator-First/Best-Fit+Inode-Slots, Journal-Commit/Replay/Abort/Partial-Discard/Multi-Txn-Ordering, Format+Save+Load-Roundtrip, redundanter Superblock-Fallback, Native-Roundtrip-Scenarios, Encryption-Flag-Propagation, fsck-Clean/Corrupt/Read-Only, Micro-Benchmark, CLI-End-to-End) plus 2 neue CLI-Integrations-Tests

### `src/services`

- [x] Journaling
- [x] Versionierung mit konfigurierbarer Byte-Budget-Bereinigung (`prune_to_budget`)
- [x] Recovery
- [x] Integrität
- [x] Indexierung
- [x] Sicherheit
- [x] Synchronisationsstatus
- [x] Kompression (LZ4 frame via `lz4_flex`)
- [x] Verschlüsselung (ChaCha20-Poly1305 via `chacha20poly1305`)
- [x] Quota-Enforcement
- [x] Copy-on-Write mit Blob-Referenz-Zählung, CoW-Klons und Snapshot-Pinning
- [x] Deduplizierung (aktiver Scan-Pass mit ref_count-Audit und Konsolidierung)

### `src/platform`

- [x] plattformneutrales Runtime-Integrationsmodell
- [x] Blueprint für Verwaltungswerkzeuge
- [x] optionaler Linux-FUSE-Adapter für `.img`-basierte Dateisystemtests
- [x] Performance-Benchmarking und Protokollierung

### `src/cli.rs`

- [x] einfacher administrativer Einstieg für Demo- und Testoperationen

## Abgleich mit den Anforderungen

Die Datei [features_corefs.md](/daten1/development/brian/corefs/features_corefs.md) bleibt die fachliche Zieldefinition. Der aktuelle Implementierungsstand deckt bereits Teile der folgenden Bereiche ab:

- [x] grundlegende Dateisystem-Funktionen
- [x] Metadaten- und ACL-Grundmodell
- [x] Versionierung in Basisform
- [x] Löschen und Wiederherstellung
- [x] Integritätsprüfung
- [x] Plattformneutralität und Integrationsmodell
- [x] Verwaltungs- und Tooling-Grundstruktur
- [x] Persistenz eines vollständigen CoreFS-Zustands
- [x] Performance-Messung und Ergebnisprotokollierung
- [x] strukturelle Prüfung persistierter Volume-Images
- [x] Linux-Nutzung und Testbarkeit über gemountete `.img`-Dateien
- [x] profilbasierte Performance-Messung mit variablen Parametern
- [x] Snapshot-Browsing und Time-Travel im Linux-FUSE-RW-Mount (`.snapshots/` und `@`-Syntax)

Nur teilweise oder noch konzeptionell abgebildet sind aktuell:

- [~] Speicherverwaltung auf echter Datenträgerebene
- [~] fortgeschrittene Integritäts- und Redundanzmechanismen
- [~] semantische Tiefenanalyse
- [~] vollständige Runtime- und Betriebssystemintegration

## Empfohlene nächste Schritte

**Legende für Status-Markierungen**

- `- [ ]` offen / nicht umgesetzt
- `- [~]` in Arbeit / teilweise umgesetzt
- `- [x]` abgeschlossen

### Phase 1: Persistenz

- [x] blockorientiertes On-Disk-Format definieren (ODF v1: 4-KiB-Blöcke, 3× Superblock, CRC32C)
- [x] Metadaten-Layout festlegen
- [x] die aktuellen binären Segment-Frames schrittweise in ein noch stärker blockorientiertes und spezialisierteres On-Disk-Format überführen
- [~] die Defragmentierungs- und Allocator-Schicht um intelligentere Reallocation-Policies, Hintergrund-Compaction und spaeter Copy-on-Write-orientierte Extent-Moves weiterentwickeln (aktive Defragmentierung, Heat-aware Reallocation und physisches CoW mit Refcount umgesetzt; echte Hintergrund-Compaction noch offen)
- [x] Performance-Baseline für zukünftige Persistenzumstellungen fortlaufend protokollieren (`PERFORMANCE_LOG.md`, `benchmark-log`)

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

Hinweis: die konkrete Umsetzung dieser Phase ist Gegenstand von **Phase 5: AnyOS-Integration** (siehe unten).

- [ ] VFS-Schnittstelle für das eigene Betriebssystem definieren (siehe 5.5)
- [ ] Kernel- und Userland-Grenzen trennen (siehe 5.5–5.7)
- [~] Mount-, Unmount- und Recovery-Lebenszyklus implementieren (Lebenszyklus für Linux-FUSE-Pfad vollständig inkl. WAL-Recovery; für AnyOS-Kernel-Treiber noch offen)

### Phase 3: Integrität und Sicherheit

- [~] blocknahes Write-Ahead-Log und Replay auf Delta-Ebene direkt im Volume (extent-/device-blockadressiertes Pending-WAL + `DeviceJournal` mit Crash-Recovery umgesetzt; vollständig physisch device-blockadressiertes WAL statt extent-orientiertem Pending-WAL noch offen)
- [x] echte Kompression (LZ4 frame format, transparent in der Schreib-/Lese-Pipeline)
- [x] echte Verschlüsselung (ChaCha20-Poly1305 AEAD, 256-Bit-Schlüssel, pro-Datei-Flag)
- [x] Quotas (Enforcement in `create_file`/`write_file`, `QuotaService::check_stats()`)
- [~] Scrubbing und Self-Healing mit Redundanzmodell (Scrubbing + Self-Healing-Scrubber umgesetzt; echte Redundanzquellen/Replikat-basierte Heilung noch offen)

### Phase 4: Erweiterte Funktionen

- [~] Time Travel (Lookup und Read über `file@<spec>` im RW-Mount umgesetzt; RO-Mount-Adressierung und persistente Symlink-Zugriffspfade noch offen)
- [x] Deduplizierung (`BlockStore::dedup_pass()` mit 3-Phasen-Scan, konfigurierbar via `config.performance.deduplication_enabled`)
- [ ] Clusterfähigkeit (Cluster-Synchronisation)
- [x] Hot/Cold-Storage (Tiering-Primitive im ODF, Heat-aware Extent-Reallocation)
- [~] semantische Inhaltsindexierung (Inhaltsklassifikation nach Dateiendung vorhanden; vollständiger Index-Service noch offen)

## Phase 5: AnyOS-Integration

Ziel: CoreFS als natives Dateisystem des eigenen Betriebssystems AnyOS (`/daten1/development/brian/anyos`) einbinden — sowohl als Kernel-Treiber (Root-FS) als auch über ein FUSE-Subsystem mit Userspace-Daemon. Zusätzlich müssen alle Wartungs-Tools (mkfs, fsck, scrub, dump, snapshot, …) auch unter AnyOS als Apps verfügbar sein.

**Strategische Entscheidungen**

- FUSE-Protokoll zwischen AnyOS-Kernel und Userspace-Daemon: **AnyOS-nativ/schlank** (Variante b) für den Start. Linux-FUSE-Wire-Kompatibilität (Variante a) bleibt als **optionale** spätere Ausbaustufe (siehe 5.10) vorgesehen; die Protokoll-Abstraktion ist so zu wählen, dass Variante a ohne Änderungen an `corefs-core` oder am Adapter-Layer nachrüstbar ist.
- Einbindung in AnyOS als **reguläre Rust-Library-Crate** (`rlib`, kein FFI/C-ABI), statisch in den Kernel gelinkt; während der Entwicklung via Path-Dependency, später ggf. Git-Dependency.
- Tool-Logik wird aus der heutigen CLI in eine eigenständige `corefs-tools`-Crate extrahiert, damit Linux-CLI und AnyOS-Apps **dieselbe** Implementierung nutzen.

**Legende**

- `- [ ]` offen
- `- [~]` in Arbeit / teilweise umgesetzt
- `- [x]` abgeschlossen

### 5.1 Workspace-Split in CoreFS

- [x] Cargo-Workspace in `corefs/` anlegen (root-Manifest mit `.` und `corefs-core` als Member; `[workspace.package]` und `[workspace.dependencies]` zentralisiert)
- [~] Crate `corefs-core` (no_std + alloc) — reine FS-Logik: `domain/`, `storage/`, `services/` (Skelett vorhanden; `platform` strikt no_std+alloc; `domain` + `config` migriert, noch hinter `std`-Feature-Gate bis SystemTime → Timestamp-Migration abgeschlossen ist; `storage` + `services` noch im main crate)
- [ ] Crate `corefs-tools` (no_std + alloc) — Operation-APIs: mkfs, fsck, repair, scrub, dump, defrag, resize, tier, snapshot
- [ ] Crate `corefs-std` (std) — `FileImageDevice`, `RawBlockDevice`, std-Clock-/Rng-Impls, CLI-Helpers
- [ ] Crate `corefs-fuse-proto` (no_std) — AnyOS-natives Wire-Format (`Request`/`Reply`-Enums, Encode/Decode via bincode)
- [ ] Crate `corefs-fuse-adapter` (no_std) — plattformneutraler CoreFs ↔ Request/Reply-Adapter
- [ ] Crate `corefs-cli` (std) — dünner Binary-Wrapper um `corefs-tools`
- [x] Existierender Linux-`fuser`-Pfad ([src/platform/linux_fuse.rs](src/platform/linux_fuse.rs)) bleibt funktional (unverändert weitergeführt)
- [x] Bestehende 636 Tests unverändert grün (631 main + 12 corefs-core + 1 doctest)

### 5.2 no_std-Migration `corefs-core`

- [x] `#![no_std]` + `extern crate alloc` in `corefs-core` (konditional über `cfg_attr(not(feature = "std"), no_std)`, zzgl. `forbid(unsafe_code)` und `warn(missing_docs)`)
- [~] `std::collections::{BTreeMap, HashMap}` → `alloc::collections::BTreeMap` / `hashbrown::HashMap` (Domain-Layer umgestellt; weitere Stellen folgen mit der storage/services-Migration)
- [x] `std::time::SystemTime` entfernen → eigener `Timestamp`-Typ (secs + nanos) — Domain-Feld-Typen, Services (`JournalEntry.timestamp`, `JournalRuntimeState.started_at`, `FileVersion.created_at`), Storage-Kodierung und alle Call-Sites im main crate migriert. `Timestamp` ist **bincode-wire-kompatibel** mit `SystemTime`; bestehende Volume-Images bleiben byte-identisch lesbar (Regressionstest `wire_compatible_with_system_time_bincode`).
- [ ] `std::path::PathBuf`/`Path` aus dem Kern entfernen — Kern arbeitet mit `&str`/`PathRef`; vollständige PathBuf-Nutzung nur in `corefs-std`/`corefs-cli`
- [ ] `std::io::{Read, Write, Error}` → eigener `io`-Trait-Satz im Kern
- [~] Abhängigkeiten auf `default-features = false` umstellen (serde mit `default-features = false, features = ["derive", "alloc"]` bereits zentral; bincode/lz4_flex/chacha20poly1305 noch im main crate)
- [x] `trait Clock` + `trait Rng` als Plattform-Abstraktionen einführen (siehe `corefs-core::platform`); zusätzlich `SystemClock`-Default-Impl unter `std`-Feature
- [x] Feature `std` für std-Bequemlichkeiten — Default = no_std; `Timestamp::now()`, `Inode::new`/`touch_*`, `VolumeDescriptor::from_config`, `From<SystemTime>`/`Into<SystemTime>` hinter `std`-Feature; `*_at(now)`-APIs auch ohne `std` verfügbar
- [ ] CI-Build für Custom-Target `x86_64-anyos` (no_std) grün
- [~] CI-Build für `x86_64-unknown-linux-gnu` grün (lokal verifiziert; CI-Hook folgt)
- [x] Alle Tests auf Linux weiterhin grün (631 main + 21 corefs-core + 1 doctest = 653; zusätzlich 17 Tests grün im strikten no_std-Modus via `cargo test -p corefs-core --no-default-features`)

### 5.3 Tool-Logik extrahieren nach `corefs-tools`

- [ ] `mkfs::format(device, params) -> Result<Report>`
- [ ] `fsck::check(device, mode) -> Result<Report>`
- [ ] `fsck::repair(device, policy) -> Result<Report>`
- [ ] `scrub::run(device, range) -> Result<Report>`
- [ ] `dump::superblock(device) -> SuperblockInfo`
- [ ] `dump::inode(device, inode_id) -> InodeDump`
- [ ] `defrag::run(device, policy) -> Result<Report>`
- [ ] `resize::grow(device, new_size) -> Result<Report>`
- [ ] `tier::migrate(device, policy) -> Result<Report>`
- [ ] `snapshot::{create, list, delete, restore}` als Library-APIs
- [ ] Strukturierte `Report`-Typen (keine String-Rückgaben im Kern)
- [ ] `corefs-cli` auf `corefs-tools` umgestellt — Linux-CLI-Verhalten unverändert

### 5.4 FUSE-Crates (AnyOS-Wire)

- [ ] `corefs-fuse-proto`: Request-/Reply-Enums (Lookup, Getattr, Setattr, Read, Write, Readdir, Create, Mkdir, Unlink, Rmdir, Rename, Symlink, Readlink, Open, Release, Flush, Fsync, Statfs, …)
- [ ] `corefs-fuse-proto`: Wire-Kodierung (bincode), Versions-Handshake, Session-Header
- [ ] `corefs-fuse-adapter`: CoreFs-API → Request/Reply, plattformneutral, async-frei
- [ ] `corefs-fuse-adapter`: Transport-Trait (Send Request / Receive Reply) für austauschbare Backends (Linux `/dev/fuse` später, AnyOS-Syscall jetzt)
- [ ] Unit-Tests für Wire-Round-Trip (Linux-Host-Tests reichen)

### 5.5 AnyOS — Kernel-Treiber-Pfad (direkter CoreFS-Treiber)

- [ ] Modul `anyos/kernel/src/fs/corefs/` anlegen
- [ ] `BlockDevice`-Impl (CoreFS-Trait) als Adapter auf `drivers/storage` (LBA↔Byte-Offset-Mapping)
- [ ] `Clock`-Impl aus AnyOS-Zeitquelle
- [ ] `Rng`-Impl aus AnyOS-Entropiequelle
- [ ] `Filesystem`-Trait ([kernel/src/fs/vfs/types.rs](../anyos/kernel/src/fs/vfs/types.rs)) implementieren: Delegation an `corefs-core::CoreFs`
- [ ] `FsType::CoreFs` im VFS-Enum ergänzen
- [ ] Superblock-Magic-Erkennung in der Boot-/Partitions-Detection
- [ ] Block-Cache-Integration ([kernel/src/fs/blockcache.rs](../anyos/kernel/src/fs/blockcache.rs)) via BlockDevice-Wrapper
- [ ] Mount als Root-FS möglich
- [ ] Read-Path funktional (readdir, read, getattr)
- [ ] Write-Path funktional (create, write, unlink, mkdir, rename, …)
- [ ] Unclean-Mount-Recovery über WAL bei Boot
- [ ] Kernel-Integration-Tests mit vorbereitetem CoreFS-Image

### 5.6 AnyOS — Generisches FUSE-Subsystem (native Variante)

- [ ] Modul `anyos/kernel/src/fs/fuse/` anlegen
- [ ] Character-Device/Syscall-Interface für Daemon-Registrierung
- [ ] Request-Queue + Reply-Matching via `unique`-ID
- [ ] AnyOS-Wire-Protokoll-Encoder/-Decoder (gemeinsam mit `corefs-fuse-proto`)
- [ ] Mount-Syscall: Daemon-Handle + Mount-Point
- [ ] FUSE-Filesystem als `FsType::Fuse` im VFS — Delegation jedes Calls an Daemon
- [ ] Crash-Handling: Daemon-Absturz → sauberer Unmount / EIO für offene Handles
- [ ] Hello-World-Test-FS-Daemon zur Validierung (unabhängig von CoreFS)
- [ ] Protokoll-Abstraktion so gewählt, dass späterer Linux-FUSE-Wire parallel einhängbar ist

### 5.7 AnyOS — `corefsd` Userspace-Daemon

- [ ] App `anyos/apps/corefsd/` anlegen
- [ ] Linkt `corefs-core` + `corefs-fuse-adapter` + `corefs-fuse-proto`
- [ ] Block-Device-Zugriff via AnyOS-Syscalls → `BlockDevice`-Impl
- [ ] Daemon registriert sich beim Kernel-FUSE-Subsystem
- [ ] Event-Loop: Request → `corefs-core` → Reply
- [ ] Sauberes Shutdown bei Unmount-Signal
- [ ] End-to-End-Test: Userspace-Mount, Datei schreiben/lesen, Unmount

### 5.8 AnyOS — Tool-Apps

- [ ] `anyos/apps/mkfs.corefs/`
- [ ] `anyos/apps/fsck.corefs/` (inkl. Repair-Policies)
- [ ] `anyos/apps/corefs-dump/` (Superblock/Inode-Inspection)
- [ ] `anyos/apps/corefs-scrub/`
- [ ] `anyos/apps/corefs-defrag/`
- [ ] `anyos/apps/corefs-snapshot/` (create/list/delete/restore)
- [ ] `anyos/apps/corefs-resize/`
- [ ] `anyos/apps/corefs-tier/`
- [ ] Gemeinsames Arg-Parsing-Hilfsmodul (Clap ist std-only → leichtgewichtiger Ersatz oder portable Teilmenge)
- [ ] Einheitliche Report-Rendering-Routinen (Text + JSON)
- [ ] Build-Integration in das CMake/Ninja-Setup von AnyOS

### 5.9 Online-Tools (spätere Ausbaustufe)

- [ ] Online-Scrub gegen gemountetes FS (via Kernel-Ioctl oder Daemon-RPC)
- [ ] Online-Snapshot gegen gemountetes FS
- [ ] Online-Defrag gegen gemountetes FS
- [ ] Tool-Dispatcher erkennt gemountetes vs. offline-Device und wählt Pfad

### 5.10 Optional: Linux-FUSE-Wire-Kompatibilität (Variante a)

**Status: optional** — nicht Bestandteil der Erstintegration. Wird nur umgesetzt, wenn der konkrete Bedarf entsteht, unveränderte Linux-FUSE-Dateisysteme (libfuse-basiert) unter AnyOS laufen zu lassen. Die Protokoll-Abstraktion in 5.4/5.6 ist so zu wählen, dass dieser Schritt ohne Änderungen an `corefs-core` oder am Adapter-Layer nachrüstbar bleibt.

**Motivation:** freie Portierung fremder FUSE-Dateisysteme (sshfs, ntfs-3g, encfs, …) ohne Code-Änderungen.

**Kosten:** erheblicher Zusatzaufwand im Kernel-FUSE-Subsystem (Version-Negotiation, `INTERRUPT`-Handling, vollständige Linux-FUSE-Semantik, binärkompatible `#[repr(C)]`-Strukturen).

- [ ] Entscheidung treffen: wird Variante a tatsächlich benötigt? (Trigger-Kriterium definieren)
- [ ] `corefs-fuse-proto::wire_linux` — `#[repr(C)]`-Strukturen nach `linux/fuse.h`
- [ ] Ziel-Protokollversion fixieren (z. B. 7.38) statt volle Versions-Matrix
- [ ] Versions-Negotiation gemäß Linux-FUSE-Protokoll
- [ ] Linux-FUSE-Mount-Options-Parsing (`default_permissions`, `allow_other`, …) im AnyOS-Kernel
- [ ] `INTERRUPT`-/`FORGET`-Opcode-Handling im Kernel-Subsystem
- [ ] AnyOS-Kernel-FUSE-Subsystem: zweiter Wire-Decoder parallel zur nativen Variante
- [ ] `/dev/fuse`-Device mit Linux-kompatibler Semantik
- [ ] Validierung mit einem unveränderten libfuse-basierten Beispiel-FS
- [ ] Validierung mit einem zweiten realen FUSE-FS (z. B. sshfs)

## Wichtige Hinweise

- Das Projekt ist aktuell ein strukturierter, getesteter Kern-, Persistenz- und Volume-Layout-Prototyp und noch kein produktionsreifes Dateisystem.
- Der Linux-Mountpfad unterstützt read-only und read-write; der RW-Pfad nutzt Dirty/Clean-Markierung, transaktionales Journal-Writeback, persistente physische Volume-Allokation, freie Extent-Wiederverwendung, ein persistentes `FREE`-Segment mit Allocator-Policy, aktive Defragmentierung, ein integriertes extent- und device-blockadressiertes Pending-WAL im Volume sowie virtuelle Read-only-Overlays für Snapshot-Browsing (`.snapshots/`) und Time-Travel (`file@<spec>`), ist aber noch kein vollständiges produktionsnahes Device-WAL mit Hintergrund-Compaction oder Copy-on-Write-Moves.
- Die virtuellen FUSE-Overlays belegen INO-Bereiche im oberen `u64`-Raum (ab `u64::MAX/4` abwärts für dynamische Knoten, `u64::MAX/2+1_000_000` für Snapshot-Root-Dirs, `u64::MAX-1` für `.snapshots/`) und sind vollständig write-protected (EROFS bei jeder Mutation).
- Performance-Messungen werden jetzt über `benchmark` und `benchmark-log` reproduzierbar ausführbar.
- Die vorhandene Testsuite ist stark für die aktuelle In-Memory-Implementierung, aber keine Garantie für `100%` messbare Coverage, da in der Umgebung keine Coverage-Tools installiert sind.
- `.codex` ist inzwischen als projektinterne Vorgabedatei befüllt.
