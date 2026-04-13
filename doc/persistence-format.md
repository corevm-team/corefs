# On-Disk-Format

CoreFS speichert Volumes in einem **mehrsegmentierten binären Format**. Implementierung: [src/storage/volume_image.rs](../src/storage/volume_image.rs) (~102 KB).

## Grundstruktur

```
┌─────────────────────────────────────────┐
│  Magic + Format-Version                 │
│  Redundanter Superblock SUPR            │
│  Redundanter Superblock SUP2            │
│  Segmenttabelle (Directory)             │
├─────────────────────────────────────────┤
│  Segment 1 (z. B. CNFG)                 │
│  Segment 2 (z. B. VOLM)                 │
│  ...                                     │
│  Segment N (DATA)                       │
└─────────────────────────────────────────┘
```

Header- und Segment-Frames werden mit 64-Byte-Alignment geschrieben.

## Superblock-Redundanz und Generation-Counter

Zwei Superblock-Kopien (`SUPR`, `SUP2`) enthalten je einen monoton wachsenden **Generation-Counter**. Beim Öffnen wird die Kopie mit höherem Counter verwendet. Bei Crash während der Aktualisierung bleibt mindestens eine konsistente Version lesbar.

## Segment-Typen

| Tag | Bedeutung |
|---|---|
| `CNFG` | Konfiguration (Policies, Volume-Metadaten) |
| `VOLM` | Volume-Deskriptor |
| `AINO` | Active Inodes |
| `DINO` | Deleted Inodes (für Restore) |
| `JOUR` | Journal-Einträge (committed) |
| `TXNJ` | Transaction Journal — Pending-WAL für RW-Sessions |
| `VERS` | Datei-Versionen (Historie) |
| `SNAP` | Snapshots inkl. `file_data` BTreeMap |
| `SYNC` | Sync-Status |
| `HOTP` | Hot-Path-Telemetrie |
| `FREE` | Free-List + Allocator-Policy |
| `BLKD` | Block-Deskriptoren |
| `DATA` | Datei-Inhalte (Extent-Frames) |

## Transaction Journal (TXNJ)

Das TXNJ-Segment enthält offene Write-Ahead-Log-Einträge. Beim Mount einer RW-Session wird TXNJ geprüft:
- committed Transaktionen → in Catalog angewendet
- pending ohne Commit-Marker → aborted und verworfen

Siehe [integrity-recovery.md](integrity-recovery.md).

## FREE-Segment

Persistiert:
- Liste freier Extents (`free_extents`)
- Allocator-Policy (First-Fit / Best-Fit / Heat-aware)
- Fragmentierungs-Metriken

Dadurch bleibt der Allocator-Zustand über Mounts erhalten.

## Block-Device-Layout

Wenn CoreFS direkt auf einem Blockgerät (nicht in einer Image-Datei) genutzt wird, gilt:

```
[ Sektor 0 ... ]   Volume-Image (mehrsegmentiert)
[ ... ]
[ nach Volume ]    Device-Journal (256 KiB, Barrier-safe)
```

Das Device-Journal ist getrennt vom Volume-Journal und dient der Barrier-Semantik beim Blockgeräte-I/O (siehe [block-devices.md](block-devices.md)).

## Alignment & Integrität

- **Alignment**: alle Segmente starten an einer 64-Byte-Grenze; Payloads sektorausgerichtet bei Blockgeräten.
- **Checksummen**: pro Block (FNV1a); Extent-Frames tragen einen Payload-Checksum.
- **Scrubbing**: `services::integrity` validiert Checksummen über alle aktiven Blöcke (`scrub`).

## Reparatur (fsck)

Mehrstufig:
1. Primär-Superblock prüfen; bei Fehler Fallback auf `SUP2`.
2. Segmenttabelle validieren; fehlende Segmente rekonstruieren.
3. Block-Deskriptoren heilen (BLKD).
4. Deep-Check über Datei-Inhalte.

Siehe `services::integrity::deep_fsck` in [src/services/integrity.rs](../src/services/integrity.rs) und die CLI-Kommandos `fsck-image`, `fsck-device`.
