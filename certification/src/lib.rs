// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! CoreFS production certification harness.
//!
//! This crate intentionally lives outside the product crates.  It exercises
//! CoreFS through public APIs and tool frontends so certification evidence does
//! not depend on private module access.

/// Certification-suite version marker.
pub const CERTIFICATION_SUITE: &str = "corefs-certification-v1";
