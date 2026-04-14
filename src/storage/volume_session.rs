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
#[path = "volume_session_tests.rs"]
mod tests;
