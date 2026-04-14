// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für Defragmentierung.
//!
//! Wrapper um [`corefs::app::CoreFsService::defragment`]. Verschiebt belegte
//! Extents im Block-Store, sodass freie Lücken wieder zusammenhängend werden,
//! und persistiert das Ergebnis über `OdfFileSession::flush`.

use crate::error::ToolsResult;
use crate::report::{Report, to_pretty_json};
use corefs::storage::ondisk::session::OdfFileSession;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Strukturierter Report einer Defrag-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefragImageReport {
    /// Pfad des Volumes.
    pub image_path: String,
    /// Anzahl Block-Records, die im Block-Store verschoben wurden.
    pub moved_entries: usize,
    /// Anzahl freier Lücken, die durch das Compaction wiedergewonnen wurden.
    pub reclaimed_gaps: usize,
    /// Anzahl Block-Adressen, die nach der Compaction noch im Volume belegt sind.
    pub final_device_blocks: u64,
}

impl Report for DefragImageReport {
    fn summary(&self) -> String {
        if self.moved_entries == 0 && self.reclaimed_gaps == 0 {
            format!(
                "defrag: nothing to do ({} blocks in use)",
                self.final_device_blocks
            )
        } else {
            format!(
                "defrag ok ({} entries moved, {} gaps reclaimed, {} blocks in use)",
                self.moved_entries, self.reclaimed_gaps, self.final_device_blocks
            )
        }
    }

    fn render_text(&self) -> String {
        format!(
            "defrag report\n─────────────\n\
             image path           : {}\n\
             moved entries        : {}\n\
             reclaimed gaps       : {}\n\
             final device blocks  : {}\n",
            self.image_path, self.moved_entries, self.reclaimed_gaps, self.final_device_blocks
        )
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Defragmentiert den Block-Store der Image-Datei `path` und persistiert
/// das Ergebnis.
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::defrag::defrag_image;
/// use corefs_tools::Report;
///
/// let report = defrag_image("/tmp/demo.img").unwrap();
/// println!("{}", report.summary());
/// ```
pub fn defrag_image(path: impl AsRef<Path>) -> ToolsResult<DefragImageReport> {
    let path = path.as_ref();
    let mut session = OdfFileSession::open(path)?;
    let (inner, _flush) = session.mutate(|svc| Ok(svc.defragment()))?;
    Ok(DefragImageReport {
        image_path: path.display().to_string(),
        moved_entries: inner.moved_entries,
        reclaimed_gaps: inner.reclaimed_gaps,
        final_device_blocks: inner.final_device_blocks,
    })
}

#[cfg(test)]
#[path = "defrag_tests.rs"]
mod tests;
