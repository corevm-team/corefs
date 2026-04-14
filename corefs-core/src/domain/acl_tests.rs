// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn full_access_grants_all_permissions() {
    let entry = AclEntry::full_access(Principal::User("alice".to_string()));

    assert!(entry.can_read);
    assert!(entry.can_write);
    assert!(entry.can_execute);
    assert_eq!(entry.principal, Principal::User("alice".to_string()));
}
