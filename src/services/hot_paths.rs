use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotPathRecord {
    pub path: String,
    pub read_ops: u64,
    pub write_ops: u64,
    pub metadata_ops: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotPathEntry {
    pub path: String,
    pub score: u64,
    pub read_ops: u64,
    pub write_ops: u64,
    pub metadata_ops: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Default)]
pub struct HotPathService {
    records: BTreeMap<String, HotPathRecord>,
}

impl HotPathService {
    pub fn record_read(&mut self, path: &str, bytes: usize) {
        let record = self.entry(path);
        record.read_ops += 1;
        record.bytes_read += bytes as u64;
    }

    pub fn record_write(&mut self, path: &str, bytes: usize) {
        let record = self.entry(path);
        record.write_ops += 1;
        record.bytes_written += bytes as u64;
    }

    pub fn record_metadata(&mut self, path: &str) {
        let record = self.entry(path);
        record.metadata_ops += 1;
    }

    pub fn hottest_paths(&self, limit: usize) -> Vec<HotPathEntry> {
        let mut entries = self
            .records
            .values()
            .map(|record| HotPathEntry {
                path: record.path.clone(),
                score: score(record),
                read_ops: record.read_ops,
                write_ops: record.write_ops,
                metadata_ops: record.metadata_ops,
                bytes_read: record.bytes_read,
                bytes_written: record.bytes_written,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
        });
        entries.truncate(limit);
        entries
    }

    pub fn records(&self) -> Vec<HotPathRecord> {
        self.records.values().cloned().collect()
    }

    pub fn from_records(records: Vec<HotPathRecord>) -> Self {
        Self {
            records: records
                .into_iter()
                .map(|record| (record.path.clone(), record))
                .collect(),
        }
    }

    fn entry(&mut self, path: &str) -> &mut HotPathRecord {
        self.records
            .entry(path.to_string())
            .or_insert_with(|| HotPathRecord {
                path: path.to_string(),
                read_ops: 0,
                write_ops: 0,
                metadata_ops: 0,
                bytes_read: 0,
                bytes_written: 0,
            })
    }
}

fn score(record: &HotPathRecord) -> u64 {
    record.read_ops
        + record.write_ops.saturating_mul(2)
        + record.metadata_ops
        + (record.bytes_read / 4096)
        + (record.bytes_written / 4096).saturating_mul(2)
}

#[cfg(test)]
#[path = "hot_paths_tests.rs"]
mod tests;
