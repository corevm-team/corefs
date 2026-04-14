// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::domain::inode::Inode;

#[derive(Debug, Default)]
pub struct RecoveryService {
    tombstones: BTreeMap<String, Inode>,
}

impl RecoveryService {
    pub fn remember(&mut self, inode: Inode) {
        self.tombstones.insert(inode.path.clone(), inode);
    }

    pub fn recover(&mut self, path: &str) -> Option<Inode> {
        self.tombstones.remove(path)
    }

    pub fn recoverable_paths(&self) -> Vec<String> {
        self.tombstones.keys().cloned().collect()
    }

    pub fn forget(&mut self, path: &str) {
        self.tombstones.remove(path);
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
