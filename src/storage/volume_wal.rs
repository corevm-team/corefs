// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Volume-WAL — Service-bound Replay-Pfad.
//!
//! Die Datentypen [`VolumeWal`] und [`WalOperation`] leben jetzt in
//! [`corefs_core::storage::volume_wal`] (no_std + alloc) und werden hier
//! transparent re-exportiert. In dieser Datei verbleibt nur die
//! [`apply_operation`]-Funktion, die einen WAL-Eintrag gegen einen laufenden
//! [`CoreFsService`] zurückspielt — sie ist std-/Service-gebunden und
//! gehört nicht in den plattformneutralen Kern.

pub use corefs_core::storage::volume_wal::{VolumeWal, WalOperation};

use crate::app::CoreFsService;
use crate::error::CoreFsResult;

/// Spielt eine einzelne [`WalOperation`] gegen den laufenden Service zurück.
pub fn apply_operation(service: &mut CoreFsService, operation: &WalOperation) -> CoreFsResult<()> {
    match operation {
        WalOperation::CreateFile { path, inode } => {
            if service.get_inode(path).is_none() {
                service.create_file_with_inode(path, b"", &[], *inode)?;
            }
        }
        WalOperation::CreateDirectory { path, inode } => {
            if service.get_inode(path).is_none() {
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
