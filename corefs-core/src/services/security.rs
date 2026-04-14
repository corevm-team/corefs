// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

#[derive(Debug, Default)]
pub struct SecurityService;

impl SecurityService {
    pub fn mark_encrypted(&self, enabled: bool) -> bool {
        enabled
    }

    pub fn secure_delete_bytes(&self, bytes: &mut [u8]) {
        for byte in bytes {
            *byte = 0;
        }
    }
}

#[cfg(test)]
#[path = "security_tests.rs"]
mod tests;
