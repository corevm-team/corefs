// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::error::{CoreFsError, CoreFsResult};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "windows-winfsp")]
mod winfsp_backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsMountReport {
    pub drive_letter: char,
    pub mount_point: PathBuf,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsUnmountReport {
    pub drive_letter: char,
    pub image_path: PathBuf,
    pub committed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsSyncReport {
    pub drive_letter: char,
    pub image_path: PathBuf,
    pub synced: bool,
}

pub fn mount_image_as_drive(
    image_path: impl AsRef<Path>,
    drive_letter: &str,
    writable: bool,
    staging_override: Option<impl AsRef<Path>>,
) -> CoreFsResult<WindowsMountReport> {
    if staging_override.is_some() {
        return Err(CoreFsError::InvalidCommand(
            "`--staging` is not supported by the native WinFSP Windows mount".to_string(),
        ));
    }

    let drive_letter = normalize_drive_letter(drive_letter)?;
    let image_path = canonical_or_original(image_path.as_ref())?;
    if !image_path.exists() {
        return Err(CoreFsError::NotFound(format!(
            "CoreFS image not found: {}",
            image_path.display()
        )));
    }

    mount_image_as_drive_native(image_path, drive_letter, writable)
}

#[cfg(feature = "windows-winfsp")]
fn mount_image_as_drive_native(
    image_path: PathBuf,
    drive_letter: char,
    writable: bool,
) -> CoreFsResult<WindowsMountReport> {
    winfsp_backend::mount_image_as_drive(image_path, drive_letter, writable)
}

#[cfg(not(feature = "windows-winfsp"))]
fn mount_image_as_drive_native(
    _image_path: PathBuf,
    drive_letter: char,
    _writable: bool,
) -> CoreFsResult<WindowsMountReport> {
    Err(CoreFsError::InvalidCommand(format!(
        "native Windows mounting requires WinFSP support. Build with `cargo build --features windows-winfsp` and install WinFSP 2.x before mounting drive {drive_letter}:"
    )))
}

pub fn unmount_image_drive(
    drive_letter: &str,
    commit_changes: bool,
) -> CoreFsResult<WindowsUnmountReport> {
    let _ = commit_changes;
    let drive_letter = normalize_drive_letter(drive_letter)?;
    Err(CoreFsError::InvalidCommand(format!(
        "native WinFSP mounts are owned by the foreground `mount-image` process; stop that process to unmount drive {drive_letter}:"
    )))
}

pub fn sync_image_drive(drive_letter: &str) -> CoreFsResult<WindowsSyncReport> {
    let drive_letter = normalize_drive_letter(drive_letter)?;
    Err(CoreFsError::InvalidCommand(format!(
        "native WinFSP mounts flush through the filesystem driver; explicit `sync-image-win` is not used for drive {drive_letter}:"
    )))
}

fn normalize_drive_letter(input: &str) -> CoreFsResult<char> {
    let trimmed = input.trim().trim_end_matches('\\').trim_end_matches('/');
    let without_colon = trimmed.trim_end_matches(':');
    let mut chars = without_colon.chars();
    let Some(letter) = chars.next() else {
        return Err(CoreFsError::InvalidInput(
            "missing Windows drive letter".to_string(),
        ));
    };
    if chars.next().is_some() || !letter.is_ascii_alphabetic() {
        return Err(CoreFsError::InvalidInput(format!(
            "invalid Windows drive designator: {input}"
        )));
    }
    Ok(letter.to_ascii_uppercase())
}

fn canonical_or_original(path: &Path) -> CoreFsResult<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(_) => Ok(path.to_path_buf()),
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
