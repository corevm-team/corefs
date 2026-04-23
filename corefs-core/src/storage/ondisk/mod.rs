// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Bausteine des CoreFS On-Disk Format (ODF v1).
//!
//! Diese Untermodule bilden die `no_std`-fähige Basis der ODF-Schichten
//! (Layout, Checksumme, Bitmap, Inodes, Extents, Superblock, Allokator,
//! Journal, Fault-Injection, Tiering, …).  Seit der Wave-5-Migration sind
//! auch die ehemals std-gebundenen Treiber (`volume`, `native`, `grouped`,
//! `journaled`, `fsck_repair`, `reader`, `scrub`) strikt `no_std + alloc` —
//! sie nutzen `bincode` 2.x (no_std-fähig) via [`crate::bincode_compat`]
//! zur PersistedState-Serialisierung.  Nur `session`, `concurrency`,
//! `benchmark`, `resilience`, `stress`, `property` verbleiben im main
//! `corefs` crate (std-gebundene Dateisystem-/Thread-/Zeit-Nutzung).

pub mod allocator;
pub mod attr_block;
pub mod bitmap;
pub mod block_group;
pub mod checksum;
pub mod dir_entry;
pub mod extent_tree;
pub mod fault_injection;
pub mod fsck;
pub mod inode;
pub mod journal;
pub mod journal_capture;
pub mod layout;
pub mod multi_group_allocator;
pub mod refcount;
pub mod superblock;
pub mod tiering;
pub mod xattr;

// Treiber-Module (Volume-Blob-Mode, Native-Layout, Grouped, Journaled,
// FSCK-Repair, Reader, Scrub).  Seit der Wave-5-Migration kompilieren sie
// strikt `no_std + alloc` — `bincode` 2.x ist no_std-fähig und über
// `crate::bincode_compat` mit dem klassischen 1.x-Wire-Format gespiegelt.
pub mod fsck_repair;
pub mod grouped;
pub mod journaled;
pub mod native;
pub mod reader;
pub mod resize;
pub mod scrub;
pub mod session;
pub mod tier;
pub mod volume;

pub use checksum::Crc32c;
