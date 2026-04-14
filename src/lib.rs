// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # CoreFS — a portable, enterprise-grade filesystem in pure Rust
//!
//! CoreFS is a platform-neutral filesystem designed first as the
//! native filesystem of the author's own operating system, and second
//! as a library that exposes every layer of the implementation for
//! embedding into other tools (test harnesses, admin tooling,
//! migration paths from legacy formats).
//!
//! The crate is strictly layered — every module sits in exactly one
//! of the six top-level namespaces and depends only on the ones
//! below it on this list:
//!
//! | layer             | module          | responsibility                                                                 |
//! |-------------------|-----------------|--------------------------------------------------------------------------------|
//! | entry points      | [`cli`]         | administrative CLI (`mkfs`, `fsck-odf`, `mount-odf`, …)                        |
//! | orchestration     | [`app`]         | [`app::CoreFsService`] — the facade tying every service together              |
//! | services          | [`services`]    | journaling, integrity, versioning, encryption, compression, quota, hot paths   |
//! | storage           | [`storage`]     | block device abstraction, in-memory block store, ODF on-disk format           |
//! | domain            | [`domain`]      | pure business types (`Inode`, `FileMetadata`, `Snapshot`, `VolumeDescriptor`) |
//! | infrastructure    | [`error`], [`config`], [`platform`] | error type, config schema, OS integration (FUSE, diagnostics) |
//!
//! ## On-disk format (ODF v1)
//!
//! The production-ready block-oriented format lives in
//! [`storage::ondisk`].  It is the canonical way to persist a CoreFS
//! volume on a [`storage::block_device::BlockDevice`] (file image,
//! USB stick, partition) with full crash consistency.
//!
//! ### Features at a glance
//!
//! * Fixed 4 KiB block geometry with dedicated regions for block
//!   bitmap, inode bitmap, inode table, journal and data.
//! * Three redundant superblock copies (primary + N/2 + N−1), each
//!   CRC32C-protected.
//! * Two layout modes behind the `layout_mode` field: a compact
//!   **blob** mode that inlines the full `PersistedState` into a
//!   system payload inode, and a **native** per-inode mode that
//!   gives every domain inode its own on-disk slot with extents,
//!   attr block and per-file `data_crc`.
//! * Two incompat-feature-gated extensions on top of native:
//!   **block groups** (per-group bitmaps, locality-preserving
//!   allocation) and **physical copy-on-write** (u16 refcount per
//!   data block).
//! * Transactional WAL with ordered-mode writes: metadata goes
//!   through [`storage::ondisk::journaled::JournaledSaveSession`],
//!   replayed at mount time via
//!   [`storage::ondisk::journaled::recover_pending_transactions`].
//! * Read-only fsck walker + crash-consistent fsck::repair pass that
//!   emits corrective ops as a single journal transaction.
//! * Self-healing scrubber that combines fsck, auto-repair and
//!   per-file data_crc verification in one end-to-end call.
//! * Hot/cold tiering primitive —
//!   [`storage::ondisk::tiering::TieredDevice`] routes I/O between
//!   two block devices based on a [`storage::ondisk::tiering::TierMap`].
//! * Session layer ([`storage::ondisk::session::OdfFileSession`] +
//!   [`storage::ondisk::session::OdfDeviceSession`]) that glues a
//!   CoreFS service instance to an ODF volume; every mutate call
//!   goes through an incremental save.
//! * On-demand reader
//!   ([`storage::ondisk::reader::OdfReader`]) for admin tools that
//!   want per-inode access without materialising the full state.
//!
//! ### Quick-start
//!
//! ```no_run
//! use corefs::config::CoreFsConfig;
//! use corefs::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
//!
//! let mut opts = OdfSessionOptions::with_defaults();
//! opts.capacity_bytes = 16 * 1024 * 1024;
//! opts.config = CoreFsConfig::default();
//!
//! // Format a fresh ODF volume in /tmp/demo.odf and mutate it.
//! let mut sess = OdfFileSession::format_new("/tmp/demo.odf", &opts)?;
//! sess.mutate(|fs| {
//!     fs.create_directory("/docs")?;
//!     fs.create_file("/docs/readme.txt", b"hello corefs", &[])?;
//!     Ok(())
//! })?;
//!
//! // Reopen and verify.
//! let sess = OdfFileSession::open("/tmp/demo.odf")?;
//! assert!(sess.service().list_paths().contains(&"/docs/readme.txt".to_string()));
//! # Ok::<(), corefs::error::CoreFsError>(())
//! ```
//!
//! ## Testing model
//!
//! The crate follows a strict `*_tests.rs` sibling convention: every
//! module `foo.rs` has a paired `foo_tests.rs` that runs as a child
//! `#[cfg(test)] mod tests;` submodule.  `cargo test` exercises
//! the full suite (see `PROJECT_PROGRESS.md` for the current
//! green-test count).
//!
//! ## Error model
//!
//! All public fallible APIs return [`error::CoreFsResult<T>`],
//! aliasing `Result<T, error::CoreFsError>`.  The error enum
//! separates validation errors, policy violations, not-found
//! errors, state-consistency errors and unsupported commands.
//!
//! ## Feature matrix
//!
//! | flag                                   | meaning                                                                          |
//! |----------------------------------------|----------------------------------------------------------------------------------|
//! | `FEATURE_INCOMPAT_PAYLOAD_INODE`       | blob mode — `PersistedState` lives in the system payload inode                   |
//! | `FEATURE_INCOMPAT_BLOCK_GROUPS`        | grouped layout — data region carved into per-group bitmaps                       |
//! | `FEATURE_INCOMPAT_PHYSICAL_COW`        | per-data-block u16 refcount table ([`storage::ondisk::refcount`])               |
//! | `FEATURE_COMPAT_BLOCK_CHECKSUMS`       | each control block carries a CRC32C trailer (always on for ODF v1)              |
//! | `FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS` | tertiary + secondary SB copies written on every save                            |
//!
//! A reader that doesn't understand a required incompat flag must
//! refuse to mount the volume.  `FEATURE_COMPAT_*` flags are purely
//! informational — readers may ignore what they don't recognise.

pub mod app;
pub mod cli;
pub mod config;
pub mod domain;
pub mod error;
pub mod platform;
pub mod services;
pub mod storage;
