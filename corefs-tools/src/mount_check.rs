// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Helpers for detecting whether a CoreFS device is currently mounted.
//!
//! Online operations (scrub / snapshot / defrag against a live filesystem)
//! require cooperation with the mounted driver — on Linux via an ioctl, on
//! AnyOS via a FUSE control channel. Neither is plumbed yet, so the tools
//! should refuse to mutate a mounted device and let the caller surface a
//! clear "not yet supported" error.
//!
//! This module provides a minimal best-effort check that the tool frontends
//! can call before taking destructive action.

// `corefs-tools` is std-only (see crate-level docs).

/// Result of a mount-status probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountStatus {
    /// Device is definitely not in the mount table.
    NotMounted,
    /// Device appears in the mount table at the given mount paths.
    Mounted {
        /// Mount paths that reference this device (may be more than one).
        mount_paths: Vec<String>,
    },
    /// The host doesn't expose a mount table we can read (e.g. AnyOS
    /// without mount-registry access). Callers should treat this as
    /// "unknown — proceed only if the caller is willing to accept the
    /// risk of double-mutation".
    Unknown,
}

/// Probe whether `device_path` is currently mounted.
///
/// On Linux-like targets we scan `/proc/mounts` for a matching first
/// column; on no_std targets we cannot inspect the kernel mount table
/// and return [`MountStatus::Unknown`].
///
/// The match is intentionally coarse: a literal string compare on the
/// device path. Symlink-resolved and LVM-mapped aliases are **not**
/// normalized here; the frontends should canonicalize paths beforehand
/// if they need tighter guarantees.
pub fn probe_device(device_path: &str) -> MountStatus {
    use std::fs;
    let contents = match fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(_) => return MountStatus::Unknown,
    };
    let mut mount_paths = Vec::new();
    for line in contents.lines() {
        let mut cols = line.split_ascii_whitespace();
        let dev = match cols.next() {
            Some(d) => d,
            None => continue,
        };
        if dev == device_path {
            if let Some(mp) = cols.next() {
                mount_paths.push(mp.to_string());
            }
        }
    }
    if mount_paths.is_empty() {
        MountStatus::NotMounted
    } else {
        MountStatus::Mounted { mount_paths }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_mounted_variant_has_no_paths() {
        // Probe an implausible device path — should be NotMounted on Linux
        // CI and Unknown elsewhere; in either case MountStatus::Mounted is
        // not the answer.
        let status = probe_device("/dev/does-not-exist-12345");
        assert!(!matches!(status, MountStatus::Mounted { .. }));
    }
}
