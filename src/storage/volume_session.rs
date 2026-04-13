use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;
use crate::storage::volume_image;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct VolumeSession {
    image_path: PathBuf,
    service: CoreFsService,
}

impl VolumeSession {
    pub fn format_new(path: impl AsRef<Path>, config: CoreFsConfig) -> CoreFsResult<Self> {
        let image_path = path.as_ref().to_path_buf();
        let service = CoreFsService::format(config);
        let session = Self {
            image_path,
            service,
        };
        session.flush()?;
        Ok(session)
    }

    pub fn open(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let image_path = path.as_ref().to_path_buf();
        if !image_path.exists() {
            return Err(CoreFsError::NotFound(format!(
                "volume image not found: {}",
                image_path.display()
            )));
        }

        let service = CoreFsService::load_image_from_path(&image_path)?;
        Ok(Self {
            image_path,
            service,
        })
    }

    pub fn open_or_format(path: impl AsRef<Path>, config: CoreFsConfig) -> CoreFsResult<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::open(path)
        } else {
            Self::format_new(path, config)
        }
    }

    pub fn image_path(&self) -> &Path {
        &self.image_path
    }

    pub fn service(&self) -> &CoreFsService {
        &self.service
    }

    pub fn service_mut(&mut self) -> &mut CoreFsService {
        &mut self.service
    }

    pub fn flush(&self) -> CoreFsResult<()> {
        let temporary = temporary_image_path(&self.image_path);
        self.service.save_image_to_path(&temporary)?;
        fs::rename(&temporary, &self.image_path).map_err(|error| {
            CoreFsError::State(format!(
                "failed to atomically replace CoreFS volume image {}: {error}",
                self.image_path.display()
            ))
        })?;
        Ok(())
    }

    pub fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut CoreFsService) -> CoreFsResult<T>,
    ) -> CoreFsResult<T> {
        let result = operation(&mut self.service)?;
        self.flush()?;
        Ok(result)
    }
}

fn temporary_image_path(path: &Path) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}.{suffix}.tmp"))
        .unwrap_or_else(|| format!("corefs.{suffix}.tmp"));

    path.with_file_name(file_name)
}

// ---------------------------------------------------------------------------
// DeviceVolumeSession — block-device-backed session
// ---------------------------------------------------------------------------

/// A volume session backed by a [`BlockDevice`] instead of a file path.
///
/// Flush writes the full volume state directly to the device via
/// sector-aligned I/O, and sync ensures durability.
#[derive(Debug)]
pub struct DeviceVolumeSession {
    device: Box<dyn BlockDevice>,
    service: CoreFsService,
}

impl DeviceVolumeSession {
    /// Formats a new CoreFS volume on the device and writes it immediately.
    pub fn format_new(
        mut device: Box<dyn BlockDevice>,
        config: CoreFsConfig,
    ) -> CoreFsResult<Self> {
        let service = CoreFsService::format(config);
        let state = service.persisted_state();
        volume_image::save_to_device(device.as_mut(), &state)?;
        Ok(Self { device, service })
    }

    /// Opens an existing CoreFS volume from a device.
    pub fn open(device: Box<dyn BlockDevice>) -> CoreFsResult<Self> {
        let state = volume_image::load_from_device(device.as_ref())?;
        let mut service = CoreFsService::from_persisted_state(state);
        if service.has_pending_wal() {
            service.recover_pending_wal()?;
        }
        Ok(Self { device, service })
    }

    /// Returns a reference to the underlying service.
    pub fn service(&self) -> &CoreFsService {
        &self.service
    }

    /// Returns a mutable reference to the underlying service.
    pub fn service_mut(&mut self) -> &mut CoreFsService {
        &mut self.service
    }

    /// Returns a reference to the underlying device.
    pub fn device(&self) -> &dyn BlockDevice {
        self.device.as_ref()
    }

    /// Writes the current volume state to the device.
    pub fn flush(&mut self) -> CoreFsResult<()> {
        let state = self.service.persisted_state();
        volume_image::save_to_device(self.device.as_mut(), &state)?;
        Ok(())
    }

    /// Executes an operation on the service, then flushes to the device.
    pub fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut CoreFsService) -> CoreFsResult<T>,
    ) -> CoreFsResult<T> {
        let result = operation(&mut self.service)?;
        self.flush()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::volume_wal::{VolumeWal, WalOperation};

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "corefs-session-{name}-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn format_mutate_and_reopen_round_trip() {
        let path = temp_path("roundtrip");
        let mut session =
            VolumeSession::format_new(&path, CoreFsConfig::default()).expect("format succeeds");
        session
            .mutate(|fs| {
                fs.create_directory("/data")?;
                fs.create_file("/data/file.txt", b"hello", &[])?;
                Ok(())
            })
            .expect("mutation succeeds");

        let reopened = VolumeSession::open(&path).expect("reopen succeeds");
        assert_eq!(
            reopened
                .service()
                .read_file("/data/file.txt")
                .expect("file exists"),
            b"hello".to_vec()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reopen_recovers_pending_wal_before_loading_service() {
        let path = temp_path("wal-recover");
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.set_pending_wal(VolumeWal {
            transaction_id: 1,
            label: "rw-writeback".to_string(),
            created_at: SystemTime::now(),
            operations: vec![
                WalOperation::CreateDirectory {
                    path: "/data".to_string(),
                    inode: crate::domain::inode::InodeId(1),
                },
                WalOperation::CreateFile {
                    path: "/data/file.txt".to_string(),
                    inode: crate::domain::inode::InodeId(2),
                },
                WalOperation::PatchExtent {
                    inode: crate::domain::inode::InodeId(2),
                    device_block: 0,
                    block_offset: 0,
                    inode_offset: 0,
                    bytes: b"hello".to_vec(),
                    final_len: 5,
                },
            ],
        });
        fs.mark_unclean_shutdown();
        fs.save_image_to_path(&path).expect("image should save");

        let reopened = VolumeSession::open(&path).expect("reopen succeeds");
        assert_eq!(
            reopened
                .service()
                .read_file("/data/file.txt")
                .expect("file exists"),
            b"hello".to_vec()
        );
        assert!(!reopened.service().has_pending_wal());

        let _ = fs::remove_file(path);
    }

    // -----------------------------------------------------------------------
    // DeviceVolumeSession tests
    // -----------------------------------------------------------------------

    use crate::storage::block_device::MemoryDevice;

    fn memory_device() -> Box<dyn crate::storage::block_device::BlockDevice> {
        // 2 MiB device with 4 KiB sectors — plenty for test volumes.
        Box::new(MemoryDevice::new(2 * 1024 * 1024, 4096).unwrap())
    }

    #[test]
    fn device_session_format_and_read_back() {
        let dev = memory_device();
        let mut session =
            DeviceVolumeSession::format_new(dev, CoreFsConfig::default()).expect("format");
        session
            .mutate(|svc| {
                svc.create_directory("/data")?;
                svc.create_file("/data/hello.txt", b"world", &[])?;
                Ok(())
            })
            .expect("mutate");

        // Re-open from the same device
        let dev2 = {
            let mem = session.device.as_ref() as *const dyn crate::storage::block_device::BlockDevice;
            // Clone the underlying MemoryDevice data for re-open.
            let mem_ref = unsafe { &*mem };
            let data = mem_ref.read_at(0, mem_ref.capacity()).unwrap();
            Box::new(MemoryDevice::from_bytes(data, 4096).unwrap())
                as Box<dyn crate::storage::block_device::BlockDevice>
        };

        let reopened = DeviceVolumeSession::open(dev2).expect("reopen");
        assert_eq!(
            reopened.service().read_file("/data/hello.txt").unwrap(),
            b"world".to_vec()
        );
    }

    #[test]
    fn device_session_format_creates_valid_volume() {
        let dev = memory_device();
        let session =
            DeviceVolumeSession::format_new(dev, CoreFsConfig::default()).expect("format");
        assert_eq!(session.service().volume_name(), "corefs");
        assert!(session.service().list_paths().is_empty()); // empty volume, no files yet
    }
}
