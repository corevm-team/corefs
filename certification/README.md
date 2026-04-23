# CoreFS Certification Suite

This root-level suite is the production evidence harness for CoreFS. It is a
separate workspace crate on purpose: tests use public APIs, administrative tool
APIs and CLI dispatch, not private unit-test hooks.

## Run

```bash
cargo test -p corefs-certification -- --nocapture
```

For a full local evidence run:

```bash
COREFS_CERT_EVIDENCE_DIR=certification/evidence cargo test -p corefs-certification -- --nocapture
```

Windows PowerShell:

```powershell
$env:COREFS_CERT_EVIDENCE_DIR = "certification\evidence"
cargo test -p corefs-certification -- --nocapture
```

## Coverage Map

| ID | Area |
| --- | --- |
| `cert_001` | CRC32C vectors, streaming equivalence and throughput gate |
| `cert_010` | `mkfs`, geometry, feature flags, superblock dump and CLI fsck JSON |
| `cert_020` | File, directory, symlink, range write, append, truncate, rename, overwrite, delete, restore and reopen |
| `cert_030` | Encryption-at-rest, compression, versioning, snapshot restore and plaintext-at-rest checks |
| `cert_031` | Keystore init, verify, rotate and wrong-key rejection |
| `cert_040` | Snapshot toolchain, full backup, restore, defrag, repair, scrub and cross-image verification |
| `cert_041` | Full and incremental backup, delete markers and truncated-stream failure |
| `cert_042` | CLI JSON surface for admin commands |
| `cert_050` | Data corruption injection and CRC scrub detection |
| `cert_051` | Structural corruption detection and repair |
| `cert_060` | Primary superblock loss and redundant-superblock fallback |
| `cert_070` | Deterministic property matrix across operation seeds |
| `cert_071` | Threaded mutation model and in-memory scrub |
| `cert_072` | Multi-threaded high-IO ODF writer load, flush, snapshots, reopen and fsck |
| `cert_073` | Parallel ODF reopen readers with payload verification and throughput evidence |
| `cert_080` | ODF format/save/load benchmark regression gates |
| `cert_081` | Service benchmark output and throughput metrics |
| `cert_090` | Cross-platform command manifest presence |
| `cert_100` | Snapshot diff, scoped restore, snapshot deletion and persistence |
| `cert_101` | Owner/mode metadata, permission masking, version retention and journal persistence |
| `cert_102` | Clone tree, copy-on-write divergence, dedup pass and persistence |
| `cert_103` | Fragmentation, hot-path optimization and legacy image roundtrip |
| `cert_104` | Aborted ODF mutation rollback-at-disk boundary |
| `cert_105` | Pending WAL replay after simulated crash/reload |
| `cert_110` | ODF xattr/ACL block CRC roundtrip and corruption rejection |
| `cert_120` | File creation existence, duplicate rejection and reopen |
| `cert_121` | Soft deletion removes from active namespace but remains recoverable/restorable |
| `cert_122` | Secure delete and expunge are irrecoverable |
| `cert_123` | Overwrite, range write, append and truncate exact semantics |
| `cert_124` | Forced flush/fsync persistence boundary |
| `cert_130` | 1,200 generated daily-use tests across file, folder, rename, quota, versioning, snapshot, recovery, dedup and persistence semantics |
| `cert_140` | Single-folder portable load, duplicate rejection, inventory, throughput and fsck |
| `cert_141` | Many-folder breadth/depth load, deep leaf access, directory throughput and fsck |
| `cert_142` | Very large file write, range patch, truncate shrink/grow, zero-fill and throughput |
| `cert_143` | Mass identical-file deduplication plus copy-on-write clone sharing/divergence |
| `cert_144` | Heavy lab load with more than 100,000 files in one directory, run explicitly with ignored tests |

## Performance Gates

The defaults are intentionally portable and conservative. Certification labs can
tighten them per platform by setting:

- `COREFS_CERT_MIN_CRC_MIB_S`
- `COREFS_CERT_MAX_FORMAT_MS`
- `COREFS_CERT_MAX_NATIVE_SAVE_MS`
- `COREFS_CERT_MAX_NATIVE_LOAD_MS`
- `COREFS_CERT_IO_WORKERS`
- `COREFS_CERT_IO_OPS_PER_WORKER`
- `COREFS_CERT_IO_BATCH_SIZE`
- `COREFS_CERT_IO_PAYLOAD_BYTES`
- `COREFS_CERT_IO_READERS`
- `COREFS_CERT_MIN_IO_WRITE_MIB_S`
- `COREFS_CERT_MIN_IO_READ_MIB_S`
- `COREFS_CERT_MASS_FILES` (default: `10000`)
- `COREFS_CERT_HEAVY_MASS_FILES` (default: `100001`, used by ignored heavy lab test)
- `COREFS_CERT_MASS_DIRS`
- `COREFS_CERT_DEEP_DIRS`
- `COREFS_CERT_LARGE_FILE_BYTES`
- `COREFS_CERT_DEDUP_IDENTICAL_FILES`
- `COREFS_CERT_DEDUP_COW_CLONES`
- `COREFS_CERT_MIN_MASS_CREATE_OPS_S`
- `COREFS_CERT_MIN_DIR_CREATE_OPS_S`
- `COREFS_CERT_MIN_LARGE_FILE_MIB_S`

Heavy lab tests are intentionally excluded from the default Rust test run. Run
them explicitly when the certification host has the needed time and memory:

```bash
cargo test -p corefs-certification --test scale_load_suite -- --ignored --nocapture
```

## Platform Scope

The Rust tests are host-neutral and run on Windows, Linux and AnyOS-compatible
userspace targets that can link `std`. Linux-specific POSIX suites (`pjdfstest`
and `xfstests`) remain in `scripts/testing/` and should be attached as external
evidence for kernel/FUSE certification.
