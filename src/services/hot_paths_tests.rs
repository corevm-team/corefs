// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn hot_paths_rank_busy_paths_first() {
    let mut service = HotPathService::default();
    service.record_read("/a", 8192);
    service.record_write("/b", 4096);
    service.record_write("/b", 4096);

    let hot = service.hottest_paths(2);
    assert_eq!(hot[0].path, "/b");
    assert_eq!(hot[1].path, "/a");
}
