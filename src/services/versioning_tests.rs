use super::*;

#[test]
fn store_and_list_versions_preserve_payloads() {
    let mut service = VersioningService::default();
    let first = service.store_version("/a.txt", b"one".to_vec());
    let second = service.store_version("/a.txt", b"two".to_vec());

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(service.list_versions("/a.txt").len(), 2);
    assert_eq!(service.list_versions("/a.txt")[0].bytes, b"one".to_vec());
    assert!(service.list_versions("/missing").is_empty());
}

#[test]
fn prune_keeps_latest_versions_only() {
    let mut service = VersioningService::default();
    service.store_version("/a.txt", b"one".to_vec());
    service.store_version("/a.txt", b"two".to_vec());
    service.store_version("/a.txt", b"three".to_vec());
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
    service.store_version("/a.txt", b"one".to_vec());
    let first = service
        .latest_version("/a.txt")
        .expect("first version")
        .clone();
    std::thread::sleep(std::time::Duration::from_millis(2));
    service.store_version("/a.txt", b"two".to_vec());

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
