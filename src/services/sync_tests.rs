use super::*;

#[test]
fn mark_synced_tracks_status_records() {
    let mut service = SyncService::default();
    service.mark_synced("/etc/corefs.conf", "node-a");

    assert_eq!(service.statuses().len(), 1);
    assert_eq!(service.statuses()[0].path, "/etc/corefs.conf");
    assert!(service.statuses()[0].synced);
    assert_eq!(service.statuses()[0].last_target.as_deref(), Some("node-a"));
}
