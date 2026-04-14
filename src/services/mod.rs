// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Services-Layer des main-`corefs`-Crates.
//!
//! Die plattformneutralen Services leben in [`corefs_core::services`] und
//! werden hier re-exportiert, damit bestehende Konsumenten ihren historischen
//! Pfad `crate::services::…` behalten können.
//!
//! Plattformspezifische Services (die `std::io` oder `std::path` brauchen)
//! bleiben weiterhin hier im main-Crate beheimatet — aktuell:
//! - [`compression`] — LZ4-Frame-Codec via `std::io`
//! - [`integrity`] — `fsck_image` / `repair_image` über `std::path`
//!
//! Die `journal`-Funktionalität ist zweigeteilt: der Kern liegt in
//! [`corefs_core::services::journal`]; die `PersistedState`-spezifische
//! Reparatur [`journal::reconcile_persisted_state`] lebt lokal.

pub mod compression;
pub mod integrity;

pub mod journal;

pub use corefs_core::services::{
    encryption, hot_paths, indexing, metadata, quota, recovery, security, semantic, sync,
    versioning,
};
