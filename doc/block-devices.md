# Blockgeräte-Workflow

Status: ✅ produktiv (Linux). Trait in `corefs-core/src/storage/block_device.rs`, Linux-Implementierung (ioctls) in `src/storage/block_device.rs`.

CoreFS kann nicht nur aus einer Image-Datei, sondern auch **direkt von einem Blockgerät** (z. B. `/dev/sdb1`) betrieben werden.

## BlockDevice-Implementierungen

| Typ | Zweck |
|---|---|
| `FileImageDevice` | Image-Datei als Block-Backend |
| `RawBlockDevice` | Direktes Raw-Device (Linux, `libc`) |
| `MemoryDevice` | Volatil, für Tests |

Alle Implementierungen bieten sektoraligned Reads/Writes, TRIM-Unterstützung und Read-only-Detection.

## CLI-Workflow

### 1. Sicherheitscheck — `probe-device`

```bash
sudo cargo run --release -- probe-device /dev/sdb1
```

Prüft Kapazität, Sektorgröße, Mount-Status, R/O-Status. **Bricht ab**, wenn das Gerät aktuell gemountet ist.

### 2. Formatieren — `mkfs-device`

```bash
sudo cargo run --release -- mkfs-device /dev/sdb1
```

Führt automatisch einen **Fake-Stick-Sanity-Check** aus (siehe unten). Der Check kann mit `--skip-check` übergangen werden, was nur für vertrauenswürdige Geräte zu empfehlen ist.

### 3. Prüfen — `fsck-device`

```bash
sudo cargo run --release -- fsck-device /dev/sdb1
```

Read-only-Prüfung aller Segmente und Checksummen.

### 4. Mount — `mount-device-rw`

```bash
sudo cargo run --release -- mount-device-rw /dev/sdb1 /mnt/usb
```

Mountet das Blockgerät direkt über FUSE (siehe [fuse-integration.md](fuse-integration.md)). Interne I/O läuft über `DeviceVolume` mit On-Demand Segment-I/O, Read-Cache und Write-Buffer.

### 5. Vollständige Verifikation — `verify-device`

```bash
sudo cargo run --release -- verify-device /dev/sdb1 --destructive --chunks 128 --chunk-size 1048576
```

**⚠️ Zerstörerisch** — überschreibt das Gerät. Schreibt Pseudo-Zufalls-Chunks über das gesamte Gerät und liest sie zurück. Deckt manipulierte Controller (Fake-Sticks mit falsch angegebener Kapazität) zuverlässig auf.

## Fake-Stick-Detection

Fake-Sticks geben vorgespiegelte Kapazität an (z. B. 1 TB), speichern aber nur einen Bruchteil und überschreiben frühere Daten zirkulär. CoreFS implementiert zwei Schutzstufen:

1. **Sanity-Check** (automatisch bei `mkfs-device`): stichprobenartige Schreib-/Lese-Tests an mehreren Positionen.
2. **Vollscan** (`verify-device --destructive`): kompletter Kapazitätsnachweis.

Wird ein Mismatch erkannt, bricht der Befehl mit einer klaren Fehlermeldung ab.

## On-Demand Segment-I/O

[DeviceVolume](../src/storage/device_volume.rs) (~42 KB) verwaltet das Volume auf dem Blockgerät **ohne vollständiges In-Memory-Laden**:

- Segmente werden bei Zugriff vom Device geladen (mit LRU-Cache).
- Mutationen werden in einen Write-Buffer gepuffert und beim Flush sektorausgerichtet zurückgeschrieben.
- Schreiboperationen durchlaufen das **Device-Journal** (256 KiB-Region hinter dem Volume) mit Barrier-Semantik.

Dies ermöglicht CoreFS-Mounts auf Geräten, die deutlich größer sind als der verfügbare RAM.

## Sicherheitsempfehlungen

- Immer `probe-device` **vor** destruktiven Operationen ausführen.
- `mkfs-device` niemals auf das Systemlaufwerk anwenden.
- `verify-device --destructive` nur auf Geräten, deren Inhalt verloren gehen darf.
- Für Produktions-USB-Sticks (nicht bekannter Herkunft) immer `verify-device` durchlaufen lassen, bevor Daten aufgespielt werden.
