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
| `cert_040` | Snapshot toolchain, full backup, restore, defrag, repair, scrub and cross-image verification |
| `cert_050` | Data corruption injection and CRC scrub detection |
| `cert_060` | Primary superblock loss and redundant-superblock fallback |
| `cert_070` | Deterministic property matrix across operation seeds |
| `cert_080` | ODF format/save/load benchmark regression gates |

## Performance Gates

The defaults are intentionally portable and conservative. Certification labs can
tighten them per platform by setting:

- `COREFS_CERT_MIN_CRC_MIB_S`
- `COREFS_CERT_MAX_FORMAT_MS`
- `COREFS_CERT_MAX_NATIVE_SAVE_MS`
- `COREFS_CERT_MAX_NATIVE_LOAD_MS`

## Platform Scope

The Rust tests are host-neutral and run on Windows, Linux and AnyOS-compatible
userspace targets that can link `std`. Linux-specific POSIX suites (`pjdfstest`
and `xfstests`) remain in `scripts/testing/` and should be attached as external
evidence for kernel/FUSE certification.
