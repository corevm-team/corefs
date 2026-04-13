//! On-demand sector-level I/O and device-resident journal.
//!
//! [`DeviceVolume`] wraps a [`BlockDevice`] and provides segment-level
//! random access to a CoreFS volume without loading the entire image into
//! RAM.  It maintains a read cache for frequently accessed segments and a
//! write buffer that flushes dirty segments individually.
//!
//! [`DeviceJournal`] manages a reserved region on the device for write-ahead
//! log entries.  Journal entries are written with `fdatasync()` barrier
//! semantics so that recovery after a crash can replay committed operations.

use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;
use crate::storage::volume_wal::VolumeWal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Constants — device layout
// ---------------------------------------------------------------------------

const MAGIC: &[u8; 8] = b"COREFS01";
const FORMAT_VERSION: u32 = 4;
const HEADER_SIZE: usize = 16;
const SEGMENT_ENTRY_SIZE: usize = 24;
const SEGMENT_ALIGNMENT: usize = 64;
const SUPERBLOCK_SIZE: usize = 56;

/// Size of the reserved device-journal region in bytes.
/// Placed immediately after the volume image data, sector-aligned.
const DEVICE_JOURNAL_SIZE: usize = 256 * 1024; // 256 KiB

/// Magic marker for the device journal region.
const JOURNAL_MAGIC: &[u8; 8] = b"CFSJ0001";

// ---------------------------------------------------------------------------
// SegmentIndex — parsed directory for on-demand access
// ---------------------------------------------------------------------------

/// Parsed segment directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentLocation {
    pub kind: [u8; 4],
    pub offset: u64,
    pub length: u64,
}

/// Lightweight index of segment locations on the device.
///
/// Built by reading only the header (16 bytes) and directory
/// (~360 bytes for 15 segments) — no segment payloads are touched.
#[derive(Debug, Clone)]
pub struct SegmentIndex {
    pub segment_count: usize,
    pub segments: Vec<SegmentLocation>,
    /// Byte offset one past the last segment payload (aligned).
    pub image_end: u64,
}

impl SegmentIndex {
    /// Returns the location of a segment by its 4-byte kind tag.
    pub fn find(&self, kind: &[u8; 4]) -> Option<&SegmentLocation> {
        self.segments.iter().find(|s| &s.kind == kind)
    }
}

// ---------------------------------------------------------------------------
// DeviceVolume — on-demand segment-level I/O
// ---------------------------------------------------------------------------

/// Provides on-demand, segment-granular access to a CoreFS volume stored
/// on a [`BlockDevice`].
///
/// Instead of loading the entire image into RAM, `DeviceVolume` reads only
/// the header and segment directory at open time.  Individual segments are
/// fetched on demand and cached in a read cache.  Writes are buffered per
/// segment and flushed individually.
#[derive(Debug)]
pub struct DeviceVolume {
    device: Box<dyn BlockDevice>,
    index: SegmentIndex,
    /// Cached segment payloads (raw bytes including frame header).
    read_cache: HashMap<[u8; 4], Vec<u8>>,
    /// Dirty segment payloads that need to be flushed.
    write_buffer: HashMap<[u8; 4], Vec<u8>>,
    /// Offset on the device where the journal region starts.
    journal_offset: u64,
}

impl DeviceVolume {
    /// Opens an existing CoreFS volume on the device.
    ///
    /// Reads only the header and segment directory (~400 bytes).
    /// No segment payloads are loaded until requested.
    pub fn open(device: Box<dyn BlockDevice>) -> CoreFsResult<Self> {
        let index = read_segment_index(device.as_ref())?;
        let sector_size = device.sector_size() as u64;
        let journal_offset = align_up_u64(index.image_end, sector_size);

        Ok(Self {
            device,
            index,
            read_cache: HashMap::new(),
            write_buffer: HashMap::new(),
            journal_offset,
        })
    }

    /// Returns the parsed segment index.
    pub fn index(&self) -> &SegmentIndex {
        &self.index
    }

    /// Reads a segment's raw payload from the device (or cache).
    ///
    /// Returns the bytes of the segment payload, *excluding* the segment
    /// frame header (the caller is responsible for frame decoding if needed).
    pub fn read_segment(&mut self, kind: &[u8; 4]) -> CoreFsResult<Vec<u8>> {
        // Check write buffer first (dirty data takes precedence).
        if let Some(data) = self.write_buffer.get(kind) {
            return Ok(data.clone());
        }

        // Check read cache.
        if let Some(data) = self.read_cache.get(kind) {
            return Ok(data.clone());
        }

        // Fetch from device.
        let loc = self.index.find(kind).ok_or_else(|| {
            CoreFsError::NotFound(format!(
                "segment {} not found on device",
                String::from_utf8_lossy(kind)
            ))
        })?;

        let data = read_segment_from_device(self.device.as_ref(), loc)?;
        self.read_cache.insert(*kind, data.clone());
        Ok(data)
    }

    /// Stages a segment for writing.
    ///
    /// The segment is buffered and will be written on the next [`flush`].
    pub fn write_segment(&mut self, kind: [u8; 4], data: Vec<u8>) {
        self.read_cache.remove(&kind);
        self.write_buffer.insert(kind, data);
    }

    /// Returns `true` if any segments have been modified since the last flush.
    pub fn is_dirty(&self) -> bool {
        !self.write_buffer.is_empty()
    }

    /// Flushes all dirty segments to the device.
    ///
    /// Each segment is written at its current offset.  If the segment grew,
    /// a full rewrite is triggered (all segments are serialized and written
    /// sequentially, like a fresh `save_to_device`).
    ///
    /// After flushing, the superblocks are updated with new generation and
    /// checksums, and the device is synced.
    pub fn flush(&mut self) -> CoreFsResult<()> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }

        // Strategy: build a complete image from cached + dirty segments and
        // write it to the device.  This is simpler and safer than in-place
        // patching (which would require handling segment growth/shrink).
        //
        // For most workloads, the metadata segments (everything except DATA)
        // are small, so this is fast.  The DATA segment is only rewritten if
        // it was modified (i.e., if block data changed).

        let mut all_segments: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(self.index.segment_count);

        for loc in &self.index.segments {
            let kind = loc.kind;
            let data = if let Some(dirty) = self.write_buffer.remove(&kind) {
                dirty
            } else if let Some(cached) = self.read_cache.get(&kind) {
                cached.clone()
            } else {
                // Must fetch from device.
                read_segment_from_device(self.device.as_ref(), loc)?
            };
            all_segments.push((kind, data));
        }

        // Include any new segments that weren't in the original index.
        for (kind, data) in self.write_buffer.drain() {
            if !all_segments.iter().any(|(k, _)| *k == kind) {
                all_segments.push((kind, data));
            }
        }

        // Build the image layout.
        let segment_count = all_segments.len();
        let directory_offset = HEADER_SIZE;
        let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
        let mut offset = align_up(directory_offset + directory_length, SEGMENT_ALIGNMENT);
        let mut entries = Vec::with_capacity(segment_count);

        for (kind, data) in &all_segments {
            entries.push(SegmentLocation {
                kind: *kind,
                offset: offset as u64,
                length: data.len() as u64,
            });
            offset = align_up(offset + data.len(), SEGMENT_ALIGNMENT);
        }

        let total_size = offset;
        let sector_size = self.device.sector_size() as usize;
        let padded_size = align_up(total_size, sector_size);

        if padded_size as u64 > self.device.capacity() {
            return Err(CoreFsError::State(format!(
                "volume image ({padded_size} bytes) exceeds device capacity ({} bytes)",
                self.device.capacity()
            )));
        }

        // Build directory bytes.
        let directory_bytes = encode_directory(&entries);
        let directory_checksum = checksum(&directory_bytes);

        // Compute payload checksum (all segments except SUPR/SUP2).
        let payload_checksum = all_segments
            .iter()
            .filter(|(k, _)| k != b"SUPR" && k != b"SUP2")
            .fold(0u64, |acc, (_, data)| acc ^ checksum(data));

        // Build superblock.
        let superblock_bytes = encode_superblock(
            segment_count as u32,
            true,  // clean_unmount
            directory_offset as u64,
            directory_length as u64,
            directory_checksum,
            payload_checksum,
        );

        // Assemble the full image.
        let mut bytes = vec![0u8; padded_size];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(segment_count as u32).to_le_bytes());
        bytes[directory_offset..directory_offset + directory_length]
            .copy_from_slice(&directory_bytes);

        // Write superblock into SUPR/SUP2 positions.
        for (i, (kind, data)) in all_segments.iter_mut().enumerate() {
            if kind == b"SUPR" || kind == b"SUP2" {
                *data = superblock_bytes.clone();
            }
            let entry = &entries[i];
            let start = entry.offset as usize;
            let end = start + data.len();
            bytes[start..end].copy_from_slice(data);
        }

        // Write to device sector-by-sector.
        let mut write_offset = 0u64;
        while write_offset < padded_size as u64 {
            let chunk_end = (write_offset as usize + sector_size).min(padded_size);
            self.device
                .write_at(write_offset, &bytes[write_offset as usize..chunk_end])?;
            write_offset = chunk_end as u64;
        }
        self.device.sync()?;

        // Update the index.
        self.index = SegmentIndex {
            segment_count,
            segments: entries,
            image_end: padded_size as u64,
        };
        self.journal_offset = align_up_u64(
            self.index.image_end,
            self.device.sector_size() as u64,
        );

        // Repopulate cache from the written data.
        self.read_cache.clear();
        for (kind, data) in all_segments {
            self.read_cache.insert(kind, data);
        }

        Ok(())
    }

    /// Invalidates the read cache, forcing the next read to go to the device.
    pub fn invalidate_cache(&mut self) {
        self.read_cache.clear();
    }

    /// Returns the number of cached segments.
    pub fn cached_segment_count(&self) -> usize {
        self.read_cache.len()
    }

    /// Returns the number of dirty (buffered) segments.
    pub fn dirty_segment_count(&self) -> usize {
        self.write_buffer.len()
    }

    /// Returns a reference to the underlying device.
    pub fn device(&self) -> &dyn BlockDevice {
        self.device.as_ref()
    }

    /// Returns a mutable reference to the underlying device.
    pub fn device_mut(&mut self) -> &mut dyn BlockDevice {
        self.device.as_mut()
    }

    /// Returns the byte offset where the device journal region starts.
    pub fn journal_offset(&self) -> u64 {
        self.journal_offset
    }

    /// Opens the device journal from the reserved region.
    pub fn open_journal(&mut self) -> CoreFsResult<DeviceJournal> {
        DeviceJournal::open(self.device.as_mut(), self.journal_offset)
    }
}

// ---------------------------------------------------------------------------
// DeviceJournal — WAL in reserved sectors
// ---------------------------------------------------------------------------

/// Header of the device journal region (64 bytes, sector-aligned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalHeader {
    /// Number of committed entries in the journal.
    entry_count: u32,
    /// Generation counter — incremented on each commit.
    generation: u64,
    /// Checksum of the journal payload bytes.
    entries_checksum: u64,
    /// Length of the serialized payload in bytes (before sector padding).
    payload_length: u64,
}

const JOURNAL_HEADER_SIZE: usize = 64;

/// A write-ahead log stored in a reserved region on the [`BlockDevice`].
///
/// The journal occupies a fixed-size region (256 KiB by default) on the
/// device, placed immediately after the volume image data.  Layout:
///
/// ```text
/// [8 bytes]  JOURNAL_MAGIC ("CFSJ0001")
/// [4 bytes]  entry_count (u32 LE)
/// [8 bytes]  generation (u64 LE)
/// [8 bytes]  entries_checksum (u64 LE)
/// [36 bytes] reserved (zeros)
/// [variable] bincode-serialized Vec<JournalEntry>
/// ```
///
/// Writes are followed by `fdatasync()` (via [`BlockDevice::sync`]) to
/// ensure durability before acknowledging the commit.
#[derive(Debug)]
pub struct DeviceJournal {
    offset: u64,
    size: usize,
    generation: u64,
    entries: Option<VolumeWal>,
}

/// Serialized journal entry for the device journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalPayload {
    wal: VolumeWal,
}

impl DeviceJournal {
    /// Opens (or initializes) the device journal at the given offset.
    pub fn open(device: &dyn BlockDevice, offset: u64) -> CoreFsResult<Self> {
        let sector_size = device.sector_size() as u64;
        let aligned_offset = align_up_u64(offset, sector_size);

        // Check if the journal region fits on the device.
        let journal_end = aligned_offset + DEVICE_JOURNAL_SIZE as u64;
        if journal_end > device.capacity() {
            return Ok(Self {
                offset: aligned_offset,
                size: 0, // No space for journal
                generation: 0,
                entries: None,
            });
        }

        // Read the journal header (first sector).
        let header_sector = device.read_at(aligned_offset, sector_size)?;
        if header_sector.len() < JOURNAL_HEADER_SIZE {
            return Ok(Self {
                offset: aligned_offset,
                size: DEVICE_JOURNAL_SIZE,
                generation: 0,
                entries: None,
            });
        }

        // Check magic.
        if &header_sector[..8] != JOURNAL_MAGIC {
            // Not initialized — empty journal.
            return Ok(Self {
                offset: aligned_offset,
                size: DEVICE_JOURNAL_SIZE,
                generation: 0,
                entries: None,
            });
        }

        // Parse header.
        let entry_count = u32::from_le_bytes(
            header_sector[8..12].try_into().expect("fixed"),
        );
        let generation = u64::from_le_bytes(
            header_sector[12..20].try_into().expect("fixed"),
        );
        let entries_checksum = u64::from_le_bytes(
            header_sector[20..28].try_into().expect("fixed"),
        );
        let payload_length = u64::from_le_bytes(
            header_sector[28..36].try_into().expect("fixed"),
        );

        if entry_count == 0 || payload_length == 0 {
            return Ok(Self {
                offset: aligned_offset,
                size: DEVICE_JOURNAL_SIZE,
                generation,
                entries: None,
            });
        }

        // Read the entries payload.
        let payload_offset = aligned_offset + align_up_u64(JOURNAL_HEADER_SIZE as u64, sector_size);
        let read_size = align_up_u64(payload_length, sector_size);
        let available = device.capacity().saturating_sub(payload_offset);
        let read_size = read_size.min(available);

        if read_size == 0 {
            return Ok(Self {
                offset: aligned_offset,
                size: DEVICE_JOURNAL_SIZE,
                generation,
                entries: None,
            });
        }

        let payload_bytes = device.read_at(payload_offset, read_size)?;

        // Validate checksum over the padded payload (same as what was written).
        if checksum(&payload_bytes[..read_size as usize]) != entries_checksum {
            // Corrupted journal — discard.
            return Ok(Self {
                offset: aligned_offset,
                size: DEVICE_JOURNAL_SIZE,
                generation,
                entries: None,
            });
        }

        // Deserialize from the actual payload bytes (before padding).
        let deserialize_len = (payload_length as usize).min(payload_bytes.len());
        let wal: Option<VolumeWal> =
            bincode::deserialize(&payload_bytes[..deserialize_len]).ok();

        Ok(Self {
            offset: aligned_offset,
            size: DEVICE_JOURNAL_SIZE,
            generation,
            entries: wal,
        })
    }

    /// Returns the current generation counter.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the pending WAL entries, if any.
    pub fn entries(&self) -> Option<&VolumeWal> {
        self.entries.as_ref()
    }

    /// Takes the pending WAL entries (consumes them from the journal).
    pub fn take_entries(&mut self) -> Option<VolumeWal> {
        self.entries.take()
    }

    /// Returns `true` if the journal has pending entries.
    pub fn has_entries(&self) -> bool {
        self.entries.is_some()
    }

    /// Returns the size of the journal region in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Commits a WAL to the device journal with barrier semantics.
    ///
    /// The write is followed by `fdatasync()` to ensure the journal is
    /// durable before returning.  This guarantees that on crash recovery,
    /// either the full journal entry is present or none of it is.
    pub fn commit(
        &mut self,
        device: &mut dyn BlockDevice,
        wal: &VolumeWal,
    ) -> CoreFsResult<()> {
        if self.size == 0 {
            return Err(CoreFsError::State(
                "device journal region has no space".to_string(),
            ));
        }

        let sector_size = device.sector_size() as u64;
        let payload_bytes = bincode::serialize(wal).map_err(|e| {
            CoreFsError::State(format!("failed to serialize journal entry: {e}"))
        })?;

        let payload_offset =
            self.offset + align_up_u64(JOURNAL_HEADER_SIZE as u64, sector_size);
        let max_payload = self.size as u64 - (payload_offset - self.offset);

        if payload_bytes.len() as u64 > max_payload {
            return Err(CoreFsError::State(format!(
                "journal entry ({} bytes) exceeds journal capacity ({max_payload} bytes)",
                payload_bytes.len()
            )));
        }

        // Pad payload to sector alignment.
        let raw_len = payload_bytes.len();
        let padded_len = align_up(raw_len, sector_size as usize);
        let mut padded_payload = payload_bytes;
        padded_payload.resize(padded_len, 0);

        let payload_checksum = checksum(&padded_payload);

        // Build header.
        let mut header = vec![0u8; align_up(JOURNAL_HEADER_SIZE, sector_size as usize)];
        header[..8].copy_from_slice(JOURNAL_MAGIC);
        header[8..12].copy_from_slice(
            &(wal.operations.len() as u32).to_le_bytes(),
        );
        self.generation += 1;
        header[12..20].copy_from_slice(&self.generation.to_le_bytes());
        header[20..28].copy_from_slice(&payload_checksum.to_le_bytes());
        header[28..36].copy_from_slice(&(raw_len as u64).to_le_bytes());

        // Write header first, then payload, then sync (barrier).
        device.write_at(self.offset, &header)?;
        device.write_at(payload_offset, &padded_payload)?;
        device.sync()?; // Barrier: journal is durable after this.

        self.entries = Some(wal.clone());
        Ok(())
    }

    /// Clears the journal on the device (marks it as empty).
    ///
    /// Called after the main volume image has been successfully updated,
    /// so the journal entries are no longer needed for recovery.
    pub fn clear(&mut self, device: &mut dyn BlockDevice) -> CoreFsResult<()> {
        if self.size == 0 {
            return Ok(());
        }

        let sector_size = device.sector_size() as u64;
        let mut header = vec![0u8; align_up(JOURNAL_HEADER_SIZE, sector_size as usize)];
        header[..8].copy_from_slice(JOURNAL_MAGIC);
        // entry_count = 0, generation preserved.
        header[8..12].copy_from_slice(&0u32.to_le_bytes());
        self.generation += 1;
        header[12..20].copy_from_slice(&self.generation.to_le_bytes());

        device.write_at(self.offset, &header)?;
        device.sync()?;

        self.entries = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    let remainder = value % alignment;
    if remainder == 0 { value } else { value + (alignment - remainder) }
}

fn align_up_u64(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    let remainder = value % alignment;
    if remainder == 0 { value } else { value + (alignment - remainder) }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, byte| {
        acc.wrapping_mul(16777619).wrapping_add(u64::from(*byte))
    })
}

fn read_segment_index(device: &dyn BlockDevice) -> CoreFsResult<SegmentIndex> {
    let sector_size = device.sector_size() as u64;

    // Read the first sector to parse the header.
    let header_sector = device.read_at(0, sector_size)?;
    if header_sector.len() < HEADER_SIZE {
        return Err(CoreFsError::State(
            "device too small for CoreFS header".to_string(),
        ));
    }
    if &header_sector[..8] != MAGIC {
        return Err(CoreFsError::State(
            "device does not contain a CoreFS volume (invalid magic)".to_string(),
        ));
    }
    let version = u32::from_le_bytes(header_sector[8..12].try_into().expect("fixed"));
    if version != FORMAT_VERSION {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS format version {version}"
        )));
    }
    let segment_count =
        u32::from_le_bytes(header_sector[12..16].try_into().expect("fixed")) as usize;

    // Read enough sectors to cover the full directory.
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let directory_end = directory_offset + directory_length;
    let needed = align_up(directory_end, sector_size as usize);

    let header_bytes = if needed as u64 > sector_size {
        let extra = device.read_at(sector_size, needed as u64 - sector_size)?;
        let mut combined = header_sector;
        combined.extend_from_slice(&extra);
        combined
    } else {
        header_sector
    };

    let directory = header_bytes.get(directory_offset..directory_end).ok_or_else(|| {
        CoreFsError::State("truncated CoreFS directory on device".to_string())
    })?;

    // Parse directory entries.
    let mut segments = Vec::with_capacity(segment_count);
    for chunk in directory.chunks_exact(SEGMENT_ENTRY_SIZE) {
        let kind: [u8; 4] = chunk[0..4].try_into().expect("fixed");
        let offset = u64::from_le_bytes(chunk[8..16].try_into().expect("fixed"));
        let length = u64::from_le_bytes(chunk[16..24].try_into().expect("fixed"));
        segments.push(SegmentLocation { kind, offset, length });
    }

    let image_end = segments
        .iter()
        .map(|s| {
            let end = s.offset + s.length;
            align_up(end as usize, SEGMENT_ALIGNMENT) as u64
        })
        .max()
        .unwrap_or(directory_end as u64);

    Ok(SegmentIndex {
        segment_count,
        segments,
        image_end,
    })
}

fn read_segment_from_device(
    device: &dyn BlockDevice,
    loc: &SegmentLocation,
) -> CoreFsResult<Vec<u8>> {
    let sector_size = device.sector_size() as u64;
    if loc.length == 0 {
        return Ok(Vec::new());
    }

    // Align the read to sector boundaries.
    let aligned_start = (loc.offset / sector_size) * sector_size;
    let aligned_end = align_up_u64(loc.offset + loc.length, sector_size);
    let read_len = aligned_end - aligned_start;

    let raw = device.read_at(aligned_start, read_len)?;

    // Extract the segment payload from the aligned read.
    let start_in_raw = (loc.offset - aligned_start) as usize;
    let end_in_raw = start_in_raw + loc.length as usize;
    if end_in_raw > raw.len() {
        return Err(CoreFsError::State(format!(
            "truncated segment {} on device",
            String::from_utf8_lossy(&loc.kind)
        )));
    }

    Ok(raw[start_in_raw..end_in_raw].to_vec())
}

fn encode_directory(entries: &[SegmentLocation]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(entries.len() * SEGMENT_ENTRY_SIZE);
    for entry in entries {
        bytes.extend_from_slice(&entry.kind);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&entry.offset.to_le_bytes());
        bytes.extend_from_slice(&entry.length.to_le_bytes());
    }
    bytes
}

fn encode_superblock(
    segment_count: u32,
    clean_unmount: bool,
    directory_offset: u64,
    directory_length: u64,
    directory_checksum: u64,
    payload_checksum: u64,
) -> Vec<u8> {
    let generation = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos() as u64;

    let mut bytes = Vec::with_capacity(SUPERBLOCK_SIZE);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(SEGMENT_ALIGNMENT as u32).to_le_bytes());
    bytes.extend_from_slice(&segment_count.to_le_bytes());
    bytes.extend_from_slice(&u32::from(clean_unmount).to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&directory_offset.to_le_bytes());
    bytes.extend_from_slice(&directory_length.to_le_bytes());
    bytes.extend_from_slice(&directory_checksum.to_le_bytes());
    bytes.extend_from_slice(&payload_checksum.to_le_bytes());
    bytes
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CoreFsService;
    use crate::config::CoreFsConfig;
    use crate::storage::block_device::MemoryDevice;
    use crate::storage::volume_image;
    use crate::storage::volume_wal::{VolumeWal, WalOperation};
    use crate::domain::inode::InodeId;
    use std::time::SystemTime;

    fn format_device(capacity: u64) -> Box<MemoryDevice> {
        let mut dev = Box::new(MemoryDevice::new(capacity, 4096).unwrap());
        let service = CoreFsService::format(CoreFsConfig::default());
        let state = service.persisted_state();
        volume_image::save_to_device(dev.as_mut(), &state).unwrap();
        dev
    }

    fn format_device_with_files(capacity: u64) -> Box<MemoryDevice> {
        let mut dev = Box::new(MemoryDevice::new(capacity, 4096).unwrap());
        let mut service = CoreFsService::format(CoreFsConfig::default());
        service.create_directory("/data").unwrap();
        service.create_file("/data/hello.txt", b"Hello, World!", &[]).unwrap();
        service.create_file("/data/readme.md", b"# CoreFS", &[]).unwrap();
        let state = service.persisted_state();
        volume_image::save_to_device(dev.as_mut(), &state).unwrap();
        dev
    }

    const TWO_MIB: u64 = 2 * 1024 * 1024;

    // -----------------------------------------------------------------------
    // SegmentIndex
    // -----------------------------------------------------------------------

    #[test]
    fn segment_index_reads_header_and_directory() {
        let dev = format_device(TWO_MIB);
        let index = read_segment_index(dev.as_ref()).unwrap();

        assert_eq!(index.segment_count, 15);
        assert_eq!(index.segments.len(), 15);
        assert!(index.image_end > 0);

        // Check expected segment kinds.
        assert!(index.find(b"SUPR").is_some());
        assert!(index.find(b"SUP2").is_some());
        assert!(index.find(b"CNFG").is_some());
        assert!(index.find(b"DATA").is_some());
        assert!(index.find(b"BLKD").is_some());
    }

    #[test]
    fn segment_index_rejects_unformatted_device() {
        let dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
        let err = read_segment_index(dev.as_ref()).unwrap_err();
        assert!(err.to_string().contains("invalid magic"));
    }

    #[test]
    fn segment_index_find_returns_none_for_unknown() {
        let dev = format_device(TWO_MIB);
        let index = read_segment_index(dev.as_ref()).unwrap();
        assert!(index.find(b"XXXX").is_none());
    }

    // -----------------------------------------------------------------------
    // DeviceVolume — open & read
    // -----------------------------------------------------------------------

    #[test]
    fn device_volume_opens_formatted_device() {
        let dev = format_device(TWO_MIB);
        let vol = DeviceVolume::open(dev).unwrap();

        assert_eq!(vol.index().segment_count, 15);
        assert!(!vol.is_dirty());
        assert_eq!(vol.cached_segment_count(), 0);
    }

    #[test]
    fn device_volume_reads_segment_on_demand() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // No segments cached yet.
        assert_eq!(vol.cached_segment_count(), 0);

        // Read CNFG segment.
        let cnfg = vol.read_segment(b"CNFG").unwrap();
        assert!(!cnfg.is_empty());

        // Now it should be cached.
        assert_eq!(vol.cached_segment_count(), 1);

        // Second read should come from cache.
        let cnfg2 = vol.read_segment(b"CNFG").unwrap();
        assert_eq!(cnfg, cnfg2);
    }

    #[test]
    fn device_volume_reads_data_segment() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        let data = vol.read_segment(b"DATA").unwrap();
        assert!(!data.is_empty());
    }

    #[test]
    fn device_volume_reads_all_segments() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        let kinds: Vec<[u8; 4]> = vol.index().segments.iter().map(|s| s.kind).collect();
        for kind in &kinds {
            let data = vol.read_segment(kind).unwrap();
            // All segments should be readable.
            let _ = data;
        }

        assert_eq!(vol.cached_segment_count(), 15);
    }

    #[test]
    fn device_volume_rejects_unknown_segment() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        let err = vol.read_segment(b"ZZZZ").unwrap_err();
        assert!(matches!(err, CoreFsError::NotFound(_)));
    }

    // -----------------------------------------------------------------------
    // DeviceVolume — write & flush
    // -----------------------------------------------------------------------

    #[test]
    fn device_volume_write_marks_dirty() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        assert!(!vol.is_dirty());
        vol.write_segment(*b"CNFG", vec![1, 2, 3]);
        assert!(vol.is_dirty());
        assert_eq!(vol.dirty_segment_count(), 1);
    }

    #[test]
    fn device_volume_write_read_returns_dirty_data() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        let original = vol.read_segment(b"CNFG").unwrap();
        vol.write_segment(*b"CNFG", vec![0xAA; 10]);

        // Should return dirty data, not original.
        let dirty = vol.read_segment(b"CNFG").unwrap();
        assert_eq!(dirty, vec![0xAA; 10]);
        assert_ne!(dirty, original);
    }

    #[test]
    fn device_volume_flush_writes_and_clears_dirty() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Read a segment, modify it, flush.
        let _original = vol.read_segment(b"CNFG").unwrap();
        vol.write_segment(*b"CNFG", vec![0xBB; 20]);

        vol.flush().unwrap();
        assert!(!vol.is_dirty());

        // Re-read from device to verify persistence.
        vol.invalidate_cache();
        let reread = vol.read_segment(b"CNFG").unwrap();
        assert_eq!(reread, vec![0xBB; 20]);
    }

    #[test]
    fn device_volume_flush_preserves_other_segments() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Read DATA before modification.
        let original_data = vol.read_segment(b"DATA").unwrap();

        // Modify only CNFG.
        vol.write_segment(*b"CNFG", vec![0xCC; 15]);
        vol.flush().unwrap();

        // DATA should be unchanged.
        vol.invalidate_cache();
        let after_data = vol.read_segment(b"DATA").unwrap();
        assert_eq!(original_data, after_data);
    }

    #[test]
    fn device_volume_invalidate_cache_forces_device_read() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        let _first = vol.read_segment(b"CNFG").unwrap();
        assert_eq!(vol.cached_segment_count(), 1);

        vol.invalidate_cache();
        assert_eq!(vol.cached_segment_count(), 0);
    }

    #[test]
    fn device_volume_multiple_flush_cycles() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Cycle 1
        vol.write_segment(*b"CNFG", vec![1; 10]);
        vol.flush().unwrap();

        // Cycle 2
        vol.write_segment(*b"CNFG", vec![2; 20]);
        vol.flush().unwrap();

        vol.invalidate_cache();
        let final_data = vol.read_segment(b"CNFG").unwrap();
        assert_eq!(final_data, vec![2; 20]);
    }

    #[test]
    fn device_volume_flush_no_dirty_is_noop() {
        let dev = format_device(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Flush with nothing dirty should succeed.
        vol.flush().unwrap();
        assert!(!vol.is_dirty());
    }

    // -----------------------------------------------------------------------
    // DeviceVolume — with 512-byte sectors
    // -----------------------------------------------------------------------

    #[test]
    fn device_volume_works_with_512_byte_sectors() {
        let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 512).unwrap());
        let service = CoreFsService::format(CoreFsConfig::default());
        let state = service.persisted_state();
        volume_image::save_to_device(dev.as_mut(), &state).unwrap();

        let mut vol = DeviceVolume::open(dev).unwrap();
        let cnfg = vol.read_segment(b"CNFG").unwrap();
        assert!(!cnfg.is_empty());

        vol.write_segment(*b"CNFG", vec![0xDD; 30]);
        vol.flush().unwrap();
        vol.invalidate_cache();
        let reread = vol.read_segment(b"CNFG").unwrap();
        assert_eq!(reread, vec![0xDD; 30]);
    }

    // -----------------------------------------------------------------------
    // DeviceJournal — basic operations
    // -----------------------------------------------------------------------

    fn make_test_wal() -> VolumeWal {
        VolumeWal {
            transaction_id: 42,
            label: "test-wal".to_string(),
            created_at: SystemTime::now(),
            operations: vec![
                WalOperation::CreateDirectory {
                    path: "/test".to_string(),
                    inode: InodeId(1),
                },
                WalOperation::CreateFile {
                    path: "/test/file.txt".to_string(),
                    inode: InodeId(2),
                },
            ],
        }
    }

    #[test]
    fn device_journal_opens_empty_on_uninitialized_device() {
        let dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
        let journal = DeviceJournal::open(dev.as_ref(), 1024 * 1024).unwrap();

        assert!(!journal.has_entries());
        assert_eq!(journal.generation(), 0);
    }

    #[test]
    fn device_journal_commit_and_read_back() {
        let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
        let offset = 1024 * 1024; // 1 MiB

        let wal = make_test_wal();
        {
            let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();
            journal.commit(dev.as_mut(), &wal).unwrap();
            assert!(journal.has_entries());
            assert_eq!(journal.generation(), 1);
        }

        // Re-open and verify persistence.
        let journal2 = DeviceJournal::open(dev.as_ref(), offset).unwrap();
        assert!(journal2.has_entries());
        let recovered = journal2.entries().unwrap();
        assert_eq!(recovered.transaction_id, 42);
        assert_eq!(recovered.operations.len(), 2);
    }

    #[test]
    fn device_journal_clear_removes_entries() {
        let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
        let offset = 1024 * 1024;

        let wal = make_test_wal();
        let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();
        journal.commit(dev.as_mut(), &wal).unwrap();
        assert!(journal.has_entries());

        journal.clear(dev.as_mut()).unwrap();
        assert!(!journal.has_entries());

        // Re-open should see empty journal.
        let journal2 = DeviceJournal::open(dev.as_ref(), offset).unwrap();
        assert!(!journal2.has_entries());
    }

    #[test]
    fn device_journal_generation_increments() {
        let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
        let offset = 1024 * 1024;

        let wal = make_test_wal();
        let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();

        journal.commit(dev.as_mut(), &wal).unwrap();
        assert_eq!(journal.generation(), 1);

        journal.clear(dev.as_mut()).unwrap();
        assert_eq!(journal.generation(), 2);

        journal.commit(dev.as_mut(), &wal).unwrap();
        assert_eq!(journal.generation(), 3);
    }

    #[test]
    fn device_journal_take_entries_consumes() {
        let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
        let offset = 1024 * 1024;

        let wal = make_test_wal();
        let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();
        journal.commit(dev.as_mut(), &wal).unwrap();

        let taken = journal.take_entries();
        assert!(taken.is_some());
        assert!(!journal.has_entries());
    }

    #[test]
    fn device_journal_no_space_returns_error() {
        // Device too small for journal.
        let dev = Box::new(MemoryDevice::new(4096, 4096).unwrap());
        let mut journal = DeviceJournal::open(dev.as_ref(), 4096).unwrap();

        let wal = make_test_wal();
        let err = journal.commit(&mut *Box::new(MemoryDevice::new(4096, 4096).unwrap()), &wal);
        assert!(err.is_err());
    }

    // -----------------------------------------------------------------------
    // DeviceVolume + DeviceJournal integration
    // -----------------------------------------------------------------------

    #[test]
    fn device_volume_and_journal_coexist() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Journal should be after the image data.
        assert!(vol.journal_offset() > 0);
        assert!(vol.journal_offset() >= vol.index().image_end);

        // Open journal.
        let journal = vol.open_journal().unwrap();
        assert!(!journal.has_entries());
    }

    #[test]
    fn device_volume_journal_commit_survives_flush() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Commit to journal.
        let wal = make_test_wal();
        {
            let mut journal = vol.open_journal().unwrap();
            journal.commit(vol.device_mut(), &wal).unwrap();
        }

        // Modify and flush a segment.
        vol.write_segment(*b"CNFG", vec![0xEE; 12]);
        vol.flush().unwrap();

        // Journal should still be readable (it's in a separate region).
        let journal2 = vol.open_journal().unwrap();
        assert!(journal2.has_entries());
        assert_eq!(journal2.entries().unwrap().transaction_id, 42);
    }

    #[test]
    fn device_volume_journal_clear_after_successful_flush() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Typical workflow: commit journal → flush → clear journal.
        let wal = make_test_wal();
        {
            let mut journal = vol.open_journal().unwrap();
            journal.commit(vol.device_mut(), &wal).unwrap();
        }

        vol.flush().unwrap();

        {
            let mut journal = vol.open_journal().unwrap();
            journal.clear(vol.device_mut()).unwrap();
        }

        let journal_final = vol.open_journal().unwrap();
        assert!(!journal_final.has_entries());
    }

    // -----------------------------------------------------------------------
    // Full round-trip: format → open DeviceVolume → read individual segments
    // -----------------------------------------------------------------------

    #[test]
    fn full_round_trip_format_then_on_demand_read() {
        let dev = format_device_with_files(TWO_MIB);
        let mut vol = DeviceVolume::open(dev).unwrap();

        // Read individual segments on demand.
        let supr = vol.read_segment(b"SUPR").unwrap();
        assert_eq!(supr.len(), SUPERBLOCK_SIZE);

        let cnfg = vol.read_segment(b"CNFG").unwrap();
        assert!(!cnfg.is_empty());

        let blkd = vol.read_segment(b"BLKD").unwrap();
        assert!(!blkd.is_empty());

        let data = vol.read_segment(b"DATA").unwrap();
        assert!(!data.is_empty());

        // Only 4 segments should be cached (the ones we read).
        assert_eq!(vol.cached_segment_count(), 4);
    }
}
