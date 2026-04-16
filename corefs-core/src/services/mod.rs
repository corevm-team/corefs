// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Services des CoreFS-Kerns.
//!
//! Dieses Modul enthält Services, die keine `std`-Abhängigkeit benötigen und
//! daher im AnyOS-Kernel gelinkt werden können. Feature-gated Services:
//! - [`compression`] — LZ4-Frame-Codec (benötigt `compression`-Feature, zieht `std` nach)
//! - [`encryption`] — ChaCha20-Poly1305 (benötigt `crypto`-Feature)
//!
//! Der plattformspezifische `integrity`-Service (`fsck_image`/`repair_image`
//! über `std::path`) verbleibt in der main-Crate.

#[cfg(feature = "compression")]
pub mod compression;
#[cfg(feature = "crypto")]
pub mod encryption;
pub mod hot_paths;
pub mod indexing;
pub mod journal;
pub mod metadata;
pub mod quota;
pub mod recovery;
pub mod security;
pub mod semantic;
pub mod sync;
pub mod versioning;
