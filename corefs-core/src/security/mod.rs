// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Sicherheitsprimitive: Keystore, KDF, Wrapping.
//!
//! Dieses Modul ist `no_std + alloc` und feature-gated unter `crypto`. Es
//! enthält eine pure-Rust-Implementierung von SHA-256 und HKDF-SHA256,
//! die im AnyOS-Kernel linkbar ist, sowie einen `Keystore`-Typ, der
//! Per-File-Schlüssel aus einem Master-Key ableitet und einen
//! Volume-Schlüssel unter einem Master-Key AEAD-wrappt.
//!
//! Siehe [`keystore`] für die zentrale API.

#[cfg(feature = "crypto")]
pub mod hkdf;
#[cfg(feature = "crypto")]
pub mod keystore;
#[cfg(feature = "crypto")]
pub mod sha256;
