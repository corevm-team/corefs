// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Principal {
    User(String),
    Group(String),
    Role(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclEntry {
    pub principal: Principal,
    pub can_read: bool,
    pub can_write: bool,
    pub can_execute: bool,
}

impl AclEntry {
    pub fn full_access(principal: Principal) -> Self {
        Self {
            principal,
            can_read: true,
            can_write: true,
            can_execute: true,
        }
    }
}

#[cfg(test)]
#[path = "acl_tests.rs"]
mod tests;
