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

## Platform Scope

The Rust tests are host-neutral and run on Windows, Linux and AnyOS-compatible
userspace targets that can link `std`. Linux-specific POSIX suites (`pjdfstest`
and `xfstests`) remain in `scripts/testing/` and should be attached as external
evidence for kernel/FUSE certification.
