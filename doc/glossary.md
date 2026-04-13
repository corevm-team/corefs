# Glossar

| Begriff | Bedeutung |
|---|---|
| **AEAD** | Authenticated Encryption with Associated Data — hier: ChaCha20-Poly1305 |
| **AINO / DINO** | Active / Deleted Inodes Segment im Volume-Image |
| **Allocator** | Modul für Inode-ID- und Extent-Verwaltung |
| **Barrier** | Write-Grenze mit Ordnungszusage auf Blockgeräten |
| **BLKD** | Block-Deskriptor-Segment |
| **BlockDevice** | Trait-Abstraktion für sektoraligned I/O (`FileImageDevice`, `RawBlockDevice`, `MemoryDevice`) |
| **BlockStore** | Extent-Management mit Copy-on-Write, Ref-Count und Defrag |
| **Catalog** | Active/Deleted Inode-Maps + Quota-Stats |
| **CoreFsConfig** | Zentrale Konfiguration mit allen Policies |
| **CoreFsError / CoreFsResult<T>** | Fehler-Enum und Result-Typalias |
| **CoW (Copy-on-Write)** | Änderungen erzeugen neue Blöcke; alte bleiben für Snapshots referenziert |
| **CNFG** | Config-Segment im Volume-Image |
| **DATA** | Datei-Inhalte-Segment |
| **Defrag** | Aktive Defragmentierung freier/benutzter Extents |
| **DeviceJournal** | 256-KiB-Region hinter dem Volume auf Blockgeräten (Barrier-safe WAL) |
| **DeviceVolume** | Volume-Zugriff direkt auf Blockgerät mit On-Demand Segment-I/O |
| **Dirty/Clean-Marker** | Superblock-Flag zur Erkennung unsauberer Unmounts |
| **Extent** | Zusammenhängender Block-Bereich für eine Datei |
| **Fake-Stick** | USB-Stick mit gefälschter Kapazitätsangabe |
| **FNV1a** | Schnelle Hash-Funktion; in CoreFS für Checksummen |
| **FREE** | Free-List-Segment mit Allocator-Policy |
| **fsck** | File-System-Check mit mehrstufiger Reparatur |
| **FUSE** | Filesystem in Userspace — Linux-Kernel-Schnittstelle |
| **Generation-Counter** | Monotone Sequenznummer zur Superblock-Auswahl bei Redundanz |
| **Heat-aware Reallocation** | Priorisiertes Placement häufig genutzter Extents |
| **HOTP** | Hot-Path-Telemetrie-Segment |
| **Inode** | Strukturierter Metadaten-Eintrag einer Datei |
| **JOUR** | Committed-Journal-Segment |
| **LZ4** | Kompressionsalgorithmus, hier als frame format |
| **Pending-WAL** | Noch nicht bestätigte WAL-Records im TXNJ |
| **Principal** | Zugriffssubjekt in ACLs (User/Group/Other) |
| **Scoped Snapshot** | Snapshot, begrenzt auf `scope_root` |
| **Scrubbing** | Online-Checksum-Validierung |
| **Secure-Delete** | Löschen mit explizitem Nulling |
| **Segment** | Strukturierter Block im Volume-Image mit Tag (z. B. `AINO`, `DATA`) |
| **SNAP** | Snapshot-Segment |
| **Soft-Delete** | Markierung als gelöscht, Daten erhalten (Restore möglich) |
| **Streaming-Write** | Zwischenflushes bei ≥ 32 MiB zur RAM-Konstantheit |
| **SUPR / SUP2** | Primärer / redundanter Superblock |
| **SYNC** | Sync-Status-Segment |
| **Tamper-Detection** | Erkennung von Manipulation via AEAD-Tag + Checksum |
| **Tiering** | Hot/Warm/Cold-Storage-Klassen |
| **Time-Travel** | Adressierung historischer Versionen via `@`-Syntax |
| **TRIM** | SSD-Hinweis, dass Bereich freigegeben ist |
| **TXNJ** | Transaction-Journal-Segment (Pending-WAL) |
| **VERS** | Datei-Versionen-Segment |
| **VOLM** | Volume-Deskriptor-Segment |
| **VolumeImage** | Das komplette, mehrsegmentiert strukturierte On-Disk-Image |
| **VolumeSession** | Lifecycle-Objekt: `format_new`, `open`, `flush`, `mutate` |
| **WAL (Write-Ahead-Log)** | Änderungen werden vor dem Anwenden protokolliert |
