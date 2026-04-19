# On-Disk- und Image-Formate

CoreFS verwendet zwei verwandte Binärformate:

1. **ODF v1** (*On-Disk Format*) — kernelnahes Layout für Blockgeräte und AnyOS (`corefs-core/src/storage/ondisk/`).
2. **Volume-Image `COREFS01`** — mehrsegmentiges Dateiformat für Host-Images und Transport (`src/storage/volume_image.rs`).

Beide nutzen CRC32C-Checksummen (Castagnoli) und redundante Superblöcke.

## ODF v1 — Native Layout

**Magic**: `"COREFSDF"` (`u64 = 0x4650534643524F20`, little-endian). Definiert in `corefs-core/src/storage/ondisk/layout.rs`.

### Feature-Flags

| Flag | Wert | Bedeutung |
|---|---|---|
| `FEATURE_INCOMPAT_PAYLOAD_INODE` | `1<<0` | `PersistedState` inline in System-Inode 0 |
| `FEATURE_INCOMPAT_BLOCK_GROUPS` | `1<<1` | Block-Group-Layout, mehrere Bitmap-Blöcke |
| `FEATURE_INCOMPAT_PHYSICAL_COW` | `1<<2` | Per-Block-Refcount-Tabelle |
| `FEATURE_COMPAT_BLOCK_CHECKSUMS` | `1<<0` | CRC32C auf alle Datenblöcke |
| `FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS` | `1<<1` | Sekundäre/tertiäre Superblöcke |

### Block-Layout

```
Block 0        Reserviert (Boot-Sektor, Null-Bytes)
Block 1        Primärer Superblock (4096 B)
Block 2..X     Block-Allocation-Bitmap
Block X..Y     Inode-Allocation-Bitmap
Block Y..Z     Inode-Tabelle (256 B pro Inode, 16 Inodes/Block)
Block Z..W     Journal-Region
Block W..N-2   Daten-Blöcke
Block N/2      Tertiäre Superblock-Kopie
Block N-1      Sekundäre Superblock-Kopie
```

### Superblock-Felder (Auszug)

`magic`, `version_major/minor`, `block_size = 4096`, `total_blocks`, `free_blocks`, `total_inodes`, `free_inodes`, Offsets zu Bitmaps / Inode-Tabelle / Journal / Daten, `secondary_superblock_block`, `tertiary_superblock_block`, `feature_{compat,incompat,ro_compat}`, `uuid[16]`, `label[32]`, `created_at`, `last_mount_at`, `last_write_at`, `mount_count`, `generation`, `state` (CLEAN=0 / DIRTY=1), `payload_inode`, `block_bitmap_crc`, `inode_bitmap_crc`, `layout_mode` (0=BLOB, 1=NATIVE), `root_inode`.

Das Feld `generation` identifiziert die aktuelle Persistenz-Runde und wird zur Auswahl der besten Superblock-Kopie während Recovery verwendet.

### On-Disk Inode (256 B)

| Feld | Grösse | Kommentar |
|---|---|---|
| `kind` | u16 | 1=FILE · 2=DIRECTORY · 3=SYMLINK |
| `mode` | u16 | POSIX-Permissions |
| `uid`, `gid` | 2× u32 | Owner |
| `size` | u64 | logische Dateigrösse (unkomprimiert) |
| `created_at`/`modified_at`/`changed_at`/`accessed_at` | 4× i64 | btime, mtime, ctime, atime |
| `link_count` | u32 | reserviert — Hardlinks nicht aktiv |
| `block_count` | u32 | belegte Datenblöcke |
| `flags` | u32 | `ENCRYPTED`, `COMPRESSED`, `SPARSE` |
| `checksum` | u32 | CRC32C des Inodes |
| `extent_block` | u64 | Wurzel des Extent-Baums |
| `xattr_block` | u64 | Block mit erweiterten Attributen |
| Padding | | auf 256 B |

### Extent-Baum

```rust
pub struct Extent {
    pub logical_block:  u64, // Offset in der Datei (in Blöcken)
    pub physical_block: u64, // Physischer Block auf dem Volume
    pub length:         u64, // Länge in Blöcken
    pub checksum:       u32, // CRC32C
}
```

Jeder Inode hat einen eigenen, B-Tree-artigen Baum. Range-Queries, Shrink und Gap-Reuse sind implementiert.

### Journal (ODF)

Frame-basiertes On-Disk-Journal (`ondisk/journal.rs`). Transaktionen enthalten Logical-Ops (create/update/delete/rename), werden beim Replay in den `catalog` reintegriert, unvollständige Transaktionen werden verworfen.

### Repair / fsck

`ondisk/fsck.rs`, `ondisk/fsck_repair.rs`, `ondisk/scrub.rs`:

- Superblock-Validierung (Magic, CRC32C, Version, Redundanzen).
- Bitmap-Konsistenz (Block-/Inode-Bitmap-CRC).
- Extent-Sanity (keine Kollisionen, kein Doppelreferenz ohne Refcount-Flag).
- Journal-Reconciliation (pending Transaktionen werden aufgelöst oder verworfen).

## Volume-Image `COREFS01`

Magic `"COREFS01"`, aktuell **Format-Version 7**, Alignment **4096 B** (Phase 2 — Altbestand mit 64 B wird weiter gelesen, Alignment im Superblock kodiert).

### Dateiaufbau

```
Header (16 B)
├─ magic[8]         "COREFS01"
├─ format_version   u32  (= 7)
└─ reserved[4]

Superblock (56 B)
├─ format_version   u32
├─ alignment        u32  (heute 4096)
├─ segment_count    u32
├─ clean_unmount    u32  (1 = clean, 0 = dirty)
├─ generation       u64
├─ directory_offset u64
├─ directory_length u64
├─ directory_checksum u64 (CRC32C)
└─ payload_checksum u64 (CRC32C)

Segment-Directory  [SegmentEntry; segment_count]
  └─ { kind:[u8;4], offset:u64, length:u64 }

Daten-Segmente (in Reihenfolge, 4 KiB-aligned):
  SUPR, SUP2, CNFG, VOLM, AINO, DINO, JOUR, VERS, SYNC,
  HOTP, SNAP, TXNJ, FREE, BLKD, DATA
```

### Segment-Typen

| Kind | Inhalt |
|---|---|
| `SUPR` | Primärer Superblock |
| `SUP2` | Sekundärer Superblock (redundant) |
| `CNFG` | `CoreFsConfig` (Security, Persistence, Performance, Quota) |
| `VOLM` | `VolumeDescriptor` (UUID, Label, Geometrie) |
| `AINO` | Aktive Inodes (Catalog) |
| `DINO` | Gelöschte Inodes (Tombstones) |
| `JOUR` | Journal-Transaktionen |
| `VERS` | Versions-Historie |
| `SYNC` | Sync-Status |
| `HOTP` | Hot-Path-Telemetrie |
| `SNAP` | Snapshots |
| `TXNJ` | Pending-WAL (crash-recovery für RW-Sessions) |
| `FREE` | Freie Extents / Allocator-Policy |
| `BLKD` | Block-Descriptors |
| `DATA` | File-Content-Blobs |

### Persistenz-Pfad

1. Segmente serialisieren (bincode + 4 KiB Alignment).
2. Beide Superblöcke schreiben (SUPR, SUP2).
3. Generation-Counter inkrementieren.
4. Atomare Sequenz: `write → flush → rename`.

### Reparatur

`IntegrityService::repair_image(path, aggressive)`:

- Wählt aus Primär-/Sekundär-Superblock die höchste gültige Generation.
- Rekonstruiert fehlende Segment-Directories aus verbleibenden Segmenten.
- Rekonstruiert `BLKD` aus Inode-Extent-Trees, falls nötig.
- `reconcile_persisted_state()` gleicht Journal ↔ Katalog ab.

## Device-Journal (256 KiB-Region nach Volume-Image)

Für Blockgeräte liegt hinter dem Image ein **Device-Journal**. Es speichert Pending-WAL-Records (`VolumeWal`), die bei Crash nach dem nächsten Mount automatisch replayed werden. Jeder Eintrag trägt eine Generation-Nummer und CRC32C.

## Offene Punkte / Verbesserungsbedarf

- **Physische Blockfreigabe bei CoW-Shared-Blocks**: Ref-Count-Freigabepfad ist implementiert, die Defrag-Pipeline könnte konsolidiertes Re-Layout aktiver Blöcke aggressiver einplanen.
- **Snapshot-Blobs**: werden aktuell in `DATA`/`SNAP` teils redundant zu `AINO`-Versionen abgelegt — zukünftig über Content-Hash + Block-Pinning statt voller Byte-Kopien.
- **Multi-Group-Allokator**: 1 `TODO` in `ondisk/grouped.rs` für eine ausstehende Block-Reservierungs-API.
- **Online-Resize**: `ondisk/resize.rs` liegt vor; es existiert noch keine CLI- oder FUSE-Integration für Live-Grow/Shrink.
