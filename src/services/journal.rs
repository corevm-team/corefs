use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
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
}
