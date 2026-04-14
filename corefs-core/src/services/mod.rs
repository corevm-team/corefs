// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Services des CoreFS-Kerns.
//!
//! Dieses Modul enthält Services, die keine `std`-Abhängigkeit benötigen und
//! daher im AnyOS-Kernel gelinkt werden können. Plattformspezifische Services
//! (z. B. `integrity`, `compression`) verbleiben in der main-Crate.

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
