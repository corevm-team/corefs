// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Enterprise-grade resilience scenarios that exercise the ODF save /
//! load paths against a [`FaultyDevice`].
//!
//! This module is test-only: it contains no production code, just the
//! end-to-end fault scenarios.  Its job is to convert the guarantees
//! documented in the journal / fsck / reader module headers into
//! mechanical assertions against real write failures, silent
//! corruption, aborted syncs and power-loss simulations.
//!
//! The scenarios live here (rather than as a sibling `_tests.rs` of
//! some other module) because each touches multiple layers at once:
//! journal + volume + native + fsck + reader.  Keeping them together
//! makes the intended invariants readable in one place.

#[cfg(test)]
#[path = "resilience_tests.rs"]
mod tests;
