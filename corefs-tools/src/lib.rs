// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-tools
//!
//! Programmatic APIs for CoreFS administrative operations — the canonical
//! home for `mkfs`, `fsck`, `scrub`, `dump`, `defrag`, `resize`, `tier` and
//! `snapshot`. Every operation is a pure function that takes an opened
//! device (or a device path), performs its work, and returns a structured
//! [`Report`] that can be rendered as text or as JSON.
//!
//! The Linux CLI (`corefs` binary) and the AnyOS userspace apps
//! (`mkfs.corefs`, `fsck.corefs`, …) are both thin frontends around the
//! same functions in this crate. That guarantees the two frontends stay
//! semantically identical.
//!
//! ## Design
//!
//! - **Pure-function APIs.** Each tool operation is reachable as a top-level
//!   function in its module, without global state. Callers supply the device
//!   they want to operate on.
//! - **Structured reports.** Return types implement the [`Report`] trait,
//!   which exposes both a one-line summary (`summary()`), a multi-line
//!   human-readable rendering (`render_text()`) and a JSON rendering
//!   (`render_json()`). Frontends pick the rendering that matches their UX.
//! - **Separation of concerns.** Argument parsing, output formatting and
//!   interactive prompts live in the frontend; `corefs-tools` is strictly
//!   non-interactive and never writes to stdout/stderr directly.
//! - **std for now.** The crate currently depends on the main `corefs`
//!   crate for storage/services implementations, which are still `std`.
//!   Once `corefs-core` absorbs the `storage` and `services` layers
//!   (Phase 5.2 follow-up), this crate can shrink to `no_std + alloc` and
//!   the AnyOS tool-apps can link it directly.
//!
//! ## Roadmap
//!
//! | Module       | Status | Scope                                                  |
//! |--------------|--------|--------------------------------------------------------|
//! | [`mkfs`]     | partial | `format_image` — format a fresh ODF volume in a file |
//! | [`fsck`]     | partial | `check_image` — read-only consistency check          |
//! | [`dump`]     | partial | `superblock`, `inode` — decode and display structures |
//! | [`scrub`]    | partial | `scrub_image` — fsck + repair + data-crc, three modes |
//! | [`repair`]   | partial | `repair_image` — applies auto-fixes from an fsck pass |
//! | `defrag`     | planned | live/offline defragmentation                          |
//! | `resize`     | planned | grow an existing volume                               |
//! | `tier`       | planned | hot/cold tiering policies                             |
//! | `snapshot`   | planned | create / list / delete / restore                      |

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod dump;
pub mod error;
pub mod fsck;
pub mod mkfs;
pub mod repair;
pub mod report;
pub mod scrub;

pub use error::{ToolsError, ToolsResult};
pub use report::Report;

/// Versions-String der `corefs-tools`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
