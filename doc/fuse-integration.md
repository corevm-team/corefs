# Linux-FUSE-Integration

Implementierung: [src/platform/linux_fuse.rs](../src/platform/linux_fuse.rs) (~114 KB). Nur auf Linux verfügbar (`#[cfg(target_os = "linux")]`).

## Voraussetzungen

- Linux-Kernel mit FUSE3-Unterstützung
- `fuser`-Crate (0.14) gebunden über `target.'cfg(target_os = "linux")'.dependencies`
- Paket `fuse3` oder `libfuse3` (je Distribution)

Diagnose-Hilfe:

```bash
cargo run -- diagnose-mount ./demo.img /tmp/corefs-mnt --create
```

## Mount-Modi

### Read-Only

```bash
cargo run -- mount-image <image> <mount-point>
```

- Snapshot-konsistenter Lesezugriff
- Unterstützt transparente Entschlüsselung
- Kein Write, kein Truncate

### Read-Write

```bash
cargo run -- mount-image-rw <image> <mount-point>
```

- Volle Read-Write-Semantik mit Writeback beim Unmount / Flush
- Dirty/Clean-Markierung beim Öffnen / Schließen
- Handle-Level Read- und Write-Caching
- **Streaming-Writes** ab ≥ 32 MiB mit Zwischenflushes (RAM-Verbrauch O(32 MiB) statt O(file_size))
- `FUSE_WRITEBACK_CACHE` aktiviert, `max_write = 1 MiB` für Kernel-Batching

### Block-Device-Mount

```bash
sudo cargo run -- mount-device-rw <device> <mount-point>
```

Mountet ein Blockgerät direkt, siehe [block-devices.md](block-devices.md).

## Virtual Overlays

### `.snapshots/`

Im RW-Mount existiert automatisch ein virtuelles Verzeichnis `.snapshots/`. Inhalt pro Snapshot:

```
/mnt/corefs/.snapshots/
├── 1-initial/
│   └── ...Snapshot-Inhalt (read-only)...
└── 2-nightly/
    └── ...
```

Snapshots sind **Read-only**-Overlays; ein Schreibversuch liefert `EROFS`.

### Time-Travel via `@`-Suffix

Dateiname-Suffix `@<spec>` adressiert direkt historische Versionen:

| Beispiel | Bedeutung |
|---|---|
| `file.txt@2026-04-13` | Version am genannten Tag |
| `file.txt@2026-04-13T10:30` | Version zum Zeitpunkt |
| `file.txt@v2` | Version Nr. 2 |

Aktivierung über `VersioningPolicy::expose_time_travel = true` (Default).

## Unmount

```bash
fusermount -u /tmp/corefs-mnt
# oder
umount /tmp/corefs-mnt
```

Der Unmount **flusht** alle Writes zurück in das Volume-Image oder Blockgerät. Danach ist das Volume in einem konsistenten Clean-Zustand.

## Diagnose

`diagnose-mount` prüft:

- Existenz und Zugriff auf Image / Mount-Point
- Ob Mount-Point aktuell belegt ist
- FUSE-Verfügbarkeit (Modul geladen, `/dev/fuse` zugreifbar)
- Berechtigungen

Mit `--create` wird der Mount-Point bei Nichtexistenz angelegt.

Implementierung: [src/platform/diagnostics.rs](../src/platform/diagnostics.rs) (~25 KB).

## Performance-Eigenschaften

- `copy_file_range()` serverseitig ohne Kernel-Roundtrips
- Handle-Cache für häufige Reads
- Direct I/O bei großen Writes (Streaming-Pfad)
- Deterministische Ops-pro-Sekunde über Benchmark-Profile messbar, siehe [performance.md](performance.md).
