use crate::app::CoreFsService;
use crate::error::CoreFsResult;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WalOperation {
    CreateFile { path: String },
    CreateDirectory { path: String },
    WriteFile { path: String, bytes: Vec<u8> },
    DeletePath { path: String },
    RenamePath { from: String, to: String },
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
        WalOperation::WriteFile { path, bytes } => {
            if service.get_inode(path).is_none() {
                service.create_file(path, bytes, &[])?;
            } else {
                service.write_file(path, bytes)?;
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
                WalOperation::WriteFile {
                    path: "/data/hello.txt".to_string(),
                    bytes: b"hello".to_vec(),
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
}
