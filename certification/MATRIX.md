# CoreFS Certification Matrix

This matrix is the executable certification scope for the root `certification`
crate. Every entry is either enforced by an automated Rust test or by an
explicit platform command listed below.

## Automated Rust Evidence

| ID | Requirement |
| --- | --- |
| `cert_001` | CRC32C vectors, streaming equivalence, throughput floor |
| `cert_010` | Native mkfs geometry, superblock fields, feature flags, CLI fsck JSON |
| `cert_011` | Native and blob layout identification and fsck cleanliness |
| `cert_020` | File, folder, symlink, rename, overwrite, delete, restore, reopen |
| `cert_021` | Explicit inode IDs, block records, extents, inode dump |
| `cert_022` | Range writes, sparse zero fill, truncate shrink/grow, reopen |
| `cert_023` | Quota limits, path validation, duplicate rejection |
| `cert_024` | Secure delete non-recoverability |
| `cert_030` | Encryption, compression, versioning, snapshot restore |
| `cert_031` | Keystore init, verify, rotate, wrong-key rejection |
| `cert_040` | Snapshot, backup, restore, defrag, repair, scrub toolchain |
| `cert_041` | Full + incremental backup, delete markers, truncated stream failure |
| `cert_042` | CLI JSON surface for admin operations |
| `cert_050` | Data corruption injection detected by scrub |
| `cert_051` | Structural corruption reported by fsck and repaired |
| `cert_060` | Redundant superblock fallback after primary loss |
| `cert_070` | Deterministic property matrix across seeds |
| `cert_071` | Threaded mutation model and in-memory scrub |
| `cert_072` | Multi-threaded high-IO ODF writer load with flush, snapshots, reopen, fsck |
| `cert_073` | Parallel ODF reopen readers under read IO load with payload verification |
| `cert_080` | ODF microbenchmark regression gates |
| `cert_081` | Service benchmark output and throughput metrics |
| `cert_090` | Cross-platform command manifest presence |
| `cert_100` | Snapshot diff, scoped restore, snapshot deletion, persistence |
| `cert_101` | Metadata owner/mode masking, version retention, journal persistence |
| `cert_102` | Clone tree, copy-on-write divergence, dedup pass, persistence |
| `cert_103` | Fragmentation, hot-path optimization, legacy image roundtrip |
| `cert_104` | Aborted ODF mutation does not persist partial changes |
| `cert_105` | Pending WAL replay after simulated crash/reload |
| `cert_110` | ODF xattr/ACL block CRC roundtrip and corruption rejection |
| `cert_120` | File creation exists, duplicate creation rejection, reopen |
| `cert_121` | File soft deletion: deleted from active namespace but recoverable/restorable |
| `cert_122` | Secure delete and expunge irrecoverability |
| `cert_123` | File overwrite, range write, append, truncate shrink/grow exact semantics |
| `cert_124` | Forced flush/fsync persistence boundary |
| `cert_130` | Generated daily-use matrix: 1,200 executable tests across file, folder, rename, quota, versioning, snapshot, recovery, dedup and persistence semantics |
| `cert_140` | Single-folder portable service load, duplicate rejection, inventory, throughput and fsck |
| `cert_141` | Many-folder breadth/depth load, deep leaf access, directory throughput and fsck |
| `cert_142` | Very large file write, range patch, truncate shrink/grow, zero-fill and throughput |
| `cert_143` | Mass identical-file deduplication plus copy-on-write clone sharing/divergence |
| `cert_144` | Heavy lab certification test: more than 100,000 files in one directory (`#[ignore]`, run explicitly) |

## Platform Commands

Windows:

```powershell
.\certification\run.ps1
cargo test -p corefs-certification -- --nocapture
cargo test --workspace
cargo check --features windows-winfsp --lib
cargo check --features windows-winfsp --bins
cargo check -p corefs-core --no-default-features
```

Linux:

```bash
./certification/run.sh
cargo test -p corefs-certification -- --nocapture
cargo test --workspace
cargo check -p corefs-core --no-default-features
./scripts/testing/run-pjdfstest.sh
sudo ./scripts/testing/run-xfstests.sh
./scripts/testing/run-stress.sh --workers 8 --duration 300
```

AnyOS:

```bash
cargo check -p corefs-core --no-default-features
cargo check -p corefs-core --no-default-features --features crypto
```

Scale knobs for certification labs:

```bash
COREFS_CERT_MASS_FILES=10000
COREFS_CERT_HEAVY_MASS_FILES=100001
COREFS_CERT_MASS_DIRS=2500
COREFS_CERT_DEEP_DIRS=128
COREFS_CERT_LARGE_FILE_BYTES=16777216
COREFS_CERT_DEDUP_IDENTICAL_FILES=1000
COREFS_CERT_DEDUP_COW_CLONES=1000
```

The external Linux suites remain outside the Rust certification crate because
they require host FUSE/kernel privileges. Their logs should be stored alongside
the generated `certification/evidence` directory for audit bundles.
