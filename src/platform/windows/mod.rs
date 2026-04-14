// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::app::CoreFsService;
use crate::domain::inode::InodeKind;
use crate::error::{CoreFsError, CoreFsResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsMountReport {
    pub drive_letter: char,
    pub staging_dir: PathBuf,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsUnmountReport {
    pub drive_letter: char,
    pub image_path: PathBuf,
    pub committed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WindowsMountSession {
    drive_letter: char,
    image_path: PathBuf,
    staging_dir: PathBuf,
    writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectionEntry {
    Directory,
    File(Vec<u8>),
    Symlink(String),
}

pub fn mount_image_as_drive(
    image_path: impl AsRef<Path>,
    drive_letter: &str,
    writable: bool,
    staging_override: Option<impl AsRef<Path>>,
) -> CoreFsResult<WindowsMountReport> {
    let drive_letter = normalize_drive_letter(drive_letter)?;
    let image_path = canonical_or_original(image_path.as_ref())?;
    if !image_path.exists() {
        return Err(CoreFsError::NotFound(format!(
            "CoreFS image not found: {}",
            image_path.display()
        )));
    }

    let session_file = session_file_path(drive_letter)?;
    if session_file.exists() {
        return Err(CoreFsError::AlreadyExists(format!(
            "drive {}: is already managed by a CoreFS Windows session",
            drive_letter
        )));
    }

    let staging_dir = match staging_override {
        Some(path) => canonical_parent_and_join(path.as_ref())?,
        None => session_root()?
            .join("staging")
            .join(drive_letter.to_string()),
    };

    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|error| {
            CoreFsError::State(format!(
                "failed to remove previous staging directory {}: {error}",
                staging_dir.display()
            ))
        })?;
    }
    fs::create_dir_all(&staging_dir).map_err(|error| {
        CoreFsError::State(format!(
            "failed to create staging directory {}: {error}",
            staging_dir.display()
        ))
    })?;

    let service = CoreFsService::load_image_from_path(&image_path)?;
    project_service_to_host(&service, &staging_dir, writable)?;
    run_subst_assign(drive_letter, &staging_dir)?;

    let session = WindowsMountSession {
        drive_letter,
        image_path,
        staging_dir: staging_dir.clone(),
        writable,
    };
    persist_session(&session)?;

    Ok(WindowsMountReport {
        drive_letter,
        staging_dir,
        writable,
    })
}

pub fn unmount_image_drive(
    drive_letter: &str,
    commit_changes: bool,
) -> CoreFsResult<WindowsUnmountReport> {
    let drive_letter = normalize_drive_letter(drive_letter)?;
    let session = load_session(drive_letter)?;

    run_subst_remove(drive_letter)?;

    let committed = if session.writable && commit_changes {
        let mut service = CoreFsService::load_image_from_path(&session.image_path)?;
        sync_host_to_service(&mut service, &session.staging_dir)?;
        service.save_image_to_path(&session.image_path)?;
        true
    } else {
        false
    };

    cleanup_session_artifacts(&session)?;

    Ok(WindowsUnmountReport {
        drive_letter,
        image_path: session.image_path,
        committed,
    })
}

fn project_service_to_host(
    service: &CoreFsService,
    root: &Path,
    writable: bool,
) -> CoreFsResult<()> {
    let mut paths = service.list_paths();
    paths.sort_by_key(|path| path.matches('/').count());

    for corefs_path in paths {
        let inode = service.get_inode(&corefs_path).ok_or_else(|| {
            CoreFsError::State(format!("missing inode while projecting {corefs_path}"))
        })?;
        let host_path = host_path_from_corefs(root, &corefs_path)?;
        match inode.kind {
            InodeKind::Directory => {
                fs::create_dir_all(&host_path).map_err(|error| {
                    CoreFsError::State(format!(
                        "failed to create directory {}: {error}",
                        host_path.display()
                    ))
                })?;
            }
            InodeKind::File => {
                if let Some(parent) = host_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        CoreFsError::State(format!(
                            "failed to create parent directory {}: {error}",
                            parent.display()
                        ))
                    })?;
                }
                let bytes = service.read_file(&corefs_path)?;
                fs::write(&host_path, bytes).map_err(|error| {
                    CoreFsError::State(format!(
                        "failed to write projected file {}: {error}",
                        host_path.display()
                    ))
                })?;
                if !writable {
                    let mut permissions = fs::metadata(&host_path).map_err(io_state)?.permissions();
                    permissions.set_readonly(true);
                    fs::set_permissions(&host_path, permissions).map_err(io_state)?;
                }
            }
            InodeKind::Symlink => {
                if let Some(parent) = host_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        CoreFsError::State(format!(
                            "failed to create parent directory {}: {error}",
                            parent.display()
                        ))
                    })?;
                }
                let target =
                    String::from_utf8_lossy(&service.read_file(&corefs_path)?).into_owned();
                create_windows_symlink(service, &host_path, &target)?;
            }
        }
    }

    Ok(())
}

fn sync_host_to_service(service: &mut CoreFsService, root: &Path) -> CoreFsResult<()> {
    let discovered = scan_host_projection(root)?;
    let mut existing_paths = service.list_paths();
    existing_paths.sort_by(|a, b| b.len().cmp(&a.len()));

    for path in existing_paths {
        if !discovered.contains_key(&path) {
            service.delete_file(&path, false)?;
        }
    }

    let mut ordered: Vec<_> = discovered.into_iter().collect();
    ordered.sort_by_key(|(path, entry)| {
        let rank = match entry {
            ProjectionEntry::Directory => 0usize,
            ProjectionEntry::Symlink(_) => 1,
            ProjectionEntry::File(_) => 2,
        };
        (path.matches('/').count(), rank, path.clone())
    });

    for (path, entry) in ordered {
        apply_projection_entry(service, &path, entry)?;
    }

    Ok(())
}

fn apply_projection_entry(
    service: &mut CoreFsService,
    path: &str,
    entry: ProjectionEntry,
) -> CoreFsResult<()> {
    let existing = service.get_inode(path).cloned();
    match entry {
        ProjectionEntry::Directory => match existing {
            Some(inode) if inode.kind == InodeKind::Directory => Ok(()),
            Some(_) => {
                service.delete_file(path, false)?;
                service.create_directory(path)
            }
            None => service.create_directory(path),
        },
        ProjectionEntry::File(bytes) => match existing {
            Some(inode) if inode.kind == InodeKind::File => {
                let current = service.read_file(path)?;
                if current != bytes {
                    service.write_file(path, &bytes)?;
                }
                Ok(())
            }
            Some(_) => {
                service.delete_file(path, false)?;
                service.create_file(path, &bytes, &[])
            }
            None => service.create_file(path, &bytes, &[]),
        },
        ProjectionEntry::Symlink(target) => match existing {
            Some(inode) if inode.kind == InodeKind::Symlink => {
                let current = String::from_utf8_lossy(&service.read_file(path)?).into_owned();
                if current != target {
                    service.delete_file(path, false)?;
                    service.create_symlink(path, &target)?;
                }
                Ok(())
            }
            Some(_) => {
                service.delete_file(path, false)?;
                service.create_symlink(path, &target)
            }
            None => service.create_symlink(path, &target),
        },
    }
}

fn scan_host_projection(root: &Path) -> CoreFsResult<BTreeMap<String, ProjectionEntry>> {
    let mut entries = BTreeMap::new();
    scan_host_projection_recursive(root, root, &mut entries)?;
    Ok(entries)
}

fn scan_host_projection_recursive(
    root: &Path,
    current: &Path,
    entries: &mut BTreeMap<String, ProjectionEntry>,
) -> CoreFsResult<()> {
    for item in fs::read_dir(current).map_err(io_state)? {
        let item = item.map_err(io_state)?;
        let path = item.path();
        let metadata = fs::symlink_metadata(&path).map_err(io_state)?;
        let corefs_path = corefs_path_from_host(root, &path)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            let target = fs::read_link(&path).map_err(io_state)?;
            entries.insert(
                corefs_path,
                ProjectionEntry::Symlink(target.to_string_lossy().into_owned()),
            );
            continue;
        }

        if file_type.is_dir() {
            entries.insert(corefs_path.clone(), ProjectionEntry::Directory);
            scan_host_projection_recursive(root, &path, entries)?;
            continue;
        }

        if file_type.is_file() {
            let bytes = fs::read(&path).map_err(io_state)?;
            entries.insert(corefs_path, ProjectionEntry::File(bytes));
        }
    }

    Ok(())
}

fn create_windows_symlink(
    service: &CoreFsService,
    link_path: &Path,
    target: &str,
) -> CoreFsResult<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    let target_path = Path::new(target);
    let is_dir = if target_path.is_absolute() {
        service
            .get_inode(target)
            .map(|inode| inode.kind == InodeKind::Directory)
            .unwrap_or(false)
    } else {
        false
    };

    let result = if is_dir {
        symlink_dir(target, link_path)
    } else {
        symlink_file(target, link_path)
    };

    result.map_err(|error| {
        CoreFsError::State(format!(
            "failed to create Windows symlink {} -> {}: {}. Enable Developer Mode or run elevated.",
            link_path.display(),
            target,
            error
        ))
    })
}

fn persist_session(session: &WindowsMountSession) -> CoreFsResult<()> {
    let session_file = session_file_path(session.drive_letter)?;
    if let Some(parent) = session_file.parent() {
        fs::create_dir_all(parent).map_err(io_state)?;
    }
    let bytes = serde_json::to_vec_pretty(session).map_err(|error| {
        CoreFsError::State(format!(
            "failed to serialize Windows mount session: {error}"
        ))
    })?;
    fs::write(&session_file, bytes).map_err(io_state)
}

fn load_session(drive_letter: char) -> CoreFsResult<WindowsMountSession> {
    let session_file = session_file_path(drive_letter)?;
    let bytes = fs::read(&session_file).map_err(|error| {
        CoreFsError::NotFound(format!(
            "no CoreFS Windows session found for drive {}: {}",
            drive_letter, error
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CoreFsError::State(format!(
            "failed to parse Windows mount session {}: {error}",
            session_file.display()
        ))
    })
}

fn cleanup_session_artifacts(session: &WindowsMountSession) -> CoreFsResult<()> {
    let session_file = session_file_path(session.drive_letter)?;
    if session_file.exists() {
        fs::remove_file(&session_file).map_err(io_state)?;
    }
    if session.staging_dir.exists() {
        fs::remove_dir_all(&session.staging_dir).map_err(io_state)?;
    }
    Ok(())
}

fn session_root() -> CoreFsResult<PathBuf> {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data).join("CoreFS").join("mounts"));
    }
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(user_profile)
            .join("AppData")
            .join("Local")
            .join("CoreFS")
            .join("mounts"));
    }
    Err(CoreFsError::State(
        "unable to resolve LOCALAPPDATA for CoreFS Windows mount state".to_string(),
    ))
}

fn session_file_path(drive_letter: char) -> CoreFsResult<PathBuf> {
    Ok(session_root()?.join(format!("{drive_letter}.json")))
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

fn run_subst_assign(drive_letter: char, staging_dir: &Path) -> CoreFsResult<()> {
    let drive = format!("{drive_letter}:");
    let output = Command::new("cmd")
        .args(["/C", "subst", &drive, &staging_dir.display().to_string()])
        .output()
        .map_err(|error| {
            CoreFsError::State(format!(
                "failed to launch Windows subst for {}: {error}",
                staging_dir.display()
            ))
        })?;
    ensure_subst_success("assign", drive_letter, output)
}

fn run_subst_remove(drive_letter: char) -> CoreFsResult<()> {
    let drive = format!("{drive_letter}:");
    let output = Command::new("cmd")
        .args(["/C", "subst", &drive, "/D"])
        .output()
        .map_err(|error| {
            CoreFsError::State(format!(
                "failed to launch Windows subst removal for {drive}: {error}"
            ))
        })?;
    ensure_subst_success("remove", drive_letter, output)
}

fn ensure_subst_success(
    action: &str,
    drive_letter: char,
    output: std::process::Output,
) -> CoreFsResult<()> {
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(CoreFsError::State(format!(
        "Windows subst failed to {action} drive {drive_letter}: {}",
        if detail.is_empty() {
            "unknown error".to_string()
        } else {
            detail
        }
    )))
}

fn host_path_from_corefs(root: &Path, corefs_path: &str) -> CoreFsResult<PathBuf> {
    let relative = corefs_path.strip_prefix('/').ok_or_else(|| {
        CoreFsError::InvalidInput(format!("invalid CoreFS path for projection: {corefs_path}"))
    })?;
    let mut host = root.to_path_buf();
    if relative.is_empty() {
        return Ok(host);
    }
    for component in relative.split('/') {
        if component.is_empty() {
            continue;
        }
        host.push(component);
    }
    Ok(host)
}

fn corefs_path_from_host(root: &Path, host_path: &Path) -> CoreFsResult<String> {
    let relative = host_path.strip_prefix(root).map_err(|error| {
        CoreFsError::State(format!(
            "failed to compute CoreFS path for {}: {error}",
            host_path.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            other => {
                return Err(CoreFsError::InvalidInput(format!(
                    "unsupported host path component in {}: {:?}",
                    host_path.display(),
                    other
                )));
            }
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn canonical_or_original(path: &Path) -> CoreFsResult<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(_) => Ok(path.to_path_buf()),
    }
}

fn canonical_parent_and_join(path: &Path) -> CoreFsResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(io_state)?;
    Ok(cwd.join(path))
}

fn io_state(error: std::io::Error) -> CoreFsError {
    CoreFsError::State(error.to_string())
}
