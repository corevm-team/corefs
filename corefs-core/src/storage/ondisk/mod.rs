// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Bausteine des CoreFS On-Disk Format (ODF v1).
//!
//! Diese Untermodule bilden die `no_std`-fähige Basis der ODF-Schichten
//! (Layout, Checksumme, Bitmap, Inodes, Extents, Superblock, …).
//! Komplexere Module (Volume, fsck, Native-Layout, …) verbleiben
//! vorerst im main `corefs` crate und werden schrittweise migriert.

pub mod attr_block;
pub mod bitmap;
pub mod block_group;
pub mod checksum;
pub mod dir_entry;
pub mod extent_tree;
pub mod inode;
pub mod layout;
pub mod refcount;
pub mod superblock;
pub mod xattr;

pub use checksum::Crc32c;
