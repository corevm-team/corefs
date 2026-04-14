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
#[path = "sync_tests.rs"]
mod tests;
