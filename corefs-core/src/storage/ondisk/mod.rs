// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Bausteine des CoreFS On-Disk Format (ODF v1).
//!
//! Diese Untermodule bilden die `no_std`-fähige Basis der ODF-Schichten
//! (Layout, Checksumme, Bitmap, Inodes, Extents, Superblock, Allokator,
//! Journal, Fault-Injection, Tiering, …).  Komplexere Top-Level-Module
//! (Volume, fsck, Native-Layout, Reader, Scrub, Session, …) verbleiben
//! vorerst im main `corefs` crate.

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
pub mod layout;
pub mod multi_group_allocator;
pub mod refcount;
pub mod superblock;
pub mod tiering;
pub mod xattr;

pub use checksum::Crc32c;
