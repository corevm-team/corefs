// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

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
    PatchExtent {
        inode: InodeId,
        device_block: u64,
        block_offset: usize,
        inode_offset: usize,
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
        WalOperation::PatchExtent {
            inode,
            device_block,
            block_offset,
            inode_offset,
            bytes,
            final_len,
        } => {
            let Some(path) = service.path_for_inode(*inode) else {
                return Ok(());
            };
            let absolute_offset = service
                .data_extents_for_inode(*inode)
                .into_iter()
                .find_map(|extent| {
                    if extent.device_block == *device_block {
                        Some(extent.inode_offset.saturating_add(*block_offset))
                    } else {
                        None
                    }
                })
                .unwrap_or(*inode_offset);
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
#[path = "volume_wal_tests.rs"]
mod tests;
