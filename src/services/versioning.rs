use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub version_id: u64,
    pub path: String,
    pub created_at: SystemTime,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct VersioningService {
    next_version: u64,
    versions: BTreeMap<String, Vec<FileVersion>>,
}

impl VersioningService {
    pub fn store_version(&mut self, path: &str, bytes: Vec<u8>) -> u64 {
        self.next_version += 1;
        let version = FileVersion {
            version_id: self.next_version,
            path: path.to_string(),
            created_at: SystemTime::now(),
            bytes,
        };
        self.versions
            .entry(path.to_string())
            .or_default()
            .push(version);
        self.next_version
    }

    pub fn list_versions(&self, path: &str) -> &[FileVersion] {
        self.versions.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn prune(&mut self, path: &str, keep_latest: usize) {
        if let Some(items) = self.versions.get_mut(path) {
            if items.len() > keep_latest {
                let split_at = items.len() - keep_latest;
                items.drain(0..split_at);
            }
        }
    }

    pub fn all_versions(&self) -> Vec<FileVersion> {
        self.versions.values().flatten().cloned().collect()
    }

    pub fn from_versions(versions: Vec<FileVersion>) -> Self {
        let mut grouped = BTreeMap::<String, Vec<FileVersion>>::new();
        let mut next_version = 0;

        for version in versions {
            next_version = next_version.max(version.version_id);
            grouped
                .entry(version.path.clone())
                .or_default()
                .push(version);
        }

        Self {
            next_version,
            versions: grouped,
        }
    }
}

#[cfg(test)]
mod tests {
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
}
