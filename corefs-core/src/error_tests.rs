// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use alloc::string::{String, ToString};
use alloc::{vec, vec::Vec};

#[test]
fn display_returns_inner_message_for_each_error_variant() {
    let variants = [
        CoreFsError::AlreadyExists("exists".to_string()),
        CoreFsError::InvalidCommand("command".to_string()),
        CoreFsError::InvalidInput("input".to_string()),
        CoreFsError::NotFound("missing".to_string()),
        CoreFsError::PolicyViolation("policy".to_string()),
        CoreFsError::State("state".to_string()),
    ];

    let rendered: Vec<String> = variants.iter().map(ToString::to_string).collect();
    assert_eq!(
        rendered,
        vec!["exists", "command", "input", "missing", "policy", "state"]
    );
}
