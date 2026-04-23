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
use corefs::storage::block_device::{BlockDevice, FileImageDevice};
use corefs::storage::ondisk::inode::{FLAG_HAS_EXTENT_INDEX, OnDiskInode, OnDiskKind};
use corefs::storage::ondisk::layout::{BLOCK_SIZE, PRIMARY_SUPERBLOCK_BLOCK};
use corefs::storage::ondisk::reader::OdfReader;
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

// =====================================================================
// dump::inode
// =====================================================================

/// Eintrag in der `extents`-Liste eines Inode-Reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtentEntry {
    /// Logischer Startblock des Extents innerhalb der Datei.
    pub logical_block: u32,
    /// Länge des Extents in Blöcken.
    pub length_blocks: u32,
    /// Physikalischer Startblock auf dem zugrundeliegenden Device.
    pub physical_block: u64,
}

/// Strukturierter Report einer Inode-Dump-Operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InodeDumpReport {
    /// Pfad des inspizierten Volumes.
    pub image_path: String,
    /// Inode-Slot-Nummer im On-Disk-Inode-Table.
    pub slot: u64,
    /// Inode-Kind als String (`File`, `Directory`, `Symlink`, `Unused`, `SystemPayload`).
    pub kind: String,
    /// POSIX-Mode-Bits.
    pub mode: u32,
    /// POSIX-Owner-UID.
    pub uid: u32,
    /// POSIX-Owner-GID.
    pub gid: u32,
    /// `link_count`-Feld des Inodes.
    pub link_count: u32,
    /// `flags`-Bitmaske (siehe `FLAG_*`-Konstanten in `corefs::storage::ondisk::inode`).
    pub flags: u32,
    /// Logische Dateigröße in Bytes.
    pub size_bytes: u64,
    /// Anzahl tatsächlich allozierter Datenblöcke.
    pub blocks_allocated: u64,
    /// crtime in Sekunden seit Epoche.
    pub created_at_secs: i64,
    /// mtime in Sekunden seit Epoche.
    pub modified_at_secs: i64,
    /// ctime in Sekunden seit Epoche.
    pub changed_at_secs: i64,
    /// atime in Sekunden seit Epoche.
    pub accessed_at_secs: i64,
    /// Inode-Generation-Counter.
    pub generation: u64,
    /// Extents, die in diesem Slot direkt eingebettet sind. Bei
    /// `flags & FLAG_HAS_EXTENT_INDEX` ist die vollständige Liste über
    /// `index_block_addr` erreichbar.
    pub extents: Vec<ExtentEntry>,
    /// Root-Block der indirekten Extent-Index-Kette (0 = keine).
    pub index_block_addr: u64,
    /// Block-Adresse des Xattr/ACL-Records (0 = keine).
    pub xattr_block_addr: u64,
    /// Domain-Inode-ID, die zu diesem Slot korrespondiert (0 für System-Slots).
    pub domain_inode_id: u64,
    /// CRC32C über den Plain-Text-Inhalt der Datei.
    pub data_crc: u64,
    /// `true`, wenn das Inode per `FLAG_HAS_EXTENT_INDEX` auf eine externe
    /// Extent-Liste verweist.
    pub has_external_extent_index: bool,
}

impl Report for InodeDumpReport {
    fn summary(&self) -> String {
        format!(
            "inode slot {} ({}, mode 0o{:o}, {} bytes, {} extents)",
            self.slot,
            self.kind,
            self.mode & 0o7777,
            self.size_bytes,
            self.extents.len(),
        )
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("inode dump\n");
        out.push_str("──────────\n");
        out.push_str(&format!("image path        : {}\n", self.image_path));
        out.push_str(&format!("slot              : {}\n", self.slot));
        out.push_str(&format!("kind              : {}\n", self.kind));
        out.push_str(&format!("mode              : 0o{:o}\n", self.mode & 0o7777));
        out.push_str(&format!(
            "uid / gid         : {} / {}\n",
            self.uid, self.gid
        ));
        out.push_str(&format!("link count        : {}\n", self.link_count));
        out.push_str(&format!("flags             : 0x{:08x}\n", self.flags));
        out.push_str(&format!("size              : {} bytes\n", self.size_bytes));
        out.push_str(&format!("blocks allocated  : {}\n", self.blocks_allocated));
        out.push_str(&format!(
            "crtime / mtime    : {} / {}\n",
            self.created_at_secs, self.modified_at_secs
        ));
        out.push_str(&format!(
            "ctime / atime     : {} / {}\n",
            self.changed_at_secs, self.accessed_at_secs
        ));
        out.push_str(&format!("generation        : {}\n", self.generation));
        out.push_str(&format!("domain inode id   : {}\n", self.domain_inode_id));
        out.push_str(&format!("data crc          : 0x{:016x}\n", self.data_crc));
        out.push_str(&format!("xattr block addr  : {}\n", self.xattr_block_addr));
        out.push_str(&format!("index block addr  : {}\n", self.index_block_addr));
        out.push_str(&format!(
            "extents (embedded): {}{}\n",
            self.extents.len(),
            if self.has_external_extent_index {
                " (external index present — see index_block_addr)"
            } else {
                ""
            }
        ));
        for (i, e) in self.extents.iter().enumerate() {
            out.push_str(&format!(
                "  [{i}] logical={} physical={} len={}\n",
                e.logical_block, e.physical_block, e.length_blocks
            ));
        }
        out
    }

    fn render_json(&self) -> String {
        to_pretty_json(self)
    }
}

/// Liest und dekodiert den Inode-Slot `slot` aus der Image-Datei `path`.
///
/// Schlägt fehl, wenn `slot` außerhalb des Inode-Tables liegt oder im
/// Inode-Bitmap als `unused` markiert ist.
///
/// # Beispiel
///
/// ```no_run
/// use corefs_tools::dump::inode;
/// use corefs_tools::Report;
///
/// let report = inode("/tmp/demo.img", 0).unwrap();
/// println!("{}", report.render_text());
/// ```
pub fn inode(path: impl AsRef<Path>, slot: u64) -> ToolsResult<InodeDumpReport> {
    let path = path.as_ref();
    let device = FileImageDevice::open(path, true)?;
    let reader = OdfReader::open(&device)?;
    let rec = reader.read_on_disk_inode(slot)?;
    Ok(inode_report_from_record(
        path.display().to_string(),
        slot,
        &rec,
    ))
}

fn inode_report_from_record(image_path: String, slot: u64, rec: &OnDiskInode) -> InodeDumpReport {
    InodeDumpReport {
        image_path,
        slot,
        kind: kind_to_string(rec.kind),
        mode: rec.mode,
        uid: rec.uid,
        gid: rec.gid,
        link_count: rec.link_count,
        flags: rec.flags,
        size_bytes: rec.size_bytes,
        blocks_allocated: rec.blocks_allocated,
        created_at_secs: rec.created_at,
        modified_at_secs: rec.modified_at,
        changed_at_secs: rec.changed_at,
        accessed_at_secs: rec.accessed_at,
        generation: rec.generation,
        extents: rec
            .extents
            .iter()
            .map(|e| ExtentEntry {
                logical_block: e.logical_block,
                length_blocks: e.length_blocks,
                physical_block: e.physical_block,
            })
            .collect(),
        index_block_addr: rec.index_block_addr,
        xattr_block_addr: rec.xattr_block_addr,
        domain_inode_id: rec.domain_inode_id,
        data_crc: rec.data_crc,
        has_external_extent_index: rec.flags & FLAG_HAS_EXTENT_INDEX != 0,
    }
}

fn kind_to_string(k: OnDiskKind) -> String {
    match k {
        OnDiskKind::Unused => "Unused",
        OnDiskKind::File => "File",
        OnDiskKind::Directory => "Directory",
        OnDiskKind::Symlink => "Symlink",
        OnDiskKind::SystemPayload => "SystemPayload",
    }
    .to_string()
}

// =====================================================================
// shared helpers
// =====================================================================

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
