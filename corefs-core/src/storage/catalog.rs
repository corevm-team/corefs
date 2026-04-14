// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! In-Memory-Katalog für aktive und soft-gelöschte Inodes.
//!
//! Hält zwei `BTreeMap`s (alloc-only) — einen für aktive Pfade und
//! einen für gelöschte. Beides ist no_std-fähig.

use crate::domain::inode::{Inode, InodeId, InodeKind};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Default)]
pub struct Catalog {
    entries: BTreeMap<String, Inode>,
    deleted: BTreeMap<String, Inode>,
}

impl Catalog {
    pub fn insert(&mut self, inode: Inode) {
        self.entries.insert(inode.path.clone(), inode);
    }

    pub fn get(&self, path: &str) -> Option<&Inode> {
        self.entries.get(path)
    }

    pub fn get_mut(&mut self, path: &str) -> Option<&mut Inode> {
        self.entries.get_mut(path)
    }

    pub fn remove(&mut self, path: &str) -> Option<Inode> {
        self.entries.remove(path)
    }

    pub fn move_to_deleted(&mut self, inode: Inode) {
        self.deleted.insert(inode.path.clone(), inode);
    }

    pub fn restore_deleted(&mut self, path: &str) -> Option<Inode> {
        self.deleted.remove(path)
    }

    /// Permanently removes a soft-deleted inode **without** restoring it to the active
    /// catalog.  Used by the `expunge_file` path to complete a permanent deletion after
    /// the recovery grace period has elapsed.
    pub fn remove_from_deleted(&mut self, path: &str) -> Option<Inode> {
        self.deleted.remove(path)
    }

    pub fn deleted_contains(&self, path: &str) -> bool {
        self.deleted.contains_key(path)
    }

    pub fn list_paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn list_deleted_paths(&self) -> Vec<String> {
        self.deleted.keys().cloned().collect()
    }

    pub fn inode_by_id(&self, inode_id: InodeId) -> Option<&Inode> {
        self.entries.values().find(|inode| inode.id == inode_id)
    }

    /// Returns `(file_count, total_bytes)` for all non-directory entries without cloning.
    pub fn quota_stats(&self) -> (usize, usize) {
        let mut files = 0usize;
        let mut bytes = 0usize;
        for inode in self.entries.values() {
            if inode.kind != InodeKind::Directory {
                files += 1;
                bytes += inode.size;
            }
        }
        (files, bytes)
    }

    pub fn active_entries(&self) -> Vec<Inode> {
        self.entries.values().cloned().collect()
    }

    pub fn deleted_entries(&self) -> Vec<Inode> {
        self.deleted.values().cloned().collect()
    }

    pub fn from_parts(active: Vec<Inode>, deleted: Vec<Inode>) -> Self {
        let entries = active
            .into_iter()
            .map(|inode| (inode.path.clone(), inode))
            .collect();
        let deleted = deleted
            .into_iter()
            .map(|inode| (inode.path.clone(), inode))
            .collect();
        Self { entries, deleted }
    }

    pub fn replace_active_entries(&mut self, active: Vec<Inode>) {
        self.entries = active
            .into_iter()
            .map(|inode| (inode.path.clone(), inode))
            .collect();
    }

    pub fn replace_deleted_entries(&mut self, deleted: Vec<Inode>) {
        self.deleted = deleted
            .into_iter()
            .map(|inode| (inode.path.clone(), inode))
            .collect();
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
