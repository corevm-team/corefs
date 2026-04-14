// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Re-Exporte des plattformneutralen Journal-Services.
//!
//! Der Kern (`JournalService`, `JournalEntry`, `JournalRuntimeState`,
//! `JournalReplayState`, `JournalRecoverySummary`, `JournalRepairSummary`,
//! `JournalTransaction`) sowie [`reconcile_persisted_state`] leben in
//! [`corefs_core::services::journal`]. Der Re-Export hier hält bestehende
//! `crate::services::journal::*`-Pfade lauffähig.

pub use corefs_core::services::journal::{
    JournalEntry, JournalRecoverySummary, JournalReplayState, JournalRepairSummary,
    JournalRuntimeState, JournalService, JournalTransaction, reconcile_persisted_state,
};

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
