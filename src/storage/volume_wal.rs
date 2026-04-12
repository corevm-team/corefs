use crate::app::CoreFsService;
use crate::error::CoreFsResult;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOperation {
    CreateFile {
        path: String,
    },
    CreateDirectory {
        path: String,
    },
    PatchFile {
        path: String,
        offset: usize,
        bytes: Vec<u8>,
        final_len: usize,
    },
    TruncateFile {
        path: String,
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
        WalOperation::CreateFile { path } => {
            if service.get_inode(path).is_none() {
                service.create_file(path, b"", &[])?;
            }
        }
        WalOperation::CreateDirectory { path } => {
            if service.get_inode(path).is_none() {
                service.create_directory(path)?;
            }
        }
        WalOperation::PatchFile {
            path,
            offset,
            bytes,
            final_len,
        } => {
            if service.get_inode(path).is_none() {
                let mut payload = Vec::new();
                payload.resize(*final_len, 0);
                let end = offset.saturating_add(bytes.len());
                if end > payload.len() {
                    payload.resize(end, 0);
                }
                payload[*offset..end].copy_from_slice(bytes);
                service.create_file(path, &payload, &[])?;
            } else {
                let mut payload = service.read_file(path)?;
                if payload.len() < *final_len {
                    payload.resize(*final_len, 0);
                }
                let end = offset.saturating_add(bytes.len());
                if end > payload.len() {
                    payload.resize(end, 0);
                }
                payload[*offset..end].copy_from_slice(bytes);
                payload.resize(*final_len, 0);
                service.write_file(path, &payload)?;
            }
        }
        WalOperation::TruncateFile { path, size } => {
            if service.get_inode(path).is_none() {
                service.create_file(path, &vec![0u8; *size], &[])?;
            } else {
                let mut payload = service.read_file(path)?;
                payload.resize(*size, 0);
                service.write_file(path, &payload)?;
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
                },
                WalOperation::PatchFile {
                    path: "/data/hello.txt".to_string(),
                    offset: 0,
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

        apply_operation(
            &mut service,
            &WalOperation::PatchFile {
                path: "/delta.txt".to_string(),
                offset: 2,
                bytes: b"XYZ".to_vec(),
                final_len: 8,
            },
        )
        .expect("patch should apply");
        apply_operation(
            &mut service,
            &WalOperation::TruncateFile {
                path: "/delta.txt".to_string(),
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
