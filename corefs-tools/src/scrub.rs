// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für den Self-Healing-Scrubber.
//!
//! Wrapper um [`corefs::storage::ondisk::scrub::run`] — kombiniert
//! strukturellen `fsck`-Walk, optional `fsck::repair` und
//! per-Datei-`data_crc`-Verifikation in einem End-to-End-Aufruf.
//!
//! Es gibt drei Voreinstellungen:
//!
//! - [`ScrubMode::Full`] — strukturelle Prüfung + Auto-Repair + CRC-Verifikation
//! - [`ScrubMode::StructuralOnly`] — strukturelle Prüfung + Auto-Repair, ohne CRC-Lesen
//! - [`ScrubMode::ReadOnly`] — strikt read-only, kein Repair, mit CRC-Verifikation

use crate::error::ToolsResult;
use crate::fsck::{FsckIssueReport, SeverityKind};
use crate::report::{Report, to_pretty_json};
use corefs::storage::block_device::FileImageDevice;
use corefs::storage::ondisk::scrub::{ScrubPlan, ScrubReport, run};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Hochsprachliche Voreinstellungen für [`scrub_image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrubMode {
    /// Strukturelle fsck + Auto-Repair + per-Datei-CRC-Verifikation.
    /// Entspricht dem klassischen Wochen-Scrub eines Enterprise-Storage-Arrays.
    Full,
    /// Strukturelle fsck + Auto-Repair, **ohne** CRC-Lesen.
    /// Sinnvoll als schneller Boot-Time-Check auf großen Volumes.
    StructuralOnly,
    /// Strikt read-only: kein Repair, aber CRC-Verifikation.
    ReadOnly,
}

impl ScrubMode {
    fn into_plan(self) -> ScrubPlan {
        match self {
            ScrubMode::Full => ScrubPlan::full(),
            ScrubMode::StructuralOnly => ScrubPlan::structural_only(),
            ScrubMode::ReadOnly => ScrubPlan::read_only(),
        }
    }
}

/// Strukturierter Report einer Scrub-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrubImageReport {
    /// Pfad des gescrubbten Volumes.
    pub image_path: String,
    /// Verwendeter Scrub-Modus.
    pub mode: ScrubMode,
    /// Anzahl Daten-Extents, die vom CRC-Walker gelesen wurden.
    pub extents_verified: u64,
    /// Anzahl Datenblöcke, deren CRC neu berechnet wurde.
    pub blocks_verified: u64,
    /// Anzahl Daten-CRC-Failures (`(slot, domain_inode_id)`-Paare).
    /// Diese sind ohne Daten-Redundanz nicht reparierbar.
    pub data_corruptions: Vec<DataCorruption>,
    /// Findings, die nach einem Auto-Repair-Pass übrig blieben (oder die
    /// Auto-Repair gefunden hätte, falls deaktiviert).
    pub residual_issues: Vec<FsckIssueReport>,
    /// Anzahl Repair-Ops, die als einzelne Journal-Transaktion committet wurden.
    pub repair_ops_committed: usize,
    /// Anzahl Issues (Error+Warning), die der initiale fsck-Pass gefunden hat.
    pub fsck_issues_before: usize,
    /// `true`, wenn keine Daten-Korruption und keine Error-Severity-Findings übrig sind.
    pub is_clean: bool,
}

/// Beschreibt eine konkrete Daten-Korruption (CRC-Mismatch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataCorruption {
    /// Inode-Slot im On-Disk-Inode-Table.
    pub slot: u64,
    /// Domain-Inode-ID (entspricht `Inode.id.0` der Domain-Schicht).
    pub domain_inode_id: u64,
}

impl ScrubImageReport {
    fn from_inner(image_path: String, mode: ScrubMode, inner: &ScrubReport) -> Self {
        Self {
            image_path,
            mode,
            extents_verified: inner.extents_verified,
            blocks_verified: inner.blocks_verified,
            data_corruptions: inner
                .data_corruptions
                .iter()
                .map(|(slot, id)| DataCorruption {
                    slot: *slot,
                    domain_inode_id: *id,
                })
                .collect(),
            residual_issues: inner
                .residual_issues
                .iter()
                .map(FsckIssueReport::from)
                .collect(),
            repair_ops_committed: inner.repair_ops_committed,
            fsck_issues_before: inner.fsck_issues_before,
            is_clean: inner.is_clean(),
        }
    }
}

impl Report for ScrubImageReport {
    fn summary(&self) -> String {
        if self.is_clean && self.data_corruptions.is_empty() {
            format!(
                "scrub clean ({} extents, {} blocks verified, {} repair ops)",
                self.extents_verified, self.blocks_verified, self.repair_ops_committed,
            )
        } else if !self.data_corruptions.is_empty() {
            format!(
                "scrub FAIL ({} data corruptions, {} residual issues)",
                self.data_corruptions.len(),
                self.residual_issues.len()
            )
        } else {
            // Errors in residual_issues, no data corruption.
            let errors = self
                .residual_issues
                .iter()
                .filter(|i| i.severity == SeverityKind::Error)
                .count();
            format!(
                "scrub FAIL ({errors} residual structural errors, {} repair ops applied)",
                self.repair_ops_committed
            )
        }
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("scrub report\n");
        out.push_str("────────────\n");
        out.push_str(&format!("image path             : {}\n", self.image_path));
        out.push_str(&format!("mode                   : {:?}\n", self.mode));
        out.push_str(&format!(
            "extents verified       : {}\n",
            self.extents_verified
        ));
        out.push_str(&format!(
            "blocks verified        : {}\n",
            self.blocks_verified
        ));
        out.push_str(&format!(
            "fsck issues (before)   : {}\n",
            self.fsck_issues_before
        ));
        out.push_str(&format!(
            "repair ops committed   : {}\n",
            self.repair_ops_committed
        ));
        out.push_str(&format!(
            "data corruptions       : {}\n",
            self.data_corruptions.len()
        ));
        for c in &self.data_corruptions {
            out.push_str(&format!("  slot={} inode={}\n", c.slot, c.domain_inode_id));
        }
        out.push_str(&format!(
            "residual issues        : {}\n",
            self.residual_issues.len()
        ));
        for i in &self.residual_issues {
            out.push_str(&format!(
                "  [{:?}] {} — {}\n",
                i.severity, i.code, i.message
            ));
        }
        out.push_str(&format!("clean                  : {}\n", self.is_clean));
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Führt eine Scrub-Operation auf der Image-Datei `path` durch.
///
/// Öffnet das Image read-write (auch im `ReadOnly`-Mode wird ein RW-Handle
/// genutzt — der Plan steuert, ob tatsächlich geschrieben wird), ruft
/// [`corefs::storage::ondisk::scrub::run`] mit dem aus `mode` abgeleiteten Plan auf
/// und übersetzt das Ergebnis in eine renderbare [`ScrubImageReport`]-Struktur.
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::scrub::{ScrubMode, scrub_image};
/// use corefs_tools::Report;
///
/// let report = scrub_image("/tmp/demo.img", ScrubMode::Full).unwrap();
/// println!("{}", report.summary());
/// ```
pub fn scrub_image(path: impl AsRef<Path>, mode: ScrubMode) -> ToolsResult<ScrubImageReport> {
    let path = path.as_ref();
    // RW-Handle: auch im ReadOnly-Mode unproblematisch, da der Plan keine
    // Writes auslöst. Vermeidet zusätzliche Code-Pfade gegenüber Plan-spezifischer
    // Öffnung.
    let mut device = FileImageDevice::open(path, false)?;
    let plan = mode.into_plan();
    let inner = run(&mut device, &plan)?;
    Ok(ScrubImageReport::from_inner(
        path.display().to_string(),
        mode,
        &inner,
    ))
}

#[cfg(test)]
#[path = "scrub_tests.rs"]
mod tests;
