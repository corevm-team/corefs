// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-std
//!
//! Std-spezifische Bindings und Konvenienz-Helfer für CoreFS.
//!
//! `corefs-core` ist `no_std + alloc`-fähig und kennt absichtlich keine
//! Plattform-IO. Diese Crate liefert die std-seitigen Implementierungen
//! der `corefs-core`-Traits ([`Clock`], [`Rng`]) und ist langfristig
//! der Sammelpunkt für alle dateibasierten Backends
//! (`FileImageDevice`, `RawBlockDevice`, `MemoryDevice`), die heute
//! noch im main `corefs` crate liegen.
//!
//! ## Aktueller Status (Phase 5.1)
//!
//! - [`SystemClock`] — Re-Export aus `corefs-core` (std-feature-gated dort)
//! - [`StdRng`] — `Rng`-Impl basierend auf einem Thread-lokalen Seed,
//!   mit Re-Seed via `std::time::SystemTime`. Nicht kryptografisch.
//!
//! In späteren Schritten wandern die std-basierten Block-Devices
//! (`FileImageDevice`, `RawBlockDevice`) hierher, sobald
//! `corefs-core` die `BlockDevice`-Trait-Definition aufnimmt.
//!
//! ## Nicht-Ziele
//!
//! Diese Crate enthält nichts, was AnyOS direkt verwendet —
//! AnyOS-Kerneltreiber und -Userspace-Apps linken `corefs-core`
//! direkt und liefern eigene Plattform-Implementierungen.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub use corefs_core::platform::{Clock, Rng, SystemClock, Timestamp};

mod rng;
pub use rng::StdRng;

/// Versions-String der `corefs-std`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
