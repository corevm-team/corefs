// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::config::StorageTier;
use crate::domain::acl::AclEntry;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentClass {
    Text,
    Binary,
    Image,
    SourceCode,
    Archive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadata {
    pub tags: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub content_class: ContentClass,
    pub storage_tier: StorageTier,
    pub acl: Vec<AclEntry>,
    pub encrypted: bool,
    pub compressed: bool,
    /// POSIX owner user ID.
    pub uid: u32,
    /// POSIX owner group ID.
    pub gid: u32,
    /// POSIX mode bits (permissions only — type is encoded in `InodeKind`).
    /// Typical values: files `0o644`, directories `0o755`, symlinks `0o777`.
    pub mode: u32,
}

impl Default for FileMetadata {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            attributes: Vec::new(),
            content_class: ContentClass::Unknown,
            storage_tier: StorageTier::Warm,
            acl: Vec::new(),
            encrypted: false,
            compressed: false,
            uid: 0,
            gid: 0,
            mode: 0o644,
        }
    }
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
