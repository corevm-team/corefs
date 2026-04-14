// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-core
//!
//! Plattformneutrale Kern-Bibliothek des CoreFS-Dateisystems.
//!
//! Diese Crate enthält die Domain- und Storage-Logik, die weder von Linux noch
//! von `std` abhängt. Sie ist `no_std`-fähig (mit `alloc`) und kann sowohl in
//! gewöhnliche Rust-Programme als auch in den Kernel des AnyOS-Betriebssystems
//! gelinkt werden.
//!
//! ## Feature-Flags
//!
//! - `std` (optional) — aktiviert `std`-Bequemlichkeiten wie `Display`-Impls auf
//!   Pfaden, `std::error::Error`-Integration und `From`-Konvertierungen zwischen
//!   [`Timestamp`](platform::Timestamp) und `std::time::SystemTime`. Ohne dieses
//!   Feature ist die Crate strikt `no_std + alloc`.
//!
//! ## Plattform-Abstraktionen
//!
//! Der Kern kennt weder Uhren noch Zufallsquellen — beides wird über Traits im
//! Modul [`platform`] bereitgestellt. Der Aufrufer (Linux-Hostprogramm,
//! AnyOS-Kerneltreiber, Userspace-Daemon) liefert passende Implementierungen.
//!
//! ## Stabilitätsstatus
//!
//! Aktuell ist die Crate im Aufbau. Die Schnittstellen sind bis zum Abschluss
//! der no_std-Migration (`PROJECT_PROGRESS.md`, Abschnitt 5.2) instabil.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

extern crate alloc;

pub mod platform;

/// Versions-String der `corefs-core`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
