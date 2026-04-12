use crate::domain::inode::InodeId;
use crate::error::CoreFsResult;
use crate::storage::block_store::BlockStore;
use crate::storage::volume_image::{self, VolumeImageInspectionReport, VolumeImageRepairReport};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRepairReport {
    pub repaired_superblocks: usize,
    pub selected_generation: u64,
    pub resulting_valid_superblocks: usize,
    pub recovered_without_valid_superblock: bool,
    pub reconstructed_segment_directory: bool,
    pub reconstructed_block_descriptors: bool,
    pub moved_to_deleted: usize,
    pub restored_to_active: usize,
    pub purged_deleted: usize,
    pub removed_orphan_blocks: usize,
    pub resized_inodes: usize,
    pub snapshot_id_adjusted: bool,
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

    pub fn repair_image(&self, path: impl AsRef<Path>) -> CoreFsResult<ImageRepairReport> {
        let report = volume_image::repair_volume_image_superblocks(path)?;
        Ok(map_repair_report(report))
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

fn map_repair_report(report: VolumeImageRepairReport) -> ImageRepairReport {
    ImageRepairReport {
        repaired_superblocks: report.repaired_superblocks,
        selected_generation: report.selected_generation,
        resulting_valid_superblocks: report.resulting_valid_superblocks,
        recovered_without_valid_superblock: report.recovered_without_valid_superblock,
        reconstructed_segment_directory: report.reconstructed_segment_directory,
        reconstructed_block_descriptors: report.reconstructed_block_descriptors,
        moved_to_deleted: report.journal_repair.moved_to_deleted,
        restored_to_active: report.journal_repair.restored_to_active,
        purged_deleted: report.journal_repair.purged_deleted,
        removed_orphan_blocks: report.journal_repair.removed_orphan_blocks,
        resized_inodes: report.journal_repair.resized_inodes,
        snapshot_id_adjusted: report.journal_repair.snapshot_id_adjusted,
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
            hot_path_records: Vec::new(),
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
        assert_eq!(report.segment_count, 13);
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

    #[test]
    fn repair_image_restores_missing_superblock_copy() {
        let service = IntegrityService;
        let path = temp_path("repair-superblock");
        let state = sample_state();
        volume_image::save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
        bytes[primary_offset] ^= 0xFF;
        fs::write(&path, bytes).expect("corrupted image should be written");

        let repaired = service
            .repair_image(&path)
            .expect("repair should succeed from secondary copy");

        assert_eq!(repaired.repaired_superblocks, 1);
        assert_eq!(repaired.resulting_valid_superblocks, 2);
        assert!(!repaired.recovered_without_valid_superblock);
        assert!(!repaired.reconstructed_segment_directory);
        assert!(!repaired.reconstructed_block_descriptors);
        assert_eq!(repaired.removed_orphan_blocks, 0);

        let report = service
            .fsck_image(&path)
            .expect("image should be healthy again");
        assert_eq!(report.valid_superblocks, 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_image_can_recover_without_any_valid_superblock() {
        let service = IntegrityService;
        let path = temp_path("repair-no-superblocks");
        let state = sample_state();
        volume_image::save_volume_image(&path, &state).expect("volume image should be written");
        let mut bytes = fs::read(&path).expect("image should exist");
        let primary_offset = u64::from_le_bytes(bytes[24..32].try_into().expect("fixed")) as usize;
        let secondary_offset =
            u64::from_le_bytes(bytes[48..56].try_into().expect("fixed")) as usize;
        bytes[primary_offset] ^= 0xFF;
        bytes[secondary_offset] ^= 0xFF;
        fs::write(&path, bytes).expect("corrupted image should be written");

        let repaired = service
            .repair_image(&path)
            .expect("repair should succeed from header directory");

        assert!(repaired.recovered_without_valid_superblock);
        assert_eq!(repaired.resulting_valid_superblocks, 2);

        let report = service
            .fsck_image(&path)
            .expect("repaired image should be healthy");
        assert_eq!(report.valid_superblocks, 2);

        let _ = fs::remove_file(path);
    }
}
