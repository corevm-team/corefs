use crate::app::CoreFsService;
use crate::error::{CoreFsError, CoreFsResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const WAL_MAGIC: &[u8; 8] = b"COREFSWL";
const WAL_VERSION: u32 = 1;

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

pub fn wal_path(image_path: impl AsRef<Path>) -> PathBuf {
    let image_path = image_path.as_ref();
    let file_name = image_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}.wal"))
        .unwrap_or_else(|| "corefs.img.wal".to_string());
    image_path.with_file_name(file_name)
}

pub fn save_wal(image_path: impl AsRef<Path>, wal: &VolumeWal) -> CoreFsResult<()> {
    let path = wal_path(image_path);
    let payload = bincode::serialize(wal)
        .map_err(|error| CoreFsError::State(format!("failed to serialize volume WAL: {error}")))?;
    let mut bytes = Vec::with_capacity(12 + payload.len());
    bytes.extend_from_slice(WAL_MAGIC);
    bytes.extend_from_slice(&WAL_VERSION.to_le_bytes());
    bytes.extend_from_slice(&payload);
    fs::write(&path, bytes).map_err(|error| {
        CoreFsError::State(format!(
            "failed to write CoreFS WAL {}: {error}",
            path.display()
        ))
    })
}

pub fn load_wal(image_path: impl AsRef<Path>) -> CoreFsResult<Option<VolumeWal>> {
    let path = wal_path(image_path);
    if !path.exists() {
        return Ok(None);
    }

    let bytes = fs::read(&path).map_err(|error| {
        CoreFsError::State(format!(
            "failed to read CoreFS WAL {}: {error}",
            path.display()
        ))
    })?;
    if bytes.len() < 12 {
        return Err(CoreFsError::State(format!(
            "truncated CoreFS WAL {}",
            path.display()
        )));
    }
    if &bytes[..8] != WAL_MAGIC {
        return Err(CoreFsError::State(format!(
            "invalid CoreFS WAL magic in {}",
            path.display()
        )));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed slice"));
    if version != WAL_VERSION {
        return Err(CoreFsError::State(format!(
            "unsupported CoreFS WAL version {} in {}",
            version,
            path.display()
        )));
    }
    let wal = bincode::deserialize(&bytes[12..]).map_err(|error| {
        CoreFsError::State(format!(
            "failed to deserialize CoreFS WAL {}: {error}",
            path.display()
        ))
    })?;
    Ok(Some(wal))
}

pub fn remove_wal(image_path: impl AsRef<Path>) -> CoreFsResult<()> {
    let path = wal_path(image_path);
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(&path).map_err(|error| {
        CoreFsError::State(format!(
            "failed to remove CoreFS WAL {}: {error}",
            path.display()
        ))
    })
}

pub fn recover_wal_into_image(image_path: impl AsRef<Path>) -> CoreFsResult<bool> {
    let image_path = image_path.as_ref();
    let Some(wal) = load_wal(image_path)? else {
        return Ok(false);
    };

    let mut service = CoreFsService::load_image_from_path(image_path)?;
    service.begin_write_transaction(&wal.label);
    for operation in &wal.operations {
        apply_operation(&mut service, operation)?;
    }
    service.commit_write_transaction();
    service.mark_clean_shutdown();
    service.save_image_to_path(image_path)?;
    remove_wal(image_path)?;
    Ok(true)
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
    use std::time::UNIX_EPOCH;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "corefs-wal-{name}-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ))
    }

    #[test]
    fn wal_round_trips_to_sidecar_file() {
        let image_path = temp_path("roundtrip");
        let wal = VolumeWal {
            transaction_id: 7,
            label: "write".to_string(),
            created_at: SystemTime::now(),
            operations: vec![WalOperation::CreateDirectory {
                path: "/data".to_string(),
            }],
        };

        save_wal(&image_path, &wal).expect("wal should save");
        let loaded = load_wal(&image_path).expect("wal should load");

        assert_eq!(loaded, Some(wal));

        remove_wal(&image_path).expect("wal should be removed");
    }

    #[test]
    fn wal_recovery_replays_pending_operations_into_image() {
        let image_path = temp_path("recover");
        let fs = CoreFsService::format(CoreFsConfig::default());
        fs.save_image_to_path(&image_path)
            .expect("image should save");

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
            ],
        };
        save_wal(&image_path, &wal).expect("wal should save");

        assert!(recover_wal_into_image(&image_path).expect("recovery should succeed"));

        let loaded = CoreFsService::load_image_from_path(&image_path).expect("image should load");
        assert_eq!(
            loaded
                .read_file("/data/hello.txt")
                .expect("file should exist"),
            b"hello".to_vec()
        );
        assert!(!wal_path(&image_path).exists());

        let _ = fs::remove_file(&image_path);
    }
}
