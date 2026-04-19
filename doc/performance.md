# Performance-Tooling

Status: ✅ Framework produktiv, 4 Profile, Markdown-Logging, JSON-History.

Implementation: `src/platform/performance.rs` + `src/platform/tools.rs`. CLI: `benchmark`, `benchmark-once`.

## BenchmarkProfile

```rust
pub enum BenchmarkProfile {
    Dev           { file_count: usize, iterations: usize },
    Ci            { file_count: usize, iterations: usize },
    Regression    { file_count: usize, iterations: usize },
    StorageHeavy  { file_count: usize, file_size: usize, iterations: usize },
}
```

| Profil | files | payload | iter | Einsatzzweck |
|---|---:|---:|---:|---|
| `dev` | 10 | 1 KiB | 3 | schnelles Dev-Feedback |
| `ci` | 100 | 1 KiB | 5 | CI-Pipeline |
| `regression` | 50 | 1 KiB | 5 | Nightly-Regression |
| `storage-heavy` | 1000 | 100 MiB | 3 | Langlauf / Storage-Stress |

Zusätzlich existieren feingranulare Custom-Profile (`balanced`, `small-files`, `metadata-heavy`, `snapshot-heavy`, `persist-heavy`) für gezielte Teiltests.

## Metriken (pro Suite)

- `create_ms`, `read_ms`, `write_ms`
- `snapshot_ms`, `restore_ms`
- `save_ms` (Image-Persistenz), `incremental_save_ms`
- `defrag_ms`, `dedup_ms`
- `cow_clone_ms`
- Durchsatz (MiB/s) und Ops/s
- Incremental-Persist-Delta (Phase 1f)

## Kommandos

```bash
# Vollständige Suite
corefs benchmark --profile ci

# Einmaliger Run
corefs benchmark-once snapshot-heavy --files 100 --payload 512 --snapshots 5

# Markdown-Log appendieren
corefs benchmark --profile regression --log PERFORMANCE_LOG.md
```

## History & Vergleich

- Markdown-Log: [PERFORMANCE_LOG.md](../PERFORMANCE_LOG.md) — chronologische Tabelle pro Run.
- JSON-History: `perf-history/<zeitstempel>_<profile>.tsv|json` — maschinenlesbar.
- Beim Start eines Runs erfolgt automatischer Abgleich mit der letzten Messung desselben Profils (Delta-Ausweisung).

## FUSE-spezifische Performance-Eigenschaften

- `FUSE_WRITEBACK_CACHE` aktiviert → Kernel puffert Schreibvorgänge.
- `max_write = 1 MiB` → grössere Batches pro FUSE-Write-Request.
- **Streaming-Writes**: ab ≥ 32 MiB → Zwischenflushes, Peak-RAM O(32 MiB) statt O(File-Size).
- **Handle-Level Read-/Write-Cache**.

## Block-Device-Performance

- On-Demand Segment-I/O über `DeviceVolume` (LRU-Read-Cache + Write-Buffer).
- Inkrementelle Persistenz: `persist_to_device_incremental()` schreibt nur geänderte Segmente (Phase 1f). Fallback auf Full-Rewrite bei Layout-Wechseln.
- Barrier-safe WAL im Device-Journal (256 KiB hinter dem Volume).

## Diagnose

```bash
corefs diagnose-mount ./demo.img /tmp/mnt --create
```

Prüft Mount-Readiness (FUSE-Verfügbarkeit, Berechtigungen, Mount-Point-Status, Backend-Typ).

## Offene Punkte / Verbesserungsbedarf

- **Automatisches Regressions-Gate**: Vergleichs-Infrastruktur existiert, Assertion-Schwellwerte (CI-Fail bei > X % Regression) fehlen (⚠️).
- **Flamegraphs / pprof-Integration**: nicht vorhanden.
- **Latenz-Histogramme** statt nur Mittelwerte.
- **Multi-Thread-Benchmarks** für nebenläufige Workloads (CoreFsService ist bereits Arc+Mutex-basiert, aber keine Benchmark-Abdeckung der Skalierung).
