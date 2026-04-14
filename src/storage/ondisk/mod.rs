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
//! ## Testing
//!
//! Every sub-module has a sibling `*_tests.rs` unit-test file.  Integration
//! tests covering whole-volume roundtrips live in [`volume_tests`].  The
//! suite exercises formatting, payload roundtrip, CRC detection, redundant
//! superblock fallback and sizing limits on a [`MemoryDevice`][mem].
//!
//! [`PersistedState`]: crate::app::PersistedState
//! [mem]: crate::storage::block_device::MemoryDevice

pub mod bitmap;
pub mod checksum;
pub mod inode;
pub mod layout;
pub mod superblock;
pub mod volume;

pub use volume::{
    FormatOptions, FormatReport, SaveReport, VolumeInfo, format_device, inspect, load_state,
    save_state,
};
