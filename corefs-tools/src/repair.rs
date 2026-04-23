// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für den fsck-Auto-Repair.
//!
//! Wrapper um [`corefs::storage::ondisk::fsck_repair::repair`]. Führt zuerst einen
//! [`corefs::storage::ondisk::fsck::check`] durch und appliziert anschließend in
//! einer einzigen Journal-Transaktion alle auto-fixbaren Korrekturen
//! (Superblock-Redundanz wiederherstellen, Bitmap-CRC-Drift korrigieren,
//! Inode-Bitmap-Inkonsistenzen aufräumen, …).
//!
//! Nicht-fixbare Issues (Double-Allocation, Out-of-Range-Extents,
//! Record-Decode-Fehler) werden in [`RepairImageReport::unfixable`]
//! zurückgegeben und müssen manuell adressiert werden.

use crate::error::ToolsResult;
use crate::fsck::FsckIssueReport;
use crate::report::{Report, to_pretty_json};
use corefs::storage::block_device::FileImageDevice;
use corefs::storage::ondisk::fsck::check;
use corefs::storage::ondisk::fsck_repair::{RepairReport, repair};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Strukturierter Report einer Repair-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairImageReport {
    /// Pfad des reparierten Volumes.
    pub image_path: String,
    /// Issue-Codes, die erfolgreich behoben wurden (z. B. `"ODF.SB.STALE"`).
    pub fixed: Vec<String>,
    /// Issues, die der Repair-Pass bewusst nicht angefasst hat
    /// (z. B. Double-Allocation, defekte Inode-Records).
    pub unfixable: Vec<FsckIssueReport>,
    /// Anzahl Journal-Ops, die in der gemeinsamen Repair-Transaktion
    /// committet wurden.
    pub ops_committed: usize,
    /// `true`, wenn keine nicht-fixbaren Issues übrig bleiben.
    pub fully_repaired: bool,
}

impl Report for RepairImageReport {
    fn summary(&self) -> String {
        if self.fixed.is_empty() && self.unfixable.is_empty() {
            "repair: nothing to do (volume already clean)".to_string()
        } else if self.fully_repaired {
            format!(
                "repair ok ({} issues fixed, {} ops committed)",
                self.fixed.len(),
                self.ops_committed
            )
        } else {
            format!(
                "repair partial ({} fixed, {} unfixable, {} ops committed)",
                self.fixed.len(),
                self.unfixable.len(),
                self.ops_committed,
            )
        }
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("repair report\n");
        out.push_str("─────────────\n");
        out.push_str(&format!("image path        : {}\n", self.image_path));
        out.push_str(&format!("ops committed     : {}\n", self.ops_committed));
        out.push_str(&format!(
            "fixed             : {} ({})\n",
            self.fixed.len(),
            self.fixed.join(", ")
        ));
        out.push_str(&format!("unfixable         : {}\n", self.unfixable.len()));
        for i in &self.unfixable {
            out.push_str(&format!(
                "  [{:?}] {} — {}\n",
                i.severity, i.code, i.message
            ));
        }
        out.push_str(&format!("fully repaired    : {}\n", self.fully_repaired));
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Führt Auto-Repair auf der Image-Datei `path` durch.
///
/// Öffnet das Image read-write, ruft zuerst [`check`] für eine aktuelle
/// fsck-Liste auf und appliziert dann [`repair`] gegen das Ergebnis.
/// Wenn das Volume bereits clean ist, ist die Operation ein No-Op und
/// liefert einen Report mit leerem `fixed`/`unfixable`.
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::repair::repair_image;
/// use corefs_tools::Report;
///
/// let report = repair_image("/tmp/demo.img").unwrap();
/// println!("{}", report.summary());
/// ```
pub fn repair_image(path: impl AsRef<Path>) -> ToolsResult<RepairImageReport> {
    let path = path.as_ref();
    let mut device = FileImageDevice::open(path, false)?;
    let fsck_report = check(&device)?;
    let repair_report = repair(&mut device, &fsck_report)?;
    Ok(from_inner(path.display().to_string(), &repair_report))
}

fn from_inner(image_path: String, inner: &RepairReport) -> RepairImageReport {
    RepairImageReport {
        image_path,
        fixed: inner.fixed.iter().map(|s| s.to_string()).collect(),
        unfixable: inner.unfixable.iter().map(FsckIssueReport::from).collect(),
        ops_committed: inner.ops_committed,
        fully_repaired: inner.unfixable.is_empty(),
    }
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
