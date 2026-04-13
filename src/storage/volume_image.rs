use crate::app::PersistedState;
use crate::config::{CoreFsConfig, StorageTier};
use crate::domain::acl::{AclEntry, Principal};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::{ContentClass, FileMetadata};
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::services::hot_paths::HotPathRecord;
use crate::services::journal::JournalEntry;
use crate::services::journal::JournalRuntimeState;
use crate::services::journal::{JournalRepairSummary, reconcile_persisted_state};
use crate::services::sync::SyncStatus;
use crate::services::versioning::FileVersion;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord, FreeExtentRecord};
use crate::storage::volume_wal::VolumeWal;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 8] = b"COREFS01";
const FORMAT_VERSION: u32 = 5;
const SEGMENT_ALIGNMENT: usize = 64;
const SEGMENT_ENTRY_SIZE: usize = 24;
const HEADER_SIZE: usize = 16;
const SUPERBLOCK_SIZE: usize = 56;
const SEGMENT_FRAME_SIZE: usize = 24;
const EXPECTED_SEGMENT_KINDS: [[u8; 4]; 15] = [
    *b"SUPR", *b"SUP2", *b"CNFG", *b"VOLM", *b"AINO", *b"DINO", *b"JOUR", *b"VERS", *b"SYNC",
    *b"HOTP", *b"SNAP", *b"TXNJ", *b"FREE", *b"BLKD", *b"DATA",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BlockDescriptor {
    inode: InodeId,
    checksum: u64,
    device_block: u64,
    allocated_blocks: u64,
    offset: u64,
    length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ConfigSegment {
    config: CoreFsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VolumeSegment {
    volume: VolumeDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VersionSegment {
    versions: Vec<FileVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SyncSegment {
    sync_statuses: Vec<SyncStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HotPathSegment {
    records: Vec<HotPathRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SnapshotSegment {
    snapshots: Vec<Snapshot>,
    next_snapshot_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalRuntimeSegment {
    clean_unmount: bool,
    runtime: JournalRuntimeState,
    pending_wal: Option<VolumeWal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BlockDescriptorSegment {
    descriptors: Vec<BlockDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FreeSpaceSegment {
    policy: AllocatorPolicy,
    extents: Vec<FreeExtentRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentEntry {
    kind: [u8; 4],
    offset: u64,
    length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Superblock {
    format_version: u32,
    alignment: u32,
    segment_count: u32,
    clean_unmount: u32,
    generation: u64,
    directory_offset: u64,
    directory_length: u64,
    directory_checksum: u64,
    payload_checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeImageInspectionReport {
    pub format_version: u32,
    pub segment_count: usize,
    pub valid_superblocks: usize,
    pub selected_generation: u64,
    pub directory_checksum_valid: bool,
    pub payload_checksum_valid: bool,
    pub segment_kinds: Vec<String>,
    pub block_descriptors: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeImageRepairReport {
    pub repaired_superblocks: usize,
    pub selected_generation: u64,
    pub resulting_valid_superblocks: usize,
    pub recovered_without_valid_superblock: bool,
    pub reconstructed_segment_directory: bool,
    pub reconstructed_block_descriptors: bool,
    pub journal_repair: JournalRepairSummary,
}

#[derive(Debug, Clone)]
struct InspectedImage {
    entries: Vec<SegmentEntry>,
    superblock: Superblock,
    report: VolumeImageInspectionReport,
}

#[derive(Debug, Clone)]
struct RecoveredImageState {
    state: PersistedState,
    reconstructed_segment_directory: bool,
    reconstructed_block_descriptors: bool,
}

pub fn save_volume_image(path: impl AsRef<Path>, state: &PersistedState) -> CoreFsResult<()> {
    let path = path.as_ref();
    let (descriptors, block_data) = split_blocks(&state.block_records, state.volume.block_size);

    let mut segments = vec![
        raw_segment_from_bytes(*b"SUPR", vec![0; SUPERBLOCK_SIZE]),
        raw_segment_from_bytes(*b"SUP2", vec![0; SUPERBLOCK_SIZE]),
        serialize_segment(
            *b"CNFG",
            &ConfigSegment {
                config: state.config.clone(),
            },
            path,
        )?,
        serialize_segment(
            *b"VOLM",
            &VolumeSegment {
                volume: state.volume.clone(),
            },
            path,
        )?,
        serialize_inode_segment(*b"AINO", &state.active_inodes, path)?,
        serialize_inode_segment(*b"DINO", &state.deleted_inodes, path)?,
        serialize_journal_segment(*b"JOUR", &state.journal_entries, path)?,
        serialize_segment(
            *b"VERS",
            &VersionSegment {
                versions: state.versions.clone(),
            },
            path,
        )?,
        serialize_segment(
            *b"SYNC",
            &SyncSegment {
                sync_statuses: state.sync_statuses.clone(),
            },
            path,
        )?,
        serialize_segment(
            *b"HOTP",
            &HotPathSegment {
                records: state.hot_path_records.clone(),
            },
            path,
        )?,
        serialize_snapshot_segment(
            *b"SNAP",
            &SnapshotSegment {
                snapshots: state.snapshots.clone(),
                next_snapshot_id: state.next_snapshot_id,
            },
            path,
        )?,
        serialize_segment(
            *b"TXNJ",
            &JournalRuntimeSegment {
                clean_unmount: state.clean_unmount,
                runtime: state.journal_runtime.clone(),
                pending_wal: state.pending_wal.clone(),
            },
            path,
        )?,
        serialize_segment(
            *b"FREE",
            &FreeSpaceSegment {
                policy: state.allocator_policy.clone(),
                extents: state.free_extents.clone(),
            },
            path,
        )?,
        serialize_segment(*b"BLKD", &BlockDescriptorSegment { descriptors }, path)?,
        serialize_bytes_segment(*b"DATA", &block_data, path)?,
    ];

    let segment_count = segments.len();
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let mut offset = align_up(directory_offset + directory_length, SEGMENT_ALIGNMENT);
    let mut entries = Vec::with_capacity(segment_count);

    for segment in &segments {
        entries.push(SegmentEntry {
            kind: segment.kind,
            offset: offset as u64,
            length: segment.payload.len() as u64,
        });
        offset = align_up(offset + segment.payload.len(), SEGMENT_ALIGNMENT);
    }

    let total_size = offset;
    let directory_bytes = directory_bytes(&entries);
    let superblock = Superblock {
        format_version: FORMAT_VERSION,
        alignment: SEGMENT_ALIGNMENT as u32,
        segment_count: segment_count as u32,
        clean_unmount: u32::from(state.clean_unmount),
        generation: current_generation(),
        directory_offset: directory_offset as u64,
        directory_length: directory_length as u64,
        directory_checksum: checksum(&directory_bytes),
        payload_checksum: checksum_of_payloads(&segments),
    };
    let superblock_bytes = encode_superblock(&superblock);
    segments[0].payload = superblock_bytes.clone();
    segments[1].payload = superblock_bytes;

    let mut bytes = vec![0u8; total_size];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&(segment_count as u32).to_le_bytes());
    bytes[directory_offset..directory_offset + directory_length].copy_from_slice(&directory_bytes);

    for (entry, segment) in entries.iter().zip(segments.iter()) {
        let start = entry.offset as usize;
        let end = start + segment.payload.len();
        bytes[start..end].copy_from_slice(&segment.payload);
    }

    fs::write(path, bytes).map_err(|error| {
        CoreFsError::State(format!(
            "failed to write CoreFS volume image to {}: {error}",
            path.display()
        ))
    })?;

    Ok(())
}

pub fn load_volume_image(path: impl AsRef<Path>) -> CoreFsResult<PersistedState> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|error| {
        CoreFsError::State(format!(
            "failed to read CoreFS volume image from {}: {error}",
            path.display()
        ))
    })?;

    let inspected = inspect_volume_image_bytes(&bytes, path)?;
    let segment_count = inspected.report.segment_count;
    let entries = inspected.entries;
    let superblock = inspected.superblock;

    if superblock.alignment as usize != SEGMENT_ALIGNMENT {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS volume image alignment {} in {}",
            superblock.alignment,
            path.display()
        )));
    }

    if superblock.segment_count as usize != segment_count {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS volume image segment count in {}",
            path.display()
        )));
    }

    persisted_state_from_entries(&bytes, &entries, path)
}

pub fn inspect_volume_image(path: impl AsRef<Path>) -> CoreFsResult<VolumeImageInspectionReport> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|error| {
        CoreFsError::State(format!(
            "failed to read CoreFS volume image from {}: {error}",
            path.display()
        ))
    })?;
    Ok(inspect_volume_image_bytes(&bytes, path)?.report)
}

pub fn repair_volume_image_superblocks(
    path: impl AsRef<Path>,
) -> CoreFsResult<VolumeImageRepairReport> {
    let path = path.as_ref();
    let before = inspect_volume_image(path).ok();
    let repaired_superblocks = before
        .as_ref()
        .map(|report| 2usize.saturating_sub(report.valid_superblocks))
        .unwrap_or(2);
    let recovered_without_valid_superblock = before.is_none();
    let recovered = match load_volume_image(path) {
        Ok(state) => RecoveredImageState {
            state,
            reconstructed_segment_directory: false,
            reconstructed_block_descriptors: false,
        },
        Err(_) => load_volume_image_relaxed(path)?,
    };
    let mut state = recovered.state;
    let journal_repair = reconcile_persisted_state(&mut state);
    let needs_rewrite = repaired_superblocks > 0
        || recovered_without_valid_superblock
        || recovered.reconstructed_segment_directory
        || recovered.reconstructed_block_descriptors
        || journal_repair != JournalRepairSummary::default();

    if needs_rewrite {
        save_volume_image(path, &state)?;
    }

    let after = inspect_volume_image(path)?;
    Ok(VolumeImageRepairReport {
        repaired_superblocks,
        selected_generation: after.selected_generation,
        resulting_valid_superblocks: after.valid_superblocks,
        recovered_without_valid_superblock,
        reconstructed_segment_directory: recovered.reconstructed_segment_directory,
        reconstructed_block_descriptors: recovered.reconstructed_block_descriptors,
        journal_repair,
    })
}

struct SegmentPayload {
    kind: [u8; 4],
    payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentFrameHeader {
    kind: [u8; 4],
    payload_length: u64,
    payload_checksum: u64,
}

fn raw_segment_from_bytes(kind: [u8; 4], payload: Vec<u8>) -> SegmentPayload {
    SegmentPayload { kind, payload }
}

fn serialize_segment<T: Serialize>(
    kind: [u8; 4],
    value: &T,
    path: &Path,
) -> CoreFsResult<SegmentPayload> {
    let payload = bincode::serialize(value).map_err(|error| {
        CoreFsError::State(format!(
            "failed to serialize CoreFS volume image segment {} for {}: {error}",
            String::from_utf8_lossy(&kind),
            path.display()
        ))
    })?;
    Ok(SegmentPayload {
        kind,
        payload: encode_segment_frame(kind, &payload),
    })
}

fn serialize_inode_segment(
    kind: [u8; 4],
    inodes: &[Inode],
    path: &Path,
) -> CoreFsResult<SegmentPayload> {
    let payload = encode_inodes_payload(inodes).map_err(|error| {
        CoreFsError::State(format!(
            "failed to serialize CoreFS volume image segment {} for {}: {error}",
            String::from_utf8_lossy(&kind),
            path.display()
        ))
    })?;
    Ok(SegmentPayload {
        kind,
        payload: encode_segment_frame(kind, &payload),
    })
}

fn serialize_journal_segment(
    kind: [u8; 4],
    entries: &[JournalEntry],
    path: &Path,
) -> CoreFsResult<SegmentPayload> {
    let payload = encode_journal_payload(entries).map_err(|error| {
        CoreFsError::State(format!(
            "failed to serialize CoreFS volume image segment {} for {}: {error}",
            String::from_utf8_lossy(&kind),
            path.display()
        ))
    })?;
    Ok(SegmentPayload {
        kind,
        payload: encode_segment_frame(kind, &payload),
    })
}

fn serialize_snapshot_segment(
    kind: [u8; 4],
    snapshot_segment: &SnapshotSegment,
    path: &Path,
) -> CoreFsResult<SegmentPayload> {
    let payload = encode_snapshot_payload(snapshot_segment).map_err(|error| {
        CoreFsError::State(format!(
            "failed to serialize CoreFS volume image segment {} for {}: {error}",
            String::from_utf8_lossy(&kind),
            path.display()
        ))
    })?;
    Ok(SegmentPayload {
        kind,
        payload: encode_segment_frame(kind, &payload),
    })
}

fn serialize_bytes_segment(
    kind: [u8; 4],
    payload: &[u8],
    _path: &Path,
) -> CoreFsResult<SegmentPayload> {
    Ok(SegmentPayload {
        kind,
        payload: encode_segment_frame(kind, payload),
    })
}

fn deserialize_segment<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    entry: &SegmentEntry,
    path: &Path,
) -> CoreFsResult<T> {
    let payload = segment_bytes(bytes, entry, path)?;
    bincode::deserialize(payload).map_err(|error| {
        CoreFsError::State(format!(
            "failed to deserialize CoreFS volume image segment {} from {}: {error}",
            String::from_utf8_lossy(&entry.kind),
            path.display()
        ))
    })
}

fn deserialize_optional_segment<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    entries: &[SegmentEntry],
    kind: &[u8; 4],
    path: &Path,
) -> CoreFsResult<Option<T>> {
    match find_segment(entries, kind) {
        Ok(entry) => deserialize_segment(bytes, entry, path).map(Some),
        Err(_) => Ok(None),
    }
}

fn deserialize_inode_segment(
    bytes: &[u8],
    entry: &SegmentEntry,
    path: &Path,
) -> CoreFsResult<Vec<Inode>> {
    let payload = segment_bytes(bytes, entry, path)?;
    decode_inodes_payload(payload).map_err(|error| {
        CoreFsError::State(format!(
            "failed to deserialize CoreFS volume image segment {} from {}: {error}",
            String::from_utf8_lossy(&entry.kind),
            path.display()
        ))
    })
}

fn deserialize_journal_segment(
    bytes: &[u8],
    entry: &SegmentEntry,
    path: &Path,
) -> CoreFsResult<Vec<JournalEntry>> {
    let payload = segment_bytes(bytes, entry, path)?;
    decode_journal_payload(payload).map_err(|error| {
        CoreFsError::State(format!(
            "failed to deserialize CoreFS volume image segment {} from {}: {error}",
            String::from_utf8_lossy(&entry.kind),
            path.display()
        ))
    })
}

fn deserialize_snapshot_segment(
    bytes: &[u8],
    entry: &SegmentEntry,
    path: &Path,
) -> CoreFsResult<SnapshotSegment> {
    let payload = segment_bytes(bytes, entry, path)?;
    decode_snapshot_payload(payload).map_err(|error| {
        CoreFsError::State(format!(
            "failed to deserialize CoreFS volume image segment {} from {}: {error}",
            String::from_utf8_lossy(&entry.kind),
            path.display()
        ))
    })
}

fn segment_bytes<'a>(bytes: &'a [u8], entry: &SegmentEntry, path: &Path) -> CoreFsResult<&'a [u8]> {
    let start = entry.offset as usize;
    let end = start.checked_add(entry.length as usize).ok_or_else(|| {
        CoreFsError::State(format!(
            "invalid CoreFS volume image segment range {} in {}",
            String::from_utf8_lossy(&entry.kind),
            path.display()
        ))
    })?;
    bytes
        .get(start..end)
        .ok_or_else(|| {
            CoreFsError::State(format!(
                "truncated CoreFS volume image segment {} in {}",
                String::from_utf8_lossy(&entry.kind),
                path.display()
            ))
        })
        .and_then(|segment| {
            if entry.kind == *b"SUPR" || entry.kind == *b"SUP2" {
                Ok(segment)
            } else {
                decode_segment_frame(segment, &entry.kind, path)
            }
        })
}

fn find_segment<'a>(entries: &'a [SegmentEntry], kind: &[u8; 4]) -> CoreFsResult<&'a SegmentEntry> {
    entries
        .iter()
        .find(|entry| &entry.kind == kind)
        .ok_or_else(|| {
            CoreFsError::State(format!(
                "missing CoreFS volume image segment {}",
                String::from_utf8_lossy(kind)
            ))
        })
}

fn split_blocks(
    block_records: &[BlockRecord],
    block_size: usize,
) -> (Vec<BlockDescriptor>, Vec<u8>) {
    let mut descriptors = Vec::with_capacity(block_records.len());
    let block_size = block_size.max(1) as u64;
    let total_size = block_records
        .iter()
        .map(|record| {
            record
                .device_block
                .saturating_add(record.allocated_blocks.max(1))
                .saturating_mul(block_size)
        })
        .max()
        .unwrap_or(0) as usize;
    let mut data = vec![0u8; total_size];

    for record in block_records {
        let offset = record.device_block.saturating_mul(block_size);
        let length = record.bytes.len() as u64;
        let start = offset as usize;
        let end = start.saturating_add(record.bytes.len());
        if end > data.len() {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(&record.bytes);
        descriptors.push(BlockDescriptor {
            inode: record.inode,
            checksum: record.checksum,
            device_block: record.device_block,
            allocated_blocks: record.allocated_blocks.max(1),
            offset,
            length,
        });
    }

    (descriptors, data)
}

fn join_blocks(descriptors: Vec<BlockDescriptor>, data: &[u8]) -> CoreFsResult<Vec<BlockRecord>> {
    let mut block_records = Vec::with_capacity(descriptors.len());

    for descriptor in descriptors {
        let start = descriptor.offset as usize;
        let end = start + descriptor.length as usize;
        let bytes = data.get(start..end).ok_or_else(|| {
            CoreFsError::State("invalid CoreFS block descriptor range in volume image".to_string())
        })?;

        block_records.push(BlockRecord {
            inode: descriptor.inode,
            bytes: bytes.to_vec(),
            checksum: descriptor.checksum,
            device_block: descriptor.device_block,
            allocated_blocks: descriptor.allocated_blocks.max(1),
        });
    }

    Ok(block_records)
}

fn align_up(value: usize, alignment: usize) -> usize {
    if value.is_multiple_of(alignment) {
        value
    } else {
        value + (alignment - (value % alignment))
    }
}

fn directory_bytes(entries: &[SegmentEntry]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(entries.len() * SEGMENT_ENTRY_SIZE);
    for entry in entries {
        bytes.extend_from_slice(&entry.kind);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&entry.offset.to_le_bytes());
        bytes.extend_from_slice(&entry.length.to_le_bytes());
    }
    bytes
}

fn parse_directory(bytes: &[u8]) -> CoreFsResult<Vec<SegmentEntry>> {
    if !bytes.len().is_multiple_of(SEGMENT_ENTRY_SIZE) {
        return Err(CoreFsError::State(
            "invalid CoreFS volume image directory size".to_string(),
        ));
    }
    let mut entries = Vec::with_capacity(bytes.len() / SEGMENT_ENTRY_SIZE);
    for chunk in bytes.chunks_exact(SEGMENT_ENTRY_SIZE) {
        entries.push(SegmentEntry {
            kind: chunk[0..4].try_into().expect("fixed slice"),
            offset: u64::from_le_bytes(chunk[8..16].try_into().expect("fixed slice")),
            length: u64::from_le_bytes(chunk[16..24].try_into().expect("fixed slice")),
        });
    }
    Ok(entries)
}

fn encode_inodes_payload(inodes: &[Inode]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, inodes.len() as u32);
    for inode in inodes {
        push_u64(&mut bytes, inode.id.0);
        push_u8(&mut bytes, encode_inode_kind(inode.kind));
        push_string(&mut bytes, &inode.path)?;
        push_u64(&mut bytes, inode.size as u64);
        push_system_time(&mut bytes, inode.created_at)?;
        push_system_time(&mut bytes, inode.modified_at)?;
        push_metadata(&mut bytes, &inode.metadata)?;
    }
    Ok(bytes)
}

fn decode_inodes_payload(bytes: &[u8]) -> Result<Vec<Inode>, String> {
    let mut cursor = 0usize;
    let count = read_u32(bytes, &mut cursor)? as usize;
    let mut inodes = Vec::with_capacity(count);
    for _ in 0..count {
        let id = InodeId(read_u64(bytes, &mut cursor)?);
        let kind = decode_inode_kind(read_u8(bytes, &mut cursor)?)?;
        let path = read_string(bytes, &mut cursor)?;
        let size = read_u64(bytes, &mut cursor)? as usize;
        let created_at = read_system_time(bytes, &mut cursor)?;
        let modified_at = read_system_time(bytes, &mut cursor)?;
        let metadata = read_metadata(bytes, &mut cursor)?;
        inodes.push(Inode {
            id,
            kind,
            path,
            size,
            created_at,
            modified_at,
            metadata,
        });
    }
    ensure_consumed(bytes, cursor)?;
    Ok(inodes)
}

fn encode_journal_payload(entries: &[JournalEntry]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, entries.len() as u32);
    for entry in entries {
        push_system_time(&mut bytes, entry.timestamp)?;
        push_string(&mut bytes, &entry.operation)?;
        push_string(&mut bytes, &entry.target)?;
        push_string(&mut bytes, &entry.details)?;
    }
    Ok(bytes)
}

fn decode_journal_payload(bytes: &[u8]) -> Result<Vec<JournalEntry>, String> {
    let mut cursor = 0usize;
    let count = read_u32(bytes, &mut cursor)? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(JournalEntry {
            timestamp: read_system_time(bytes, &mut cursor)?,
            operation: read_string(bytes, &mut cursor)?,
            target: read_string(bytes, &mut cursor)?,
            details: read_string(bytes, &mut cursor)?,
        });
    }
    ensure_consumed(bytes, cursor)?;
    Ok(entries)
}

fn encode_snapshot_payload(segment: &SnapshotSegment) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    push_u64(&mut bytes, segment.next_snapshot_id);
    push_u32(&mut bytes, segment.snapshots.len() as u32);
    for snapshot in &segment.snapshots {
        push_u64(&mut bytes, snapshot.id);
        push_string(&mut bytes, &snapshot.name)?;
        push_string(&mut bytes, &snapshot.scope_root)?;
        push_system_time(&mut bytes, snapshot.created_at)?;
        push_u32(&mut bytes, snapshot.paths.len() as u32);
        for path in &snapshot.paths {
            push_string(&mut bytes, path)?;
        }
        // file_data: key-value pairs (path → raw bytes).
        push_u32(&mut bytes, snapshot.file_data.len() as u32);
        for (path, data) in &snapshot.file_data {
            push_string(&mut bytes, path)?;
            push_blob(&mut bytes, data)?;
        }
    }
    Ok(bytes)
}

fn decode_snapshot_payload(bytes: &[u8]) -> Result<SnapshotSegment, String> {
    let mut cursor = 0usize;
    let next_snapshot_id = read_u64(bytes, &mut cursor)?;
    let count = read_u32(bytes, &mut cursor)? as usize;
    let mut snapshots = Vec::with_capacity(count);
    for _ in 0..count {
        let id = read_u64(bytes, &mut cursor)?;
        let name = read_string(bytes, &mut cursor)?;
        let scope_root = read_string(bytes, &mut cursor)?;
        let created_at = read_system_time(bytes, &mut cursor)?;
        let path_count = read_u32(bytes, &mut cursor)? as usize;
        let mut paths = Vec::with_capacity(path_count);
        for _ in 0..path_count {
            paths.push(read_string(bytes, &mut cursor)?);
        }
        // file_data: deserialise key-value pairs (path → raw bytes).
        let file_data_count = read_u32(bytes, &mut cursor)? as usize;
        let mut file_data = std::collections::BTreeMap::new();
        for _ in 0..file_data_count {
            let key = read_string(bytes, &mut cursor)?;
            let value = read_blob(bytes, &mut cursor)?;
            file_data.insert(key, value);
        }
        snapshots.push(Snapshot {
            id,
            name,
            scope_root,
            created_at,
            paths,
            file_data,
        });
    }
    ensure_consumed(bytes, cursor)?;
    Ok(SnapshotSegment {
        snapshots,
        next_snapshot_id,
    })
}

fn push_metadata(bytes: &mut Vec<u8>, metadata: &FileMetadata) -> Result<(), String> {
    push_u32(bytes, metadata.tags.len() as u32);
    for tag in &metadata.tags {
        push_string(bytes, tag)?;
    }
    push_u32(bytes, metadata.attributes.len() as u32);
    for (key, value) in &metadata.attributes {
        push_string(bytes, key)?;
        push_string(bytes, value)?;
    }
    push_u8(bytes, encode_content_class(&metadata.content_class));
    push_u8(bytes, encode_storage_tier(&metadata.storage_tier));
    push_u32(bytes, metadata.acl.len() as u32);
    for entry in &metadata.acl {
        push_acl_entry(bytes, entry)?;
    }
    push_bool(bytes, metadata.encrypted);
    push_bool(bytes, metadata.compressed);
    push_u32(bytes, metadata.uid);
    push_u32(bytes, metadata.gid);
    push_u32(bytes, metadata.mode);
    Ok(())
}

fn read_metadata(bytes: &[u8], cursor: &mut usize) -> Result<FileMetadata, String> {
    let tag_count = read_u32(bytes, cursor)? as usize;
    let mut tags = Vec::with_capacity(tag_count);
    for _ in 0..tag_count {
        tags.push(read_string(bytes, cursor)?);
    }

    let attr_count = read_u32(bytes, cursor)? as usize;
    let mut attributes = Vec::with_capacity(attr_count);
    for _ in 0..attr_count {
        attributes.push((read_string(bytes, cursor)?, read_string(bytes, cursor)?));
    }

    let content_class = decode_content_class(read_u8(bytes, cursor)?)?;
    let storage_tier = decode_storage_tier(read_u8(bytes, cursor)?)?;
    let acl_count = read_u32(bytes, cursor)? as usize;
    let mut acl = Vec::with_capacity(acl_count);
    for _ in 0..acl_count {
        acl.push(read_acl_entry(bytes, cursor)?);
    }

    let encrypted = read_bool(bytes, cursor)?;
    let compressed = read_bool(bytes, cursor)?;
    let uid = read_u32(bytes, cursor)?;
    let gid = read_u32(bytes, cursor)?;
    let mode = read_u32(bytes, cursor)?;
    Ok(FileMetadata {
        tags,
        attributes,
        content_class,
        storage_tier,
        acl,
        encrypted,
        compressed,
        uid,
        gid,
        mode,
    })
}

fn push_acl_entry(bytes: &mut Vec<u8>, entry: &AclEntry) -> Result<(), String> {
    push_principal(bytes, &entry.principal)?;
    push_bool(bytes, entry.can_read);
    push_bool(bytes, entry.can_write);
    push_bool(bytes, entry.can_execute);
    Ok(())
}

fn read_acl_entry(bytes: &[u8], cursor: &mut usize) -> Result<AclEntry, String> {
    Ok(AclEntry {
        principal: read_principal(bytes, cursor)?,
        can_read: read_bool(bytes, cursor)?,
        can_write: read_bool(bytes, cursor)?,
        can_execute: read_bool(bytes, cursor)?,
    })
}

fn push_principal(bytes: &mut Vec<u8>, principal: &Principal) -> Result<(), String> {
    match principal {
        Principal::User(value) => {
            push_u8(bytes, 1);
            push_string(bytes, value)?;
        }
        Principal::Group(value) => {
            push_u8(bytes, 2);
            push_string(bytes, value)?;
        }
        Principal::Role(value) => {
            push_u8(bytes, 3);
            push_string(bytes, value)?;
        }
    }
    Ok(())
}

fn read_principal(bytes: &[u8], cursor: &mut usize) -> Result<Principal, String> {
    match read_u8(bytes, cursor)? {
        1 => Ok(Principal::User(read_string(bytes, cursor)?)),
        2 => Ok(Principal::Group(read_string(bytes, cursor)?)),
        3 => Ok(Principal::Role(read_string(bytes, cursor)?)),
        other => Err(format!("unknown principal kind {other}")),
    }
}

fn encode_inode_kind(kind: InodeKind) -> u8 {
    match kind {
        InodeKind::File => 1,
        InodeKind::Directory => 2,
        InodeKind::Symlink => 3,
    }
}

fn decode_inode_kind(value: u8) -> Result<InodeKind, String> {
    match value {
        1 => Ok(InodeKind::File),
        2 => Ok(InodeKind::Directory),
        3 => Ok(InodeKind::Symlink),
        other => Err(format!("unknown inode kind {other}")),
    }
}

fn encode_content_class(value: &ContentClass) -> u8 {
    match value {
        ContentClass::Text => 1,
        ContentClass::Binary => 2,
        ContentClass::Image => 3,
        ContentClass::SourceCode => 4,
        ContentClass::Archive => 5,
        ContentClass::Unknown => 6,
    }
}

fn decode_content_class(value: u8) -> Result<ContentClass, String> {
    match value {
        1 => Ok(ContentClass::Text),
        2 => Ok(ContentClass::Binary),
        3 => Ok(ContentClass::Image),
        4 => Ok(ContentClass::SourceCode),
        5 => Ok(ContentClass::Archive),
        6 => Ok(ContentClass::Unknown),
        other => Err(format!("unknown content class {other}")),
    }
}

fn encode_storage_tier(value: &StorageTier) -> u8 {
    match value {
        StorageTier::Hot => 1,
        StorageTier::Warm => 2,
        StorageTier::Cold => 3,
    }
}

fn decode_storage_tier(value: u8) -> Result<StorageTier, String> {
    match value {
        1 => Ok(StorageTier::Hot),
        2 => Ok(StorageTier::Warm),
        3 => Ok(StorageTier::Cold),
        other => Err(format!("unknown storage tier {other}")),
    }
}

fn push_system_time(bytes: &mut Vec<u8>, time: SystemTime) -> Result<(), String> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system time before unix epoch is unsupported".to_string())?;
    push_u64(bytes, duration.as_secs());
    push_u32(bytes, duration.subsec_nanos());
    Ok(())
}

fn read_system_time(bytes: &[u8], cursor: &mut usize) -> Result<SystemTime, String> {
    let secs = read_u64(bytes, cursor)?;
    let nanos = read_u32(bytes, cursor)?;
    Ok(UNIX_EPOCH + std::time::Duration::new(secs, nanos))
}

fn push_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let len = u32::try_from(value.len()).map_err(|_| "string too large".to_string())?;
    push_u32(bytes, len);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> Result<String, String> {
    let len = read_u32(bytes, cursor)? as usize;
    let raw = read_exact(bytes, cursor, len)?;
    String::from_utf8(raw.to_vec()).map_err(|error| format!("invalid utf8 string: {error}"))
}

fn push_blob(bytes: &mut Vec<u8>, blob: &[u8]) -> Result<(), String> {
    let len = u32::try_from(blob.len()).map_err(|_| "blob too large".to_string())?;
    push_u32(bytes, len);
    bytes.extend_from_slice(blob);
    Ok(())
}

fn read_blob(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>, String> {
    let len = read_u32(bytes, cursor)? as usize;
    let raw = read_exact(bytes, cursor, len)?;
    Ok(raw.to_vec())
}

fn push_bool(bytes: &mut Vec<u8>, value: bool) {
    push_u8(bytes, u8::from(value));
}

fn read_bool(bytes: &[u8], cursor: &mut usize) -> Result<bool, String> {
    match read_u8(bytes, cursor)? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(format!("invalid bool value {other}")),
    }
}

fn push_u8(bytes: &mut Vec<u8>, value: u8) {
    bytes.push(value);
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, String> {
    let raw = read_exact(bytes, cursor, 1)?;
    Ok(raw[0])
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let raw = read_exact(bytes, cursor, 4)?;
    Ok(u32::from_le_bytes(raw.try_into().expect("fixed slice")))
}

fn read_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let raw = read_exact(bytes, cursor, 8)?;
    Ok(u64::from_le_bytes(raw.try_into().expect("fixed slice")))
}

fn read_exact<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| "binary cursor overflow".to_string())?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| "truncated binary payload".to_string())?;
    *cursor = end;
    Ok(slice)
}

fn ensure_consumed(bytes: &[u8], cursor: usize) -> Result<(), String> {
    if cursor == bytes.len() {
        Ok(())
    } else {
        Err("binary payload has trailing bytes".to_string())
    }
}

fn encode_superblock(superblock: &Superblock) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SUPERBLOCK_SIZE);
    bytes.extend_from_slice(&superblock.format_version.to_le_bytes());
    bytes.extend_from_slice(&superblock.alignment.to_le_bytes());
    bytes.extend_from_slice(&superblock.segment_count.to_le_bytes());
    bytes.extend_from_slice(&superblock.clean_unmount.to_le_bytes());
    bytes.extend_from_slice(&superblock.generation.to_le_bytes());
    bytes.extend_from_slice(&superblock.directory_offset.to_le_bytes());
    bytes.extend_from_slice(&superblock.directory_length.to_le_bytes());
    bytes.extend_from_slice(&superblock.directory_checksum.to_le_bytes());
    bytes.extend_from_slice(&superblock.payload_checksum.to_le_bytes());
    bytes
}

fn encode_segment_frame(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SEGMENT_FRAME_SIZE + payload.len());
    bytes.extend_from_slice(&kind);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&checksum(payload).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn decode_segment_frame<'a>(
    bytes: &'a [u8],
    expected_kind: &[u8; 4],
    path: &Path,
) -> CoreFsResult<&'a [u8]> {
    let header = parse_segment_frame_header(bytes, path)?;
    if &header.kind != expected_kind {
        return Err(CoreFsError::State(format!(
            "mismatched CoreFS segment frame kind {} in {}",
            String::from_utf8_lossy(&header.kind),
            path.display()
        )));
    }
    let payload_start = SEGMENT_FRAME_SIZE;
    let payload_end = payload_start
        .checked_add(header.payload_length as usize)
        .ok_or_else(|| {
            CoreFsError::State(format!(
                "invalid CoreFS segment payload range {} in {}",
                String::from_utf8_lossy(expected_kind),
                path.display()
            ))
        })?;
    let payload = bytes.get(payload_start..payload_end).ok_or_else(|| {
        CoreFsError::State(format!(
            "truncated CoreFS segment payload {} in {}",
            String::from_utf8_lossy(expected_kind),
            path.display()
        ))
    })?;
    if checksum(payload) != header.payload_checksum {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS segment payload checksum {} in {}",
            String::from_utf8_lossy(expected_kind),
            path.display()
        )));
    }
    Ok(payload)
}

fn parse_segment_frame_header(bytes: &[u8], path: &Path) -> CoreFsResult<SegmentFrameHeader> {
    if bytes.len() < SEGMENT_FRAME_SIZE {
        return Err(CoreFsError::State(format!(
            "truncated CoreFS segment frame in {}",
            path.display()
        )));
    }
    Ok(SegmentFrameHeader {
        kind: bytes[0..4].try_into().expect("fixed slice"),
        payload_length: u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice")),
        payload_checksum: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice")),
    })
}

fn decode_superblock(bytes: &[u8]) -> CoreFsResult<Superblock> {
    if bytes.len() < SUPERBLOCK_SIZE {
        return Err(CoreFsError::State(
            "truncated CoreFS superblock segment".to_string(),
        ));
    }
    Ok(Superblock {
        format_version: u32::from_le_bytes(bytes[0..4].try_into().expect("fixed slice")),
        alignment: u32::from_le_bytes(bytes[4..8].try_into().expect("fixed slice")),
        segment_count: u32::from_le_bytes(bytes[8..12].try_into().expect("fixed slice")),
        clean_unmount: u32::from_le_bytes(bytes[12..16].try_into().expect("fixed slice")),
        generation: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed slice")),
        directory_offset: u64::from_le_bytes(bytes[24..32].try_into().expect("fixed slice")),
        directory_length: u64::from_le_bytes(bytes[32..40].try_into().expect("fixed slice")),
        directory_checksum: u64::from_le_bytes(bytes[40..48].try_into().expect("fixed slice")),
        payload_checksum: u64::from_le_bytes(bytes[48..56].try_into().expect("fixed slice")),
    })
}

fn load_volume_image_relaxed(path: &Path) -> CoreFsResult<RecoveredImageState> {
    let bytes = fs::read(path).map_err(|error| {
        CoreFsError::State(format!(
            "failed to read CoreFS volume image from {}: {error}",
            path.display()
        ))
    })?;
    let (entries, reconstructed_segment_directory) = load_directory_for_recovery(&bytes, path)?;
    match persisted_state_from_entries_relaxed(&bytes, &entries, path) {
        Ok((state, reconstructed_block_descriptors)) => Ok(RecoveredImageState {
            state,
            reconstructed_segment_directory,
            reconstructed_block_descriptors,
        }),
        Err(_) => {
            let entries = reconstruct_directory_from_payloads(&bytes, path)?;
            let (state, _) = persisted_state_from_entries_relaxed(&bytes, &entries, path)?;
            Ok(RecoveredImageState {
                state,
                reconstructed_segment_directory: true,
                reconstructed_block_descriptors: true,
            })
        }
    }
}

fn load_directory_from_header(bytes: &[u8], path: &Path) -> CoreFsResult<Vec<SegmentEntry>> {
    if bytes.len() < HEADER_SIZE {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS volume image, file too small: {}",
            path.display()
        )));
    }
    if &bytes[..8] != MAGIC {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS volume image magic in {}",
            path.display()
        )));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed slice"));
    if version != FORMAT_VERSION {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS volume image version {} in {}",
            version,
            path.display()
        )));
    }
    let segment_count = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed slice")) as usize;
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let directory_end = directory_offset + directory_length;
    let directory = bytes.get(directory_offset..directory_end).ok_or_else(|| {
        CoreFsError::State(format!(
            "truncated CoreFS volume image segment directory in {}",
            path.display()
        ))
    })?;
    parse_directory(directory)
}

fn load_directory_for_recovery(
    bytes: &[u8],
    path: &Path,
) -> CoreFsResult<(Vec<SegmentEntry>, bool)> {
    match load_directory_from_header(bytes, path) {
        Ok(entries) => Ok((entries, false)),
        Err(_) => Ok((reconstruct_directory_from_payloads(bytes, path)?, true)),
    }
}

fn persisted_state_from_entries(
    bytes: &[u8],
    entries: &[SegmentEntry],
    path: &Path,
) -> CoreFsResult<PersistedState> {
    let config = deserialize_optional_segment::<ConfigSegment>(bytes, entries, b"CNFG", path)?
        .map(|segment| segment.config)
        .unwrap_or_default();
    let volume = deserialize_optional_segment::<VolumeSegment>(bytes, entries, b"VOLM", path)?
        .map(|segment| segment.volume)
        .unwrap_or_else(|| VolumeDescriptor::from_config(&config));
    let active = match find_segment(entries, b"AINO") {
        Ok(entry) => deserialize_inode_segment(bytes, entry, path)?,
        Err(_) => Vec::new(),
    };
    let deleted = match find_segment(entries, b"DINO") {
        Ok(entry) => deserialize_inode_segment(bytes, entry, path)?,
        Err(_) => Vec::new(),
    };
    let journal = match find_segment(entries, b"JOUR") {
        Ok(entry) => deserialize_journal_segment(bytes, entry, path)?,
        Err(_) => Vec::new(),
    };
    let versions = deserialize_optional_segment::<VersionSegment>(bytes, entries, b"VERS", path)?
        .map(|segment| segment.versions)
        .unwrap_or_default();
    let sync = deserialize_optional_segment::<SyncSegment>(bytes, entries, b"SYNC", path)?
        .map(|segment| segment.sync_statuses)
        .unwrap_or_default();
    let hot_paths = deserialize_optional_segment::<HotPathSegment>(bytes, entries, b"HOTP", path)?
        .map(|segment| segment.records)
        .unwrap_or_default();
    let snapshots = match find_segment(entries, b"SNAP") {
        Ok(entry) => deserialize_snapshot_segment(bytes, entry, path)?,
        Err(_) => SnapshotSegment {
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        },
    };
    let journal_runtime =
        deserialize_optional_segment::<JournalRuntimeSegment>(bytes, entries, b"TXNJ", path)?
            .unwrap_or(JournalRuntimeSegment {
                clean_unmount: true,
                runtime: JournalRuntimeState::default(),
                pending_wal: None,
            });
    let free_space =
        deserialize_optional_segment::<FreeSpaceSegment>(bytes, &entries, b"FREE", path)?
            .unwrap_or(FreeSpaceSegment {
                policy: AllocatorPolicy::default(),
                extents: Vec::new(),
            });

    let block_records = match (
        deserialize_optional_segment::<BlockDescriptorSegment>(bytes, entries, b"BLKD", path)?,
        find_segment(entries, b"DATA").ok(),
    ) {
        (Some(descriptors), Some(data_entry)) => {
            let data = segment_bytes(bytes, data_entry, path)?;
            join_blocks(descriptors.descriptors, data)?
        }
        _ => Vec::new(),
    };
    let free_extents = if free_space.extents.is_empty() {
        reconstruct_free_extents_from_records(&block_records)
    } else {
        free_space.extents
    };
    validate_free_space_layout(&block_records, &free_extents)?;

    Ok(PersistedState {
        config,
        volume,
        clean_unmount: journal_runtime.clean_unmount && superblock_clean(entries, bytes, path)?,
        pending_wal: journal_runtime.pending_wal,
        active_inodes: active,
        deleted_inodes: deleted,
        allocator_policy: free_space.policy,
        free_extents,
        hot_path_records: hot_paths,
        block_records,
        journal_entries: journal,
        journal_runtime: journal_runtime.runtime,
        versions,
        sync_statuses: sync,
        snapshots: snapshots.snapshots,
        next_snapshot_id: snapshots.next_snapshot_id,
    })
}

fn persisted_state_from_entries_relaxed(
    bytes: &[u8],
    entries: &[SegmentEntry],
    path: &Path,
) -> CoreFsResult<(PersistedState, bool)> {
    match persisted_state_from_entries(bytes, entries, path) {
        Ok(state) => Ok((state, false)),
        Err(_) => {
            let mut state = persisted_state_without_blocks(bytes, entries, path)?;
            state.block_records = reconstruct_block_records_from_data(
                &state.active_inodes,
                &state.deleted_inodes,
                state.volume.block_size,
                bytes,
                entries,
                path,
            )?;
            state.free_extents = reconstruct_free_extents_from_records(&state.block_records);
            Ok((state, true))
        }
    }
}

fn persisted_state_without_blocks(
    bytes: &[u8],
    entries: &[SegmentEntry],
    path: &Path,
) -> CoreFsResult<PersistedState> {
    let config = deserialize_optional_segment::<ConfigSegment>(bytes, entries, b"CNFG", path)?
        .map(|segment| segment.config)
        .unwrap_or_default();
    let volume = deserialize_optional_segment::<VolumeSegment>(bytes, entries, b"VOLM", path)?
        .map(|segment| segment.volume)
        .unwrap_or_else(|| VolumeDescriptor::from_config(&config));
    let active = match find_segment(entries, b"AINO") {
        Ok(entry) => deserialize_inode_segment(bytes, entry, path)?,
        Err(_) => Vec::new(),
    };
    let deleted = match find_segment(entries, b"DINO") {
        Ok(entry) => deserialize_inode_segment(bytes, entry, path)?,
        Err(_) => Vec::new(),
    };
    let journal = match find_segment(entries, b"JOUR") {
        Ok(entry) => deserialize_journal_segment(bytes, entry, path)?,
        Err(_) => Vec::new(),
    };
    let versions = deserialize_optional_segment::<VersionSegment>(bytes, entries, b"VERS", path)?
        .map(|segment| segment.versions)
        .unwrap_or_default();
    let sync = deserialize_optional_segment::<SyncSegment>(bytes, entries, b"SYNC", path)?
        .map(|segment| segment.sync_statuses)
        .unwrap_or_default();
    let hot_paths = deserialize_optional_segment::<HotPathSegment>(bytes, entries, b"HOTP", path)?
        .map(|segment| segment.records)
        .unwrap_or_default();
    let snapshots = match find_segment(entries, b"SNAP") {
        Ok(entry) => deserialize_snapshot_segment(bytes, entry, path)?,
        Err(_) => SnapshotSegment {
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        },
    };
    let journal_runtime =
        deserialize_optional_segment::<JournalRuntimeSegment>(bytes, entries, b"TXNJ", path)?
            .unwrap_or(JournalRuntimeSegment {
                clean_unmount: true,
                runtime: JournalRuntimeState::default(),
                pending_wal: None,
            });

    Ok(PersistedState {
        config,
        volume,
        clean_unmount: journal_runtime.clean_unmount,
        pending_wal: journal_runtime.pending_wal,
        active_inodes: active,
        deleted_inodes: deleted,
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: hot_paths,
        block_records: Vec::new(),
        journal_entries: journal,
        journal_runtime: journal_runtime.runtime,
        versions,
        sync_statuses: sync,
        snapshots: snapshots.snapshots,
        next_snapshot_id: snapshots.next_snapshot_id,
    })
}

fn reconstruct_block_records_from_data(
    active_inodes: &[Inode],
    deleted_inodes: &[Inode],
    block_size: usize,
    bytes: &[u8],
    entries: &[SegmentEntry],
    path: &Path,
) -> CoreFsResult<Vec<BlockRecord>> {
    let data_entry = find_segment(entries, b"DATA")?;
    let data = segment_bytes(bytes, data_entry, path)?;
    let mut file_like_inodes: Vec<_> = active_inodes
        .iter()
        .chain(deleted_inodes.iter())
        .filter(|inode| {
            matches!(
                inode.kind,
                crate::domain::inode::InodeKind::File | crate::domain::inode::InodeKind::Symlink
            )
        })
        .cloned()
        .collect();
    file_like_inodes.sort_by_key(|inode| inode.id);

    let required_length: usize = file_like_inodes.iter().map(|inode| inode.size).sum();
    if required_length > data.len() {
        return Err(CoreFsError::State(format!(
            "cannot reconstruct block descriptors, DATA segment too small in {}",
            path.display()
        )));
    }

    let block_size = block_size.max(1);
    let mut offset = 0usize;
    let mut records = Vec::with_capacity(file_like_inodes.len());
    for inode in file_like_inodes {
        let end = offset + inode.size;
        let payload = data
            .get(offset..end)
            .ok_or_else(|| {
                CoreFsError::State(format!(
                    "cannot reconstruct payload for inode {} in {}",
                    inode.id.0,
                    path.display()
                ))
            })?
            .to_vec();
        records.push(BlockRecord {
            inode: inode.id,
            checksum: checksum(&payload),
            bytes: payload,
            device_block: (offset / block_size) as u64,
            allocated_blocks: inode.size.max(1).div_ceil(block_size) as u64,
        });
        offset = end;
    }

    Ok(records)
}

fn reconstruct_free_extents_from_records(block_records: &[BlockRecord]) -> Vec<FreeExtentRecord> {
    let mut occupied: Vec<(u64, u64)> = block_records
        .iter()
        .map(|record| {
            (
                record.device_block,
                record
                    .device_block
                    .saturating_add(record.allocated_blocks.max(1)),
            )
        })
        .collect();
    occupied.sort_by_key(|(start, _)| *start);

    let mut extents = Vec::new();
    let mut cursor = 0u64;
    for (start, end) in occupied {
        if start > cursor {
            extents.push(FreeExtentRecord {
                device_block: cursor,
                allocated_blocks: start - cursor,
            });
        }
        cursor = cursor.max(end);
    }
    extents
}

fn validate_free_space_layout(
    block_records: &[BlockRecord],
    free_extents: &[FreeExtentRecord],
) -> CoreFsResult<()> {
    let mut free = free_extents.to_vec();
    free.sort_by_key(|extent| extent.device_block);

    for extent in &free {
        if extent.allocated_blocks == 0 {
            return Err(CoreFsError::State(
                "invalid CoreFS FREE segment extent with zero length".to_string(),
            ));
        }
    }

    for pair in free.windows(2) {
        let left = pair[0];
        let right = pair[1];
        if left.device_block.saturating_add(left.allocated_blocks) > right.device_block {
            return Err(CoreFsError::State(
                "invalid CoreFS FREE segment overlap".to_string(),
            ));
        }
    }

    let occupied: Vec<(u64, u64)> = block_records
        .iter()
        .map(|record| {
            (
                record.device_block,
                record
                    .device_block
                    .saturating_add(record.allocated_blocks.max(1)),
            )
        })
        .collect();

    for extent in &free {
        let free_start = extent.device_block;
        let free_end = extent.device_block.saturating_add(extent.allocated_blocks);
        for (occupied_start, occupied_end) in &occupied {
            if free_start < *occupied_end && *occupied_start < free_end {
                return Err(CoreFsError::State(
                    "CoreFS FREE segment overlaps allocated blocks".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn reconstruct_directory_from_payloads(
    bytes: &[u8],
    path: &Path,
) -> CoreFsResult<Vec<SegmentEntry>> {
    validate_header_basics(bytes, path)?;
    let segment_count = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed slice")) as usize;
    if segment_count != EXPECTED_SEGMENT_KINDS.len() {
        return Err(CoreFsError::State(format!(
            "unsupported reconstructed segment count {} in {}",
            segment_count,
            path.display()
        )));
    }

    let mut entries = Vec::with_capacity(EXPECTED_SEGMENT_KINDS.len());
    let mut offset = align_up(
        HEADER_SIZE + (segment_count * SEGMENT_ENTRY_SIZE),
        SEGMENT_ALIGNMENT,
    );

    entries.push(SegmentEntry {
        kind: *b"SUPR",
        offset: offset as u64,
        length: SUPERBLOCK_SIZE as u64,
    });
    offset = align_up(offset + SUPERBLOCK_SIZE, SEGMENT_ALIGNMENT);

    entries.push(SegmentEntry {
        kind: *b"SUP2",
        offset: offset as u64,
        length: SUPERBLOCK_SIZE as u64,
    });
    offset = align_up(offset + SUPERBLOCK_SIZE, SEGMENT_ALIGNMENT);

    for kind in [
        *b"CNFG", *b"VOLM", *b"AINO", *b"DINO", *b"JOUR", *b"VERS", *b"SYNC", *b"HOTP", *b"SNAP",
        *b"TXNJ", *b"FREE",
    ] {
        let length = detect_framed_segment_length(bytes, offset, &kind, path)?;
        entries.push(SegmentEntry {
            kind,
            offset: offset as u64,
            length: length as u64,
        });
        offset = align_up(offset + length, SEGMENT_ALIGNMENT);
    }

    let descriptor_length = detect_framed_segment_length(bytes, offset, b"BLKD", path)?;
    let descriptor_entry = SegmentEntry {
        kind: *b"BLKD",
        offset: offset as u64,
        length: descriptor_length as u64,
    };
    entries.push(descriptor_entry.clone());
    offset = align_up(offset + descriptor_length, SEGMENT_ALIGNMENT);

    let descriptors: BlockDescriptorSegment = deserialize_segment(bytes, &descriptor_entry, path)?;
    let data_payload_length = descriptors
        .descriptors
        .iter()
        .map(|descriptor| descriptor.offset + descriptor.length)
        .max()
        .unwrap_or(0) as usize;
    let data_length = SEGMENT_FRAME_SIZE + data_payload_length;
    let data_end = offset + data_length;
    if data_end > bytes.len() {
        return Err(CoreFsError::State(format!(
            "reconstructed DATA segment exceeds image size in {}",
            path.display()
        )));
    }
    let data_entry = SegmentEntry {
        kind: *b"DATA",
        offset: offset as u64,
        length: data_length as u64,
    };
    let _ = segment_bytes(bytes, &data_entry, path)?;
    entries.push(SegmentEntry {
        kind: *b"DATA",
        offset: offset as u64,
        length: data_length as u64,
    });

    Ok(entries)
}

fn detect_framed_segment_length(
    bytes: &[u8],
    offset: usize,
    kind: &[u8; 4],
    path: &Path,
) -> CoreFsResult<usize> {
    let segment = bytes.get(offset..).ok_or_else(|| {
        CoreFsError::State(format!(
            "reconstructed segment {} starts outside image {}",
            String::from_utf8_lossy(kind),
            path.display()
        ))
    })?;
    let header = parse_segment_frame_header(segment, path)?;
    if &header.kind != kind {
        return Err(CoreFsError::State(format!(
            "unable to reconstruct segment {}, frame kind mismatch in {}",
            String::from_utf8_lossy(kind),
            path.display()
        )));
    }
    let total_length = SEGMENT_FRAME_SIZE
        .checked_add(header.payload_length as usize)
        .ok_or_else(|| {
            CoreFsError::State(format!(
                "invalid reconstructed segment length {} in {}",
                String::from_utf8_lossy(kind),
                path.display()
            ))
        })?;
    let frame = segment.get(..total_length).ok_or_else(|| {
        CoreFsError::State(format!(
            "truncated reconstructed segment {} in {}",
            String::from_utf8_lossy(kind),
            path.display()
        ))
    })?;
    let _ = decode_segment_frame(frame, kind, path)?;
    Ok(total_length)
}

fn inspect_volume_image_bytes(bytes: &[u8], path: &Path) -> CoreFsResult<InspectedImage> {
    validate_header_basics(bytes, path)?;
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed slice"));
    let segment_count = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed slice")) as usize;
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let directory_end = directory_offset + directory_length;
    let directory = bytes.get(directory_offset..directory_end).ok_or_else(|| {
        CoreFsError::State(format!(
            "truncated CoreFS volume image segment directory in {}",
            path.display()
        ))
    })?;
    let entries = load_directory_from_header(bytes, path)?;

    let primary_entry = find_segment(&entries, b"SUPR")?;
    let secondary_entry = find_segment(&entries, b"SUP2")?;
    let expected_payload_checksum =
        checksum_of_segment_data(bytes, &entries, &[*b"SUPR", *b"SUP2"], path)?;
    let expected_directory_checksum = checksum(directory);

    let (superblock, valid_superblocks) = read_best_superblock(
        bytes,
        primary_entry,
        secondary_entry,
        expected_directory_checksum,
        expected_payload_checksum,
        segment_count,
        path,
    )?;

    validate_required_segments(&entries)?;
    let block_descriptors: BlockDescriptorSegment =
        deserialize_segment(bytes, find_segment(&entries, b"BLKD")?, path)?;
    let data = segment_bytes(bytes, find_segment(&entries, b"DATA")?, path)?;
    let block_records = join_blocks(block_descriptors.descriptors.clone(), data)?;
    let free_space =
        deserialize_optional_segment::<FreeSpaceSegment>(bytes, &entries, b"FREE", path)?
            .unwrap_or(FreeSpaceSegment {
                policy: AllocatorPolicy::default(),
                extents: Vec::new(),
            });
    let free_extents = if free_space.extents.is_empty() {
        reconstruct_free_extents_from_records(&block_records)
    } else {
        free_space.extents
    };
    validate_free_space_layout(&block_records, &free_extents)?;

    Ok(InspectedImage {
        report: VolumeImageInspectionReport {
            format_version: version,
            segment_count,
            valid_superblocks,
            selected_generation: superblock.generation,
            directory_checksum_valid: superblock.directory_checksum == expected_directory_checksum,
            payload_checksum_valid: superblock.payload_checksum == expected_payload_checksum,
            segment_kinds: entries
                .iter()
                .map(|entry| String::from_utf8_lossy(&entry.kind).to_string())
                .collect(),
            block_descriptors: block_descriptors.descriptors.len(),
        },
        entries,
        superblock,
    })
}

fn validate_header_basics(bytes: &[u8], path: &Path) -> CoreFsResult<()> {
    if bytes.len() < HEADER_SIZE {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS volume image, file too small: {}",
            path.display()
        )));
    }

    if &bytes[..8] != MAGIC {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS volume image magic in {}",
            path.display()
        )));
    }

    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed slice"));
    if version != FORMAT_VERSION {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS volume image version {} in {}",
            version,
            path.display()
        )));
    }

    Ok(())
}

fn read_best_superblock(
    bytes: &[u8],
    primary: &SegmentEntry,
    secondary: &SegmentEntry,
    expected_directory_checksum: u64,
    expected_payload_checksum: u64,
    expected_segment_count: usize,
    path: &Path,
) -> CoreFsResult<(Superblock, usize)> {
    let mut valid = Vec::new();

    for entry in [primary, secondary] {
        let payload = segment_bytes(bytes, entry, path)?;
        if let Ok(superblock) = decode_superblock(payload) {
            if is_valid_superblock(
                &superblock,
                expected_directory_checksum,
                expected_payload_checksum,
                expected_segment_count,
            ) {
                valid.push(superblock);
            }
        }
    }

    let valid_count = valid.len();
    if let Some(best) = valid
        .into_iter()
        .max_by_key(|superblock| superblock.generation)
    {
        return Ok((best, valid_count));
    }

    Err(CoreFsError::State(format!(
        "no valid CoreFS superblock copy found in {}",
        path.display()
    )))
}

fn is_valid_superblock(
    superblock: &Superblock,
    expected_directory_checksum: u64,
    expected_payload_checksum: u64,
    expected_segment_count: usize,
) -> bool {
    superblock.directory_checksum == expected_directory_checksum
        && superblock.payload_checksum == expected_payload_checksum
        && superblock.format_version == FORMAT_VERSION
        && superblock.alignment as usize == SEGMENT_ALIGNMENT
        && superblock.segment_count as usize == expected_segment_count
}

fn superblock_clean(entries: &[SegmentEntry], bytes: &[u8], path: &Path) -> CoreFsResult<bool> {
    let directory_offset = HEADER_SIZE;
    let directory_length = entries.len() * SEGMENT_ENTRY_SIZE;
    let directory = bytes
        .get(directory_offset..directory_offset + directory_length)
        .ok_or_else(|| {
            CoreFsError::State(format!(
                "truncated CoreFS volume image segment directory in {}",
                path.display()
            ))
        })?;
    let expected_directory_checksum = checksum(directory);
    let expected_payload_checksum =
        checksum_of_segment_data(bytes, entries, &[*b"SUPR", *b"SUP2"], path)?;
    let (superblock, _) = read_best_superblock(
        bytes,
        find_segment(entries, b"SUPR")?,
        find_segment(entries, b"SUP2")?,
        expected_directory_checksum,
        expected_payload_checksum,
        entries.len(),
        path,
    )?;
    Ok(superblock.clean_unmount != 0)
}

fn validate_required_segments(entries: &[SegmentEntry]) -> CoreFsResult<()> {
    for kind in [
        *b"SUPR", *b"SUP2", *b"CNFG", *b"VOLM", *b"AINO", *b"DINO", *b"JOUR", *b"VERS", *b"SYNC",
        *b"HOTP", *b"SNAP", *b"TXNJ", *b"BLKD", *b"DATA",
    ] {
        let _ = find_segment(entries, &kind)?;
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, byte| {
        acc.wrapping_mul(16777619).wrapping_add(u64::from(*byte))
    })
}

fn checksum_of_payloads(segments: &[SegmentPayload]) -> u64 {
    segments
        .iter()
        .filter(|segment| segment.kind != *b"SUPR" && segment.kind != *b"SUP2")
        .fold(0u64, |acc, segment| {
            acc ^ checksum(segment_payload_for_checksum(segment))
        })
}

fn checksum_of_segment_data(
    bytes: &[u8],
    entries: &[SegmentEntry],
    skip: &[[u8; 4]],
    path: &Path,
) -> CoreFsResult<u64> {
    let mut value = 0u64;
    for entry in entries {
        if skip.iter().any(|kind| entry.kind == *kind) {
            continue;
        }
        value ^= checksum(segment_bytes(bytes, entry, path)?);
    }
    Ok(value)
}

fn segment_payload_for_checksum<'a>(segment: &'a SegmentPayload) -> &'a [u8] {
    if segment.kind == *b"SUPR" || segment.kind == *b"SUP2" {
        &segment.payload
    } else {
        &segment.payload[SEGMENT_FRAME_SIZE..]
    }
}

fn current_generation() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos() as u64
}

// ---------------------------------------------------------------------------
// BlockDevice-based persistence
// ---------------------------------------------------------------------------

use crate::storage::block_device::BlockDevice;

/// Serializes the full volume state to a [`BlockDevice`].
///
/// The image is built in memory (identical binary format as the file-based
/// path), then written sector-aligned to the device.  The device must be
/// large enough to hold the entire image.
pub fn save_to_device(
    device: &mut dyn BlockDevice,
    state: &PersistedState,
) -> CoreFsResult<()> {
    let label = "<device>";
    let path = Path::new(label);
    let (descriptors, block_data) = split_blocks(&state.block_records, state.volume.block_size);

    let mut segments = vec![
        raw_segment_from_bytes(*b"SUPR", vec![0; SUPERBLOCK_SIZE]),
        raw_segment_from_bytes(*b"SUP2", vec![0; SUPERBLOCK_SIZE]),
        serialize_segment(*b"CNFG", &ConfigSegment { config: state.config.clone() }, path)?,
        serialize_segment(*b"VOLM", &VolumeSegment { volume: state.volume.clone() }, path)?,
        serialize_inode_segment(*b"AINO", &state.active_inodes, path)?,
        serialize_inode_segment(*b"DINO", &state.deleted_inodes, path)?,
        serialize_journal_segment(*b"JOUR", &state.journal_entries, path)?,
        serialize_segment(*b"VERS", &VersionSegment { versions: state.versions.clone() }, path)?,
        serialize_segment(*b"SYNC", &SyncSegment { sync_statuses: state.sync_statuses.clone() }, path)?,
        serialize_segment(*b"HOTP", &HotPathSegment { records: state.hot_path_records.clone() }, path)?,
        serialize_snapshot_segment(*b"SNAP", &SnapshotSegment { snapshots: state.snapshots.clone(), next_snapshot_id: state.next_snapshot_id }, path)?,
        serialize_segment(*b"TXNJ", &JournalRuntimeSegment { clean_unmount: state.clean_unmount, runtime: state.journal_runtime.clone(), pending_wal: state.pending_wal.clone() }, path)?,
        serialize_segment(*b"FREE", &FreeSpaceSegment { policy: state.allocator_policy.clone(), extents: state.free_extents.clone() }, path)?,
        serialize_segment(*b"BLKD", &BlockDescriptorSegment { descriptors }, path)?,
        serialize_bytes_segment(*b"DATA", &block_data, path)?,
    ];

    let segment_count = segments.len();
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let mut offset = align_up(directory_offset + directory_length, SEGMENT_ALIGNMENT);
    let mut entries = Vec::with_capacity(segment_count);

    for segment in &segments {
        entries.push(SegmentEntry {
            kind: segment.kind,
            offset: offset as u64,
            length: segment.payload.len() as u64,
        });
        offset = align_up(offset + segment.payload.len(), SEGMENT_ALIGNMENT);
    }

    let total_size = offset;
    let sector_size = device.sector_size() as usize;
    let padded_size = align_up(total_size, sector_size);

    if padded_size as u64 > device.capacity() {
        return Err(CoreFsError::State(format!(
            "volume image ({padded_size} bytes) exceeds device capacity ({} bytes)",
            device.capacity()
        )));
    }

    let directory_bytes = directory_bytes(&entries);
    let superblock = Superblock {
        format_version: FORMAT_VERSION,
        alignment: SEGMENT_ALIGNMENT as u32,
        segment_count: segment_count as u32,
        clean_unmount: u32::from(state.clean_unmount),
        generation: current_generation(),
        directory_offset: directory_offset as u64,
        directory_length: directory_length as u64,
        directory_checksum: checksum(&directory_bytes),
        payload_checksum: checksum_of_payloads(&segments),
    };
    let superblock_bytes = encode_superblock(&superblock);
    segments[0].payload = superblock_bytes.clone();
    segments[1].payload = superblock_bytes;

    let mut bytes = vec![0u8; padded_size];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&(segment_count as u32).to_le_bytes());
    bytes[directory_offset..directory_offset + directory_length].copy_from_slice(&directory_bytes);

    for (entry, segment) in entries.iter().zip(segments.iter()) {
        let start = entry.offset as usize;
        let end = start + segment.payload.len();
        bytes[start..end].copy_from_slice(&segment.payload);
    }

    // Write sector-by-sector to support devices with limited write buffers.
    let mut write_offset = 0u64;
    while write_offset < padded_size as u64 {
        let chunk_end = (write_offset as usize + sector_size).min(padded_size);
        device.write_at(write_offset, &bytes[write_offset as usize..chunk_end])?;
        write_offset = chunk_end as u64;
    }
    device.sync()?;

    Ok(())
}

/// Loads a volume state from a [`BlockDevice`].
///
/// Reads the header first to determine the image size, then loads the
/// full image and delegates to the existing byte-level parser.
pub fn load_from_device(device: &dyn BlockDevice) -> CoreFsResult<PersistedState> {
    let label = "<device>";
    let path = Path::new(label);
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
    let version = u32::from_le_bytes(header_sector[8..12].try_into().expect("fixed slice"));
    if version != FORMAT_VERSION {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS format version {version} on device"
        )));
    }

    let segment_count = u32::from_le_bytes(
        header_sector[12..16].try_into().expect("fixed slice"),
    ) as usize;

    // Calculate how much data we need: parse the directory to find the last segment end.
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let directory_end = directory_offset + directory_length;
    let needed_for_directory = align_up(directory_end, sector_size as usize);

    // Read enough sectors to cover the full directory.
    let header_bytes = if needed_for_directory as u64 > sector_size {
        let extra = device.read_at(sector_size, needed_for_directory as u64 - sector_size)?;
        let mut combined = header_sector;
        combined.extend_from_slice(&extra);
        combined
    } else {
        header_sector
    };

    // Parse directory to find the total image extent.
    let directory = header_bytes.get(directory_offset..directory_end).ok_or_else(|| {
        CoreFsError::State("truncated CoreFS directory on device".to_string())
    })?;
    let entries = parse_directory(directory)?;
    let image_end = entries
        .iter()
        .map(|e| e.offset + e.length)
        .max()
        .unwrap_or(directory_end as u64);
    let total_read = align_up(image_end as usize, sector_size as usize);
    let total_read = total_read.min(device.capacity() as usize);

    // Read the full image.
    let bytes = if total_read as u64 > header_bytes.len() as u64 {
        let remaining_offset = header_bytes.len() as u64;
        let remaining = device.read_at(remaining_offset, total_read as u64 - remaining_offset)?;
        let mut full = header_bytes;
        full.extend_from_slice(&remaining);
        full
    } else {
        header_bytes[..total_read].to_vec()
    };

    let inspected = inspect_volume_image_bytes(&bytes, path)?;
    let superblock = inspected.superblock;

    if superblock.alignment as usize != SEGMENT_ALIGNMENT {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS alignment {} on device",
            superblock.alignment
        )));
    }

    persisted_state_from_entries(&bytes, &inspected.entries, path)
}

/// Inspects a CoreFS volume on a [`BlockDevice`] and returns structural
/// integrity metadata.
///
/// Reads the volume image from the device and runs the same checks as
/// [`inspect_volume_image`] (superblock validation, checksum verification,
/// segment presence).  Does not modify the device.
pub fn inspect_device(device: &dyn BlockDevice) -> CoreFsResult<VolumeImageInspectionReport> {
    let label = "<device>";
    let path = Path::new(label);
    let sector_size = device.sector_size() as u64;

    // Read header.
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

    let segment_count =
        u32::from_le_bytes(header_sector[12..16].try_into().expect("fixed")) as usize;
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let directory_end = directory_offset + directory_length;
    let needed_for_directory = align_up(directory_end, sector_size as usize);

    // Read header + directory.
    let header_bytes = if needed_for_directory as u64 > sector_size {
        let extra = device.read_at(sector_size, needed_for_directory as u64 - sector_size)?;
        let mut combined = header_sector;
        combined.extend_from_slice(&extra);
        combined
    } else {
        header_sector
    };

    let directory = header_bytes.get(directory_offset..directory_end).ok_or_else(|| {
        CoreFsError::State("truncated CoreFS directory on device".to_string())
    })?;
    let entries = parse_directory(directory)?;
    let image_end = entries
        .iter()
        .map(|e| e.offset + e.length)
        .max()
        .unwrap_or(directory_end as u64);
    let total_read = align_up(image_end as usize, sector_size as usize);
    let total_read = total_read.min(device.capacity() as usize);

    // Read the full image for checksum validation.
    let bytes = if total_read as u64 > header_bytes.len() as u64 {
        let remaining_offset = header_bytes.len() as u64;
        let remaining = device.read_at(remaining_offset, total_read as u64 - remaining_offset)?;
        let mut full = header_bytes;
        full.extend_from_slice(&remaining);
        full
    } else {
        header_bytes[..total_read].to_vec()
    };

    Ok(inspect_volume_image_bytes(&bytes, path)?.report)
}

/// Builds a serialized CoreFS image as a byte vector (for use with
/// [`BlockDevice::write_at`] or direct device formatting).
pub fn build_volume_image_bytes(state: &PersistedState) -> CoreFsResult<Vec<u8>> {
    let label = "<memory>";
    let path = Path::new(label);
    let (descriptors, block_data) = split_blocks(&state.block_records, state.volume.block_size);

    let mut segments = vec![
        raw_segment_from_bytes(*b"SUPR", vec![0; SUPERBLOCK_SIZE]),
        raw_segment_from_bytes(*b"SUP2", vec![0; SUPERBLOCK_SIZE]),
        serialize_segment(*b"CNFG", &ConfigSegment { config: state.config.clone() }, path)?,
        serialize_segment(*b"VOLM", &VolumeSegment { volume: state.volume.clone() }, path)?,
        serialize_inode_segment(*b"AINO", &state.active_inodes, path)?,
        serialize_inode_segment(*b"DINO", &state.deleted_inodes, path)?,
        serialize_journal_segment(*b"JOUR", &state.journal_entries, path)?,
        serialize_segment(*b"VERS", &VersionSegment { versions: state.versions.clone() }, path)?,
        serialize_segment(*b"SYNC", &SyncSegment { sync_statuses: state.sync_statuses.clone() }, path)?,
        serialize_segment(*b"HOTP", &HotPathSegment { records: state.hot_path_records.clone() }, path)?,
        serialize_snapshot_segment(*b"SNAP", &SnapshotSegment { snapshots: state.snapshots.clone(), next_snapshot_id: state.next_snapshot_id }, path)?,
        serialize_segment(*b"TXNJ", &JournalRuntimeSegment { clean_unmount: state.clean_unmount, runtime: state.journal_runtime.clone(), pending_wal: state.pending_wal.clone() }, path)?,
        serialize_segment(*b"FREE", &FreeSpaceSegment { policy: state.allocator_policy.clone(), extents: state.free_extents.clone() }, path)?,
        serialize_segment(*b"BLKD", &BlockDescriptorSegment { descriptors }, path)?,
        serialize_bytes_segment(*b"DATA", &block_data, path)?,
    ];

    let segment_count = segments.len();
    let directory_offset = HEADER_SIZE;
    let directory_length = segment_count * SEGMENT_ENTRY_SIZE;
    let mut offset = align_up(directory_offset + directory_length, SEGMENT_ALIGNMENT);
    let mut entries = Vec::with_capacity(segment_count);

    for segment in &segments {
        entries.push(SegmentEntry {
            kind: segment.kind,
            offset: offset as u64,
            length: segment.payload.len() as u64,
        });
        offset = align_up(offset + segment.payload.len(), SEGMENT_ALIGNMENT);
    }

    let total_size = offset;
    let directory_bytes = directory_bytes(&entries);
    let superblock = Superblock {
        format_version: FORMAT_VERSION,
        alignment: SEGMENT_ALIGNMENT as u32,
        segment_count: segment_count as u32,
        clean_unmount: u32::from(state.clean_unmount),
        generation: current_generation(),
        directory_offset: directory_offset as u64,
        directory_length: directory_length as u64,
        directory_checksum: checksum(&directory_bytes),
        payload_checksum: checksum_of_payloads(&segments),
    };
    let superblock_bytes = encode_superblock(&superblock);
    segments[0].payload = superblock_bytes.clone();
    segments[1].payload = superblock_bytes;

    let mut bytes = vec![0u8; total_size];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[12..16].copy_from_slice(&(segment_count as u32).to_le_bytes());
    bytes[directory_offset..directory_offset + directory_length].copy_from_slice(&directory_bytes);

    for (entry, segment) in entries.iter().zip(segments.iter()) {
        let start = entry.offset as usize;
        let end = start + segment.payload.len();
        bytes[start..end].copy_from_slice(&segment.payload);
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PersistedState;
    use crate::config::CoreFsConfig;
    use crate::domain::inode::{Inode, InodeId, InodeKind};
    use crate::domain::metadata::FileMetadata;
    use crate::domain::snapshot::Snapshot;
    use crate::domain::volume::VolumeDescriptor;
    use crate::services::journal::{JournalEntry, JournalRuntimeState};
    use crate::storage::block_store::BlockRecord;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "corefs-{name}-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ))
    }

    fn sample_state() -> PersistedState {
        PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: Vec::new(),
            deleted_inodes: Vec::new(),
            allocator_policy: AllocatorPolicy::default(),
            free_extents: Vec::new(),
            hot_path_records: vec![HotPathRecord {
                path: "/hot.txt".to_string(),
                read_ops: 0,
                write_ops: 3,
                metadata_ops: 1,
                bytes_read: 0,
                bytes_written: 4096,
            }],
            block_records: Vec::new(),
            journal_entries: Vec::new(),
            journal_runtime: JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: vec![Snapshot {
                id: 1,
                name: "baseline".to_string(),
                scope_root: "/".to_string(),
                created_at: SystemTime::now(),
                paths: vec!["/".to_string()],
                file_data: std::collections::BTreeMap::new(),
            }],
            next_snapshot_id: 1,
        }
    }

    #[test]
    fn save_and_load_volume_image_round_trip() {
        let path = temp_path("roundtrip");
        let mut state = sample_state();
        state.allocator_policy = AllocatorPolicy {
            strategy: crate::storage::block_store::AllocationStrategy::FirstFit,
            split_threshold_blocks: 2,
            coalesce_on_release: true,
            tail_trim_enabled: true,
            background_compaction_enabled: true,
            fragmentation_threshold_percent: 40,
        };
        state.free_extents = vec![FreeExtentRecord {
            device_block: 0,
            allocated_blocks: 4,
        }];
        state.block_records = vec![BlockRecord {
            inode: InodeId(7),
            bytes: b"payload".to_vec(),
            checksum: checksum(b"payload"),
            device_block: 4,
            allocated_blocks: 2,
        }];

        save_volume_image(&path, &state).expect("volume image should be written");
        let loaded = load_volume_image(&path).expect("volume image should be loaded");

        assert_eq!(loaded.config, state.config);
        assert_eq!(loaded.next_snapshot_id, 1);
        assert_eq!(loaded.snapshots.len(), 1);
        assert_eq!(loaded.allocator_policy, state.allocator_policy);
        assert_eq!(loaded.free_extents, state.free_extents);
        assert_eq!(loaded.hot_path_records, state.hot_path_records);
        assert_eq!(loaded.block_records, state.block_records);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn invalid_magic_is_rejected() {
        let path = temp_path("invalid-magic");
        fs::write(&path, b"not-a-corefs-image").expect("test fixture should be written");

        let result = load_volume_image(&path);
        assert!(matches!(result, Err(CoreFsError::State(_))));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn segmented_volume_image_contains_directory_and_segments() {
        let path = temp_path("segmented-layout");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let bytes = fs::read(&path).expect("image should exist");

        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().expect("fixed")),
            FORMAT_VERSION
        );
        assert_eq!(
            u32::from_le_bytes(bytes[12..16].try_into().expect("fixed")),
            EXPECTED_SEGMENT_KINDS.len() as u32
        );
        assert_eq!(&bytes[16..20], b"SUPR");
        assert_eq!(&bytes[40..44], b"SUP2");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn align_up_moves_offsets_to_alignment() {
        assert_eq!(align_up(64, 64), 64);
        assert_eq!(align_up(65, 64), 128);
    }

    #[test]
    fn secondary_superblock_can_be_used_as_fallback() {
        let path = temp_path("superblock-fallback");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
        bytes[primary_offset] ^= 0xFF;
        fs::write(&path, bytes).expect("mutated image should be written");

        let loaded = load_volume_image(&path).expect("secondary superblock should still work");
        assert_eq!(loaded.next_snapshot_id, 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn inspect_volume_image_reports_segment_health() {
        let path = temp_path("inspect");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let report = inspect_volume_image(&path).expect("inspection should succeed");

        assert_eq!(report.format_version, FORMAT_VERSION);
        assert_eq!(report.segment_count, EXPECTED_SEGMENT_KINDS.len());
        assert_eq!(report.valid_superblocks, 2);
        assert!(report.directory_checksum_valid);
        assert!(report.payload_checksum_valid);
        assert!(report.segment_kinds.iter().any(|kind| kind == "DATA"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn newer_valid_superblock_generation_is_preferred() {
        let path = temp_path("superblock-generation");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let entries = parse_directory(
            &bytes[HEADER_SIZE..HEADER_SIZE + (EXPECTED_SEGMENT_KINDS.len() * SEGMENT_ENTRY_SIZE)],
        )
        .expect("directory should parse");
        let secondary = find_segment(&entries, b"SUP2").expect("secondary superblock should exist");
        let generation_offset = secondary.offset as usize + 16;
        let old_generation = u64::from_le_bytes(
            bytes[generation_offset..generation_offset + 8]
                .try_into()
                .expect("fixed slice"),
        );
        bytes[generation_offset..generation_offset + 8]
            .copy_from_slice(&(old_generation + 7).to_le_bytes());
        fs::write(&path, bytes).expect("mutated image should be written");

        let report = inspect_volume_image(&path).expect("inspection should succeed");
        assert_eq!(report.valid_superblocks, 2);
        assert_eq!(report.selected_generation, old_generation + 7);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_volume_image_rebuilds_both_superblock_copies() {
        let path = temp_path("repair-superblocks");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
        bytes[primary_offset] ^= 0xFF;
        fs::write(&path, bytes).expect("corrupted image should be written");

        let repaired =
            repair_volume_image_superblocks(&path).expect("repair should restore redundancy");
        assert_eq!(repaired.repaired_superblocks, 1);
        assert_eq!(repaired.resulting_valid_superblocks, 2);

        let report = inspect_volume_image(&path).expect("inspection should succeed");
        assert_eq!(report.valid_superblocks, 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_volume_image_applies_journal_reconciliation() {
        let path = temp_path("repair-journal");
        let mut active_inode = Inode::new(
            InodeId(1),
            InodeKind::File,
            "/a".to_string(),
            FileMetadata::default(),
        );
        active_inode.size = 999;
        let deleted_inode = Inode::new(
            InodeId(2),
            InodeKind::File,
            "/b".to_string(),
            FileMetadata::default(),
        );
        let state = PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: vec![active_inode],
            deleted_inodes: vec![deleted_inode],
            allocator_policy: AllocatorPolicy::default(),
            free_extents: Vec::new(),
            hot_path_records: Vec::new(),
            block_records: vec![
                BlockRecord {
                    inode: InodeId(1),
                    bytes: b"hello".to_vec(),
                    checksum: 123,
                    device_block: 0,
                    allocated_blocks: 1,
                },
                BlockRecord {
                    inode: InodeId(99),
                    bytes: b"orphan".to_vec(),
                    checksum: 456,
                    device_block: 1,
                    allocated_blocks: 1,
                },
            ],
            journal_entries: vec![
                JournalEntry {
                    timestamp: SystemTime::now(),
                    operation: "delete".to_string(),
                    target: "/a".to_string(),
                    details: String::new(),
                },
                JournalEntry {
                    timestamp: SystemTime::now(),
                    operation: "restore".to_string(),
                    target: "/b".to_string(),
                    details: String::new(),
                },
            ],
            journal_runtime: JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        };

        save_volume_image(&path, &state).expect("volume image should be written");
        let repaired = repair_volume_image_superblocks(&path).expect("repair should succeed");

        assert_eq!(repaired.journal_repair.moved_to_deleted, 1);
        assert_eq!(repaired.journal_repair.restored_to_active, 1);
        assert_eq!(repaired.journal_repair.removed_orphan_blocks, 1);
        assert_eq!(repaired.journal_repair.resized_inodes, 1);

        let loaded = load_volume_image(&path).expect("repaired image should load");
        assert_eq!(loaded.active_inodes.len(), 1);
        assert_eq!(loaded.active_inodes[0].path, "/b");
        assert_eq!(loaded.deleted_inodes.len(), 1);
        assert_eq!(loaded.deleted_inodes[0].path, "/a");
        assert_eq!(loaded.block_records.len(), 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_volume_image_recovers_from_missing_valid_superblocks_via_header_directory() {
        let path = temp_path("repair-header-fallback");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
        let secondary_offset =
            u64::from_le_bytes(bytes[48..56].try_into().expect("fixed")) as usize;
        bytes[primary_offset] ^= 0xFF;
        bytes[secondary_offset] ^= 0xFF;
        fs::write(&path, bytes).expect("corrupted image should be written");

        let repaired = repair_volume_image_superblocks(&path)
            .expect("repair should succeed via header directory recovery");

        assert!(repaired.recovered_without_valid_superblock);
        assert!(!repaired.reconstructed_segment_directory);
        assert!(repaired.reconstructed_block_descriptors);
        assert_eq!(repaired.resulting_valid_superblocks, 2);

        let report = inspect_volume_image(&path).expect("repaired image should inspect");
        assert_eq!(report.valid_superblocks, 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_volume_image_reconstructs_corrupted_directory_entries() {
        let path = temp_path("repair-corrupted-directory");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        bytes[72..80].copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[80..88].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, bytes).expect("corrupted directory should be written");

        let repaired = repair_volume_image_superblocks(&path)
            .expect("repair should reconstruct the directory");

        assert!(repaired.reconstructed_segment_directory);
        assert!(repaired.reconstructed_block_descriptors);
        assert_eq!(repaired.resulting_valid_superblocks, 2);

        let loaded = load_volume_image(&path).expect("repaired image should load cleanly");
        assert_eq!(loaded.next_snapshot_id, 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_volume_image_reconstructs_corrupted_block_descriptors() {
        let path = temp_path("repair-corrupted-blkd");
        let mut inode = Inode::new(
            InodeId(7),
            InodeKind::File,
            "/payload.txt".to_string(),
            FileMetadata::default(),
        );
        inode.size = 5;
        let state = PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: vec![inode],
            deleted_inodes: Vec::new(),
            allocator_policy: AllocatorPolicy::default(),
            free_extents: Vec::new(),
            hot_path_records: Vec::new(),
            block_records: vec![BlockRecord {
                inode: InodeId(7),
                bytes: b"hello".to_vec(),
                checksum: checksum(b"hello"),
                device_block: 0,
                allocated_blocks: 1,
            }],
            journal_entries: Vec::new(),
            journal_runtime: JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        };

        save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let entries = parse_directory(
            &bytes[HEADER_SIZE..HEADER_SIZE + (EXPECTED_SEGMENT_KINDS.len() * SEGMENT_ENTRY_SIZE)],
        )
        .expect("directory should parse");
        let blkd_offset = find_segment(&entries, b"BLKD")
            .expect("blkd segment should exist")
            .offset as usize;
        if let Some(byte) = bytes.get_mut(blkd_offset) {
            *byte ^= 0xFF;
        }
        fs::write(&path, bytes).expect("corrupted image should be written");

        let repaired = repair_volume_image_superblocks(&path)
            .expect("repair should reconstruct block descriptors");

        assert!(repaired.reconstructed_block_descriptors);
        let loaded = load_volume_image(&path).expect("repaired image should load");
        assert_eq!(loaded.block_records.len(), 1);
        assert_eq!(loaded.block_records[0].bytes, b"hello".to_vec());

        let _ = fs::remove_file(path);
    }

    // -----------------------------------------------------------------------
    // BlockDevice integration tests
    // -----------------------------------------------------------------------

    use crate::storage::block_device::MemoryDevice;

    fn memory_device(capacity: u64) -> MemoryDevice {
        MemoryDevice::new(capacity, 4096).unwrap()
    }

    #[test]
    fn save_and_load_from_device_round_trip() {
        let state = sample_state();
        let mut dev = memory_device(2 * 1024 * 1024);

        save_to_device(&mut dev, &state).unwrap();
        let loaded = load_from_device(&dev).unwrap();

        assert_eq!(loaded.config.volume_name, state.config.volume_name);
        assert_eq!(loaded.active_inodes.len(), state.active_inodes.len());
        assert_eq!(loaded.deleted_inodes.len(), state.deleted_inodes.len());
        assert_eq!(loaded.journal_entries.len(), state.journal_entries.len());
        assert_eq!(loaded.versions.len(), state.versions.len());
        assert_eq!(loaded.snapshots.len(), state.snapshots.len());
        assert_eq!(loaded.next_snapshot_id, state.next_snapshot_id);
        assert_eq!(loaded.block_records.len(), state.block_records.len());
        assert_eq!(loaded.free_extents.len(), state.free_extents.len());
    }

    #[test]
    fn save_to_device_rejects_insufficient_capacity() {
        // Build a state with enough data to exceed a very small device.
        let mut state = sample_state();
        state.block_records = vec![
            crate::storage::block_store::BlockRecord {
                inode: InodeId(1),
                bytes: vec![0xAA; 8192],
                checksum: 999,
                device_block: 0,
                allocated_blocks: 2,
            },
        ];
        // 4 KiB device cannot fit the header + segments + 8 KiB data.
        let mut dev = memory_device(4096);

        let err = save_to_device(&mut dev, &state).unwrap_err();
        assert!(err.to_string().contains("exceeds device capacity"));
    }

    #[test]
    fn load_from_device_rejects_unformatted() {
        let dev = memory_device(2 * 1024 * 1024); // All zeros
        let err = load_from_device(&dev).unwrap_err();
        assert!(err.to_string().contains("invalid magic"));
    }

    #[test]
    fn save_and_load_device_preserves_block_data() {
        let mut state = sample_state();
        state.block_records = vec![
            crate::storage::block_store::BlockRecord {
                inode: InodeId(1),
                bytes: b"device-test-payload".to_vec(),
                checksum: 12345,
                device_block: 0,
                allocated_blocks: 1,
            },
        ];
        let mut dev = memory_device(2 * 1024 * 1024);

        save_to_device(&mut dev, &state).unwrap();
        let loaded = load_from_device(&dev).unwrap();

        assert_eq!(loaded.block_records.len(), 1);
        assert_eq!(loaded.block_records[0].bytes, b"device-test-payload".to_vec());
        assert_eq!(loaded.block_records[0].inode, InodeId(1));
    }

    #[test]
    fn save_and_load_device_empty_state() {
        let state = PersistedState {
            config: CoreFsConfig::default(),
            volume: crate::domain::volume::VolumeDescriptor::from_config(&CoreFsConfig::default()),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: Vec::new(),
            deleted_inodes: Vec::new(),
            allocator_policy: AllocatorPolicy::default(),
            free_extents: Vec::new(),
            hot_path_records: Vec::new(),
            block_records: Vec::new(),
            journal_entries: Vec::new(),
            journal_runtime: crate::services::journal::JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        };
        let mut dev = memory_device(2 * 1024 * 1024);

        save_to_device(&mut dev, &state).unwrap();
        let loaded = load_from_device(&dev).unwrap();

        assert_eq!(loaded.config.volume_name, "corefs");
        assert!(loaded.active_inodes.is_empty());
        assert!(loaded.block_records.is_empty());
    }

    #[test]
    fn save_and_load_device_with_512_byte_sectors() {
        let state = sample_state();
        let mut dev = MemoryDevice::new(2 * 1024 * 1024, 512).unwrap();

        save_to_device(&mut dev, &state).unwrap();
        let loaded = load_from_device(&dev).unwrap();

        assert_eq!(loaded.config.volume_name, state.config.volume_name);
        assert_eq!(loaded.active_inodes.len(), state.active_inodes.len());
    }

    #[test]
    fn build_volume_image_bytes_creates_valid_image() {
        let state = sample_state();
        let bytes = build_volume_image_bytes(&state).unwrap();

        // Should start with MAGIC
        assert_eq!(&bytes[..8], MAGIC);
        // Should have correct format version
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(version, FORMAT_VERSION);
    }

    #[test]
    fn device_round_trip_matches_file_round_trip() {
        let state = sample_state();

        // File path
        let file_path = temp_path("device-vs-file");
        save_volume_image(&file_path, &state).unwrap();
        let file_loaded = load_volume_image(&file_path).unwrap();

        // Device path
        let mut dev = memory_device(2 * 1024 * 1024);
        save_to_device(&mut dev, &state).unwrap();
        let device_loaded = load_from_device(&dev).unwrap();

        // Both should produce identical state
        assert_eq!(file_loaded.config, device_loaded.config);
        assert_eq!(file_loaded.active_inodes.len(), device_loaded.active_inodes.len());
        assert_eq!(file_loaded.block_records.len(), device_loaded.block_records.len());
        assert_eq!(file_loaded.snapshots.len(), device_loaded.snapshots.len());
        assert_eq!(file_loaded.next_snapshot_id, device_loaded.next_snapshot_id);
        for (f, d) in file_loaded.block_records.iter().zip(device_loaded.block_records.iter()) {
            assert_eq!(f.inode, d.inode);
            assert_eq!(f.bytes, d.bytes);
        }

        let _ = fs::remove_file(file_path);
    }

    #[test]
    fn multiple_save_load_cycles_on_same_device() {
        let mut dev = memory_device(2 * 1024 * 1024);

        // Cycle 1: empty state
        let state1 = PersistedState {
            config: CoreFsConfig::default(),
            volume: crate::domain::volume::VolumeDescriptor::from_config(&CoreFsConfig::default()),
            clean_unmount: true,
            pending_wal: None,
            active_inodes: Vec::new(),
            deleted_inodes: Vec::new(),
            allocator_policy: AllocatorPolicy::default(),
            free_extents: Vec::new(),
            hot_path_records: Vec::new(),
            block_records: Vec::new(),
            journal_entries: Vec::new(),
            journal_runtime: crate::services::journal::JournalRuntimeState::default(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: Vec::new(),
            next_snapshot_id: 0,
        };
        save_to_device(&mut dev, &state1).unwrap();

        // Cycle 2: overwrite with sample state
        let state2 = sample_state();
        save_to_device(&mut dev, &state2).unwrap();
        let loaded = load_from_device(&dev).unwrap();
        assert_eq!(loaded.active_inodes.len(), state2.active_inodes.len());
        assert_eq!(loaded.next_snapshot_id, state2.next_snapshot_id);
    }
}
