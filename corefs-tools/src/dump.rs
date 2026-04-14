// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tool-Operationen für die Inspektion von On-Disk-Strukturen.
//!
//! Liefert read-only Dekodierungen einzelner Strukturen (Superblock, später
//! Inode-Records, Allocator-Bitmaps, Journal-Header, …). Diese Operationen
//! schreiben nichts und sind damit auch auf inkonsistenten Volumes sicher
//! aufrufbar.

use crate::error::ToolsResult;
use crate::report::{Report, to_pretty_json};
use corefs::storage::block_device::FileImageDevice;
use corefs::storage::ondisk::layout::{BLOCK_SIZE, PRIMARY_SUPERBLOCK_BLOCK};
use corefs::storage::ondisk::superblock::{LAYOUT_MODE_NATIVE, Superblock};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Strukturierter Report für [`superblock`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperblockReport {
    /// Pfad des inspizierten Volumes.
    pub image_path: String,
    /// Magic-Wert (sollte `0x434F_5246_5300_4F44` für ODF v1 sein).
    pub magic: u64,
    /// Ob das Magic mit dem erwarteten Wert übereinstimmt.
    pub magic_ok: bool,
    /// Format-Version (major.minor).
    pub version: String,
    /// Block-Größe in Bytes.
    pub block_size: u32,
    /// Gesamtanzahl 4-KiB-Blöcke.
    pub total_blocks: u64,
    /// Anzahl freier Blöcke laut Superblock.
    pub free_blocks: u64,
    /// Inode-Slot-Kapazität.
    pub total_inodes: u64,
    /// Anzahl freier Inode-Slots laut Superblock.
    pub free_inodes: u64,
    /// Generation-Counter (zählt aufeinanderfolgende Saves).
    pub generation: u64,
    /// Layout-Modus: `"blob"` (0) oder `"native"` (1).
    pub layout_mode: String,
    /// Volume-Label (UTF-8, abgeschnitten an erstem 0-Byte).
    pub label: String,
    /// 16-Byte-UUID, hex-codiert.
    pub uuid_hex: String,
    /// Sekunden seit Epoch beim Format.
    pub created_at_secs: i64,
    /// Sekunden seit Epoch des letzten Mounts.
    pub last_mount_at_secs: i64,
    /// Anzahl Mount-Vorgänge seit Format.
    pub mount_count: u32,
    /// Position des sekundären Superblocks (Backup #1).
    pub secondary_superblock_block: u64,
    /// Position des tertiären Superblocks (Backup #2).
    pub tertiary_superblock_block: u64,
    /// Aktive Compat-Feature-Bits.
    pub feature_compat: u64,
    /// Aktive Incompat-Feature-Bits.
    pub feature_incompat: u64,
    /// Aktive ReadOnly-Compat-Feature-Bits.
    pub feature_ro_compat: u64,
}

impl Report for SuperblockReport {
    fn summary(&self) -> String {
        format!(
            "superblock {} ({}, gen {}, {} blocks, {} inodes)",
            if self.magic_ok { "ok" } else { "BAD MAGIC" },
            self.layout_mode,
            self.generation,
            self.total_blocks,
            self.total_inodes,
        )
    }

    fn render_text(&self) -> String {
        format!(
            "superblock dump\n\
             ───────────────\n\
             image path        : {}\n\
             magic             : 0x{:016x} ({})\n\
             version           : {}\n\
             block size        : {} bytes\n\
             total blocks      : {} ({} free)\n\
             total inodes      : {} ({} free)\n\
             generation        : {}\n\
             layout mode       : {}\n\
             label             : {}\n\
             uuid              : {}\n\
             created at        : {} (epoch secs)\n\
             last mount at     : {} (epoch secs)\n\
             mount count       : {}\n\
             secondary sb block: {}\n\
             tertiary sb block : {}\n\
             feature_compat    : 0x{:016x}\n\
             feature_incompat  : 0x{:016x}\n\
             feature_ro_compat : 0x{:016x}\n",
            self.image_path,
            self.magic,
            if self.magic_ok { "ok" } else { "MISMATCH" },
            self.version,
            self.block_size,
            self.total_blocks,
            self.free_blocks,
            self.total_inodes,
            self.free_inodes,
            self.generation,
            self.layout_mode,
            self.label,
            self.uuid_hex,
            self.created_at_secs,
            self.last_mount_at_secs,
            self.mount_count,
            self.secondary_superblock_block,
            self.tertiary_superblock_block,
            self.feature_compat,
            self.feature_incompat,
            self.feature_ro_compat,
        )
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

const ODF_MAGIC: u64 = corefs::storage::ondisk::layout::ODF_MAGIC;

/// Liest und dekodiert den primären Superblock einer Image-Datei.
///
/// Schlägt nicht fehl, wenn das Magic falsch ist — der Aufrufer prüft
/// stattdessen [`SuperblockReport::magic_ok`]. Das erlaubt es Diagnose-Tools,
/// auch verdächtige Volumes zu inspizieren.
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::dump::superblock;
/// use corefs_tools::Report;
///
/// let report = superblock("/tmp/demo.img").unwrap();
/// println!("{}", report.render_text());
/// ```
pub fn superblock(path: impl AsRef<Path>) -> ToolsResult<SuperblockReport> {
    let path = path.as_ref();
    let device = FileImageDevice::open(path, true)?;
    let block_bytes = device.read_at(PRIMARY_SUPERBLOCK_BLOCK * BLOCK_SIZE, BLOCK_SIZE)?;
    let sb = Superblock::decode_block(&block_bytes)?;

    let label = decode_label(&sb.label);
    let layout_mode_str = if sb.layout_mode == LAYOUT_MODE_NATIVE {
        "native".to_string()
    } else if sb.layout_mode == 0 {
        "blob".to_string()
    } else {
        format!("unknown({})", sb.layout_mode)
    };

    Ok(SuperblockReport {
        image_path: path.display().to_string(),
        magic: sb.magic,
        magic_ok: sb.magic == ODF_MAGIC,
        version: format!("{}.{}", sb.version_major, sb.version_minor),
        block_size: sb.block_size,
        total_blocks: sb.total_blocks,
        free_blocks: sb.free_blocks,
        total_inodes: sb.total_inodes,
        free_inodes: sb.free_inodes,
        generation: sb.generation,
        layout_mode: layout_mode_str,
        label,
        uuid_hex: hex_encode(&sb.uuid),
        created_at_secs: sb.created_at,
        last_mount_at_secs: sb.last_mount_at,
        mount_count: sb.mount_count,
        secondary_superblock_block: sb.secondary_superblock_block,
        tertiary_superblock_block: sb.tertiary_superblock_block,
        feature_compat: sb.feature_compat,
        feature_incompat: sb.feature_incompat,
        feature_ro_compat: sb.feature_ro_compat,
    })
}

/// Bedarfsabhängige `BlockDevice`-Verwendung erfordert den read_at-Trait.
use corefs::storage::block_device::BlockDevice;

fn decode_label(bytes: &[u8; 32]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
#[path = "dump_tests.rs"]
mod tests;
