use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId};
use crate::domain::snapshot::Snapshot;
use crate::domain::volume::VolumeDescriptor;
use crate::error::{CoreFsError, CoreFsResult};
use crate::services::journal::JournalEntry;
use crate::services::sync::SyncStatus;
use crate::services::versioning::FileVersion;
use crate::storage::block_store::BlockRecord;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 8] = b"COREFS01";
const FORMAT_VERSION: u32 = 4;
const SEGMENT_ALIGNMENT: usize = 64;
const SEGMENT_ENTRY_SIZE: usize = 24;
const HEADER_SIZE: usize = 16;
const SUPERBLOCK_SIZE: usize = 52;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BlockDescriptor {
    inode: InodeId,
    checksum: u64,
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
struct InodeSegment {
    inodes: Vec<Inode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalSegment {
    journal_entries: Vec<JournalEntry>,
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
struct SnapshotSegment {
    snapshots: Vec<Snapshot>,
    next_snapshot_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BlockDescriptorSegment {
    descriptors: Vec<BlockDescriptor>,
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

#[derive(Debug, Clone)]
struct InspectedImage {
    entries: Vec<SegmentEntry>,
    superblock: Superblock,
    report: VolumeImageInspectionReport,
}

pub fn save_volume_image(path: impl AsRef<Path>, state: &PersistedState) -> CoreFsResult<()> {
    let path = path.as_ref();
    let (descriptors, block_data) = split_blocks(&state.block_records);

    let mut segments = vec![
        segment_from_bytes(*b"SUPR", vec![0; SUPERBLOCK_SIZE]),
        segment_from_bytes(*b"SUP2", vec![0; SUPERBLOCK_SIZE]),
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
        serialize_segment(
            *b"AINO",
            &InodeSegment {
                inodes: state.active_inodes.clone(),
            },
            path,
        )?,
        serialize_segment(
            *b"DINO",
            &InodeSegment {
                inodes: state.deleted_inodes.clone(),
            },
            path,
        )?,
        serialize_segment(
            *b"JOUR",
            &JournalSegment {
                journal_entries: state.journal_entries.clone(),
            },
            path,
        )?,
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
            *b"SNAP",
            &SnapshotSegment {
                snapshots: state.snapshots.clone(),
                next_snapshot_id: state.next_snapshot_id,
            },
            path,
        )?,
        serialize_segment(*b"BLKD", &BlockDescriptorSegment { descriptors }, path)?,
        segment_from_bytes(*b"DATA", block_data),
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

    let config: ConfigSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"CNFG")?, path)?;
    let volume: VolumeSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"VOLM")?, path)?;
    let active: InodeSegment = deserialize_segment(&bytes, find_segment(&entries, b"AINO")?, path)?;
    let deleted: InodeSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"DINO")?, path)?;
    let journal: JournalSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"JOUR")?, path)?;
    let versions: VersionSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"VERS")?, path)?;
    let sync: SyncSegment = deserialize_segment(&bytes, find_segment(&entries, b"SYNC")?, path)?;
    let snapshots: SnapshotSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"SNAP")?, path)?;
    let block_descriptors: BlockDescriptorSegment =
        deserialize_segment(&bytes, find_segment(&entries, b"BLKD")?, path)?;
    let data = segment_bytes(&bytes, find_segment(&entries, b"DATA")?, path)?;
    let block_records = join_blocks(block_descriptors.descriptors, data)?;

    Ok(PersistedState {
        config: config.config,
        volume: volume.volume,
        active_inodes: active.inodes,
        deleted_inodes: deleted.inodes,
        block_records,
        journal_entries: journal.journal_entries,
        versions: versions.versions,
        sync_statuses: sync.sync_statuses,
        snapshots: snapshots.snapshots,
        next_snapshot_id: snapshots.next_snapshot_id,
    })
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

struct SegmentPayload {
    kind: [u8; 4],
    payload: Vec<u8>,
}

fn segment_from_bytes(kind: [u8; 4], payload: Vec<u8>) -> SegmentPayload {
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
    Ok(SegmentPayload { kind, payload })
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

fn segment_bytes<'a>(bytes: &'a [u8], entry: &SegmentEntry, path: &Path) -> CoreFsResult<&'a [u8]> {
    let start = entry.offset as usize;
    let end = start + entry.length as usize;
    bytes.get(start..end).ok_or_else(|| {
        CoreFsError::State(format!(
            "truncated CoreFS volume image segment {} in {}",
            String::from_utf8_lossy(&entry.kind),
            path.display()
        ))
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

fn split_blocks(block_records: &[BlockRecord]) -> (Vec<BlockDescriptor>, Vec<u8>) {
    let mut descriptors = Vec::with_capacity(block_records.len());
    let mut data = Vec::new();

    for record in block_records {
        let offset = data.len() as u64;
        let length = record.bytes.len() as u64;
        data.extend_from_slice(&record.bytes);
        descriptors.push(BlockDescriptor {
            inode: record.inode,
            checksum: record.checksum,
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

fn encode_superblock(superblock: &Superblock) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SUPERBLOCK_SIZE);
    bytes.extend_from_slice(&superblock.format_version.to_le_bytes());
    bytes.extend_from_slice(&superblock.alignment.to_le_bytes());
    bytes.extend_from_slice(&superblock.segment_count.to_le_bytes());
    bytes.extend_from_slice(&superblock.generation.to_le_bytes());
    bytes.extend_from_slice(&superblock.directory_offset.to_le_bytes());
    bytes.extend_from_slice(&superblock.directory_length.to_le_bytes());
    bytes.extend_from_slice(&superblock.directory_checksum.to_le_bytes());
    bytes.extend_from_slice(&superblock.payload_checksum.to_le_bytes());
    bytes
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
        generation: u64::from_le_bytes(bytes[12..20].try_into().expect("fixed slice")),
        directory_offset: u64::from_le_bytes(bytes[20..28].try_into().expect("fixed slice")),
        directory_length: u64::from_le_bytes(bytes[28..36].try_into().expect("fixed slice")),
        directory_checksum: u64::from_le_bytes(bytes[36..44].try_into().expect("fixed slice")),
        payload_checksum: u64::from_le_bytes(bytes[44..52].try_into().expect("fixed slice")),
    })
}

fn inspect_volume_image_bytes(bytes: &[u8], path: &Path) -> CoreFsResult<InspectedImage> {
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
    let entries = parse_directory(directory)?;

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
    let _ = join_blocks(block_descriptors.descriptors.clone(), data)?;

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
            if superblock.directory_checksum == expected_directory_checksum
                && superblock.payload_checksum == expected_payload_checksum
                && superblock.format_version == FORMAT_VERSION
                && superblock.alignment as usize == SEGMENT_ALIGNMENT
                && superblock.segment_count as usize == expected_segment_count
            {
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

fn validate_required_segments(entries: &[SegmentEntry]) -> CoreFsResult<()> {
    for kind in [
        *b"SUPR", *b"SUP2", *b"CNFG", *b"VOLM", *b"AINO", *b"DINO", *b"JOUR", *b"VERS", *b"SYNC",
        *b"SNAP", *b"BLKD", *b"DATA",
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
        .fold(0u64, |acc, segment| acc ^ checksum(&segment.payload))
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

fn current_generation() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PersistedState;
    use crate::config::CoreFsConfig;
    use crate::domain::snapshot::Snapshot;
    use crate::domain::volume::VolumeDescriptor;
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
            active_inodes: Vec::new(),
            deleted_inodes: Vec::new(),
            block_records: Vec::new(),
            journal_entries: Vec::new(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: vec![Snapshot {
                id: 1,
                name: "baseline".to_string(),
                scope_root: "/".to_string(),
                created_at: SystemTime::now(),
                paths: vec!["/".to_string()],
            }],
            next_snapshot_id: 1,
        }
    }

    #[test]
    fn save_and_load_volume_image_round_trip() {
        let path = temp_path("roundtrip");
        let state = sample_state();

        save_volume_image(&path, &state).expect("volume image should be written");
        let loaded = load_volume_image(&path).expect("volume image should be loaded");

        assert_eq!(loaded.config, state.config);
        assert_eq!(loaded.next_snapshot_id, 1);
        assert_eq!(loaded.snapshots.len(), 1);

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
            12
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
        assert_eq!(report.segment_count, 12);
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
        let entries = parse_directory(&bytes[HEADER_SIZE..HEADER_SIZE + (12 * SEGMENT_ENTRY_SIZE)])
            .expect("directory should parse");
        let secondary = find_segment(&entries, b"SUP2").expect("secondary superblock should exist");
        let generation_offset = secondary.offset as usize + 12;
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
}
