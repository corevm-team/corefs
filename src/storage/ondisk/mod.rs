// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # CoreFS On-Disk Format (ODF v1)
//!
//! This module implements the first production-oriented, block-oriented
//! on-disk format of CoreFS.  It lives alongside the legacy
//! [`crate::storage::volume_image`] segment-frame format and can be selected
//! explicitly by callers that want the new layout.
//!
//! ## Design goals
//!
//! * **Block-oriented.** The volume is divided into fixed 4 KiB blocks.
//!   Every operation reads and writes at block granularity.
//! * **Self-describing.** A [`superblock::Superblock`] at block 1 identifies
//!   the volume, records the geometry of every region and carries an UUID
//!   plus human-readable label for admin tooling.
//! * **Redundant.** Two additional superblock copies are kept at the
//!   middle and last block of the volume.  The loader transparently falls
//!   back to them when the primary copy is unreadable.
//! * **Checksummed.** Every control structure (superblock, inode record,
//!   payload blob) carries a CRC32C checksum (Castagnoli polynomial,
//!   `0x1EDC6F41`).  Single-bit corruption is detected on read.
//! * **Versioned.** The superblock exposes a major/minor version and
//!   three feature-flag fields (`compat`, `incompat`, `ro_compat`) that let
//!   newer volumes refuse to be mounted by older builds.
//! * **Extent-based.** Inodes reference their data through inline extents
//!   (up to eight per inode in v1) instead of fixed block pointers.  This
//!   scales from a single 4 KiB block up to the full data region in one
//!   contiguous run.
//! * **Payload isolation.** The `INCOMPAT_PAYLOAD_INODE` feature flag
//!   signals that the domain [`PersistedState`] blob is stored in the
//!   system inode (index 0).  Follow-up format revisions may replace it
//!   with per-user inodes and a directory hierarchy on disk; the feature
//!   flag is the integration point.
//!
//! ## Layout
//!
//! See [`layout`] for the exact block allocation and the geometry planner.
//!
//! ## Public API
//!
//! * [`volume::format_device`] — initialise a fresh volume.
//! * [`volume::save_state`]    — persist a [`PersistedState`] snapshot.
//! * [`volume::load_state`]    — read a previously saved snapshot.
//! * [`volume::inspect`]       — structural health report without reading
//!   the payload.
//!
//! ## Module overview
//!
//! | module          | responsibility                                          |
//! |-----------------|---------------------------------------------------------|
//! | [`layout`]      | block-level geometry planner + constants                 |
//! | [`checksum`]    | CRC32C Castagnoli primitive                              |
//! | [`superblock`]  | structured 4 KiB superblock, version + feature flags     |
//! | [`bitmap`]      | block / inode allocation bitmaps                         |
//! | [`allocator`]   | first-fit / best-fit allocator over the bitmap pair      |
//! | [`inode`]       | fixed 256 B on-disk inode record with extents            |
//! | [`extent_tree`] | indirect-block chain for large-extent-count inodes       |
//! | [`attr_block`]  | per-inode bincode attr block (holds serialized `Inode`)  |
//! | [`xattr`]       | structured xattr + ACL block (format-neutral payload)    |
//! | [`dir_entry`]   | 4 KiB directory-entry blocks with chaining               |
//! | [`journal`]     | transactional write-ahead log (WAL)                      |
//! | [`fsck`]        | read-only structural consistency checker                 |
//! | [`native`]      | native per-inode layout (alternative to the blob mode)   |
//! | [`volume`]      | blob-mode driver (`format_device`, `save_state`, …)      |
//!
//! ## Open architectural questions (not yet implemented in v1)
//!
//! * **Block groups** — a multi-group layout (like ext4) would split the
//!   data region into independently-bitmapped zones, improving allocation
//!   locality and paralleling `fsck`.  ODF v1 is single-group on purpose:
//!   adding groups later is an incompat-feature bump that can reuse the
//!   existing allocator and fsck walker per-group without changing the
//!   inode / extent / journal formats.
//! * **Incremental saves** — the current [`native::save_state_native`]
//!   rewrites every allocated inode slot.  Dirty-inode tracking + a
//!   `save_state_native_incremental` path is a natural follow-up because
//!   the journal already provides the transactional primitives needed
//!   (see [`journal::TxnBuilder`]).
//! * **Repair** — [`fsck::check`] is read-only; a companion
//!   `fsck::repair` would consume an `FsckReport` and emit targeted
//!   journal transactions through [`journal::TxnBuilder`] to restore
//!   consistency.
//!
//! ## Testing
//!
//! Every sub-module has a sibling `*_tests.rs` unit-test file.  Integration
//! tests covering whole-volume roundtrips live in [`volume_tests`].  The
//! suite exercises formatting, payload roundtrip, CRC detection, redundant
//! superblock fallback and sizing limits on a [`MemoryDevice`][mem].
//!
//! [`PersistedState`]: crate::app::PersistedState
//! [mem]: crate::storage::block_device::MemoryDevice

pub mod allocator;
pub mod attr_block;
pub mod benchmark;
pub mod bitmap;
pub mod block_group;
pub mod checksum;
pub mod concurrency;
pub mod dir_entry;
pub mod extent_tree;
pub mod fault_injection;
pub mod fsck;
pub mod fsck_repair;
pub mod grouped;
pub mod inode;
pub mod journal;
pub mod journaled;
pub mod layout;
pub mod multi_group_allocator;
pub mod native;
pub mod reader;
pub mod refcount;
pub mod resilience;
pub mod session;
pub mod stress;
pub mod superblock;
pub mod volume;
pub mod xattr;

pub use volume::{
    FormatOptions, FormatReport, SaveReport, VolumeInfo, format_device, inspect, load_state,
    save_state,
};
