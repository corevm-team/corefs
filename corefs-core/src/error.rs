// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Einheitlicher Fehlertyp für `corefs-core`-Operationen.
//!
//! [`CoreFsError`] ist `no_std + alloc`-fähig: alle Varianten tragen
//! eine `String`-Diagnose. Die `std::error::Error`-Integration ist hinter
//! dem `std`-Feature versteckt, damit der Typ auch im AnyOS-Kernel
//! kompiliert.

use alloc::string::String;
use core::fmt::{self, Display, Formatter};

/// Standardfehlertyp aller `corefs-core`-Operationen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreFsError {
    /// Eine angeforderte Ressource existiert bereits.
    AlreadyExists(String),
    /// Ein vom Aufrufer übergebenes Kommando ist syntaktisch ungültig.
    InvalidCommand(String),
    /// Eine Eingabe verletzt eine Vorbedingung (Alignment, Range, Format, …).
    InvalidInput(String),
    /// Eine angeforderte Ressource wurde nicht gefunden.
    NotFound(String),
    /// Eine Operation ist policy-seitig nicht erlaubt (Quota, ACL, …).
    PolicyViolation(String),
    /// Konsistenz- oder I/O-Fehler im Persistenz-Layer.
    State(String),
}

impl Display for CoreFsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists(message)
            | Self::InvalidCommand(message)
            | Self::InvalidInput(message)
            | Self::NotFound(message)
            | Self::PolicyViolation(message)
            | Self::State(message) => f.write_str(message),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for CoreFsError {}

/// Standard-Result-Typ aller `corefs-core`-Operationen.
pub type CoreFsResult<T> = Result<T, CoreFsError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
