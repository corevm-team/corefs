// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn mark_encrypted_returns_requested_state() {
    let service = SecurityService;
    assert!(service.mark_encrypted(true));
    assert!(!service.mark_encrypted(false));
}

#[test]
fn secure_delete_zeroes_all_bytes() {
    let service = SecurityService;
    let mut bytes = vec![1, 2, 3, 4];
    service.secure_delete_bytes(&mut bytes);
    assert_eq!(bytes, vec![0, 0, 0, 0]);
}
