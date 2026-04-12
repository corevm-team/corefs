use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub path: String,
    pub synced: bool,
    pub last_target: Option<String>,
}

#[derive(Debug, Default)]
pub struct SyncService {
    records: Vec<SyncStatus>,
}

impl SyncService {
    pub fn mark_synced(&mut self, path: &str, target: &str) {
        self.records.push(SyncStatus {
            path: path.to_string(),
            synced: true,
            last_target: Some(target.to_string()),
        });
    }

    pub fn statuses(&self) -> &[SyncStatus] {
        &self.records
    }

    pub fn from_statuses(records: Vec<SyncStatus>) -> Self {
        Self { records }
    }
}

#[cfg(test)]
mod tests {
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
}
