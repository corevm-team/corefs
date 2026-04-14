// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use corefs_core::platform::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub version_id: u64,
    pub path: String,
    pub created_at: Timestamp,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct VersioningService {
    next_version: u64,
    versions: BTreeMap<String, Vec<FileVersion>>,
}

impl VersioningService {
    pub fn store_version(&mut self, path: &str, bytes: Vec<u8>) -> u64 {
        self.next_version += 1;
        let version = FileVersion {
            version_id: self.next_version,
            path: path.to_string(),
            created_at: Timestamp::now(),
            bytes,
        };
        self.versions
            .entry(path.to_string())
            .or_default()
            .push(version);
        self.next_version
    }

    pub fn list_versions(&self, path: &str) -> &[FileVersion] {
        self.versions.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn latest_version(&self, path: &str) -> Option<&FileVersion> {
        self.versions.get(path).and_then(|items| items.last())
    }

    pub fn version_by_id(&self, path: &str, version_id: u64) -> Option<&FileVersion> {
        self.list_versions(path)
            .iter()
            .find(|version| version.version_id == version_id)
    }

    pub fn version_at_or_before(&self, path: &str, instant: Timestamp) -> Option<&FileVersion> {
        self.list_versions(path)
            .iter()
            .rev()
            .find(|version| version.created_at <= instant)
    }

    pub fn prune(&mut self, path: &str, keep_latest: usize) {
        if let Some(items) = self.versions.get_mut(path) {
            if items.len() > keep_latest {
                let split_at = items.len() - keep_latest;
                items.drain(0..split_at);
            }
        }
    }

    /// Total bytes occupied by all stored versions across all paths.
    pub fn total_bytes(&self) -> usize {
        self.versions
            .values()
            .flat_map(|items| items.iter())
            .map(|v| v.bytes.len())
            .sum()
    }

    /// Prune the oldest versions (across all paths) until the total stored
    /// version bytes no longer exceed `max_bytes`.  Paths that end up with no
    /// versions are removed from the index.
    pub fn prune_to_budget(&mut self, max_bytes: usize) {
        while self.total_bytes() > max_bytes {
            // Find the path whose oldest version is the globally earliest.
            let oldest_path = self
                .versions
                .iter()
                .filter(|(_, items)| !items.is_empty())
                .min_by_key(|(_, items)| items[0].created_at)
                .map(|(path, _)| path.clone());

            let Some(path) = oldest_path else { break };

            if let Some(items) = self.versions.get_mut(&path) {
                if !items.is_empty() {
                    items.remove(0);
                }
            }
            // Remove the path entry entirely once it has no versions left.
            if self.versions.get(&path).is_some_and(|v| v.is_empty()) {
                self.versions.remove(&path);
            }
        }
    }

    pub fn all_versions(&self) -> Vec<FileVersion> {
        self.versions.values().flatten().cloned().collect()
    }

    pub fn remap_prefix(&mut self, old_prefix: &str, new_prefix: &str) {
        let mut versions = self.all_versions();
        let prefix = format!("{old_prefix}/");

        for version in &mut versions {
            if version.path == old_prefix {
                version.path = new_prefix.to_string();
            } else if version.path.starts_with(&prefix) {
                version.path = format!("{new_prefix}/{}", &version.path[prefix.len()..]);
            }
        }

        *self = Self::from_versions(versions);
    }

    pub fn from_versions(versions: Vec<FileVersion>) -> Self {
        let mut grouped = BTreeMap::<String, Vec<FileVersion>>::new();
        let mut next_version = 0;

        for version in versions {
            next_version = next_version.max(version.version_id);
            grouped
                .entry(version.path.clone())
                .or_default()
                .push(version);
        }

        Self {
            next_version,
            versions: grouped,
        }
    }
}

#[cfg(test)]
#[path = "versioning_tests.rs"]
mod tests;
