// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Extent-based Block-Store mit CoW, Dedup, Defragmentierung und
//! Heat-Tracking. Plattformneutral (alloc + hashbrown statt std).
//!
//! ## Phase-A-Refactor
//!
//! `BlockRecord` ist nun metadata-only — kein `bytes`-Feld mehr.
//! Datei-Inhalte leben auf einem [`BlockDevice`].  Für den alten
//! test-/Compat-Pfad (`write(inode, Vec<u8>)` / `read(inode)`)
//! hält `BlockStore` intern ein `MemoryDevice`, das als Fallback-
//! Gerät für die alten Signaturen fungiert.

use crate::domain::inode::InodeId;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use super::ondisk::checksum::Crc32c;
use super::ondisk::layout::BLOCK_SIZE;

const MAX_RMW_BYTES: usize = 16 * 1024 * 1024;

fn align_down_u64(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        value
    } else {
        value - (value % alignment)
    }
}

fn align_up_u64(value: u64, alignment: u64) -> u64 {
    if alignment == 0 || value.is_multiple_of(alignment) {
        value
    } else {
        value + (alignment - (value % alignment))
    }
}

// ---------------------------------------------------------------------------
// Flag constants for ExtentRef.flags
// ---------------------------------------------------------------------------

/// Extent-Flag: dieser Bereich ist ein Loch (keine physischen Daten).
pub const EXTENT_HOLE: u32 = 1 << 0;
/// Extent-Flag: Daten dieses Extents sind LZ4-komprimiert.
pub const EXTENT_COMPRESSED: u32 = 1 << 1;
/// Extent-Flag: Daten dieses Extents sind verschlüsselt.
pub const EXTENT_ENCRYPTED: u32 = 1 << 2;

// ---------------------------------------------------------------------------
// ExtentRef — one physical extent of a file
// ---------------------------------------------------------------------------

/// Beschreibt einen physischen Extent einer Datei auf dem Device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExtentRef {
    /// Logischer Block-Offset innerhalb der Datei (file-relative).
    pub logical_block: u32,
    /// Logische Länge in Bytes (was der Aufrufer sieht).
    pub logical_len: u32,
    /// Physische Startadresse auf dem Device.
    pub physical_block: u64,
    /// Anzahl belegter Device-Blöcke.
    pub length_blocks: u32,
    /// Physisch gespeicherte Bytes (nach Komprimierung, ≤ `length_blocks * BLOCK_SIZE`).
    pub physical_len: u32,
    /// CRC32C über die rohen Device-Bytes (pre-decrypt, pre-decompress).
    pub content_crc: u32,
    /// EXTENT_HOLE | EXTENT_COMPRESSED | EXTENT_ENCRYPTED.
    pub flags: u32,
}

// ---------------------------------------------------------------------------
// BlockRecord — metadata-only (no bytes in RAM)
// ---------------------------------------------------------------------------

/// Metadaten eines persistierten Inode-Inhalts. Kein `bytes`-Feld —
/// Datei-Bytes leben auf dem `BlockDevice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub inode: InodeId,
    /// Logische Datei-Größe (was der Aufrufer sieht).
    pub logical_size: u64,
    /// Extents in logischer Reihenfolge.
    pub extents: Vec<ExtentRef>,
    /// CRC32C über den gesamten logischen Inhalt.
    pub content_crc: u32,
    /// Globale Flags (z. B. EXTENT_COMPRESSED | EXTENT_ENCRYPTED).
    pub flags: u32,
}

impl BlockRecord {
    /// Liefert die Gesamtzahl belegter Device-Blöcke.
    pub fn total_blocks(&self) -> u64 {
        self.extents
            .iter()
            .map(|e| u64::from(e.length_blocks))
            .sum()
    }

    /// Liefert den Device-Block des ersten Extents (oder 0).
    pub fn first_physical_block(&self) -> u64 {
        self.extents
            .first()
            .map(|e| e.physical_block)
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// OldBlockRecord — backward-compat wrapper (tests, load-path, etc.)
// ---------------------------------------------------------------------------

/// Backward-Compat-Wrapper, der die alten Felder `bytes`, `checksum`,
/// `device_block` und `allocated_blocks` emuliert.
///
/// Wird von `BlockStore::read(inode)` und `BlockStore::remove(inode)` geliefert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldBlockRecord {
    pub inode: InodeId,
    pub bytes: Vec<u8>,
    /// FNV-artiger Checksum über `bytes` — für Test-Kompatibilität.
    pub checksum: u64,
    pub device_block: u64,
    pub allocated_blocks: u64,
}

impl OldBlockRecord {
    fn from_record_and_bytes(rec: &BlockRecord, bytes: Vec<u8>) -> Self {
        let cs = checksum(&bytes);
        OldBlockRecord {
            inode: rec.inode,
            bytes,
            checksum: cs,
            device_block: rec.first_physical_block(),
            allocated_blocks: rec.total_blocks(),
        }
    }
}

// ---------------------------------------------------------------------------
// Free list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeExtentRecord {
    pub device_block: u64,
    pub allocated_blocks: u64,
}

type FreeExtent = FreeExtentRecord;

// ---------------------------------------------------------------------------
// Allocator policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationStrategy {
    BestFit,
    FirstFit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocatorPolicy {
    pub strategy: AllocationStrategy,
    pub split_threshold_blocks: u64,
    pub coalesce_on_release: bool,
    pub tail_trim_enabled: bool,
    pub background_compaction_enabled: bool,
    pub fragmentation_threshold_percent: u8,
}

impl Default for AllocatorPolicy {
    fn default() -> Self {
        Self {
            strategy: AllocationStrategy::BestFit,
            split_threshold_blocks: 1,
            coalesce_on_release: true,
            tail_trim_enabled: true,
            background_compaction_enabled: false,
            fragmentation_threshold_percent: 25,
        }
    }
}

// ---------------------------------------------------------------------------
// Report types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupeStats {
    pub logical_blocks: usize,
    pub unique_blobs: usize,
    pub deduplicated_blocks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefragmentationReport {
    pub moved_entries: usize,
    pub reclaimed_gaps: usize,
    pub final_device_blocks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentationReport {
    pub free_extents: usize,
    pub total_free_blocks: u64,
    pub largest_free_extent: u64,
    pub fragmented_free_blocks: u64,
    pub fragmentation_percent: u8,
    pub needs_compaction: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationReport {
    pub before: FragmentationReport,
    pub after: FragmentationReport,
    pub heat_reallocation: Option<HeatReallocationReport>,
    pub defragmentation: Option<DefragmentationReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatReallocationReport {
    pub prioritized_inodes: usize,
    pub promoted_hot_inodes: usize,
    pub moved_entries: usize,
    pub final_device_blocks: u64,
}

/// Report returned by an explicit deduplication pass over all blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupePassReport {
    /// Total blobs inspected.
    pub blobs_scanned: usize,
    /// Total bytes inspected across all blobs.
    pub bytes_scanned: usize,
    /// Number of duplicate blobs consolidated.
    pub duplicates_consolidated: usize,
    /// Bytes reclaimed by consolidation.
    pub bytes_reclaimed: usize,
    /// Number of hash collisions detected.
    pub hash_collisions: usize,
    /// Reference-count mismatches.
    pub ref_count_mismatches: usize,
}

/// Snapshot of copy-on-write sharing state across all blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CowStats {
    pub shared_blobs: usize,
    pub shared_logical_bytes: usize,
    pub bytes_saved_by_sharing: usize,
    pub exclusive_blobs: usize,
    pub max_ref_count: usize,
}

/// A freed device-block range, suitable for issuing TRIM/discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreedExtent {
    pub device_block: u64,
    pub block_count: u64,
}

// ---------------------------------------------------------------------------
// DedupEntry — for dedup table
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct DedupEntry {
    physical_block: u64,
    length_blocks: u32,
    physical_len: u32,
    ref_count: u32,
}

// ---------------------------------------------------------------------------
// BlockStore
// ---------------------------------------------------------------------------

/// Extent-basierter Block-Store.
///
/// Hält keine Datei-Bytes im RAM — Inhalte leben auf dem `BlockDevice`.
/// Jede I/O-Methode nimmt `device: &mut dyn BlockDevice` entgegen.
///
/// **Backward-Compat-Pfad**: für Tests und den alten API-Kontrakt hält
/// `BlockStore` intern ein `MemoryDevice` (`compat_device`).  Die alten
/// Signaturen `write(inode, Vec<u8>)` und `read(inode) -> Option<OldBlockRecord>`
/// arbeiten gegen dieses interne Device.
pub struct BlockStore {
    block_size: usize,
    next_device_block: u64,
    policy: AllocatorPolicy,
    free_extents: Vec<FreeExtent>,
    /// Metadata-only records.
    records: BTreeMap<InodeId, BlockRecord>,
    /// CRC32C → DedupEntry für Deduplizierung.
    dedup_table: HashMap<u32, DedupEntry>,
    /// Pending TRIM extents.
    pending_trims: Vec<FreedExtent>,
    /// Internes Device für den Compat-Pfad (write/read ohne explizites device-Arg).
    compat_device: MemoryDevice,
}

impl core::fmt::Debug for BlockStore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BlockStore")
            .field("block_size", &self.block_size)
            .field("next_device_block", &self.next_device_block)
            .field("records_count", &self.records.len())
            .finish()
    }
}

/// Interne Konstante: wie groß das compat MemoryDevice ist (64 MiB).
const COMPAT_DEVICE_BYTES: u64 = 64 * 1024 * 1024;

impl BlockStore {
    pub fn with_block_size(block_size: usize) -> Self {
        Self::with_block_size_and_policy(block_size, AllocatorPolicy::default())
    }

    pub fn with_block_size_and_policy(block_size: usize, policy: AllocatorPolicy) -> Self {
        // The compat device uses the block_size as sector size (must be power-of-2, ≥4).
        // We round up to the next power-of-2 if needed, minimum 4.
        let effective_block_size = block_size.max(4);
        // Next power of two >= effective_block_size
        let sector_size = {
            let mut s = 1u32;
            while (s as usize) < effective_block_size {
                s <<= 1;
            }
            s
        };
        // Compat device must be a multiple of sector_size.
        // Use at least 64 MiB aligned to sector_size.
        let cap = ((COMPAT_DEVICE_BYTES / u64::from(sector_size)) * u64::from(sector_size)).max(u64::from(sector_size));
        let compat_device = MemoryDevice::new(cap, sector_size).expect("compat device");
        Self {
            block_size: block_size.max(1),
            next_device_block: 0,
            policy,
            free_extents: Vec::new(),
            records: BTreeMap::new(),
            dedup_table: HashMap::new(),
            pending_trims: Vec::new(),
            compat_device,
        }
    }

    // -----------------------------------------------------------------------
    // Compat write/read (backward-compatible API using internal MemoryDevice)
    // -----------------------------------------------------------------------

    /// Schreibt `bytes` für `inode` auf das interne Compat-Device.
    ///
    /// Gibt die Anzahl geschriebener Bytes zurück.
    pub fn write(&mut self, inode: InodeId, bytes: Vec<u8>) -> usize {
        let size = bytes.len();
        let crc = if bytes.is_empty() { 0 } else { Crc32c::hash(&bytes) };

        // Block-aligned size
        let bs = self.block_size as u64;
        let needed_blocks = if size == 0 { 1 } else { (size as u64).div_ceil(bs) };

        // Check if existing record can be reused
        let existing = self.records.get(&inode).cloned();
        let (phys_block, allocated_blocks) = match &existing {
            Some(rec) if rec.total_blocks() >= needed_blocks => {
                let phys_block = rec.first_physical_block();
                let existing_blocks = rec.total_blocks();
                // Release old dedup ref
                let old_crc = rec.content_crc;
                self.dedup_release(old_crc, phys_block, existing_blocks as u32);
                // If shrinking, free the tail
                if existing_blocks > needed_blocks {
                    let tail_block = phys_block.saturating_add(needed_blocks);
                    let tail_count = existing_blocks - needed_blocks;
                    self.pending_trims.push(FreedExtent {
                        device_block: tail_block,
                        block_count: tail_count,
                    });
                    self.insert_free_extent(FreeExtent {
                        device_block: tail_block,
                        allocated_blocks: tail_count,
                    });
                }
                (phys_block, needed_blocks)
            }
            _ => {
                // Release old record if any
                if let Some(rec) = &existing {
                    let phys_block = rec.first_physical_block();
                    let existing_blocks = rec.total_blocks();
                    let old_crc = rec.content_crc;
                    self.dedup_release(old_crc, phys_block, existing_blocks as u32);
                    self.pending_trims.push(FreedExtent {
                        device_block: phys_block,
                        block_count: existing_blocks,
                    });
                    self.insert_free_extent(FreeExtent {
                        device_block: phys_block,
                        allocated_blocks: existing_blocks,
                    });
                }
                self.allocate_extent(needed_blocks.max(1))
            }
        };
        self.records.remove(&inode);

        // Write bytes to compat device.
        //
        // Phase-3 hot-path fix: pre-Phase-3 this always allocated a
        // padded scratch buffer and memcpy'd the payload into it
        // before handing it off to `compat_device.write_at`.  For
        // block-aligned payloads that intermediate buffer is a pure
        // copy, and for a 32 MiB seq_write it was one of three full
        // copies the raw write path did per call (clone → aligned buf
        // → device).  The fix skips the scratch buffer whenever the
        // payload already fills the allocated span exactly, writing
        // the caller's bytes in-place.  Only the rare unaligned-tail
        // case still pays for a padded copy.
        //
        // Phase-3d correctness fix: on-demand grow the compat device
        // when the allocator hands us an offset past the current
        // capacity.  Pre-3d the store silently dropped the write,
        // which let `write_file` "succeed" while leaving the on-disk
        // content truncated to the initial 64 MiB — a correctness
        // issue that made large-file workloads lose data.  We double
        // the capacity (or jump to `end`, whichever is larger) so
        // repeated growth is amortised.
        let byte_offset = phys_block * bs;
        let padded_len = (allocated_blocks * bs) as usize;
        let end_offset = byte_offset.saturating_add(padded_len as u64);
        if end_offset > self.compat_device.capacity() {
            let cur = self.compat_device.capacity();
            let target = core::cmp::max(cur.saturating_mul(2), end_offset);
            let ss = u64::from(self.compat_device.sector_size());
            let aligned = target.div_ceil(ss) * ss;
            let _ = self.compat_device.resize(aligned);
        }
        if end_offset <= self.compat_device.capacity() {
            if padded_len == size {
                let _ = self.compat_device.write_at(byte_offset, &bytes[..size]);
            } else {
                let mut buf = vec![0u8; padded_len];
                let copy_end = size.min(padded_len);
                buf[..copy_end].copy_from_slice(&bytes[..copy_end]);
                let _ = self.compat_device.write_at(byte_offset, &buf);
            }
        }

        // Update dedup table
        self.dedup_insert(crc, phys_block, allocated_blocks as u32, size as u32);

        let extent = ExtentRef {
            logical_block: 0,
            logical_len: size as u32,
            physical_block: phys_block,
            length_blocks: allocated_blocks as u32,
            physical_len: size as u32,
            content_crc: crc,
            flags: 0,
        };
        let record = BlockRecord {
            inode,
            logical_size: size as u64,
            extents: if size == 0 { Vec::new() } else { vec![extent] },
            content_crc: crc,
            flags: 0,
        };
        self.records.insert(inode, record);
        size
    }

    /// Liest die Bytes für `inode` vom internen Compat-Device zurück.
    pub fn read(&self, inode: InodeId) -> Option<OldBlockRecord> {
        let rec = self.records.get(&inode)?;
        let bytes = self.read_bytes_internal(rec);
        Some(OldBlockRecord::from_record_and_bytes(rec, bytes))
    }

    /// Hängt `extra` an den bestehenden Inhalt von `inode` an.
    ///
    /// Phase-1 P2: für plain (unflagged) Records wird ein **neuer Extent**
    /// am Ende angehängt — es werden keine bestehenden Bytes gelesen oder
    /// umkopiert.  Das macht sequentielle Streaming-Writes O(append_size)
    /// statt O(existing + append) und eliminiert die quadratische
    /// Skalierung, die bei wiederholten `append_to_inode`-Calls auf
    /// wachsende Dateien entstand (siehe `PERFORMANCE_LOG.md` Phase 1).
    ///
    /// Für Records mit `flags != 0` (compressed/encrypted) fällt die
    /// Funktion auf den klassischen Read-Modify-Write-Pfad zurück, weil
    /// ein Plain-Extent hinter einem komprimierten Prefix die
    /// read/decrypt/decompress-Pipeline brechen würde.  Multi-format
    /// per-Extent-Support ist Scope für P2b.
    pub fn append_to_inode(&mut self, inode: InodeId, extra: &[u8]) -> usize {
        // Empty append: no-op, return current logical size.
        if extra.is_empty() {
            return self
                .records
                .get(&inode)
                .map(|r| r.logical_size as usize)
                .unwrap_or(0);
        }

        // No existing record → full write path.
        let Some(existing) = self.records.get(&inode).cloned() else {
            return self.write(inode, extra.to_vec());
        };

        // Empty file (no extents) → full write path.
        if existing.extents.is_empty() || existing.logical_size == 0 {
            return self.write(inode, extra.to_vec());
        }

        // Compressed / encrypted prefix blocks the cheap append path.
        // Appending a plain extent behind the existing compressed/encrypted
        // extents would mean mixed pipeline state per extent, which the
        // current read_bytes_internal does not support.  Fall back to RMW.
        if existing.flags != 0 || existing.extents.iter().any(|e| e.flags != 0) {
            let existing_bytes = self.read_bytes_internal(&existing);
            let mut new_bytes = existing_bytes;
            new_bytes.extend_from_slice(extra);
            return self.write(inode, new_bytes);
        }

        // ── Fast path: allocate a new extent and append it ──────────────
        let bs = self.block_size as u64;
        let extra_len = extra.len();
        let needed_blocks = (extra_len as u64).div_ceil(bs);
        let (new_phys, new_alloc) = self.allocate_extent(needed_blocks);

        // Materialise extra bytes, zero-padded to the allocated block span.
        let padded_len = (new_alloc * bs) as usize;
        let mut buf = vec![0u8; padded_len];
        buf[..extra_len].copy_from_slice(extra);

        let byte_offset = new_phys * bs;
        let end_offset = byte_offset.saturating_add(padded_len as u64);
        if end_offset > self.compat_device.capacity() {
            let cur = self.compat_device.capacity();
            let target = core::cmp::max(cur.saturating_mul(2), end_offset);
            let ss = u64::from(self.compat_device.sector_size());
            let aligned = target.div_ceil(ss) * ss;
            let _ = self.compat_device.resize(aligned);
        }
        if end_offset <= self.compat_device.capacity() {
            let _ = self.compat_device.write_at(byte_offset, &buf);
        }

        // Per-extent CRC is over the logical (unpadded) appended bytes,
        // matching the convention used for single-extent records elsewhere.
        let extra_crc = Crc32c::hash(extra);

        // Logical block offset of the new extent = blocks covered by all
        // preceding extents.  We use length_blocks (allocated) rather than
        // logical_len / bs so the mapping matches how read_bytes_internal
        // iterates device blocks.
        let logical_block_start: u32 = existing
            .extents
            .iter()
            .map(|e| e.length_blocks)
            .sum();

        let new_extent = ExtentRef {
            logical_block: logical_block_start,
            logical_len: extra_len as u32,
            physical_block: new_phys,
            length_blocks: new_alloc as u32,
            physical_len: extra_len as u32,
            content_crc: extra_crc,
            flags: 0,
        };

        // Incremental record-level CRC over (existing_logical_bytes || extra).
        // Crc32c::hash = !update(!0, data); hash-combine requires the raw
        // running seed.  We recover it as !existing.content_crc, advance
        // with the extra bytes, then invert back.
        let combined_crc = !Crc32c::update(!existing.content_crc, extra);

        // Dedup bookkeeping: once a record spans multiple extents its
        // combined CRC can no longer be resolved back to a single
        // (phys_block, length_blocks) tuple, so we drop the old entry to
        // avoid handing out stale dedup hits.  We do NOT insert a new entry
        // for the combined CRC; multi-extent dedup is out of scope here.
        let first_extent = &existing.extents[0];
        let first_blocks = first_extent.length_blocks;
        let first_phys = first_extent.physical_block;
        self.dedup_release(existing.content_crc, first_phys, first_blocks);

        let mut rec = existing;
        rec.extents.push(new_extent);
        rec.logical_size = rec.logical_size.saturating_add(extra_len as u64);
        rec.content_crc = combined_crc;
        let new_size = rec.logical_size as usize;
        self.records.insert(inode, rec);

        new_size
    }

    /// Liest alle Bytes für `inode` (intern).
    fn read_bytes_internal(&self, rec: &BlockRecord) -> Vec<u8> {
        if rec.extents.is_empty() || rec.logical_size == 0 {
            return Vec::new();
        }
        let bs = self.block_size as u64;
        let mut out = Vec::with_capacity(rec.logical_size as usize);
        for ext in &rec.extents {
            let byte_offset = ext.physical_block * bs;
            let read_len = u64::from(ext.length_blocks) * bs;
            if byte_offset + read_len <= self.compat_device.capacity() {
                if let Ok(buf) = self.compat_device.read_at(byte_offset, read_len) {
                    let want = (ext.logical_len as usize).min(buf.len());
                    out.extend_from_slice(&buf[..want]);
                }
            }
        }
        out.truncate(rec.logical_size as usize);
        out
    }

    // -----------------------------------------------------------------------
    // New device-passing API (Phase A target API)
    // -----------------------------------------------------------------------

    /// Schreibt `data` an Byte-Offset `offset` für `inode` auf `device`.
    pub fn write_at(
        &mut self,
        device: &mut dyn BlockDevice,
        inode: InodeId,
        offset: u64,
        data: &[u8],
    ) -> CoreFsResult<()> {
        if data.is_empty() {
            return Ok(());
        }

        let current_size = self
            .records
            .get(&inode)
            .map(|r| r.logical_size)
            .unwrap_or(0);

        // Fast paths used by AnyOS' kernel driver:
        //
        // * A full rewrite at offset 0 does not need to read old content.
        // * A write exactly at EOF is an append.  Preserve existing extents
        //   and add one new extent instead of doing read-all + write-all.
        if offset == 0 && data.len() as u64 >= current_size {
            return self.write_device(device, inode, data);
        }
        if offset == current_size {
            self.append_device(device, inode, data)?;
            return Ok(());
        }

        // Correct fallback for true random writes / sparse growth.  This is
        // still O(file_size), but `read_all` now streams extent ranges instead
        // of materialising unrelated device blocks.  A later patch can replace
        // this with per-block partial rewrite + CRC recompute.
        let existing = self.read_all(device, inode).unwrap_or_default();
        let write_end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| CoreFsError::InvalidInput("block store write offset overflow".into()))?;
        let new_size_u64 = write_end.max(existing.len() as u64);
        if new_size_u64 > MAX_RMW_BYTES as u64 {
            return Err(CoreFsError::InvalidInput(format!(
                "block store random write would materialize {new_size_u64} bytes"
            )));
        }
        let new_size = new_size_u64 as usize;
        let mut new_bytes = vec![0u8; new_size];
        new_bytes[..existing.len()].copy_from_slice(&existing);
        let start = offset as usize;
        let end = start + data.len();
        if end <= new_bytes.len() {
            new_bytes[start..end].copy_from_slice(data);
        }
        self.write_device(device, inode, &new_bytes)
    }

    /// Vollständiger Schreibvorgang: ersetzt den kompletten Inhalt von `inode`.
    pub fn write_device(
        &mut self,
        device: &mut dyn BlockDevice,
        inode: InodeId,
        data: &[u8],
    ) -> CoreFsResult<()> {
        let size = data.len();
        let bs = BLOCK_SIZE;
        let needed_blocks = if size == 0 { 1u64 } else { (size as u64).div_ceil(bs) };
        let crc = if data.is_empty() { 0 } else { Crc32c::hash(data) };

        // Remove old record and free its blocks
        if let Some(old_rec) = self.records.remove(&inode) {
            let phys_block = old_rec.first_physical_block();
            let existing_blocks = old_rec.total_blocks();
            let old_crc = old_rec.content_crc;
            self.dedup_release(old_crc, phys_block, existing_blocks as u32);
            self.pending_trims.push(FreedExtent {
                device_block: phys_block,
                block_count: existing_blocks,
            });
            self.insert_free_extent(FreeExtent {
                device_block: phys_block,
                allocated_blocks: existing_blocks,
            });
        }

        let (phys_block, allocated_blocks) = self.allocate_extent(needed_blocks);

        // Write to device
        if size > 0 {
            let padded_len = (allocated_blocks * bs) as usize;
            let mut buf = vec![0u8; padded_len];
            buf[..size.min(padded_len)].copy_from_slice(&data[..size.min(padded_len)]);
            device.write_at(phys_block * bs, &buf)?;
        }

        self.dedup_insert(crc, phys_block, allocated_blocks as u32, size as u32);

        let extent = ExtentRef {
            logical_block: 0,
            logical_len: size as u32,
            physical_block: phys_block,
            length_blocks: allocated_blocks as u32,
            physical_len: size as u32,
            content_crc: crc,
            flags: 0,
        };
        let record = BlockRecord {
            inode,
            logical_size: size as u64,
            extents: if size == 0 { Vec::new() } else { vec![extent] },
            content_crc: crc,
            flags: 0,
        };
        self.records.insert(inode, record);
        Ok(())
    }

    /// Liest `out.len()` Bytes ab Offset `offset` für `inode` von `device`.
    pub fn read_bytes(
        &self,
        device: &dyn BlockDevice,
        inode: InodeId,
        offset: u64,
        out: &mut [u8],
    ) -> CoreFsResult<usize> {
        let rec = match self.records.get(&inode) {
            Some(r) => r,
            None => return Ok(0),
        };
        if out.is_empty() || offset >= rec.logical_size {
            return Ok(0);
        }

        let wanted_end = offset
            .saturating_add(out.len() as u64)
            .min(rec.logical_size);
        let mut copied = 0usize;
        let mut logical_cursor = 0u64;
        let sector_size = u64::from(device.sector_size());
        let block_size = BLOCK_SIZE;

        for ext in &rec.extents {
            let ext_len = u64::from(ext.logical_len).min(ext.physical_len as u64);
            let ext_start = logical_cursor;
            let ext_end = ext_start.saturating_add(ext_len);
            logical_cursor = ext_end;

            if ext_len == 0 || ext_end <= offset {
                continue;
            }
            if ext_start >= wanted_end {
                break;
            }

            let overlap_start = offset.max(ext_start);
            let overlap_end = wanted_end.min(ext_end);
            if overlap_start >= overlap_end {
                continue;
            }

            let inside_extent = overlap_start - ext_start;
            let physical_start = ext
                .physical_block
                .saturating_mul(block_size)
                .saturating_add(inside_extent);
            let physical_end = physical_start.saturating_add(overlap_end - overlap_start);
            let aligned_start = align_down_u64(physical_start, sector_size);
            let aligned_end = align_up_u64(physical_end, sector_size);
            let buf = device.read_at(aligned_start, aligned_end - aligned_start)?;
            let slice_start = (physical_start - aligned_start) as usize;
            let slice_end = slice_start + (overlap_end - overlap_start) as usize;
            let out_start = (overlap_start - offset) as usize;
            let out_end = out_start + (overlap_end - overlap_start) as usize;
            out[out_start..out_end].copy_from_slice(&buf[slice_start..slice_end]);
            copied = copied.max(out_end);
        }

        Ok(copied)
    }

    /// Liest den kompletten Inhalt von `inode` von `device`.
    pub fn read_all(&self, device: &dyn BlockDevice, inode: InodeId) -> CoreFsResult<Vec<u8>> {
        let rec = match self.records.get(&inode) {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };
        self.read_all_from_device(device, rec)
    }

    fn read_all_from_device(
        &self,
        device: &dyn BlockDevice,
        rec: &BlockRecord,
    ) -> CoreFsResult<Vec<u8>> {
        if rec.extents.is_empty() || rec.logical_size == 0 {
            return Ok(Vec::new());
        }
        let bs = BLOCK_SIZE;
        let mut out = Vec::with_capacity(rec.logical_size as usize);
        for ext in &rec.extents {
            let byte_offset = ext.physical_block * bs;
            let read_len = u64::from(ext.length_blocks) * bs;
            let buf = device.read_at(byte_offset, read_len)?;
            let want = (ext.logical_len as usize).min(buf.len());
            out.extend_from_slice(&buf[..want]);
        }
        out.truncate(rec.logical_size as usize);
        Ok(out)
    }

    /// Hängt `extra` an den Inhalt von `inode` auf `device` an.
    pub fn append_device(
        &mut self,
        device: &mut dyn BlockDevice,
        inode: InodeId,
        extra: &[u8],
    ) -> CoreFsResult<usize> {
        if extra.is_empty() {
            return Ok(self.records.get(&inode).map(|r| r.logical_size as usize).unwrap_or(0));
        }

        let Some(existing) = self.records.get(&inode).cloned() else {
            self.write_device(device, inode, extra)?;
            return Ok(extra.len());
        };

        if existing.extents.is_empty() || existing.logical_size == 0 {
            self.write_device(device, inode, extra)?;
            return Ok(extra.len());
        }

        if existing.flags != 0 || existing.extents.iter().any(|e| e.flags != 0) {
            let mut bytes = self.read_all_from_device(device, &existing)?;
            bytes.extend_from_slice(extra);
            let len = bytes.len();
            self.write_device(device, inode, &bytes)?;
            return Ok(len);
        }

        let bs = BLOCK_SIZE;
        let extra_len = extra.len();
        let combined_crc = !Crc32c::update(!existing.content_crc, extra);

        if let Some(first) = existing.extents.first() {
            self.dedup_release(existing.content_crc, first.physical_block, first.length_blocks);
        }

        let mut rec = existing;
        let mut remaining = extra;

        if let Some(last) = rec.extents.last_mut() {
            let capacity = u64::from(last.length_blocks).saturating_mul(bs);
            let used = u64::from(last.physical_len);
            if last.flags == 0 && used < capacity {
                let tail_len = remaining.len().min((capacity - used) as usize);
                if tail_len > 0 {
                    let block_offset = last.physical_block * bs;
                    let block_len = u64::from(last.length_blocks) * bs;
                    let mut block = device.read_at(block_offset, block_len)?;
                    let start = used as usize;
                    block[start..start + tail_len].copy_from_slice(&remaining[..tail_len]);
                    device.write_at(block_offset, &block)?;

                    last.logical_len = last.logical_len.saturating_add(tail_len as u32);
                    last.physical_len = last.physical_len.saturating_add(tail_len as u32);
                    last.content_crc = Crc32c::hash(&block[..last.physical_len as usize]);
                    remaining = &remaining[tail_len..];
                }
            }
        }

        if !remaining.is_empty() {
            let needed_blocks = (remaining.len() as u64).div_ceil(bs);
            let (new_phys, new_alloc) = self.allocate_extent(needed_blocks.max(1));
            let padded_len = (new_alloc * bs) as usize;
            let mut buf = vec![0u8; padded_len];
            buf[..remaining.len()].copy_from_slice(remaining);
            device.write_at(new_phys * bs, &buf)?;

            let logical_block_start = rec.extents.iter().map(|e| e.length_blocks).sum();
            let new_extent = ExtentRef {
                logical_block: logical_block_start,
                logical_len: remaining.len() as u32,
                physical_block: new_phys,
                length_blocks: new_alloc as u32,
                physical_len: remaining.len() as u32,
                content_crc: Crc32c::hash(remaining),
                flags: 0,
            };
            rec.extents.push(new_extent);
        }

        rec.logical_size = rec.logical_size.saturating_add(extra_len as u64);
        rec.content_crc = combined_crc;
        let new_size = rec.logical_size as usize;
        self.records.insert(inode, rec);
        Ok(new_size)
    }

    /// Entfernt `inode` aus dem Store und gibt den `BlockRecord` zurück.
    pub fn remove_inode(
        &mut self,
        device: &mut dyn BlockDevice,
        inode: InodeId,
    ) -> Option<BlockRecord> {
        let rec = self.records.remove(&inode)?;
        let phys_block = rec.first_physical_block();
        let existing_blocks = rec.total_blocks();
        let old_crc = rec.content_crc;
        self.dedup_release(old_crc, phys_block, existing_blocks as u32);
        self.pending_trims.push(FreedExtent {
            device_block: phys_block,
            block_count: existing_blocks,
        });
        self.insert_free_extent(FreeExtent {
            device_block: phys_block,
            allocated_blocks: existing_blocks,
        });
        let _ = device; // device arg für zukünftige TRIM-Forwarding
        Some(rec)
    }

    /// Setzt die Größe von `inode` auf `new_size`.
    pub fn truncate(
        &mut self,
        device: &mut dyn BlockDevice,
        inode: InodeId,
        new_size: u64,
    ) -> CoreFsResult<()> {
        let existing = self.read_all(device, inode).unwrap_or_default();
        let mut new_bytes = existing;
        new_bytes.resize(new_size as usize, 0);
        self.write_device(device, inode, &new_bytes)
    }

    /// Verifiziert den Inhalt von `inode` auf `device`.
    pub fn verify_device(&self, device: &dyn BlockDevice, inode: InodeId) -> bool {
        let rec = match self.records.get(&inode) {
            Some(r) => r,
            None => return false,
        };
        let bytes = match self.read_all_from_device(device, rec) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let crc = if bytes.is_empty() { 0 } else { Crc32c::hash(&bytes) };
        crc == rec.content_crc
    }

    // -----------------------------------------------------------------------
    // Old API (compat — uses internal MemoryDevice)
    // -----------------------------------------------------------------------

    pub fn contains(&self, inode: InodeId) -> bool {
        self.records.contains_key(&inode)
    }

    /// Entfernt `inode` und gibt einen `OldBlockRecord` zurück.
    pub fn remove(&mut self, inode: InodeId) -> Option<OldBlockRecord> {
        let rec = self.records.get(&inode)?.clone();
        let bytes = self.read_bytes_internal(&rec);
        self.records.remove(&inode);

        let phys_block = rec.first_physical_block();
        let existing_blocks = rec.total_blocks();
        let old_crc = rec.content_crc;
        self.dedup_release(old_crc, phys_block, existing_blocks as u32);
        self.pending_trims.push(FreedExtent {
            device_block: phys_block,
            block_count: existing_blocks,
        });
        self.insert_free_extent(FreeExtent {
            device_block: phys_block,
            allocated_blocks: existing_blocks,
        });
        Some(OldBlockRecord::from_record_and_bytes(&rec, bytes))
    }

    /// Verifiziert den Inhalt von `inode` gegen das interne Compat-Device.
    pub fn verify(&self, inode: InodeId) -> bool {
        let rec = match self.records.get(&inode) {
            Some(r) => r,
            None => return false,
        };
        let bytes = self.read_bytes_internal(rec);
        let crc = if bytes.is_empty() { 0 } else { Crc32c::hash(&bytes) };
        crc == rec.content_crc
    }

    /// Gibt alle `BlockRecord`s zurück (Metadaten).
    pub fn records(&self) -> Vec<BlockRecord> {
        self.records.values().cloned().collect()
    }

    /// Gibt eine Referenz auf den `BlockRecord` für `inode`.
    pub fn record(&self, inode: InodeId) -> Option<&BlockRecord> {
        self.records.get(&inode)
    }

    // -----------------------------------------------------------------------
    // from_records constructors
    // -----------------------------------------------------------------------

    pub fn from_records(records: Vec<BlockRecord>) -> Self {
        let mut store = Self::default();
        store.ingest_records(records);
        store
    }

    pub fn from_records_with_block_size(records: Vec<BlockRecord>, block_size: usize) -> Self {
        let mut store = Self::with_block_size(block_size);
        store.ingest_records(records);
        store
    }

    pub fn from_records_with_allocator(
        records: Vec<BlockRecord>,
        block_size: usize,
        policy: AllocatorPolicy,
        free_extents: Vec<FreeExtentRecord>,
    ) -> Self {
        let mut store = Self::with_block_size_and_policy(block_size, policy);
        store.ingest_records(records);
        if store.adopt_free_extents(free_extents).is_err() {
            store.rebuild_free_extents();
        }
        store
    }

    /// Konstruiert einen `BlockStore` mit explizitem `next_device_block`-Wert
    /// (verwendet beim Mount nach `load_state_native`).
    pub fn from_records_with_allocator_and_start(
        records: Vec<BlockRecord>,
        block_size: usize,
        policy: AllocatorPolicy,
        free_extents: Vec<FreeExtentRecord>,
        first_data_block: u64,
    ) -> Self {
        let mut store = Self::with_block_size_and_policy(block_size, policy);
        store.next_device_block = first_data_block;
        store.ingest_records(records);
        if store.adopt_free_extents(free_extents).is_err() {
            store.rebuild_free_extents();
        }
        store
    }

    // -----------------------------------------------------------------------
    // Allocator policy access
    // -----------------------------------------------------------------------

    pub fn allocator_policy(&self) -> &AllocatorPolicy {
        &self.policy
    }

    pub fn free_extents(&self) -> Vec<FreeExtentRecord> {
        self.free_extents.clone()
    }

    pub fn drain_freed_extents(&mut self) -> Vec<FreedExtent> {
        core::mem::take(&mut self.pending_trims)
    }

    pub fn pending_trims(&self) -> &[FreedExtent] {
        &self.pending_trims
    }

    pub fn set_allocator_policy(&mut self, policy: AllocatorPolicy) {
        self.policy = policy;
        if self.policy.coalesce_on_release {
            self.normalize_free_extents();
        } else {
            self.free_extents.sort_by_key(|extent| extent.device_block);
            if self.policy.tail_trim_enabled {
                self.trim_free_tail();
            }
        }
    }

    // -----------------------------------------------------------------------
    // CoW / clone
    // -----------------------------------------------------------------------

    pub fn is_shared(&self, inode: InodeId) -> bool {
        let Some(rec) = self.records.get(&inode) else {
            return false;
        };
        let crc = rec.content_crc;
        if let Some(entry) = self.dedup_table.get(&crc) {
            entry.ref_count > 1
        } else {
            false
        }
    }

    pub fn blob_checksum(&self, inode: InodeId) -> Option<u64> {
        self.records
            .get(&inode)
            .map(|rec| u64::from(rec.content_crc))
    }

    /// Klont `source` nach `target` (CoW-Semantik).
    pub fn clone_for_inode(&mut self, source: InodeId, target: InodeId) -> bool {
        let Some(source_rec) = self.records.get(&source).cloned() else {
            return false;
        };
        // Read bytes from compat device, write to new location for target
        let bytes = self.read_bytes_internal(&source_rec);
        let crc = source_rec.content_crc;

        // Allocate new extent for target
        let size = bytes.len();
        let bs = self.block_size as u64;
        let needed_blocks = if size == 0 { 1u64 } else { (size as u64).div_ceil(bs) };
        let (phys_block, allocated_blocks) = self.allocate_extent(needed_blocks.max(1));

        // Write to compat device
        if size > 0 {
            let padded_len = (allocated_blocks * bs) as usize;
            let mut buf = vec![0u8; padded_len];
            buf[..size.min(padded_len)].copy_from_slice(&bytes[..size.min(padded_len)]);
            let byte_offset = phys_block * bs;
            let end_offset = byte_offset.saturating_add(padded_len as u64);
            if end_offset > self.compat_device.capacity() {
                let cur = self.compat_device.capacity();
                let target = core::cmp::max(cur.saturating_mul(2), end_offset);
                let ss = u64::from(self.compat_device.sector_size());
                let aligned = target.div_ceil(ss) * ss;
                let _ = self.compat_device.resize(aligned);
            }
            if end_offset <= self.compat_device.capacity() {
                let _ = self.compat_device.write_at(byte_offset, &buf);
            }
        }

        // Increment dedup ref_count by 1 for the clone (not insert fresh).
        // The source already has ref_count=1; after clone both source and target share it → ref_count=2.
        if let Some(entry) = self.dedup_table.get_mut(&crc) {
            entry.ref_count += 1;
        } else {
            // No existing entry (e.g. source was empty) — create one with ref_count=2.
            self.dedup_table.insert(crc, DedupEntry {
                physical_block: phys_block,
                length_blocks: allocated_blocks as u32,
                physical_len: size as u32,
                ref_count: 2,
            });
        }

        let extent = ExtentRef {
            logical_block: 0,
            logical_len: size as u32,
            physical_block: phys_block,
            length_blocks: allocated_blocks as u32,
            physical_len: size as u32,
            content_crc: crc,
            flags: 0,
        };
        let record = BlockRecord {
            inode: target,
            logical_size: size as u64,
            extents: if size == 0 { Vec::new() } else { vec![extent] },
            content_crc: crc,
            flags: 0,
        };
        self.records.insert(target, record);
        true
    }

    // -----------------------------------------------------------------------
    // Statistics
    // -----------------------------------------------------------------------

    pub fn cow_stats(&self) -> CowStats {
        let mut shared_blobs = 0usize;
        let mut shared_logical_bytes = 0usize;
        let mut bytes_saved_by_sharing = 0usize;
        let mut exclusive_blobs = 0usize;
        let mut max_ref_count = 0usize;

        for entry in self.dedup_table.values() {
            let rc = entry.ref_count as usize;
            max_ref_count = max_ref_count.max(rc);
            let size = entry.physical_len as usize;
            if rc > 1 {
                shared_blobs += 1;
                shared_logical_bytes =
                    shared_logical_bytes.saturating_add(size.saturating_mul(rc));
                bytes_saved_by_sharing =
                    bytes_saved_by_sharing.saturating_add(size.saturating_mul(rc - 1));
            } else if rc == 1 {
                exclusive_blobs += 1;
            }
        }

        CowStats {
            shared_blobs,
            shared_logical_bytes,
            bytes_saved_by_sharing,
            exclusive_blobs,
            max_ref_count,
        }
    }

    pub fn dedupe_stats(&self) -> DedupeStats {
        let logical_blocks = self.records.len();
        let unique_blobs = self.dedup_table.len();
        let deduplicated_blocks = logical_blocks.saturating_sub(unique_blobs);
        DedupeStats {
            logical_blocks,
            unique_blobs,
            deduplicated_blocks,
        }
    }

    pub fn dedup_pass(&mut self) -> DedupePassReport {
        // Simplified dedup pass: verify ref counts match records
        let blobs_scanned = self.dedup_table.len();
        let bytes_scanned: usize = self
            .dedup_table
            .values()
            .map(|e| e.physical_len as usize)
            .sum();

        // Rebuild expected ref counts from records
        let mut expected_refs: HashMap<u32, u32> = HashMap::new();
        for rec in self.records.values() {
            *expected_refs.entry(rec.content_crc).or_insert(0) += 1;
        }

        let mut ref_count_mismatches = 0usize;
        for (crc, entry) in self.dedup_table.iter_mut() {
            let expected = expected_refs.get(crc).copied().unwrap_or(0);
            if entry.ref_count != expected {
                entry.ref_count = expected;
                ref_count_mismatches += 1;
            }
        }
        // Remove entries with zero refs
        self.dedup_table.retain(|_, e| e.ref_count > 0);

        DedupePassReport {
            blobs_scanned,
            bytes_scanned,
            duplicates_consolidated: 0,
            bytes_reclaimed: 0,
            hash_collisions: 0,
            ref_count_mismatches,
        }
    }

    // -----------------------------------------------------------------------
    // Fragmentation + optimization
    // -----------------------------------------------------------------------

    pub fn fragmentation_report(&self) -> FragmentationReport {
        let total_free_blocks = self
            .free_extents
            .iter()
            .map(|e| e.allocated_blocks)
            .sum::<u64>();
        let largest_free_extent = self
            .free_extents
            .iter()
            .map(|e| e.allocated_blocks)
            .max()
            .unwrap_or(0);
        let fragmented_free_blocks = total_free_blocks.saturating_sub(largest_free_extent);
        let fragmentation_percent = if total_free_blocks == 0 {
            0
        } else {
            ((fragmented_free_blocks.saturating_mul(100)) / total_free_blocks).min(100) as u8
        };
        let threshold = self.policy.fragmentation_threshold_percent.min(100);
        FragmentationReport {
            free_extents: self.free_extents.len(),
            total_free_blocks,
            largest_free_extent,
            fragmented_free_blocks,
            fragmentation_percent,
            needs_compaction: self.policy.background_compaction_enabled
                && fragmentation_percent >= threshold
                && self.free_extents.len() > 1,
        }
    }

    pub fn defragment(&mut self) -> DefragmentationReport {
        let mut entries: Vec<_> = self
            .records
            .iter()
            .map(|(inode, rec)| (*inode, rec.clone()))
            .collect();
        entries.sort_by_key(|(_, rec)| rec.first_physical_block());

        let original_free = self.free_extents.len();
        let mut cursor = 0u64;
        let mut moved_entries = 0usize;

        for (_, rec) in &mut entries {
            let old_block = rec.first_physical_block();
            let blocks = rec.total_blocks();
            if old_block != cursor {
                // Move data in compat device
                let bs = self.block_size as u64;
                if blocks > 0
                    && old_block * bs + blocks * bs <= self.compat_device.capacity()
                    && cursor * bs + blocks * bs <= self.compat_device.capacity()
                {
                    if let Ok(data) = self.compat_device.read_at(old_block * bs, blocks * bs) {
                        let _ = self.compat_device.write_at(cursor * bs, &data);
                    }
                }
                // Update extent
                for ext in &mut rec.extents {
                    ext.physical_block = cursor;
                }
                moved_entries += 1;
            }
            cursor = cursor.saturating_add(blocks.max(1));
        }

        self.records = entries.into_iter().collect();
        self.free_extents.clear();
        self.next_device_block = cursor;

        DefragmentationReport {
            moved_entries,
            reclaimed_gaps: original_free,
            final_device_blocks: cursor,
        }
    }

    pub fn optimize(&mut self) -> OptimizationReport {
        self.optimize_with_priorities(&[])
    }

    pub fn optimize_with_priorities(&mut self, prioritized: &[InodeId]) -> OptimizationReport {
        let before = self.fragmentation_report();
        let threshold = self.policy.fragmentation_threshold_percent.min(100);
        let should_compact = before.fragmentation_percent >= threshold && before.free_extents > 1;
        let heat_reallocation = if !prioritized.is_empty()
            && (should_compact || self.has_misplaced_priorities(prioritized))
        {
            Some(self.reallocate_prioritized_extents(prioritized))
        } else {
            None
        };
        let after_heat = self.fragmentation_report();
        let defragmentation = if heat_reallocation.is_none()
            && after_heat.fragmentation_percent >= threshold
            && after_heat.free_extents > 1
        {
            Some(self.defragment())
        } else {
            None
        };
        let after = self.fragmentation_report();
        OptimizationReport {
            before,
            after,
            heat_reallocation,
            defragmentation,
        }
    }

    fn has_misplaced_priorities(&self, prioritized: &[InodeId]) -> bool {
        let mut entries: Vec<_> = self.records.values().collect();
        entries.sort_by_key(|rec| rec.first_physical_block());
        let prioritized_filtered: Vec<_> = prioritized
            .iter()
            .copied()
            .filter(|inode| self.records.contains_key(inode))
            .collect();
        if prioritized_filtered.is_empty() {
            return false;
        }
        entries
            .iter()
            .take(prioritized_filtered.len())
            .map(|rec| rec.inode)
            .ne(prioritized_filtered)
    }

    pub fn reallocate_prioritized_extents(
        &mut self,
        prioritized: &[InodeId],
    ) -> HeatReallocationReport {
        let priority_map: HashMap<InodeId, usize> = prioritized
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, inode)| self.records.contains_key(inode))
            .map(|(index, inode)| (inode, index))
            .collect();
        let prioritized_inodes = priority_map.len();

        let mut entries: Vec<_> = self
            .records
            .iter()
            .map(|(inode, rec)| (*inode, rec.clone()))
            .collect();
        entries.sort_by(|left, right| {
            let left_rank = priority_map.get(&left.0).copied().unwrap_or(usize::MAX);
            let right_rank = priority_map.get(&right.0).copied().unwrap_or(usize::MAX);
            left_rank
                .cmp(&right_rank)
                .then_with(|| left.1.first_physical_block().cmp(&right.1.first_physical_block()))
        });

        let mut cursor = 0u64;
        let mut moved_entries = 0usize;
        let mut promoted_hot_inodes = 0usize;

        for (inode, rec) in &mut entries {
            let original = rec.first_physical_block();
            let blocks = rec.total_blocks().max(1);
            if original != cursor {
                // Move in compat device
                let bs = self.block_size as u64;
                if blocks > 0
                    && original * bs + blocks * bs <= self.compat_device.capacity()
                    && cursor * bs + blocks * bs <= self.compat_device.capacity()
                {
                    if let Ok(data) = self.compat_device.read_at(original * bs, blocks * bs) {
                        let _ = self.compat_device.write_at(cursor * bs, &data);
                    }
                }
                for ext in &mut rec.extents {
                    ext.physical_block = cursor;
                }
                moved_entries += 1;
                if priority_map.contains_key(inode) && cursor < original {
                    promoted_hot_inodes += 1;
                }
            }
            cursor = cursor.saturating_add(blocks);
        }

        self.records = entries.into_iter().collect();
        self.free_extents.clear();
        self.next_device_block = cursor;

        HeatReallocationReport {
            prioritized_inodes,
            promoted_hot_inodes,
            moved_entries,
            final_device_blocks: cursor,
        }
    }

    // -----------------------------------------------------------------------
    // Internal allocator helpers
    // -----------------------------------------------------------------------

    fn ingest_records(&mut self, records: Vec<BlockRecord>) {
        for record in records {
            let next = record
                .first_physical_block()
                .saturating_add(record.total_blocks().max(1));
            self.next_device_block = self.next_device_block.max(next);

            // Register in dedup table (logical_size as physical_len approximation)
            self.dedup_insert(
                record.content_crc,
                record.first_physical_block(),
                record.total_blocks() as u32,
                record.logical_size as u32,
            );

            self.records.insert(record.inode, record);
        }
        self.rebuild_free_extents();
    }

    fn allocate_extent(&mut self, required_blocks: u64) -> (u64, u64) {
        let index = match self.policy.strategy {
            AllocationStrategy::BestFit => self
                .free_extents
                .iter()
                .enumerate()
                .filter(|(_, e)| e.allocated_blocks >= required_blocks)
                .min_by_key(|(_, e)| e.allocated_blocks)
                .map(|(i, _)| i),
            AllocationStrategy::FirstFit => self
                .free_extents
                .iter()
                .enumerate()
                .find(|(_, e)| e.allocated_blocks >= required_blocks)
                .map(|(i, _)| i),
        };

        if let Some(index) = index {
            let extent = self.free_extents.remove(index);
            let remainder = extent.allocated_blocks.saturating_sub(required_blocks);
            if remainder >= self.policy.split_threshold_blocks.max(1) {
                self.insert_free_extent(FreeExtent {
                    device_block: extent.device_block.saturating_add(required_blocks),
                    allocated_blocks: remainder,
                });
                return (extent.device_block, required_blocks);
            }
            return (extent.device_block, extent.allocated_blocks);
        }

        let device_block = self.next_device_block;
        self.next_device_block = self.next_device_block.saturating_add(required_blocks);
        (device_block, required_blocks)
    }

    fn insert_free_extent(&mut self, extent: FreeExtent) {
        if extent.allocated_blocks == 0 {
            return;
        }
        self.free_extents.push(extent);
        if self.policy.coalesce_on_release {
            self.normalize_free_extents();
        } else {
            self.free_extents.sort_by_key(|e| e.device_block);
            if self.policy.tail_trim_enabled {
                self.trim_free_tail();
            }
        }
    }

    fn normalize_free_extents(&mut self) {
        self.free_extents.sort_by_key(|e| e.device_block);
        let mut merged: Vec<FreeExtent> = Vec::with_capacity(self.free_extents.len());
        for extent in self.free_extents.drain(..) {
            if let Some(last) = merged.last_mut() {
                let last_end = last.device_block.saturating_add(last.allocated_blocks);
                if last_end >= extent.device_block {
                    let extent_end = extent.device_block.saturating_add(extent.allocated_blocks);
                    last.allocated_blocks = extent_end.saturating_sub(last.device_block);
                    continue;
                }
            }
            merged.push(extent);
        }
        self.free_extents = merged;
        if self.policy.tail_trim_enabled {
            self.trim_free_tail();
        }
    }

    fn trim_free_tail(&mut self) {
        while let Some(last) = self.free_extents.last().copied() {
            let last_end = last.device_block.saturating_add(last.allocated_blocks);
            if last_end != self.next_device_block {
                break;
            }
            self.next_device_block = last.device_block;
            self.free_extents.pop();
        }
    }

    fn rebuild_free_extents(&mut self) {
        let mut occupied: Vec<(u64, u64)> = self
            .records
            .values()
            .map(|rec| {
                let start = rec.first_physical_block();
                let end = start.saturating_add(rec.total_blocks().max(1));
                (start, end)
            })
            .collect();
        occupied.sort_by_key(|(start, _)| *start);
        self.free_extents.clear();
        let mut cursor = 0u64;
        for (start, end) in occupied {
            if start > cursor {
                self.free_extents.push(FreeExtent {
                    device_block: cursor,
                    allocated_blocks: start - cursor,
                });
            }
            cursor = cursor.max(end);
        }
        self.next_device_block = self.next_device_block.max(cursor);
        if self.policy.tail_trim_enabled {
            self.trim_free_tail();
        }
    }

    fn adopt_free_extents(&mut self, free_extents: Vec<FreeExtentRecord>) -> Result<(), ()> {
        self.free_extents = free_extents
            .into_iter()
            .filter(|e| e.allocated_blocks > 0)
            .collect();
        if self.policy.coalesce_on_release {
            self.normalize_free_extents();
        } else {
            self.free_extents.sort_by_key(|e| e.device_block);
            if self.policy.tail_trim_enabled {
                self.trim_free_tail();
            }
        }
        self.validate_allocator_state()
    }

    fn validate_allocator_state(&mut self) -> Result<(), ()> {
        let mut occupied: Vec<(u64, u64)> = self
            .records
            .values()
            .map(|rec| {
                let start = rec.first_physical_block();
                let end = start.saturating_add(rec.total_blocks().max(1));
                (start, end)
            })
            .collect();
        occupied.sort_by_key(|(start, _)| *start);

        let mut free = self.free_extents.clone();
        free.sort_by_key(|e| e.device_block);
        for pair in free.windows(2) {
            let left = pair[0];
            let right = pair[1];
            if left.device_block.saturating_add(left.allocated_blocks) > right.device_block {
                return Err(());
            }
        }
        for extent in &free {
            let free_start = extent.device_block;
            let free_end = extent.device_block.saturating_add(extent.allocated_blocks);
            for (occ_start, occ_end) in &occupied {
                if free_start < *occ_end && *occ_start < free_end {
                    return Err(());
                }
            }
        }
        let max_free_end = free
            .iter()
            .map(|e| e.device_block.saturating_add(e.allocated_blocks))
            .max()
            .unwrap_or(0);
        let max_occ_end = occupied.iter().map(|(_, end)| *end).max().unwrap_or(0);
        self.next_device_block = self
            .next_device_block
            .max(max_free_end.max(max_occ_end));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Dedup table helpers
    // -----------------------------------------------------------------------

    fn dedup_insert(&mut self, crc: u32, physical_block: u64, length_blocks: u32, physical_len: u32) {
        if let Some(entry) = self.dedup_table.get_mut(&crc) {
            entry.ref_count += 1;
        } else {
            self.dedup_table.insert(
                crc,
                DedupEntry {
                    physical_block,
                    length_blocks,
                    physical_len,
                    ref_count: 1,
                },
            );
        }
    }

    fn dedup_release(&mut self, crc: u32, _physical_block: u64, _length_blocks: u32) {
        if let Some(entry) = self.dedup_table.get_mut(&crc) {
            entry.ref_count = entry.ref_count.saturating_sub(1);
            if entry.ref_count == 0 {
                self.dedup_table.remove(&crc);
            }
        }
    }
}

impl Default for BlockStore {
    fn default() -> Self {
        Self::with_block_size(4096)
    }
}

// ---------------------------------------------------------------------------
// Checksum compatibility helpers (FNV-like, for OldBlockRecord)
// ---------------------------------------------------------------------------

/// Kompatibler FNV-artiger Checksum (gleiche Semantik wie vor dem Refactor).
pub fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, byte| {
        acc.wrapping_mul(16777619).wrapping_add(u64::from(*byte))
    })
}

fn required_blocks(size: usize, block_size: usize) -> u64 {
    if size == 0 {
        1
    } else {
        size.div_ceil(block_size) as u64
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "block_store_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "block_store_characterization_tests.rs"]
mod char_tests;
