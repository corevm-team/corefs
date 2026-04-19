# Tests

Gesamtstand: **~979 Tests grün** (Unit + Integration + E2E + Stress + Fault-Injection + Concurrency).

## Ausführung

```bash
cargo test                            # alle Tests
cargo test <modulname>                # gezielt
cargo test -- --nocapture             # mit stdout
cargo test --release                  # optimiert (Stress/Perf-Tests)
```

Alle Module tragen ihre Tests inline (`mod <name>_tests;`) bzw. in separaten `*_tests.rs`-Dateien neben dem Modul.

## Testverteilung (Überblick, 83 Testmodule)

| Schicht | Fokus | Beispiele |
|---|---|---|
| **Domain** | Struct-Roundtrips, Invarianten | `inode_tests`, `metadata_tests`, `snapshot_tests`, `acl_tests`, `volume_tests` |
| **Storage (Block)** | CoW, Allocator, Catalog, WAL | `block_device_tests`, `block_store_tests`, `block_store_characterization_tests`, `allocator_tests`, `catalog_tests`, `volume_wal_tests`, `volume_session_tests` |
| **Storage (Image)** | Format, Reparatur, Backup | `volume_image_tests`, `backup_tests` (14) |
| **Storage (ODF)** | Superblock, Inode, Extent, Journal, fsck | 30+ Module in `ondisk/` |
| **Services** | Journal, Versioning, Encryption, Compression, Recovery, Quota, Hot-Paths, Indexing, Semantic | 12 Module |
| **App** | End-to-End über `CoreFsService`, Concurrency, Stress, Fault-Injection | `app_tests` (75), `concurrency_tests` (10), `fault_injection_tests` (10), `stress_tests` (10), `content_roundtrip_characterization_tests` |
| **Platform** | FUSE, Performance, Runtime, Diagnostics, Tools | `linux_fuse_tests` (27), `performance_tests`, `diagnostics_tests`, `runtime_tests`, `tools_tests` |
| **CLI / Config / Error** | Parsing, Validation | `cli_tests`, `config_tests`, `error_tests` |
| **Security** | SHA-256, HMAC, HKDF, Keystore | NIST-, RFC-4231-, RFC-5869-Vektoren (15 Tests) |

## Schwerpunkte

- **CoW & Blob-Sharing** — Ref-Counting, Materialisierung, Shrink.
- **Snapshot-Lifecycle** — create / restore / diff / delete, scoped + voll.
- **Encryption-Pipeline** — Roundtrips, Tamper-Detection, Compression×Encryption-Reihenfolge.
- **Integrität** — CRC32C, Superblock-Fallback, Segment-Directory-Reconstruction.
- **Block-Device** — Alignment, TRIM, R/O-Erkennung, Fake-Stick-Simulation.
- **FUSE** — Caching, Snapshot-Overlays, Time-Travel, Streaming-Writes, Race-freie Handles.
- **Backup** — full/incremental Roundtrip, CRC-Detection, truncated-stream, volume-id-stability.

## Concurrency, Fault-Injection, Stress

| Kategorie | Tests | Abdeckung |
|---|---|---|
| Concurrency | `src/app/concurrency_tests.rs` (10) | Send/Sync-Bounds, Arc+Mutex-Serialisierung, Snapshot-Isolation, Worker-Thread-Handoff |
| Fault-Injection | `src/app/fault_injection_tests.rs` (10) | ENOSPC-Recovery, Power-Loss-Simulation, Silent-Corruption-Detection |
| Stress | `src/app/stress_tests.rs` (10) | 2000 Dateien, 300 Verzeichnistiefen, Snapshot-Churn, Clone-Kaskaden, Dedup-Druck |

## Externe Test-Suiten

Verzeichnis: [scripts/testing/](../scripts/testing/).

### pjdfstest (POSIX-Compliance)

```bash
./scripts/testing/run-pjdfstest.sh
```

Prüft: `chmod`, `chown`, `link`, `symlink`, `mkdir`, `rmdir`, `open`, `rename`, `unlink`, `truncate`, `utimensat`.

Hinweis: `link`-Tests zeigen erwartungsgemäss Fails, da Hardlinks nicht implementiert sind.

### xfstests (Kernel-FS-Standard)

```bash
sudo ./scripts/testing/run-xfstests.sh
```

Gruppen: `generic/quick`, `generic/posix`, `generic/perms`, `generic/attr`, `generic/rw`, `generic/auto`.

### Eigener Stresstest

```bash
./scripts/testing/run-stress.sh --workers 8 --duration 300
```

Parallele Workloads: File-Ops, Dir-Ops, Rename, Concurrent Append. Anschliessend automatisch `fsck`.

### Gesamtlauf

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

## Integrationsszenarien

- [tests/fuse_handler_e2e.rs](../tests/fuse_handler_e2e.rs) — End-to-End Rust-Test: mount + Shell-Ops + unzip + Unmount + Revalidierung.
- [scripts/corefs-e2e-linux-rw.sh](../scripts/corefs-e2e-linux-rw.sh) — gleicher Ablauf als Shell-Skript.

## Offene Punkte / Verbesserungsbedarf

- **Performance-Regression-Gate** (P1): Benchmark-History existiert, automatisches Fail bei Schwellwert-Überschreitung fehlt.
- **Coverage-Messung**: nicht integriert (z. B. `cargo-llvm-cov`).
- **Mutations-Testing**: nicht vorhanden.
- **xfstests vollständige Suite**: aktuell nur kuratierte Gruppen; generic-Voll-Run benötigt Hardlinks.
- **Multi-Volume-Szenarien**: keine dedizierten Tests für parallel gemountete Volumes.

## Commit-Regel

Aus [CLAUDE.md](../CLAUDE.md):

> Laufen alle Tests erfolgreich durch (`cargo test`), wird unmittelbar ein Commit erstellt. **Kein Commit bei fehlschlagenden Tests.**
