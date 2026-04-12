use crate::domain::inode::InodeId;
use crate::error::CoreFsResult;
use crate::storage::block_store::BlockStore;
use crate::storage::volume_image::{self, VolumeImageInspectionReport};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub checked_paths: usize,
    pub valid_blocks: usize,
    pub invalid_blocks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageIntegrityReport {
    pub format_version: u32,
    pub segment_count: usize,
    pub valid_superblocks: usize,
    pub selected_generation: u64,
    pub directory_checksum_valid: bool,
    pub payload_checksum_valid: bool,
    pub block_descriptors: usize,
}

#[derive(Debug, Default)]
pub struct IntegrityService;

impl IntegrityService {
    pub fn scrub(
        &self,
        inode_ids: impl Iterator<Item = InodeId>,
        block_store: &BlockStore,
    ) -> IntegrityReport {
        let mut checked_paths = 0;
        let mut valid_blocks = 0;
        let mut invalid_blocks = 0;

        for inode_id in inode_ids {
            checked_paths += 1;
            if block_store.verify(inode_id) {
                valid_blocks += 1;
            } else {
                invalid_blocks += 1;
            }
        }

        IntegrityReport {
            checked_paths,
            valid_blocks,
            invalid_blocks,
        }
    }

    pub fn fsck_image(&self, path: impl AsRef<Path>) -> CoreFsResult<ImageIntegrityReport> {
        let report = volume_image::inspect_volume_image(path)?;
        Ok(map_image_report(report))
    }
}

fn map_image_report(report: VolumeImageInspectionReport) -> ImageIntegrityReport {
    ImageIntegrityReport {
        format_version: report.format_version,
        segment_count: report.segment_count,
        valid_superblocks: report.valid_superblocks,
        selected_generation: report.selected_generation,
        directory_checksum_valid: report.directory_checksum_valid,
        payload_checksum_valid: report.payload_checksum_valid,
        block_descriptors: report.block_descriptors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PersistedState;
    use crate::config::CoreFsConfig;
    use crate::domain::snapshot::Snapshot;
    use crate::domain::volume::VolumeDescriptor;
    use crate::storage::block_store::BlockStore;
    use crate::storage::volume_image;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "corefs-integrity-{name}-{}-{}.img",
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
                created_at: SystemTime::now(),
                paths: vec!["/".to_string()],
            }],
            next_snapshot_id: 1,
        }
    }

    #[test]
    fn scrub_counts_valid_and_invalid_blocks() {
        let service = IntegrityService;
        let mut store = BlockStore::default();
        store.write(InodeId(1), b"ok".to_vec());

        let report = service.scrub([InodeId(1), InodeId(2)].into_iter(), &store);

        assert_eq!(report.checked_paths, 2);
        assert_eq!(report.valid_blocks, 1);
        assert_eq!(report.invalid_blocks, 1);
    }

    #[test]
    fn fsck_image_reports_valid_volume_image() {
        let service = IntegrityService;
        let path = temp_path("fsck-ok");
        let state = sample_state();
        volume_image::save_volume_image(&path, &state).expect("volume image should be written");

        let report = service.fsck_image(&path).expect("fsck should succeed");

        assert_eq!(report.format_version, 4);
        assert_eq!(report.segment_count, 12);
        assert_eq!(report.valid_superblocks, 2);
        assert!(report.directory_checksum_valid);
        assert!(report.payload_checksum_valid);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn fsck_image_rejects_corrupted_volume_image() {
        let service = IntegrityService;
        let path = temp_path("fsck-corrupt");
        let state = sample_state();
        volume_image::save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        bytes[0] ^= 0xFF;
        fs::write(&path, bytes).expect("corrupted image should be written");

        let report = service.fsck_image(&path);

        assert!(report.is_err());

        let _ = fs::remove_file(path);
    }
}
