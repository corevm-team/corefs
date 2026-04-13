use crate::app::CoreFsService;
use crate::domain::inode::InodeId;
use crate::error::CoreFsResult;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOperation {
    CreateFile {
        path: String,
        inode: InodeId,
    },
    CreateDirectory {
        path: String,
        inode: InodeId,
    },
    PatchBlock {
        inode: InodeId,
        block_index: usize,
        block_offset: usize,
        bytes: Vec<u8>,
        final_len: usize,
    },
    TruncateInode {
        inode: InodeId,
        size: usize,
    },
    DeletePath {
        path: String,
    },
    RenamePath {
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeWal {
    pub transaction_id: u64,
    pub label: String,
    pub created_at: SystemTime,
    pub operations: Vec<WalOperation>,
}

impl VolumeWal {
    pub fn new(transaction_id: u64, label: impl Into<String>) -> Self {
        Self {
            transaction_id,
            label: label.into(),
            created_at: SystemTime::now(),
            operations: Vec::new(),
        }
    }

    pub fn push(&mut self, operation: WalOperation) {
        self.operations.push(operation);
    }
}

pub fn apply_operation(service: &mut CoreFsService, operation: &WalOperation) -> CoreFsResult<()> {
    match operation {
        WalOperation::CreateFile { path, inode } => {
            if service.get_inode(&path).is_none() {
                service.create_file_with_inode(path, b"", &[], *inode)?;
            }
        }
        WalOperation::CreateDirectory { path, inode } => {
            if service.get_inode(&path).is_none() {
                service.create_directory_with_inode(path, *inode)?;
            }
        }
        WalOperation::PatchBlock {
            inode,
            block_index,
            block_offset,
            bytes,
            final_len,
        } => {
            let Some(path) = service.path_for_inode(*inode) else {
                return Ok(());
            };
            let absolute_offset = block_index
                .saturating_mul(service.block_size())
                .saturating_add(*block_offset);
            if service.get_inode(&path).is_none() {
                let mut payload = Vec::new();
                payload.resize(*final_len, 0);
                let end = absolute_offset.saturating_add(bytes.len());
                if end > payload.len() {
                    payload.resize(end, 0);
                }
                payload[absolute_offset..end].copy_from_slice(bytes);
                service.create_file_with_inode(&path, &payload, &[], *inode)?;
            } else {
                let mut payload = service.read_file(&path)?;
                if payload.len() < *final_len {
                    payload.resize(*final_len, 0);
                }
                let end = absolute_offset.saturating_add(bytes.len());
                if end > payload.len() {
                    payload.resize(end, 0);
                }
                payload[absolute_offset..end].copy_from_slice(bytes);
                payload.resize(*final_len, 0);
                service.write_file(&path, &payload)?;
            }
        }
        WalOperation::TruncateInode { inode, size } => {
            let Some(path) = service.path_for_inode(*inode) else {
                return Ok(());
            };
            if service.get_inode(&path).is_none() {
                service.create_file_with_inode(&path, &vec![0u8; *size], &[], *inode)?;
            } else {
                let mut payload = service.read_file(&path)?;
                payload.resize(*size, 0);
                service.write_file(&path, &payload)?;
            }
        }
        WalOperation::DeletePath { path } => {
            if service.get_inode(path).is_some() {
                service.delete_file(path, false)?;
            }
        }
        WalOperation::RenamePath { from, to } => {
            if service.get_inode(from).is_some() {
                service.rename_entry(from, to)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CoreFsConfig;

    #[test]
    fn wal_operations_apply_to_service() {
        let mut service = CoreFsService::format(CoreFsConfig::default());
        let wal = VolumeWal {
            transaction_id: 1,
            label: "rw-writeback".to_string(),
            created_at: SystemTime::now(),
            operations: vec![
                WalOperation::CreateDirectory {
                    path: "/data".to_string(),
                    inode: InodeId(1),
                },
                WalOperation::CreateFile {
                    path: "/data/hello.txt".to_string(),
                    inode: InodeId(2),
                },
                WalOperation::PatchBlock {
                    inode: InodeId(2),
                    block_index: 0,
                    block_offset: 0,
                    bytes: b"hello".to_vec(),
                    final_len: 5,
                },
                WalOperation::RenamePath {
                    from: "/data/hello.txt".to_string(),
                    to: "/data/world.txt".to_string(),
                },
            ],
        };

        for operation in &wal.operations {
            apply_operation(&mut service, operation).expect("operation should apply");
        }

        assert_eq!(
            service
                .read_file("/data/world.txt")
                .expect("file should exist"),
            b"hello".to_vec()
        );
    }

    #[test]
    fn wal_patch_and_truncate_apply_as_deltas() {
        let mut service = CoreFsService::format(CoreFsConfig::default());
        service
            .create_file("/delta.txt", b"abcdefgh", &[])
            .expect("file");
        let inode = service.inode_for_path("/delta.txt").expect("inode");

        apply_operation(
            &mut service,
            &WalOperation::PatchBlock {
                inode,
                block_index: 0,
                block_offset: 2,
                bytes: b"XYZ".to_vec(),
                final_len: 8,
            },
        )
        .expect("patch should apply");
        apply_operation(
            &mut service,
            &WalOperation::TruncateInode {
                inode,
                size: 6,
            },
        )
        .expect("truncate should apply");

        assert_eq!(
            service.read_file("/delta.txt").expect("file should exist"),
            b"abXYZf".to_vec()
        );
    }
}
