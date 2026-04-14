// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Binary-Einstieg von [`corefs-cli`]. Delegiert ans [`lib`]-Modul, damit
//! die Argument-Parser und Dispatch-Logik unit-testbar bleiben.
//!
//! Exit-Codes:
//! - `0` — Erfolg
//! - `1` — Tool-Fehler (Image korrupt, I/O-Fehler, Snapshot nicht gefunden, …)
//! - `2` — Argument-/Usage-Fehler (unbekanntes Subcommand, fehlende Flag, …)

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    match corefs_cli::dispatch(&args, &mut stdout, &mut stderr) {
        corefs_cli::ExitStatus::Ok => ExitCode::from(0),
        corefs_cli::ExitStatus::ToolError => ExitCode::from(1),
        corefs_cli::ExitStatus::UsageError => ExitCode::from(2),
    }
}
