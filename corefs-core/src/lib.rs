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
//! Die Migration läuft inkrementell. Die Module [`platform`], [`config`] und
//! [`domain`] sind bereits strikt `no_std + alloc` — sie kompilieren im
//! AnyOS-Kernel. `storage` und `services` folgen in späteren Schritten.
//!
//! ## Feature-Flags
//!
//! - `std` — aktiviert std-seitige Bequemlichkeiten: `Timestamp::now()`,
//!   [`platform::SystemClock`], `From<SystemTime>`/`Into<SystemTime>`-Konversionen,
//!   `Inode::new`/`touch_modified`/`touch_changed` (die Varianten ohne
//!   explizites `Timestamp`-Argument). Ohne dieses Feature ist die Crate
//!   strikt `no_std + alloc` und verwendet durchgängig `*_at(now: Timestamp)`-APIs.
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
pub mod config;
pub mod domain;
pub mod error;
pub mod services;
pub mod storage;

/// Versions-String der `corefs-core`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
