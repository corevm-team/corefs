// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::WindowsMountReport;
use crate::app::CoreFsService;
use crate::domain::inode::{Inode, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::FileImageDevice;
use crate::storage::volume_image::{self, DeviceImageCache};
use corefs_core::platform::Timestamp;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use winfsp::U16CStr;
use winfsp::filesystem::{
    DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo,
    WideNameInfo,
};
use winfsp::host::{FileSystemHost, FileSystemParams, OperationGuardStrategy, VolumeParams};
use winfsp_sys::{FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES, FspCleanupDelete};

const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x0000_0020;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;
const STATUS_DIRECTORY_NOT_EMPTY: i32 = 0xC000_0101u32 as i32;
const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2u32 as i32;
const STATUS_INVALID_PARAMETER: i32 = 0xC000_000Du32 as i32;

const VOLUME_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
const SECTOR_SIZE: u16 = 4096;
const BACKGROUND_FLUSH_INTERVAL_MS: u64 = 1_000;
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn mount_image_as_drive(
    image_path: impl AsRef<Path>,
    drive_letter: char,
    writable: bool,
) -> CoreFsResult<WindowsMountReport> {
    let image_path = image_path.as_ref().to_path_buf();
    if !image_path.exists() {
        return Err(CoreFsError::NotFound(format!(
            "CoreFS image not found: {}",
            image_path.display()
        )));
    }

    winfsp::winfsp_init().map_err(|error| {
        CoreFsError::State(format!(
            "failed to initialize WinFSP. Install WinFSP 2.x and ensure winfsp-x64.dll is available: {error}"
        ))
    })?;

    STOP_REQUESTED.store(false, Ordering::SeqCst);

    let drive = format!("{drive_letter}:");
    let state = CoreFsWinFsp::open_state(image_path.clone(), writable)?;
    let context = CoreFsWinFsp {
        state: Arc::clone(&state),
    };
    let mut params = VolumeParams::new();
    params
        .filesystem_name("CoreFS")
        .sector_size(SECTOR_SIZE)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .volume_serial_number(0xC0FE_2026)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(false)
        .read_only_volume(!writable)
        .post_cleanup_when_modified_only(true)
        .flush_and_purge_on_cleanup(true);

    let options = FileSystemParams {
        use_dir_info_by_name: false,
        volume_params: params,
        guard_strategy: OperationGuardStrategy::Fine,
        debug_mode: Default::default(),
    };

    let mut host = FileSystemHost::new_with_options(options, context).map_err(winfsp_host_state)?;
    host.mount(drive.as_str()).map_err(winfsp_host_state)?;
    if let Err(error) = install_console_stop_handler() {
        host.unmount();
        return Err(error);
    }
    if let Err(error) = host.start().map_err(winfsp_host_state) {
        uninstall_console_stop_handler();
        host.unmount();
        return Err(error);
    }

    let flush_state = Arc::clone(&state);
    let flusher = thread::spawn(move || {
        while !STOP_REQUESTED.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(BACKGROUND_FLUSH_INTERVAL_MS));
            let _ = flush_state.persist_if_dirty();
            let _ = flush_state.sync_deferred();
        }
        let _ = flush_state.persist_if_dirty();
        let _ = flush_state.sync_deferred();
    });

    while !STOP_REQUESTED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(250));
    }

    host.stop();
    let _ = state.persist_if_dirty();
    let _ = state.sync_deferred();
    let _ = flusher.join();
    host.unmount();
    uninstall_console_stop_handler();
    Ok(WindowsMountReport {
        drive_letter,
        mount_point: PathBuf::from(drive),
        writable,
    })
}

#[derive(Debug)]
struct CoreFsWinFsp {
    state: Arc<CoreFsState>,
}

#[derive(Debug)]
struct CoreFsState {
    writable: bool,
    flush_mode: FlushMode,
    service: Mutex<CoreFsService>,
    device: Mutex<FileImageDevice>,
    cache: Mutex<Option<DeviceImageCache>>,
    dirty: AtomicBool,
    deferred_sync_dirty: AtomicBool,
}

#[derive(Debug)]
struct CoreFsHandle {
    path: String,
    kind: InodeKind,
    delete_on_cleanup: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushMode {
    Strict,
    Deferred,
}

impl CoreFsWinFsp {
    fn open_state(image_path: PathBuf, writable: bool) -> CoreFsResult<Arc<CoreFsState>> {
        let service = CoreFsService::load_image_from_path(&image_path)?;
        let flush_mode = windows_flush_mode();
        let mut device = if flush_mode == FlushMode::Strict {
            FileImageDevice::open_with_write_through(&image_path, !writable, writable)?
        } else {
            FileImageDevice::open(&image_path, !writable)?
        };
        if flush_mode == FlushMode::Deferred {
            device.set_defer_data_sync(true);
        }
        let cache = if writable {
            volume_image::load_device_image_cache(&device).ok()
        } else {
            None
        };
        Ok(Arc::new(CoreFsState {
            writable,
            flush_mode,
            service: Mutex::new(service),
            device: Mutex::new(device),
            cache: Mutex::new(cache),
            dirty: AtomicBool::new(false),
            deferred_sync_dirty: AtomicBool::new(false),
        }))
    }

    fn fill_file_info(
        service: &CoreFsService,
        path: &str,
        info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if path == "/" {
            fill_root_info(service, info);
            return Ok(());
        }

        let inode = service
            .get_inode(path)
            .ok_or_else(|| nt(STATUS_OBJECT_NAME_NOT_FOUND))?;
        fill_inode_info(service, inode, info)
    }

    fn ensure_writable(&self) -> winfsp::Result<()> {
        if self.state.writable {
            Ok(())
        } else {
            Err(nt(STATUS_MEDIA_WRITE_PROTECTED))
        }
    }

    fn child_names(service: &CoreFsService, parent: &str) -> Vec<String> {
        let mut names = Vec::new();
        for path in service.list_paths() {
            if parent_path(&path) == parent {
                names.push(base_name(&path));
            }
        }
        names.sort();
        names.dedup();
        names
    }

    fn is_directory_empty(service: &CoreFsService, path: &str) -> bool {
        !service
            .list_paths()
            .into_iter()
            .any(|candidate| parent_path(&candidate) == path)
    }
}

impl CoreFsState {
    fn mark_dirty(&self) {
        if self.writable {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    fn persist_if_dirty(&self) -> winfsp::Result<()> {
        if !self.writable || !self.dirty.swap(false, Ordering::SeqCst) {
            return Ok(());
        }

        let mut service = self.service.lock().map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let mut device = self.device.lock().map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let mut cache = self.cache.lock().map_err(|_| nt(STATUS_ACCESS_DENIED))?;

        let state = service.persisted_state();
        let dirty = service.dirty_inodes_snapshot();
        let dirty_bytes = service.read_dirty_block_bytes(&dirty);
        let trace = std::env::var_os("COREFS_PERF_TRACE").is_some();
        let started = Instant::now();
        let result = volume_image::persist_to_device_incremental_partial_with_bytes_and_grow(
            &mut *device,
            &state,
            &dirty_bytes,
            || service.read_all_block_bytes(),
            &mut *cache,
            |dev, needed| {
                let sector_size = dev.sector_size() as u64;
                let target = needed.saturating_mul(5) / 4;
                let aligned = target.div_ceil(sector_size) * sector_size;
                dev.resize(aligned)
            },
        )
        .map(|report| {
            if trace {
                eprintln!(
                    "[winfsp persist] elapsed={:?} flush_mode={:?} dirty_inodes={} dirty_data_inodes={} report={:?}",
                    started.elapsed(),
                    self.flush_mode,
                    dirty.len(),
                    dirty_bytes.len(),
                    report
                );
            }
        });

        if let Err(error) = result {
            self.dirty.store(true, Ordering::SeqCst);
            return Err(corefs_to_winfsp(error));
        }

        if self.flush_mode == FlushMode::Deferred {
            self.deferred_sync_dirty.store(true, Ordering::SeqCst);
        }
        let _ = service.take_dirty_inodes();
        Ok(())
    }

    fn sync_deferred(&self) -> winfsp::Result<()> {
        if !self.writable
            || self.flush_mode != FlushMode::Deferred
            || !self.deferred_sync_dirty.swap(false, Ordering::SeqCst)
        {
            return Ok(());
        }

        let trace = std::env::var_os("COREFS_PERF_TRACE").is_some();
        let started = Instant::now();
        let mut device = self.device.lock().map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        if let Err(error) = device.force_sync() {
            self.deferred_sync_dirty.store(true, Ordering::SeqCst);
            return Err(corefs_to_winfsp(error));
        }
        if trace {
            eprintln!("[winfsp deferred-sync] elapsed={:?}", started.elapsed());
        }
        Ok(())
    }
}

impl FileSystemContext for CoreFsWinFsp {
    type FileContext = CoreFsHandle;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let path = corefs_path_from_wide(file_name)?;
        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let mut info = FileInfo::default();
        Self::fill_file_info(&service, &path, &mut info)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes: info.file_attributes,
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: FILE_ACCESS_RIGHTS,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let path = corefs_path_from_wide(file_name)?;
        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let kind = if path == "/" {
            InodeKind::Directory
        } else {
            service
                .get_inode(&path)
                .ok_or_else(|| nt(STATUS_OBJECT_NAME_NOT_FOUND))?
                .kind
        };

        Self::fill_file_info(&service, &path, file_info.as_mut())?;
        file_info.set_normalized_name(file_name.as_slice(), None);

        Ok(CoreFsHandle {
            path,
            kind,
            delete_on_cleanup: AtomicBool::new(false),
        })
    }

    fn close(&self, _context: Self::FileContext) {}

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: FILE_ACCESS_RIGHTS,
        _file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
        _security_descriptor: Option<&[c_void]>,
        allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        self.ensure_writable()?;
        let path = corefs_path_from_wide(file_name)?;
        if path == "/" {
            return Err(nt(STATUS_OBJECT_NAME_COLLISION));
        }

        let mut service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        if service.get_inode(&path).is_some() {
            return Err(nt(STATUS_OBJECT_NAME_COLLISION));
        }
        let parent = parent_path(&path);
        if parent != "/" {
            let parent_inode = service
                .get_inode(&parent)
                .ok_or_else(|| nt(STATUS_OBJECT_NAME_NOT_FOUND))?;
            if parent_inode.kind != InodeKind::Directory {
                return Err(nt(STATUS_NOT_A_DIRECTORY));
            }
        }

        let kind = if create_options & FILE_DIRECTORY_FILE != 0 {
            service.create_directory(&path).map_err(corefs_to_winfsp)?;
            InodeKind::Directory
        } else {
            let bytes = vec![0; allocation_size as usize];
            service
                .create_file(&path, &bytes, &[])
                .map_err(corefs_to_winfsp)?;
            InodeKind::File
        };
        self.state.mark_dirty();

        Self::fill_file_info(&service, &path, file_info.as_mut())?;
        file_info.set_normalized_name(file_name.as_slice(), None);
        Ok(CoreFsHandle {
            path,
            kind,
            delete_on_cleanup: AtomicBool::new(false),
        })
    }

    fn cleanup(&self, context: &Self::FileContext, file_name: Option<&U16CStr>, flags: u32) {
        if flags & FspCleanupDelete as u32 == 0 && !context.delete_on_cleanup.load(Ordering::SeqCst)
        {
            return;
        }
        if !self.state.writable || context.path == "/" {
            return;
        }

        let path = file_name
            .and_then(|name| corefs_path_from_wide(name).ok())
            .unwrap_or_else(|| context.path.clone());

        if let Ok(mut service) = self.state.service.lock() {
            let _ = service.delete_file(&path, false);
            self.state.mark_dirty();
        }
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if self.state.writable && self.state.flush_mode == FlushMode::Strict {
            self.state.persist_if_dirty()?;
        }
        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let path = context.map(|handle| handle.path.as_str()).unwrap_or("/");
        Self::fill_file_info(&service, path, file_info)
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        Self::fill_file_info(&service, &context.path, file_info)
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if context.kind != InodeKind::Directory {
            return Err(nt(STATUS_NOT_A_DIRECTORY));
        }

        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let mut entries = Vec::new();
        entries.push((".".to_string(), context.path.clone()));
        entries.push(("..".to_string(), parent_path(&context.path)));
        for name in Self::child_names(&service, &context.path) {
            let path = if context.path == "/" {
                format!("/{name}")
            } else {
                format!("{}/{}", context.path, name)
            };
            entries.push((name, path));
        }

        let marker_name = marker.inner_as_cstr().map(|value| value.to_string_lossy());
        let mut cursor = 0u32;

        for (name, path) in entries {
            if let Some(marker) = marker_name.as_deref() {
                if name.as_str() <= marker {
                    continue;
                }
            }

            let mut dir_info = DirInfo::<255>::new();
            Self::fill_file_info(&service, &path, dir_info.file_info_mut())?;
            dir_info
                .set_name(name)
                .map_err(|_| nt(STATUS_INVALID_PARAMETER))?;
            if !dir_info.append_to_buffer(buffer, &mut cursor) {
                break;
            }
        }

        DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
        Ok(cursor)
    }

    fn rename(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        self.ensure_writable()?;
        let new_path = corefs_path_from_wide(new_file_name)?;
        if new_path == "/" {
            return Err(nt(STATUS_OBJECT_NAME_COLLISION));
        }

        let mut service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        if service.get_inode(&new_path).is_some() && !replace_if_exists {
            return Err(nt(STATUS_OBJECT_NAME_COLLISION));
        }
        service
            .rename_entry(&context.path, &new_path)
            .map_err(corefs_to_winfsp)?;
        self.state.mark_dirty();
        Ok(())
    }

    fn set_basic_info(
        &self,
        context: &Self::FileContext,
        _file_attributes: u32,
        _creation_time: u64,
        _last_access_time: u64,
        _last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.ensure_writable()?;
        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        Self::fill_file_info(&service, &context.path, file_info)
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        self.ensure_writable()?;
        if delete_file && context.kind == InodeKind::Directory {
            let service = self
                .state
                .service
                .lock()
                .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
            if !Self::is_directory_empty(&service, &context.path) {
                return Err(nt(STATUS_DIRECTORY_NOT_EMPTY));
            }
        }
        context
            .delete_on_cleanup
            .store(delete_file, Ordering::SeqCst);
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        _set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.ensure_writable()?;
        if context.kind != InodeKind::File {
            return Err(nt(STATUS_FILE_IS_A_DIRECTORY));
        }

        let mut service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        service
            .truncate_file(&context.path, new_size)
            .map_err(corefs_to_winfsp)?;
        self.state.mark_dirty();
        Self::fill_file_info(&service, &context.path, file_info)
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> winfsp::Result<u32> {
        if context.kind != InodeKind::File && context.kind != InodeKind::Symlink {
            return Err(nt(STATUS_FILE_IS_A_DIRECTORY));
        }

        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let bytes = service
            .read_file_range(&context.path, offset, buffer.len())
            .map_err(corefs_to_winfsp)?;
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len() as u32)
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        _constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        self.ensure_writable()?;
        if context.kind != InodeKind::File {
            return Err(nt(STATUS_FILE_IS_A_DIRECTORY));
        }

        let mut service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let current_size = service
            .get_inode(&context.path)
            .ok_or_else(|| nt(STATUS_OBJECT_NAME_NOT_FOUND))?
            .size;
        let start = if write_to_eof {
            current_size
        } else {
            offset as usize
        };

        let was_dirty = service.has_dirty_inodes();
        if start == current_size {
            service
                .extend_file(&context.path, buffer)
                .map_err(corefs_to_winfsp)?;
        } else {
            service
                .write_file_range(&context.path, start as u64, buffer)
                .map_err(corefs_to_winfsp)?;
        }
        if service.has_dirty_inodes() || was_dirty {
            self.state.mark_dirty();
        }
        Self::fill_file_info(&service, &context.path, file_info)?;
        Ok(buffer.len() as u32)
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        let service = self
            .state
            .service
            .lock()
            .map_err(|_| nt(STATUS_ACCESS_DENIED))?;
        let used = service.logical_bytes_used() as u64;
        out_volume_info.total_size = VOLUME_SIZE_BYTES;
        out_volume_info.free_size = VOLUME_SIZE_BYTES.saturating_sub(used);
        out_volume_info.set_volume_label(service.volume_name());
        Ok(())
    }
}

fn fill_root_info(service: &CoreFsService, info: &mut FileInfo) {
    let now = filetime_now();
    info.file_attributes = FILE_ATTRIBUTE_DIRECTORY;
    info.reparse_tag = 0;
    info.allocation_size = 0;
    info.file_size = 0;
    info.creation_time = now;
    info.last_access_time = now;
    info.last_write_time = now;
    info.change_time = now;
    info.index_number = 1;
    info.hard_links = 0;
    info.ea_size = 0;
    let _ = service;
}

fn fill_inode_info(
    service: &CoreFsService,
    inode: &Inode,
    info: &mut FileInfo,
) -> winfsp::Result<()> {
    let size = match inode.kind {
        InodeKind::Directory => 0,
        InodeKind::File | InodeKind::Symlink => inode.size as u64,
    };
    info.file_attributes = match inode.kind {
        InodeKind::Directory => FILE_ATTRIBUTE_DIRECTORY,
        InodeKind::File | InodeKind::Symlink => {
            let mut attrs = FILE_ATTRIBUTE_ARCHIVE;
            if inode.metadata.mode & 0o222 == 0 {
                attrs |= FILE_ATTRIBUTE_READONLY;
            }
            if attrs == 0 {
                FILE_ATTRIBUTE_NORMAL
            } else {
                attrs
            }
        }
    };
    info.reparse_tag = 0;
    info.allocation_size = align_up(size, SECTOR_SIZE as u64);
    info.file_size = size;
    info.creation_time = timestamp_to_filetime(inode.created_at);
    info.last_access_time = timestamp_to_filetime(inode.accessed_at);
    info.last_write_time = timestamp_to_filetime(inode.modified_at);
    info.change_time = timestamp_to_filetime(inode.changed_at);
    info.index_number = inode.id.0.saturating_add(1);
    info.hard_links = 0;
    info.ea_size = 0;
    let _ = service;
    Ok(())
}

fn corefs_path_from_wide(file_name: &U16CStr) -> winfsp::Result<String> {
    let raw = file_name.to_string_lossy();
    let trimmed = raw.trim_end_matches('\0');
    if trimmed.is_empty() || trimmed == "\\" || trimmed == "/" {
        return Ok("/".to_string());
    }

    let normalized = trimmed.replace('\\', "/");
    let path = if normalized.starts_with('/') {
        normalized
    } else {
        format!("/{normalized}")
    };
    if path.split('/').any(|part| part == "..") {
        return Err(nt(STATUS_INVALID_PARAMETER));
    }
    Ok(path)
}

fn parent_path(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or_default().to_string()
}

fn timestamp_to_filetime(timestamp: Timestamp) -> u64 {
    const WINDOWS_TICK: u64 = 10_000_000;
    const SEC_TO_UNIX_EPOCH: u64 = 11_644_473_600;
    timestamp
        .as_secs()
        .saturating_add(SEC_TO_UNIX_EPOCH)
        .saturating_mul(WINDOWS_TICK)
        .saturating_add(timestamp.subsec_nanos() as u64 / 100)
}

fn filetime_now() -> u64 {
    Timestamp::now().pipe(timestamp_to_filetime)
}

fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    value.div_ceil(alignment).saturating_mul(alignment)
}

fn windows_flush_mode() -> FlushMode {
    match std::env::var("COREFS_WINDOWS_FLUSH_MODE") {
        Ok(value)
            if value.eq_ignore_ascii_case("deferred")
                || value.eq_ignore_ascii_case("relaxed")
                || value.eq_ignore_ascii_case("performance") =>
        {
            FlushMode::Deferred
        }
        _ => FlushMode::Strict,
    }
}

fn nt(status: i32) -> winfsp::FspError {
    NTSTATUS(status).into()
}

fn install_console_stop_handler() -> CoreFsResult<()> {
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), true).map_err(|error| {
            CoreFsError::State(format!(
                "failed to register Windows console stop handler for WinFSP mount: {error}"
            ))
        })
    }
}

fn uninstall_console_stop_handler() {
    let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            STOP_REQUESTED.store(true, Ordering::SeqCst);
            true.into()
        }
        _ => false.into(),
    }
}

fn corefs_to_winfsp(error: CoreFsError) -> winfsp::FspError {
    match error {
        CoreFsError::NotFound(_) => nt(STATUS_OBJECT_NAME_NOT_FOUND),
        CoreFsError::AlreadyExists(_) => nt(STATUS_OBJECT_NAME_COLLISION),
        CoreFsError::PolicyViolation(_) => nt(STATUS_ACCESS_DENIED),
        CoreFsError::InvalidInput(_) | CoreFsError::InvalidCommand(_) => {
            nt(STATUS_INVALID_PARAMETER)
        }
        CoreFsError::State(_) => nt(STATUS_ACCESS_DENIED),
    }
}

fn winfsp_host_state(error: windows::core::Error) -> CoreFsError {
    CoreFsError::State(format!("WinFSP mount failed: {error}"))
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
