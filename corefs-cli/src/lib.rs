// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-cli
//!
//! Schlanke Kommandozeilen-Frontend um die Operationen aus
//! [`corefs-tools`]. Das gesamte Dispatch- und Argument-Parsing liegt in
//! dieser Library, damit es unit-testbar ist; das `main`-Binary delegiert
//! nur an [`dispatch`] und mappt das Ergebnis auf einen Exit-Code.
//!
//! ## Subcommands
//!
//! ```text
//! corefs-cli mkfs <path> --capacity <bytes> [--label <name>] [--blob]
//! corefs-cli fsck <path> [--json]
//! corefs-cli repair <path> [--json]
//! corefs-cli scrub <path> [--mode full|structural|read-only] [--json]
//! corefs-cli dump-superblock <path> [--json]
//! corefs-cli dump-inode <path> --slot <n> [--json]
//! corefs-cli snapshot list <path> [--json]
//! corefs-cli snapshot create <path> --name <n> [--scope <root>] [--json]
//! corefs-cli snapshot delete <path> --id <n> [--json]
//! corefs-cli snapshot restore <path> --id <n> [--json]
//! corefs-cli defrag <path> [--json]
//! corefs-cli help
//! ```
//!
//! Standard-Ausgabe ist Text. Mit `--json` liefert jeder Subcommand seinen
//! Report als JSON. Bei Erfolg landet der Report auf stdout, Diagnosen
//! und Usage-Fehler auf stderr.
//!
//! ## Exit-Codes
//!
//! - `0` — Erfolg
//! - `1` — Tool-Fehler (Image korrupt, I/O, Snapshot nicht gefunden, …)
//! - `2` — Argument-/Usage-Fehler

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

use corefs_tools::Report;
use corefs_tools::mkfs::{FormatImageOptions, LayoutMode};
use corefs_tools::scrub::ScrubMode;
use corefs_tools::snapshot::CreateOptions;
use corefs_tools::{ToolsError, defrag, dump, fsck, mkfs, repair, scrub, snapshot};
use std::io::Write;

/// Dispatch-Ergebnis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// Operation erfolgreich.
    Ok,
    /// Tool-Operation fehlgeschlagen (z. B. korruptes Image, fehlender Pfad).
    ToolError,
    /// Argument-/Usage-Fehler (unbekanntes Subcommand, fehlende Flag).
    UsageError,
}

/// Argumente parsen und an die richtige Tool-Operation delegieren.
///
/// Schreibt Reports auf `out`, Diagnosen/Hilfetexte auf `err`. Tritt nie
/// ein Panic ein — alle Fehler werden in [`ExitStatus`] gemappt.
pub fn dispatch<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    if args.is_empty() {
        write_usage(err);
        return ExitStatus::UsageError;
    }
    match args[0].as_str() {
        "help" | "-h" | "--help" => {
            write_usage(out);
            ExitStatus::Ok
        }
        "mkfs" => run_mkfs(&args[1..], out, err),
        "fsck" => run_fsck(&args[1..], out, err),
        "repair" => run_repair(&args[1..], out, err),
        "scrub" => run_scrub(&args[1..], out, err),
        "dump-superblock" => run_dump_superblock(&args[1..], out, err),
        "dump-inode" => run_dump_inode(&args[1..], out, err),
        "snapshot" => run_snapshot(&args[1..], out, err),
        "defrag" => run_defrag(&args[1..], out, err),
        other => {
            let _ = writeln!(err, "corefs-cli: unknown subcommand: {other}");
            write_usage(err);
            ExitStatus::UsageError
        }
    }
}

// =====================================================================
// Subcommand handlers
// =====================================================================

fn run_mkfs<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    let positional = match collect_positional(args, 1, "mkfs <path>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let path = &positional[0];
    let capacity = match parse_required_u64(args, "--capacity") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let label = parse_string(args, "--label").unwrap_or_else(|| "corefs".to_string());
    let blob = has_flag(args, "--blob");
    let json = has_flag(args, "--json");

    let opts = FormatImageOptions {
        label,
        layout_mode: if blob { LayoutMode::Blob } else { LayoutMode::Native },
        ..Default::default()
    };
    let result = mkfs::format_image(path, capacity, &opts);
    finish(result, out, err, json)
}

fn run_fsck<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    let positional = match collect_positional(args, 1, "fsck <path>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let json = has_flag(args, "--json");
    finish(fsck::check_image(&positional[0]), out, err, json)
}

fn run_repair<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    let positional = match collect_positional(args, 1, "repair <path>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let json = has_flag(args, "--json");
    finish(repair::repair_image(&positional[0]), out, err, json)
}

fn run_scrub<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    let positional = match collect_positional(args, 1, "scrub <path>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let mode_str = parse_string(args, "--mode").unwrap_or_else(|| "full".to_string());
    let mode = match mode_str.as_str() {
        "full" => ScrubMode::Full,
        "structural" | "structural-only" => ScrubMode::StructuralOnly,
        "read-only" | "readonly" => ScrubMode::ReadOnly,
        other => {
            return usage_err(
                err,
                &format!("scrub: unknown --mode value '{other}' (use full|structural|read-only)"),
            );
        }
    };
    let json = has_flag(args, "--json");
    finish(scrub::scrub_image(&positional[0], mode), out, err, json)
}

fn run_dump_superblock<O: Write, E: Write>(
    args: &[String],
    out: &mut O,
    err: &mut E,
) -> ExitStatus {
    let positional = match collect_positional(args, 1, "dump-superblock <path>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let json = has_flag(args, "--json");
    finish(dump::superblock(&positional[0]), out, err, json)
}

fn run_dump_inode<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    let positional = match collect_positional(args, 1, "dump-inode <path> --slot <n>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let slot = match parse_required_u64(args, "--slot") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let json = has_flag(args, "--json");
    finish(dump::inode(&positional[0], slot), out, err, json)
}

fn run_snapshot<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    if args.is_empty() {
        return usage_err(err, "snapshot: missing subcommand (list|create|delete|restore)");
    }
    match args[0].as_str() {
        "list" => {
            let positional = match collect_positional(&args[1..], 1, "snapshot list <path>") {
                Ok(v) => v,
                Err(msg) => return usage_err(err, &msg),
            };
            let json = has_flag(&args[1..], "--json");
            finish(snapshot::list(&positional[0]), out, err, json)
        }
        "create" => {
            let positional = match collect_positional(&args[1..], 1, "snapshot create <path> --name <n>") {
                Ok(v) => v,
                Err(msg) => return usage_err(err, &msg),
            };
            let name = match parse_string(&args[1..], "--name") {
                Some(v) => v,
                None => return usage_err(err, "snapshot create: --name is required"),
            };
            let scope_root = parse_string(&args[1..], "--scope");
            let json = has_flag(&args[1..], "--json");
            finish(
                snapshot::create(&positional[0], &CreateOptions { name, scope_root }),
                out,
                err,
                json,
            )
        }
        "delete" => {
            let positional = match collect_positional(&args[1..], 1, "snapshot delete <path> --id <n>") {
                Ok(v) => v,
                Err(msg) => return usage_err(err, &msg),
            };
            let id = match parse_required_u64(&args[1..], "--id") {
                Ok(v) => v,
                Err(msg) => return usage_err(err, &msg),
            };
            let json = has_flag(&args[1..], "--json");
            finish(snapshot::delete(&positional[0], id), out, err, json)
        }
        "restore" => {
            let positional = match collect_positional(&args[1..], 1, "snapshot restore <path> --id <n>") {
                Ok(v) => v,
                Err(msg) => return usage_err(err, &msg),
            };
            let id = match parse_required_u64(&args[1..], "--id") {
                Ok(v) => v,
                Err(msg) => return usage_err(err, &msg),
            };
            let json = has_flag(&args[1..], "--json");
            finish(snapshot::restore(&positional[0], id), out, err, json)
        }
        other => usage_err(err, &format!("snapshot: unknown subcommand '{other}' (use list|create|delete|restore)")),
    }
}

fn run_defrag<O: Write, E: Write>(args: &[String], out: &mut O, err: &mut E) -> ExitStatus {
    let positional = match collect_positional(args, 1, "defrag <path>") {
        Ok(v) => v,
        Err(msg) => return usage_err(err, &msg),
    };
    let json = has_flag(args, "--json");
    finish(defrag::defrag_image(&positional[0]), out, err, json)
}

// =====================================================================
// Common output / error finalisation
// =====================================================================

fn finish<R: Report, O: Write, E: Write>(
    result: Result<R, ToolsError>,
    out: &mut O,
    err: &mut E,
    json: bool,
) -> ExitStatus {
    match result {
        Ok(report) => {
            let rendered = if json {
                report.render_json()
            } else {
                report.render_text()
            };
            let _ = writeln!(out, "{rendered}");
            ExitStatus::Ok
        }
        Err(e) => {
            let _ = writeln!(err, "corefs-cli: {e}");
            ExitStatus::ToolError
        }
    }
}

fn usage_err<E: Write>(err: &mut E, msg: &str) -> ExitStatus {
    let _ = writeln!(err, "corefs-cli: {msg}");
    ExitStatus::UsageError
}

// =====================================================================
// Argument parsing helpers
// =====================================================================

/// Sammelt die ersten `expected` positionalen Argumente (Tokens, die nicht
/// mit `--` beginnen und nicht direkt nach einer wertbehafteten Flag stehen).
fn collect_positional(args: &[String], expected: usize, hint: &str) -> Result<Vec<String>, String> {
    let mut positional = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg.starts_with("--") {
            // Wertbehaftete Flags konsumieren das nächste Token (außer Boolean-Flags
            // wie --json/--blob, die wir hier konservativ trotzdem überspringen
            // dürften — der Wert wäre dann das nächste positionale, was die
            // collect_positional-Aufrufer-Spec verletzt). Wir gehen davon aus, dass
            // alle Flags wertbehaftet sind, außer sie kommen ohne Folgewert.
            //
            // Einfache Heuristik: wenn das nächste Token mit `--` beginnt oder
            // gar keins folgt, ist die aktuelle Flag boolean. Sonst frisst sie
            // den Wert.
            if let Some(next) = iter.peek() {
                if !next.starts_with("--") && !is_known_bool_flag(arg) {
                    iter.next();
                }
            }
            continue;
        }
        positional.push(arg.clone());
    }
    if positional.len() < expected {
        return Err(format!("usage: {hint}"));
    }
    Ok(positional)
}

fn is_known_bool_flag(flag: &str) -> bool {
    matches!(flag, "--json" | "--blob" | "--help" | "-h")
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn parse_string(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            return iter.next().cloned();
        }
    }
    None
}

fn parse_required_u64(args: &[String], flag: &str) -> Result<u64, String> {
    let s = parse_string(args, flag)
        .ok_or_else(|| format!("missing required flag {flag}"))?;
    s.parse::<u64>()
        .map_err(|_| format!("flag {flag}: '{s}' is not a valid unsigned integer"))
}

// =====================================================================
// Usage banner
// =====================================================================

fn write_usage<W: Write>(w: &mut W) {
    let banner = "\
corefs-cli — programmatic frontend for CoreFS administration tools

USAGE:
    corefs-cli <SUBCOMMAND> [args...]

SUBCOMMANDS:
    mkfs <path> --capacity <bytes> [--label <name>] [--blob]
        Create and format a new file-image volume.

    fsck <path> [--json]
        Read-only consistency check.

    repair <path> [--json]
        Apply auto-fixable repairs from an fsck pass.

    scrub <path> [--mode full|structural|read-only] [--json]
        Run the self-healing scrubber.

    dump-superblock <path> [--json]
        Decode and display the primary superblock.

    dump-inode <path> --slot <n> [--json]
        Decode and display a single inode slot.

    snapshot list <path> [--json]
    snapshot create <path> --name <n> [--scope <root>] [--json]
    snapshot delete <path> --id <n> [--json]
    snapshot restore <path> --id <n> [--json]
        Snapshot lifecycle.

    defrag <path> [--json]
        Defragment the block store.

    help
        Show this message.

EXIT CODES:
    0 — success
    1 — tool error (corrupt image, missing file, snapshot not found, …)
    2 — usage error (unknown subcommand, missing flag)
";
    let _ = writeln!(w, "{banner}");
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
