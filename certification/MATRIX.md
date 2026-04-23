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

The external Linux suites remain outside the Rust certification crate because
they require host FUSE/kernel privileges. Their logs should be stored alongside
the generated `certification/evidence` directory for audit bundles.
