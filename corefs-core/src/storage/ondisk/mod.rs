// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Bausteine des CoreFS On-Disk Format (ODF v1).
//!
//! Diese Untermodule bilden die `no_std`-fähige Basis der ODF-Schichten
//! (Layout, Checksumme, Bitmap, …). Komplexere Module (Volume, fsck,
//! Native-Layout, …) verbleiben vorerst im main `corefs` crate und
//! werden schrittweise migriert.

pub mod bitmap;
pub mod checksum;
pub mod layout;

pub use checksum::Crc32c;
