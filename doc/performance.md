# Performance-Tooling

Framework: [src/platform/performance.rs](../src/platform/performance.rs) (~12 KB). CLI-Einstieg: `benchmark`, `benchmark-log`.

## BenchmarkConfig

```rust
pub struct BenchmarkConfig {
    pub profile: BenchmarkProfile,
    pub file_count: usize,
    pub payload_size: usize,
    pub snapshot_count: usize,
    pub persist_runs: usize,
}
```

## Profile

| Profil | files | payload | snapshots | saves | Fokus |
|---|---:|---:|---:|---:|---|
| `balanced` | 4 | 64 B | 1 | 1 | Ausgewogene Last |
| `small-files` | 100 | 16 B | 1 | 1 | Viele kleine Dateien |
| `metadata-heavy` | 200 | 0 B | 0 | 1 | Metadaten-Operationen |
| `snapshot-heavy` | 8 | 256 B | 5 | 1 | Snapshot-intensiv |
| `persist-heavy` | 8 | 256 B | 2 | 2 | Persistenz-Fokus |

## Kommandos

### Einmaliger Benchmark

```bash
cargo run --release -- benchmark --profile snapshot-heavy \
    --files 100 --payload 512 --snapshots 5
```

Ausgabe:
- `create_ms`, `read_ms`, `snapshot_ms`, `save_ms`
- Durchsatz (MiB/s)
- `create_ops_per_sec`, `read_ops_per_sec`

### Mit Markdown-Log

```bash
cargo run --release -- benchmark-log ./PERFORMANCE_LOG.md --profile balanced
```

Appendet das Ergebnis als Zeile an [PERFORMANCE_LOG.md](../PERFORMANCE_LOG.md):

```
| Timestamp | Profile | Files | Payload | Snap | Saves | Create ms | Read ms | Snap ms | Save ms | MiB | Create ops/s | Read ops/s |
```

## Suites (konzeptionell)

Laut [CLAUDE.md](../CLAUDE.md) vordefinierte Suites:
- `dev`
- `ci`
- `regression`
- `storage-heavy`

Diese werden schrittweise ausgebaut. Benchmark-Ergebnisse sollen automatisch mit früheren Messungen vergleichbar werden (Regressions-Gate geplant).

## FUSE-spezifische Performance-Eigenschaften

- **`FUSE_WRITEBACK_CACHE`** aktiviert → Kernel puffert Writes.
- **`max_write = 1 MiB`** → größere Batches pro FUSE-Write-Request.
- **Streaming-Writes**: ab ≥ 32 MiB führen Zwischenflushes zu konstantem RAM-Verbrauch O(32 MiB).
- **Handle-Level-Cache** für Read/Write.

## Block-Device-Performance

- On-Demand Segment-I/O über [DeviceVolume](../src/storage/device_volume.rs).
- LRU-Read-Cache + Write-Buffer.
- Barrier-safe WAL im Device-Journal (256 KiB hinter dem Volume).

## Diagnose

```bash
cargo run -- diagnose-mount ./demo.img /tmp/mnt --create
```

Prüft Mount-Readiness (FUSE-Verfügbarkeit, Berechtigungen, Mount-Point-Status).
