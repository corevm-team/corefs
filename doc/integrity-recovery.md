# Integrität & Recovery

## Checksummen

- Algorithmus: **FNV1a** (schnell, für Integritätsprüfung ausreichend).
- Pro Block eine Checksumme; Extent-Frames tragen zusätzlich einen Payload-Checksum.
- Validierung bei Read, Scrubbing, fsck.

Implementierung: [src/services/integrity.rs](../src/services/integrity.rs) (~17.5 KB).

## Scrubbing

```bash
cargo run -- scrub
```

- Durchläuft alle aktiven Blöcke.
- Erkennt Bit-Fehler / Silent Corruption ab Checksum-Mismatch.
- Online-fähig (blockiert den Mount nicht).

## Transaktionales Journal

Implementierung: [src/services/journal.rs](../src/services/journal.rs) (~16 KB).

- `tx_begin() → tx_commit()` / `tx_abort()`
- Pending-Einträge im **TXNJ**-Segment
- Committed-Einträge im **JOUR**-Segment
- Recovery-Marker für saubere Unterscheidung beim Mount

## WAL (Write-Ahead-Log)

Implementierung: [src/storage/volume_wal.rs](../src/storage/volume_wal.rs).

Extent- und Device-Block-adressierte Records für:
- `PatchExtent` — inkrementelle Block-Updates
- `Truncate` — Dateiverkürzung
- `Delete` — Inode- / Extent-Freigabe

## Crash-Recovery

Implementierung: [src/services/recovery.rs](../src/services/recovery.rs).

Ablauf beim Mount:
1. Superblock validieren; bei Defekt auf `SUP2` fallen.
2. Pending-WAL im TXNJ prüfen:
   - Mit Commit-Marker → anwenden.
   - Ohne Commit-Marker → verwerfen (Abort offener Transaktionen).
3. Volume als clean markieren (Superblock-Generation erhöhen, Flush).

Dies läuft automatisch bei `VolumeSession::open()` und vor jedem FUSE-Mount.

## fsck

### `fsck-image <path> [--repair]`

```bash
cargo run -- fsck-image ./corefs.img
cargo run -- fsck-image ./corefs.img --repair
```

### `fsck-device <device>`

Read-only-Check auf Blockgerät:

```bash
sudo cargo run -- fsck-device /dev/sdb1
```

### Reparatur-Stufen

Bei `--repair`:

1. **Superblock-Fallback** — defekter SUPR durch intakte `SUP2` ersetzen.
2. **Segmenttabellen-Rekonstruktion** — wenn Verzeichnis korrumpiert.
3. **Block-Deskriptor-Heilung** — BLKD-Segment aus vorhandenen Extents neu aufbauen.
4. **Deep-fsck** — Inhaltscheck gegen Metadaten.

Rückgabe: `AdminReport` mit Anzahl reparierter Items und verbleibender Warnungen.

## Typische Fehlerfälle & Reaktion

| Szenario | Reaktion |
|---|---|
| Crash während Transaktion | Recovery verwirft pending-Einträge |
| Superblock-Korruption | Fallback auf SUP2, fsck heilt Primary |
| Bit-Rot in Datei-Extent | Scrubbing erkennt; Read liefert `CoreFsError` |
| Fake-Stick (vorgespiegelte Kapazität) | `mkfs-device` Sanity-Check bricht ab; `verify-device --destructive` zur Diagnose |
| Unclean Unmount | Automatisches Recovery beim nächsten Mount |
