# Feature-Katalog

Legende: ✅ produktiv · 🔶 teilweise / Basis vorhanden · ⚠️ Lücke / geplant

Grundlage: [features_corefs.md](../features_corefs.md) und [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md).

## Grundlegende Dateisystem-Funktionen

| Feature | Status | Bemerkung |
|---|---|---|
| Case-sensitives Verhalten | ✅ | |
| Symbolische Links | ✅ | |
| Beliebige Verzeichnistiefe | ✅ | |
| Beliebig lange Pfadnamen | ✅ | |
| Beliebig viele Dateien pro Verzeichnis | ✅ | O(1) via Catalog-Map |
| Effiziente Auflösung | ✅ | O(1) Lookup |
| ACLs und erweiterte Zugriffsrechte | ✅ | `AclEntry`, Principal-Modell |

## Speicherverwaltung

| Feature | Status |
|---|---|
| Journaling | ✅ Transaktionales Journal mit pending WAL |
| Copy-on-Write | ✅ Blob-Referenzzählung im BlockStore |
| Transparente Kompression | ✅ LZ4 frame format, ≥ 64 B |
| Interne Deduplizierung | ✅ aktiver Dedup-Pass |
| Blockoptimierung / Defrag | ✅ Defrag + Heat-aware Reallocation |
| TRIM / Discard | ✅ FreedExtent-Tracking |
| Quotas | ✅ `max_files`, `max_bytes` Enforcement |
| Hot/Cold-Storage-Tiering | 🔶 Basis vorhanden |

## Versionierung & Time-Travel

| Feature | Status |
|---|---|
| Automatische Dateiversionierung | ✅ pro-Datei Historie |
| Snapshots (auch scoped) | ✅ `scope_root` |
| Byte-Budget-Pruning | ✅ Default 64 MiB |
| Time-Travel via `@`-Syntax | ✅ `file@2026-04-13`, `file@v2` |
| Snapshot-Browsing via `.snapshots/` | ✅ im RW-Mount |
| Snapshot-Diff | ✅ added / removed / modified / unchanged |
| Auto-Purge bei Plattenknappheit | 🔶 Byte-Budget vorhanden, Auto-Purge optional |
| Backup/Export-Schnittstelle | 🔶 geplant |

## Löschen & Wiederherstellung

| Feature | Status |
|---|---|
| Soft-Delete | ✅ Inode als gelöscht markiert |
| Restore gelöschter Dateien | ✅ `restore_file()` |
| Secure-Delete mit Nulling | ✅ `delete --secure` |

## Integrität & Fehlertoleranz

| Feature | Status |
|---|---|
| Checksummen (FNV1a) | ✅ |
| Transaktionsbasis | ✅ `tx_begin`/`commit`/`abort` |
| fsck mit Multi-Level-Reparatur | ✅ Superblock-Fallback, Segment-Rekonstruktion |
| Online-Scrubbing | ✅ |
| Unclean-Mount-Recovery | ✅ |
| WAL-Recovery vor Mount | ✅ |
| Self-Healing mit Redundanz | 🔶 konzeptionell |
| Bit-Rot-Erkennung | 🔶 Silent-Corruption geplant |

Details in [integrity-recovery.md](integrity-recovery.md).

## Metadaten & Semantik

| Feature | Status |
|---|---|
| Tags und Attribute pro Datei | ✅ BTreeMap, unbegrenzt |
| Semantische Inhaltsklassifikation | ✅ nach Dateiendung |
| Metadaten-API / CLI | ✅ |
| Indexierung | ✅ Basis |

## Sicherheit & Verschlüsselung

| Feature | Status |
|---|---|
| Verschlüsselung ruhender Daten | ✅ ChaCha20-Poly1305, 256 Bit |
| Transparente Enc/Dec-Pipeline | ✅ |
| Datei-Ebene-Verschlüsselung | ✅ `encrypted` Flag in Metadata |
| Schlüsselableitung | ✅ `derive_key_from()` |
| Tamper-Detection | ✅ AEAD-Auth |

Details in [security.md](security.md).

## Persistenz & On-Disk-Format

| Feature | Status |
|---|---|
| Mehrsegmentiges binäres Volume-Image | ✅ |
| Redundante Superblocks (SUPR, SUP2) | ✅ Generation-Counter-Selektion |
| Segmenttabelle mit Alignment | ✅ |
| Spezialisierte Segment-Frames | ✅ AINO, DINO, JOUR, VERS, SNAP, BLKD, DATA, TXNJ, FREE, HOTP, SYNC |
| Physische Block-Allokation | ✅ |
| Persistentes FREE-Segment mit Allocator-Policy | ✅ |
| Pending-WAL in TXNJ | ✅ für RW-Sessions |
| On-Demand Segment-I/O | ✅ `DeviceVolume` |
| Device-Journal (Barrier-safe) | ✅ 256 KiB |
| Physical Block-Sharing on-disk | 🔶 aktuell logisches CoW im RAM |

Details in [persistence-format.md](persistence-format.md).

## Performance

| Feature | Status |
|---|---|
| Hot-Path-Erkennung | ✅ Zugriffszähler pro Inode |
| Heat-aware Extent-Reallocation | ✅ |
| Fragmentierungsmetriken | ✅ |
| Defrag / Compaction | ✅ CLI |
| FUSE-Durchsatz-Optimierungen | ✅ WRITEBACK_CACHE, max_write = 1 MiB |
| Streaming-Writes | ✅ ≥ 32 MiB Zwischenflushes |
| Handle-Level Read/Write-Cache | ✅ |
| Benchmark-Framework | ✅ 5 Profile, Markdown-Logging |
| Hintergrund-Rebalancing | 🔶 konzeptionell |

Details in [performance.md](performance.md).

## Plattform-Unterstützung

| Feature | Status |
|---|---|
| Plattformneutrales Runtime-Blueprint | ✅ |
| Linux-FUSE (ro & rw) | ✅ |
| Block-Device-Abstraktion | ✅ File / Raw / Memory |
| Mount-Werkzeuge | ✅ |
| Fake-Stick-Detection | ✅ Sanity + `verify-device` |
| Native Kernel-VFS-Integration | 🔶 geplant (AnyOS) |

Details in [fuse-integration.md](fuse-integration.md), [block-devices.md](block-devices.md).

## Cluster

| Feature | Status |
|---|---|
| Sync-Status-Verfolgung | ✅ Service |
| Cluster-Synchronisation | 🔶 Modell vorhanden, nicht aktiv |
| Konsistente Replikation | ⚠️ später |

## Test-Lücken (siehe [testing.md](testing.md))

- **Concurrency**: 0 Multi-Thread-Tests (P0)
- **Fault Injection**: 0 Tests (P0)
- **Stress & Skalierung**: fehlen (P0)
- **Performance-Regression-Gate**: Infrastruktur vorhanden, keine Assertions (P1)
