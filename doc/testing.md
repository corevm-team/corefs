# Tests

Gesamtstand aktuell: **283 / 283 Unit-Tests grün**.

## Unit-Tests (in `src/`)

Alle Module tragen ihre Tests inline (`#[cfg(test)] mod tests { ... }`). Ausführung:

```bash
cargo test                            # alle Tests
cargo test <modulname>                # gezielt
cargo test -- --nocapture             # mit stdout
cargo test --release                  # optimiert
```

### Testverteilung nach Schicht

| Schicht | Module | ~Anzahl | Schwerpunkte |
|---|---|---:|---|
| Storage | `block_device.rs` | 60 | Alignment, Memory/File-Devices, TRIM, R/O |
| Storage | `block_store.rs` | 21 | CoW, Dedup, Defrag, Hot-Path |
| Storage | `volume_image.rs` | 12 | Format, Superblock, Segmenttabellen, Reparatur |
| Storage | `allocator`, `catalog`, `volume_wal`, `volume_session` | 10 | WAL-Ops, Session-Lifecycle |
| App | `mod.rs`, `tests.rs` | 75 | Datei-Ops, Snapshots, Klonen, Encryption |
| Platform | `linux_fuse.rs` | 27 | Caching, Snapshots, Time-Travel |
| Platform | `performance`, `diagnostics`, `runtime`, `tools` | 11 | Benchmark-Profile, Diagnose |
| Services | `encryption`, `compression`, `security` | 10 | ChaCha20, LZ4, Tamper |
| Services | `integrity`, `recovery`, `journal` | 12 | Scrubbing, fsck, Replay |
| Services | `versioning`, `metadata`, `quota` | 7 | Version-Pruning, Quota |
| Services | sonstige | 4 | je Basistest |
| Domain | alle | 4 | je Basistest |
| CLI/Config | `cli`, `config`, `error` | 7 | Kommandozeile, Konfig |

### Gut abgedeckte Bereiche

- **Copy-on-Write & Blob-Sharing** (~9 Tests)
- **Snapshot-Lifecycle** (~15 Tests)
- **Encryption-Pipeline** (~6 Tests)
- **Integritäts- & Recovery-Pfade** (~12 Tests)
- **Block-Device-Abstraktion** (~60 Tests)
- **FUSE-Integration** (~27 Tests)

## Externe Test-Suiten

Verzeichnis: [scripts/testing/](../scripts/testing/). Vollständige Referenz: [scripts/testing/TESTING.md](../scripts/testing/TESTING.md).

### pjdfstest (POSIX-Compliance)

```bash
./scripts/testing/run-pjdfstest.sh
```

Prüft: `chmod`, `chown`, `link`, `symlink`, `mkdir`, `rmdir`, `open`, `rename`, `unlink`, `truncate`, `utimensat`.

### xfstests (Kernel-FS-Standard)

```bash
sudo ./scripts/testing/run-xfstests.sh
```

Prüft Gruppen: `generic/quick`, `generic/posix`, `generic/perms`, `generic/attr`, `generic/rw`, `generic/auto`.

### Stresstest

```bash
./scripts/testing/run-stress.sh --workers 8 --duration 300
```

Parallele Workloads: File-Ops, Dir-Ops, Link/Rename, Concurrent Append. Anschließend automatisch `fsck`.

### Alle Suiten

```bash
./scripts/testing/run-all.sh                    # komplett
./scripts/testing/run-all.sh --quick            # nur pjdfstest + Stress
./scripts/testing/run-all.sh --skip-xfstests    # ohne xfstests
./scripts/testing/run-all.sh --keep             # Artefakte behalten
```

### Installation

```bash
./scripts/testing/install-test-suites.sh
```

Klont pjdfstest und xfstests nach `scripts/testing/suites/` und baut sie lokal.

## Integrationsszenarien

- [scripts/corefs-e2e-linux-rw.sh](../scripts/corefs-e2e-linux-rw.sh) — End-to-End: `mkfs-image` → RW-Mount → Shell-Ops → optional ZIP-Workload → Unmount → Revalidierung.

## Lücken (P0 / P1)

Status aktuell:
- **P0 Concurrency**: 0 Multi-Thread-Tests. Race-Conditions unter CoW-Materialisierung, parallele Snapshots, Ref-Count-Mutationen — nicht abgedeckt.
- **P0 Fault-Injection**: 0 Tests. ENOSPC-Recovery, partielle I/O-Fehler, Bit-Rot, Journal-Korruption, Power-Loss — nicht simuliert.
- **P0 Stress & Skalierung**: 10k+ Dateien/Dir, 100+ MB Writes, tiefe Bäume (500+), Langläufer — nicht gemessen.
- **P1 Performance-Regression-Gate**: Benchmarks existieren, aber keine Assertions / Schwellwerte.

## Commit-Regel

Aus [CLAUDE.md](../CLAUDE.md):

> Laufen alle Tests erfolgreich durch (`cargo test`), wird unmittelbar ein Commit erstellt. **Kein Commit bei fehlschlagenden Tests.**
