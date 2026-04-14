// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Re-Export des plattformneutralen Fehlertyps aus
//! [`corefs_core::error`].
//!
//! Der Typ liegt in der `corefs-core`-Crate, damit er auch im
//! AnyOS-Kernel ohne `std` verfügbar ist. Im main `corefs` crate
//! erreichen Konsumenten ihn unverändert über `crate::error::CoreFsError`
//! / `crate::error::CoreFsResult`.

pub use corefs_core::error::{CoreFsError, CoreFsResult};
