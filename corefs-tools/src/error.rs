// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Einheitlicher Fehlertyp für alle Tool-Operationen.

use corefs::error::CoreFsError;
use std::fmt;
use std::io;

/// Standard-Result-Typ für alle `corefs-tools`-Operationen.
pub type ToolsResult<T> = Result<T, ToolsError>;

/// Fehler, der von einer Tool-Operation zurückgegeben werden kann.
///
/// Die Varianten fassen `CoreFsError`, `std::io::Error` und
/// Argument-Validierungsfehler unter einem einheitlichen Typ zusammen,
/// sodass Frontends (CLI, AnyOS-Apps) nur eine Fehlerhierarchie
/// behandeln müssen.
#[derive(Debug)]
pub enum ToolsError {
    /// Ein Fehler aus der CoreFS-Kern-Bibliothek (Block-I/O, Format-Prüfung, …).
    Core(CoreFsError),
    /// Ein Fehler bei Datei-/Pfad-Operationen außerhalb des Block-Device-Layers
    /// (typischerweise beim Öffnen der Image-Datei).
    Io {
        /// Beschreibender Kontext, z. B. `"open volume image /tmp/foo.img"`.
        context: String,
        /// Der zugrundeliegende `std::io::Error`.
        source: io::Error,
    },
    /// Ein Eingabeargument ist ungültig (z. B. Kapazität < minimal nötige Größe).
    InvalidArgument(String),
}

impl fmt::Display for ToolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolsError::Core(err) => write!(f, "corefs error: {err:?}"),
            ToolsError::Io { context, source } => write!(f, "io error ({context}): {source}"),
            ToolsError::InvalidArgument(msg) => write!(f, "invalid argument: {msg}"),
        }
    }
}

impl std::error::Error for ToolsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ToolsError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<CoreFsError> for ToolsError {
    fn from(value: CoreFsError) -> Self {
        ToolsError::Core(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_invalid_argument() {
        let e = ToolsError::InvalidArgument("capacity too small".to_string());
        assert_eq!(e.to_string(), "invalid argument: capacity too small");
    }

    #[test]
    fn display_io_wraps_context() {
        let e = ToolsError::Io {
            context: "open /tmp/x.img".to_string(),
            source: io::Error::new(io::ErrorKind::NotFound, "missing"),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("open /tmp/x.img"));
        assert!(rendered.contains("missing"));
    }

    #[test]
    fn from_corefs_error_preserves_variant() {
        let core_err = CoreFsError::NotFound("nowhere".to_string());
        let tools_err: ToolsError = core_err.into();
        assert!(matches!(tools_err, ToolsError::Core(_)));
    }
}
