use crate::domain::inode::InodeId;
use crate::storage::block_store::BlockStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub checked_paths: usize,
    pub valid_blocks: usize,
    pub invalid_blocks: usize,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::block_store::BlockStore;

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
}
