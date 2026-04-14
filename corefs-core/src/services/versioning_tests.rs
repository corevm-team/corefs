// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::platform::Timestamp;

#[test]
fn store_and_list_versions_preserve_payloads() {
    let mut service = VersioningService::default();
    let first = service.store_version_at("/a.txt", b"one".to_vec(), Timestamp::EPOCH);
    let second = service.store_version_at("/a.txt", b"two".to_vec(), Timestamp::EPOCH);

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(service.list_versions("/a.txt").len(), 2);
    assert_eq!(service.list_versions("/a.txt")[0].bytes, b"one".to_vec());
    assert!(service.list_versions("/missing").is_empty());
}

#[test]
fn prune_keeps_latest_versions_only() {
    let mut service = VersioningService::default();
    service.store_version_at("/a.txt", b"one".to_vec(), Timestamp::EPOCH);
    service.store_version_at("/a.txt", b"two".to_vec(), Timestamp::EPOCH);
    service.store_version_at("/a.txt", b"three".to_vec(), Timestamp::EPOCH);
    service.prune("/a.txt", 2);

    let versions = service.list_versions("/a.txt");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].bytes, b"two".to_vec());
    assert_eq!(versions[1].bytes, b"three".to_vec());

    service.prune("/missing", 2);
    assert!(service.list_versions("/missing").is_empty());
}

#[test]
fn version_queries_resolve_latest_id_and_timestamp() {
    let mut service = VersioningService::default();
    let t1 = Timestamp::from_secs_nanos(1_000, 0);
    let t2 = Timestamp::from_secs_nanos(1_000, 2_000_000);
    service.store_version_at("/a.txt", b"one".to_vec(), t1);
    let first = service
        .latest_version("/a.txt")
        .expect("first version")
        .clone();
    service.store_version_at("/a.txt", b"two".to_vec(), t2);

    let latest = service.latest_version("/a.txt").expect("latest version");
    assert_eq!(latest.bytes, b"two".to_vec());
    assert_eq!(
        service
            .version_by_id("/a.txt", first.version_id)
            .expect("version by id")
            .bytes,
        b"one".to_vec()
    );
    assert_eq!(
        service
            .version_at_or_before("/a.txt", first.created_at)
            .expect("version by time")
            .bytes,
        b"one".to_vec()
    );
}
