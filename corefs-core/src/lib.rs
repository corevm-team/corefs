// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-core
//!
//! Plattformneutrale Kern-Bibliothek des CoreFS-Dateisystems.
//!
//! Diese Crate enthält schrittweise die Domain- und Storage-Logik, die weder
//! von Linux noch von `std` abhängt. Sie ist langfristig `no_std`-fähig
//! (mit `alloc`) und kann sowohl in gewöhnliche Rust-Programme als auch in den
//! Kernel des AnyOS-Betriebssystems gelinkt werden.
//!
//! ## Aktueller Migrationsstand (Phase 5.2)
//!
//! Die Migration läuft inkrementell. Der `platform`-Modul ist bereits
//! strikt `no_std + alloc`. Die Module [`config`] und [`domain`] sind
//! aktuell noch an `std` gebunden (`std::time::SystemTime` in einigen
//! Domain-Typen) und daher hinter dem Feature-Flag `std` versteckt.
//! Spätere Schritte ersetzen `SystemTime` durch [`platform::Timestamp`]
//! und heben die Feature-Gates auf.
//!
//! ## Feature-Flags
//!
//! - `std` — aktiviert die Module [`config`] und [`domain`] sowie std-seitige
//!   Bequemlichkeiten (`From<SystemTime> for Timestamp`, zukünftig
//!   `std::error::Error`-Impls). Ohne dieses Feature ist die Crate strikt
//!   `no_std + alloc`.
//!
//! ## Plattform-Abstraktionen
//!
//! Der Kern kennt weder Uhren noch Zufallsquellen — beides wird über Traits im
//! Modul [`platform`] bereitgestellt. Der Aufrufer (Linux-Hostprogramm,
//! AnyOS-Kerneltreiber, Userspace-Daemon) liefert passende Implementierungen.
//!
//! ## Stabilitätsstatus
//!
//! Die Crate ist im Aufbau. Schnittstellen sind bis zum Abschluss der
//! no_std-Migration (`PROJECT_PROGRESS.md`, Abschnitt 5.2) instabil.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

extern crate alloc;

pub mod platform;

#[cfg(feature = "std")]
pub mod config;

#[cfg(feature = "std")]
pub mod domain;

/// Versions-String der `corefs-core`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
