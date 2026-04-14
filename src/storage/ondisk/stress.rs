// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Stress- and scalability-style scenarios against the ODF session
//! layer.  These tests exercise the format beyond the size regimes
//! covered by the per-module unit tests and surface any O(n²) or
//! memory-bound behaviour hiding in the save / load paths.
//!
//! Every scenario runs against an in-memory [`MemoryDevice`] so the
//! test suite stays hermetic; no temp files or real sync barriers are
//! involved.  Scale is tuned to stay within a few hundred milliseconds
//! per test in debug builds.

#[cfg(test)]
#[path = "stress_tests.rs"]
mod tests;
