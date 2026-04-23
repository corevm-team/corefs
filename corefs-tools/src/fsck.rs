// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für die Konsistenzprüfung (`fsck`).
//!
//! Wrapper um [`corefs::storage::ondisk::fsck::check`], der einen
//! Image-Pfad öffnet, den Read-only-Walker aufruft und das Ergebnis
//! als strukturierten [`FsckCheckReport`] zurückliefert.

use crate::error::ToolsResult;
use crate::report::{Report, to_pretty_json};
use corefs::storage::block_device::FileImageDevice;
use corefs::storage::ondisk::fsck::{FsckIssue, FsckReport, Severity, check};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Strukturierte Severity-Repräsentation für JSON-/Text-Reports.
///
/// Spiegelt [`corefs::storage::ondisk::fsck::Severity`] wider, ist aber
/// `Serialize`-/`Deserialize`-fähig, sodass JSON-Konsumenten die Severity
/// stabil parsen können.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityKind {
    /// Beobachtung ohne Konsequenz.
    Info,
    /// FS bleibt nutzbar, aber etwas ist auffällig.
    Warning,
    /// Inkonsistenz, manueller Eingriff sinnvoll.
    Error,
}

impl From<Severity> for SeverityKind {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Info => SeverityKind::Info,
            Severity::Warning => SeverityKind::Warning,
            Severity::Error => SeverityKind::Error,
        }
    }
}

/// Einzelnes Finding einer fsck-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsckIssueReport {
    /// Severity-Klassifikation.
    pub severity: SeverityKind,
    /// Stabiler Issue-Code (z. B. `"ODF.SB.MAGIC"`).
    pub code: String,
    /// Menschenlesbare Beschreibung der Ursache.
    pub message: String,
}

impl From<&FsckIssue> for FsckIssueReport {
    fn from(i: &FsckIssue) -> Self {
        Self {
            severity: i.severity.into(),
            code: i.code.to_string(),
            message: i.message.clone(),
        }
    }
}

/// Strukturierter Report für eine fsck-Check-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsckCheckReport {
    /// Pfad des geprüften Volumes.
    pub image_path: String,
    /// Anzahl Inodes, die der Walker besucht hat.
    pub inodes_checked: u64,
    /// Anzahl Extents, die der Walker dereferenziert hat.
    pub extents_checked: u64,
    /// Anzahl Datenblöcke, die mindestens einmal referenziert wurden.
    pub blocks_referenced: u64,
    /// Liste aller Findings, in Walk-Reihenfolge.
    pub issues: Vec<FsckIssueReport>,
    /// `true`, wenn keine Findings der Severity `Error` vorliegen.
    pub is_clean: bool,
}

impl FsckCheckReport {
    fn from_inner(image_path: String, inner: &FsckReport) -> Self {
        Self {
            image_path,
            inodes_checked: inner.inodes_checked,
            extents_checked: inner.extents_checked,
            blocks_referenced: inner.blocks_referenced,
            issues: inner.issues.iter().map(FsckIssueReport::from).collect(),
            is_clean: inner.is_clean(),
        }
    }

    /// Anzahl Findings einer bestimmten Severity.
    pub fn count(&self, sev: SeverityKind) -> usize {
        self.issues.iter().filter(|i| i.severity == sev).count()
    }
}

impl Report for FsckCheckReport {
    fn summary(&self) -> String {
        let errors = self.count(SeverityKind::Error);
        let warnings = self.count(SeverityKind::Warning);
        let infos = self.count(SeverityKind::Info);
        if self.issues.is_empty() {
            format!(
                "fsck clean ({} inodes, {} extents)",
                self.inodes_checked, self.extents_checked
            )
        } else if self.is_clean {
            // No errors, but warnings/info present.
            format!(
                "fsck ok ({} findings: {warnings} warnings, {infos} info)",
                self.issues.len()
            )
        } else {
            format!("fsck FAIL ({errors} errors, {warnings} warnings, {infos} info)")
        }
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("fsck report\n");
        out.push_str("───────────\n");
        out.push_str(&format!("image path        : {}\n", self.image_path));
        out.push_str(&format!("inodes checked    : {}\n", self.inodes_checked));
        out.push_str(&format!("extents checked   : {}\n", self.extents_checked));
        out.push_str(&format!("blocks referenced : {}\n", self.blocks_referenced));
        out.push_str(&format!("clean             : {}\n", self.is_clean));
        out.push_str(&format!("issues            : {}\n", self.issues.len()));
        for issue in &self.issues {
            out.push_str(&format!(
                "  [{:?}] {} — {}\n",
                issue.severity, issue.code, issue.message
            ));
        }
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Führt einen Read-only-Konsistenz-Check für die Image-Datei `path` durch.
///
/// Öffnet die Datei read-only, ruft [`corefs::storage::ondisk::fsck::check`]
/// auf und überführt das Ergebnis in eine renderbare [`FsckCheckReport`]-Struktur.
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::fsck::check_image;
/// use corefs_tools::Report;
///
/// let report = check_image("/tmp/demo.img").unwrap();
/// println!("{}", report.summary());
/// ```
pub fn check_image(path: impl AsRef<Path>) -> ToolsResult<FsckCheckReport> {
    let path = path.as_ref();
    let device = FileImageDevice::open(path, true)?;
    let inner = check(&device)?;
    Ok(FsckCheckReport::from_inner(
        path.display().to_string(),
        &inner,
    ))
}

#[cfg(test)]
#[path = "fsck_tests.rs"]
mod tests;
