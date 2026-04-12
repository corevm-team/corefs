use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub timestamp: SystemTime,
    pub operation: String,
    pub target: String,
    pub details: String,
}

#[derive(Debug, Default)]
pub struct JournalService {
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalReplayState {
    pub active_paths: BTreeSet<String>,
    pub deleted_paths: BTreeSet<String>,
    pub snapshot_count: usize,
}

impl JournalService {
    pub fn record(&mut self, operation: &str, target: &str, details: impl Into<String>) {
        self.entries.push(JournalEntry {
            timestamp: SystemTime::now(),
            operation: operation.to_string(),
            target: target.to_string(),
            details: details.into(),
        });
    }

    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self { entries }
    }

    pub fn replay(&self) -> JournalReplayState {
        let mut state = JournalReplayState::default();

        for entry in &self.entries {
            match entry.operation.as_str() {
                "create_file" | "create_directory" | "create_symlink" | "write_file"
                | "restore" => {
                    state.active_paths.insert(entry.target.clone());
                    state.deleted_paths.remove(&entry.target);
                }
                "delete" => {
                    state.active_paths.remove(&entry.target);
                    state.deleted_paths.insert(entry.target.clone());
                }
                "secure_delete" => {
                    state.active_paths.remove(&entry.target);
                    state.deleted_paths.remove(&entry.target);
                }
                "snapshot" => {
                    state.snapshot_count += 1;
                }
                "format" | "sync" => {}
                _ => {}
            }
        }

        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_appends_journal_entries() {
        let mut journal = JournalService::default();
        journal.record("create", "/tmp/file", "bytes=4");

        assert_eq!(journal.entries().len(), 1);
        assert_eq!(journal.entries()[0].operation, "create");
        assert_eq!(journal.entries()[0].target, "/tmp/file");
        assert_eq!(journal.entries()[0].details, "bytes=4");
    }

    #[test]
    fn replay_tracks_active_deleted_and_snapshots() {
        let mut journal = JournalService::default();
        journal.record("create_file", "/a", "");
        journal.record("delete", "/a", "");
        journal.record("restore", "/a", "");
        journal.record("snapshot", "/", "name=one");
        journal.record("secure_delete", "/a", "");

        let replay = journal.replay();
        assert!(!replay.active_paths.contains("/a"));
        assert!(!replay.deleted_paths.contains("/a"));
        assert_eq!(replay.snapshot_count, 1);
    }
}
