# Feature-Katalog

Legende: ✅ produktiv · 🔶 teilweise / Basis vorhanden · ⚠️ geplant / Stub · ❌ nicht implementiert

Grundlage: Ist-Analyse des Quellcodes (Stand 2026-04). Ergänzende Referenzen: [features_corefs.md](../features_corefs.md), [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md).

## Grundlegende Dateisystem-Funktionen

| Feature | Status | Bemerkung / Verbesserungsbedarf |
|---|---|---|
| Case-sensitive Pfade | ✅ | BTreeMap mit String-Key; Case-Insensitive-Modus nicht vorgesehen |
| Symbolische Links | ✅ | `InodeKind::Symlink`, Target in Domain-Inode |
| Hardlinks | ❌ | `link_count`-Feld reserviert, aber keine Semantik, kein `link()`-API |
| Beliebige Verzeichnistiefe | ✅ | Stress-Test mit 300 Ebenen |
| Beliebig lange Pfadnamen | ✅ | begrenzt durch On-Disk `DirectoryEntry.name` (232 B) |
| Effiziente Pfadauflösung | ✅ | O(log n) via BTreeMap im Catalog |
| Listing grosser Verzeichnisse | 🔶 | heute O(n) Filter auf globaler Map — kein paralleler Parent-Index |
| Extended Attributes (xattr) | ✅ | `BTreeMap<String, Vec<u8>>` pro Inode, On-Disk `xattr_block` |
| ACLs (Speicherung) | ✅ | `AclEntry` mit `Principal::{Unix,Group,Other,Extended}` |
| ACL-Enforcement (FUSE) | 🔶 | Nur POSIX-Mode wird in FUSE geprüft; erweiterte Einträge werden nicht evaluiert |

## Speicherverwaltung

| Feature | Status | Bemerkung |
|---|---|---|
| Copy-on-Write | ✅ | Per-Block-Ref-Count (`FEATURE_INCOMPAT_PHYSICAL_COW`) |
| Extent-basierte Speicherung | ✅ | B-Tree-artiger `ExtentTree` pro Inode |
| Transparente Kompression | ✅ | LZ4-Frame, Schwelle ≥ 64 B |
| Transparente Verschlüsselung | ✅ | ChaCha20-Poly1305 AEAD, Per-File-Key via HKDF |
| Interne Deduplizierung | ✅ | Expliziter `dedup_pass()` (3-Phasen, Hash-basiert) |
| Defrag / Heat-aware Reallocation | ✅ | `defragment()`, `optimize_storage()` |
| TRIM / Discard | ✅ | FreedExtent-Tracking + `BLKDISCARD` ioctl |
| Quotas | ✅ | `max_files`, `max_bytes` Enforcement |
| Bitmap-Allokator mit Gap-Reuse | ✅ | Persistente Free-List (`FREE`-Segment) |
| Hot/Cold-Tiering | 🔶 | `ondisk/tiering.rs` vorhanden, nicht produktiv verwendet |
| Multi-Group-Allokator | 🔶 | Layout implementiert; 1 TODO-Marker für Multi-Group-Block-Reservierung |

## Versionierung & Snapshots

| Feature | Status | Bemerkung |
|---|---|---|
| Auto-Versionierung pro Datei | ✅ | `VersioningService` speichert vor jedem Überschreiben |
| Byte-Budget-Pruning | ✅ | Default 64 MiB, konfigurierbar |
| Snapshots (voll) | ✅ | `create_snapshot()` pinnt Bytes + Metadaten |
| Snapshots (scoped) | ✅ | `create_snapshot_scoped(name, root)` für Teilbaum |
| Snapshot-Restore | ✅ | `SnapshotRestoreReport` mit Per-Pfad-Skip-Liste |
| Snapshot-Diff | ✅ | added / removed / modified / unchanged |
| Time-Travel-Addressierung | ✅ | `file@2026-04-01T12:00:00Z` via FUSE-Overlay |
| Snapshot-Browsing `.snapshots/…` | ✅ | Read-only Overlay im RW-Mount |
| Snapshot-Speichereffizienz | 🔶 | speichert heute **unkomprimierte** Bytes in RAM+Disk; könnte Content-Hashes + Block-Pinning nutzen |
| Auto-Purge bei Plattenknappheit | ⚠️ | Byte-Budget existiert, aber kein ENOSPC-getriebener Auto-Purge |

## Löschen & Wiederherstellung

| Feature | Status | Bemerkung |
|---|---|---|
| Soft-Delete | ✅ | Inode nach `deleted_inodes` verschoben |
| Restore aus Papierkorb | ✅ | `recover(path)` holt Inode aus Tombstone |
| Expunge (endgültig) | ✅ | `expunge_file()` |
| Secure-Delete | ✅ | `delete --secure`, überschreibt Blocks vor Freigabe |

## Integrität & Fehlertoleranz

| Feature | Status | Bemerkung |
|---|---|---|
| CRC32C auf Superblock / Segmente / Extents | ✅ | Castagnoli-Polynom, `ondisk/checksum.rs` |
| Redundante Superblöcke | ✅ | Primär + sekundär + tertiär, Generation-Counter |
| Transaktionales Journal | ✅ | begin / record / commit / abort mit Replay |
| WAL (Extent-adressiert) | ✅ | `volume_wal.rs`, Device-Journal 256 KiB |
| Multi-Level fsck / Repair | ✅ | Superblock → Segment-Directory → Block-Descriptors → Journal |
| Online-Scrubbing | ✅ | `scrub()` auf offenem Volume |
| Unclean-Mount-Recovery | ✅ | `STATE_DIRTY` + Generation-basierte Reparatur |
| Silent-Corruption-Detection | ✅ | CRC32C auf Datenblöcken (FEATURE_COMPAT_BLOCK_CHECKSUMS) |
| Self-Healing via Block-Redundanz | ⚠️ | Konzeptionell vorgesehen, Mirror-Modus nicht implementiert |

## Metadaten & Semantik

| Feature | Status | Bemerkung |
|---|---|---|
| POSIX-Timestamps (btime/mtime/ctime/atime) | ✅ | 3 Haupt-Timestamps aktiv gepflegt; atime optional |
| Tags pro Datei | ✅ | `FileMetadata.tags: Vec<String>` |
| Erweiterte Attribute | ✅ | siehe oben |
| Content-Classification | ✅ | Dateiendungsbasiert (Text/Image/Source/Archive/Binary) |
| Semantisches Query / Facetting | ⚠️ | POC in `services/semantic.rs`, nicht produktiv |
| Volltext-Index | ❌ | nicht implementiert |

## Sicherheit & Verschlüsselung

| Feature | Status | Bemerkung |
|---|---|---|
| Verschlüsselung ruhender Daten | ✅ | ChaCha20-Poly1305, 256-Bit, AEAD |
| Per-File-Key-Derivation | ✅ | HKDF-SHA256(master_key, info=inode_id) |
| Master-Key-Rotation | ✅ | ohne Re-Encryption möglich (HKDF-Property) |
| Keystore-Format | ✅ | `COREFSKS`-Magic, Password-KDF-wrapped Master-Key |
| Pure-Rust SHA-256 / HKDF | ✅ | RFC-3394 / RFC-5869, im Kern (no_std) |
| Secure-Erase | ✅ | Überschreiben vor Freigabe |
| Password-KDF | 🔶 | Aktuell lightweight; Argon2 noch nicht verdrahtet für Produktiv-Use |
| ACL-Enforcement | 🔶 | siehe oben |

Details: [security.md](security.md), [key-management.md](key-management.md).

## Persistenz & On-Disk-Format

| Feature | Status | Bemerkung |
|---|---|---|
| Mehrsegmentiges Image-Format | ✅ | Magic `COREFS01`, Version 7, 4 KiB Alignment |
| Redundante Superblöcke (SUPR, SUP2) | ✅ | Generation-basierte Auswahl |
| Spezialisierte Segment-Frames | ✅ | AINO, DINO, JOUR, VERS, SYNC, HOTP, SNAP, TXNJ, FREE, BLKD, DATA, VOLM, CNFG |
| Atomare Schreibsequenz | ✅ | write → flush → rename |
| On-Demand Segment-I/O | ✅ | `DeviceVolume` mit Read-Cache / Write-Buffer |
| Device-Journal (Barrier-safe) | ✅ | 256 KiB, generationsgeprüft |
| Inkrementelle Persistenz | ✅ | nur geänderte Segmente schreiben |
| Image-Repair aus Backup-Superblock | ✅ | plus Segment-Directory-Rekonstruktion |
| Native ODF-Layout (kernelnah) | ✅ | `LAYOUT_MODE_NATIVE`, Groupped Layout optional |
| Rückwärtskompatible Alignment-Wechsel | ✅ | 64 B → 4096 B über Superblock-Feld |

Details: [persistence-format.md](persistence-format.md).

## Performance

| Feature | Status | Bemerkung |
|---|---|---|
| Hot-Path-Telemetrie | ✅ | `HotPathService` pro Inode |
| Heat-aware Extent-Reallocation | ✅ | |
| Fragmentierungs-Metriken | ✅ | `fragmentation_report()` |
| Defrag / Compaction | ✅ | `defragment()` + CLI |
| FUSE-WRITEBACK_CACHE | ✅ | |
| `max_write` = 1 MiB | ✅ | |
| Streaming-Writes ≥ 32 MiB | ✅ | Zwischenflushes begrenzen RAM auf O(32 MiB) |
| Handle-Level Read/Write-Cache | ✅ | Per-Handle Buffer |
| Benchmark-Framework | ✅ | 4 Profile (dev/ci/regression/storage-heavy), Markdown-Logging, JSON-History |
| Inkrementeller Persist-Pfad | ✅ | Phase 1f, siehe `PERFORMANCE_LOG.md` |
| Hintergrund-Rebalancing | ⚠️ | nicht als Scheduler implementiert |

Details: [performance.md](performance.md).

## Plattform-Unterstützung

| Feature | Status | Bemerkung |
|---|---|---|
| Plattformneutrale Kern-Bibliothek | ✅ | `corefs-core` ist `no_std + alloc` |
| Linux-FUSE (RO + RW) | ✅ | FUSE v31, alle Kern-Ops (lookup, getattr, read, write, mkdir, rmdir, unlink, rename, open, release, create, statfs, setattr, readdir, readlink, symlink, copy_file_range) |
| Block-Device-Abstraktion | ✅ | File / Raw (ioctl) / Memory |
| Mount-Werkzeuge | ✅ | `mount-image`, `mount-image-ro`, `mount-device-rw` |
| Fake-Stick-Detection | ✅ | `probe-device`, `verify-device --destructive` |
| AnyOS-Kernel-VFS-Integration | 🔶 | `corefs-core` no_std-ready, Kernel-Treiber POC (`corefs-tools`, `corefs-fuse-adapter`) |
| Windows-Adapter | ❌ | Stub-Verzeichnis `src/platform/windows/` |

Details: [fuse-integration.md](fuse-integration.md), [block-devices.md](block-devices.md), [anyos-integration.md](anyos-integration.md).

## Backup & Export

| Feature | Status | Bemerkung |
|---|---|---|
| Frame-basiertes Backup-Format | ✅ | Magic `COREFSBK`, CRC32C pro Frame |
| Vollständiges Backup | ✅ | `backup dump` |
| Inkrementelles Backup | ✅ | `backup dump-incremental <since>` |
| Restore | ✅ | `backup restore` |
| Blob-Provider-Trait | ✅ | Entkoppelt I/O von Format |

Details: [backup.md](backup.md).

## Cluster / Replikation

| Feature | Status | Bemerkung |
|---|---|---|
| Sync-Status-Tracking | 🔶 | `SyncService` vorhanden, nur Zustandshaltung |
| Multi-Node-Synchronisation | ⚠️ | konzeptionell, keine Implementierung |
| Konsistente Replikation | ⚠️ | später |

## Offene Verbesserungspunkte (kompakt)

1. **Snapshot-Speichereffizienz** — Content-Hashes + Block-Pinning statt voller Byte-Kopien.
2. **Directory-Listing** — paralleler Parent-Index für schnelle Listings grosser Verzeichnisse.
3. **ACL-Enforcement** — FUSE-Ops sollten `AclEntry`-Einträge berücksichtigen.
4. **Password-KDF** — Argon2 verdrahten statt FNV-ähnlicher Ableitung.
5. **Hardlinks** — fehlende Semantik einführen (`link_count`, `link()` API).
6. **Cluster-Sync** — echten Replikations-Algorithmus und Konsens verdrahten.
7. **Tiering** — vorhandenen `ondisk/tiering.rs` produktiv anschliessen.
8. **Semantic-Indexing** — Queries produktiv verfügbar machen.
9. **AnyOS-Kerneltreiber** — vom POC zur produktiven VFS-Integration.
10. **Windows-Adapter** — Modulskelett mit Entwickler-API.
