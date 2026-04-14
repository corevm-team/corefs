// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für die Volume-Initialisierung (`mkfs`).
//!
//! Stellt eine plattformneutrale Wrapper-Schnittstelle um
//! [`corefs::storage::ondisk::volume::format_device`] bereit, kombiniert
//! mit dem Anlegen einer File-Backed-Image-Datei in einem einzigen Aufruf.

use crate::error::{ToolsError, ToolsResult};
use crate::report::{Report, to_pretty_json};
use corefs::storage::block_device::FileImageDevice;
use corefs::storage::ondisk::volume::{FormatOptions, format_device};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default-Sektorgröße für neu erzeugte Image-Dateien.
pub const DEFAULT_SECTOR_SIZE: u32 = 4096;

/// Optionen für [`format_image`].
#[derive(Debug, Clone)]
pub struct FormatImageOptions {
    /// Volume-Label (Anzeigename auf dem Datenträger).
    pub label: String,
    /// 16-Byte-UUID — `[0u8; 16]` lässt das Format-Modul einen Default vergeben.
    pub uuid: [u8; 16],
    /// Anzahl Inode-Slots im Inode-Table.
    /// `None` ⇒ ODF-Default (`super::layout::DEFAULT_INODE_COUNT`).
    pub inode_count: Option<u64>,
    /// Größe des Journal-Bereichs in 4-KiB-Blöcken.
    /// `None` ⇒ ODF-Default (`super::layout::DEFAULT_JOURNAL_BLOCKS`).
    pub journal_blocks: Option<u64>,
    /// Sektorgröße der zu erzeugenden Image-Datei. Muss eine Zweierpotenz sein.
    pub sector_size: u32,
}

impl Default for FormatImageOptions {
    fn default() -> Self {
        Self {
            label: "corefs".to_string(),
            uuid: [0u8; 16],
            inode_count: None,
            journal_blocks: None,
            sector_size: DEFAULT_SECTOR_SIZE,
        }
    }
}

/// Strukturierter Report, der von [`format_image`] zurückgegeben wird.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkfsReport {
    /// Pfad der erzeugten Image-Datei.
    pub image_path: String,
    /// Volume-Label.
    pub label: String,
    /// Tatsächliche Kapazität der Image-Datei in Bytes.
    pub capacity_bytes: u64,
    /// Anzahl 4-KiB-Blöcke insgesamt.
    pub total_blocks: u64,
    /// Größe des Inode-Tables (Slot-Anzahl).
    pub inode_count: u64,
    /// Größe der Journal-Region in 4-KiB-Blöcken.
    pub journal_blocks: u64,
    /// Generation-Counter des frisch geschriebenen Superblocks (üblicherweise 1).
    pub generation: u64,
    /// 16-Byte-UUID des Volumes (hex-codiert).
    pub uuid_hex: String,
}

impl Report for MkfsReport {
    fn summary(&self) -> String {
        format!(
            "formatted {} ({:.1} MiB, {} blocks, {} inodes)",
            self.image_path,
            (self.capacity_bytes as f64) / (1024.0 * 1024.0),
            self.total_blocks,
            self.inode_count,
        )
    }

    fn render_text(&self) -> String {
        format!(
            "mkfs report\n\
             ───────────\n\
             image path        : {}\n\
             label             : {}\n\
             capacity          : {} bytes ({:.2} MiB)\n\
             total blocks      : {}\n\
             inode slots       : {}\n\
             journal blocks    : {}\n\
             superblock gen    : {}\n\
             uuid              : {}\n",
            self.image_path,
            self.label,
            self.capacity_bytes,
            (self.capacity_bytes as f64) / (1024.0 * 1024.0),
            self.total_blocks,
            self.inode_count,
            self.journal_blocks,
            self.generation,
            self.uuid_hex,
        )
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Erzeugt eine neue Image-Datei und formatiert sie mit dem ODF v1 Layout.
///
/// Schlägt fehl, wenn die Datei bereits existiert (`create_new` Semantik) oder
/// die `capacity_bytes` keine Vielfache der `sector_size` ist.
///
/// Die zurückgegebene [`MkfsReport`]-Struktur ist sowohl menschen- als auch
/// JSON-renderbar via [`crate::Report`].
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::mkfs::{FormatImageOptions, format_image};
/// use corefs_tools::Report;
///
/// let opts = FormatImageOptions {
///     label: "demo".to_string(),
///     ..Default::default()
/// };
/// let report = format_image("/tmp/demo.img", 16 * 1024 * 1024, &opts).unwrap();
/// println!("{}", report.summary());
/// ```
pub fn format_image(
    path: impl AsRef<Path>,
    capacity_bytes: u64,
    options: &FormatImageOptions,
) -> ToolsResult<MkfsReport> {
    let path = path.as_ref();
    if capacity_bytes == 0 {
        return Err(ToolsError::InvalidArgument(
            "capacity must be greater than zero".to_string(),
        ));
    }

    let mut device = FileImageDevice::create(path, capacity_bytes, options.sector_size)?;

    let mut format_opts = FormatOptions {
        label: options.label.clone(),
        uuid: options.uuid,
        ..Default::default()
    };
    if let Some(ic) = options.inode_count {
        format_opts.inode_count = ic;
    }
    if let Some(jb) = options.journal_blocks {
        format_opts.journal_blocks = jb;
    }

    let format_report = format_device(&mut device, &format_opts)?;

    Ok(MkfsReport {
        image_path: path.display().to_string(),
        label: options.label.clone(),
        capacity_bytes,
        total_blocks: format_report.geometry.total_blocks,
        inode_count: format_report.geometry.inode_count,
        journal_blocks: format_report.geometry.journal_blocks,
        generation: format_report.generation,
        uuid_hex: hex_encode(&format_report.uuid),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
#[path = "mkfs_tests.rs"]
mod tests;
