# Integrität & Recovery

Status: ✅ Kernpfade produktiv (Checksums, Journal, WAL, Repair). Einzelne Erweiterungen offen (siehe unten).

## Checksummen

- **Algorithmus**: **CRC32C** (Castagnoli) — implementiert in `corefs-core/src/storage/ondisk/checksum.rs`.
- Eingesetzt auf:
  - Superblock (gesamte 4096 B),
  - Segment-Directory und Payload im Volume-Image,
  - Extent-Einträgen (`Extent.checksum`),
  - Block-/Inode-Bitmap (`block_bitmap_crc`, `inode_bitmap_crc`),
  - On-Disk Inodes (`DiskInode.checksum`),
  - Backup-Frames (`COREFSBK`).
- Validiert bei Read, Scrubbing, fsck und beim Volume-Open.

Hinweis: Ältere Dokumentation sprach von FNV1a. Der aktive Algorithmus auf Disk ist CRC32C. Intern verwendet der `BlockStore` zusätzlich eine schnelle FNV-ähnliche 64-Bit-Prüfsumme zur Dedup-Hash-Lookup (nicht zur Integritätsgarantie).

## Transaktionales Journal ✅

Implementierung: `corefs-core/src/services/journal.rs`, On-Disk-Frame in `corefs-core/src/storage/ondisk/journal.rs`, Host-Persistenz im `JOUR`-Segment des Volume-Images.

```text
begin_transaction("rw-writeback") → record(...) → commit_transaction(id)
                                             └→ abort_transaction(id)
```

- Aggregierte `JournalReplayState` beim Mount (letzte commit/abort IDs, unclean_shutdown).
- `JournalRepairSummary` dokumentiert reconcilierte Inkonsistenzen.

## WAL (Write-Ahead-Log) ✅

Implementierung: `src/storage/volume_wal.rs` + Device-Journal hinter dem Volume-Image (256 KiB, Generation-Counter).

Record-Typen:

- `PatchExtent` — partielle Block-Updates (inode, device_block, block_offset, inode_offset, payload).
- `Truncate` — Dateiverkürzung.
- `Delete` — Inode- / Extent-Freigabe.

WAL-Replay läuft automatisch beim `VolumeSession::open()` — vor jedem RW-Mount.

## Crash-Recovery ✅

Implementierung: `corefs-core/src/services/recovery.rs`.

Ablauf beim Mount:

1. **Superblock-Auswahl** — aus SUPR / SUP2 / tertiärer Kopie wird die höchste gültige Generation mit intakter CRC32C gewählt.
2. **Pending-WAL** aus `TXNJ` / Device-Journal laden und prüfen:
   - vollständige Transaktionen mit Commit-Marker werden angewendet,
   - offene Transaktionen werden verworfen.
3. **Journal↔Catalog-Reconciliation** via `JournalService::repair()`.
4. **Volume als clean markieren** (State-Flag zurücksetzen, Generation erhöhen, Flush).

## Online-Scrubbing ✅

```bash
corefs scrub
```

- Durchläuft alle aktiven Blöcke und Inodes.
- Prüft CRC32C der Datenblöcke und Extent-Checksummen.
- Erkennt Silent Corruption.
- Blockiert den Mount nicht.

Implementierung: `src/services/integrity.rs` + `corefs-core/src/storage/ondisk/scrub.rs`.

## fsck & Reparatur ✅

Images:

```bash
corefs fsck-image    ./corefs.img              # read-only Prüfung
corefs repair-image  ./corefs.img [--aggressive]
corefs inspect-image ./corefs.img              # Detaildump
```

Blockgeräte (nur lesend):

```bash
sudo corefs fsck-device /dev/sdb1
```

### Reparatur-Stufen

| Stufe | Beschreibung |
|---|---|
| **1** | Superblock-Fallback auf SUP2 / tertiären Superblock (höchste gültige Generation) |
| **2** | Segment-Directory-Rekonstruktion aus verbleibenden, lesbaren Segmenten |
| **3** | Block-Descriptor-Heilung aus Inode-Extent-Trees |
| **4** | `reconcile_persisted_state()` — Journal-Abgleich mit aktivem Katalog |
| **5** | Deep-fsck: Inhaltscheck gegen Inode-Metadaten (Grössen, Encryption/Compression-Flags, CRC32C) |

Rückgabe: `RepairReport` mit Anzahl reparierter Items, verbleibenden Warnungen und Before/After-Generation.

## Typische Fehlerfälle

| Szenario | Reaktion |
|---|---|
| Crash während Transaktion | Recovery verwirft Pending-Einträge |
| Superblock-Korruption | Fallback auf SUP2 / tertiären Block |
| Bit-Rot in Datei-Extent | Scrubbing erkennt, Read liefert `CoreFsError::Integrity` |
| Fake-Stick (vorgespiegelte Kapazität) | `mkfs-device` Sanity-Check bricht ab; `verify-device --destructive` zur Diagnose |
| Unclean Unmount | automatisches Recovery beim nächsten Mount |
| Segment-Directory korrupt | Rekonstruktion aus Segment-Inhalten |
| Journal ↔ Katalog drift | `reconcile_persisted_state()` korrigiert nach Generation-Majorität |

## Offene Punkte / Verbesserungsbedarf

- **Self-Healing mit Redundanz**: Bei CRC-Fehlern auf Datenblöcken könnten CoW-Ref-Count-Kopien zurückgelesen werden; heute wird ein Fehler zurückgegeben (⚠️).
- **Bit-Rot-Repair** durch automatische Block-Migration (⚠️, bisher nur Erkennung).
- **Online-Grow/Shrink** (`ondisk/resize.rs`): Code vorhanden, CLI-Integration fehlt.
- **Audit-Log** mit kryptographischer Signatur für Forensik.
