use crate::config::QuotaPolicy;
use crate::domain::inode::{Inode, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaReport {
    pub used_files: usize,
    pub used_bytes: usize,
    pub max_files: Option<usize>,
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Default)]
pub struct QuotaService;

impl QuotaService {
    pub fn report(&self, policy: &QuotaPolicy, active_inodes: &[Inode]) -> QuotaReport {
        let used_files = active_inodes
            .iter()
            .filter(|inode| inode.kind != InodeKind::Directory)
            .count();
        let used_bytes = active_inodes
            .iter()
            .filter(|inode| inode.kind != InodeKind::Directory)
            .map(|inode| inode.size)
            .sum();

        QuotaReport {
            used_files,
            used_bytes,
            max_files: policy.max_files,
            max_bytes: policy.max_bytes,
        }
    }

    /// Enforce quota using pre-computed stats (from `Catalog::quota_stats`).
    /// Avoids cloning all inodes; preferred over `enforce_delta` for hot paths.
    pub fn check_stats(
        &self,
        policy: &QuotaPolicy,
        current_files: usize,
        current_bytes: usize,
        file_delta: isize,
        byte_delta: isize,
    ) -> CoreFsResult<()> {
        let projected_files = apply_delta(current_files, file_delta)?;
        let projected_bytes = apply_delta(current_bytes, byte_delta)?;

        if let Some(limit) = policy.max_files
            && projected_files > limit
        {
            return Err(CoreFsError::PolicyViolation(format!(
                "quota exceeded: projected files {projected_files} > limit {limit}"
            )));
        }

        if let Some(limit) = policy.max_bytes
            && projected_bytes > limit
        {
            return Err(CoreFsError::PolicyViolation(format!(
                "quota exceeded: projected bytes {projected_bytes} > limit {limit}"
            )));
        }

        Ok(())
    }

    pub fn enforce_delta(
        &self,
        policy: &QuotaPolicy,
        active_inodes: &[Inode],
        file_delta: isize,
        byte_delta: isize,
    ) -> CoreFsResult<()> {
        let report = self.report(policy, active_inodes);
        let projected_files = apply_delta(report.used_files, file_delta)?;
        let projected_bytes = apply_delta(report.used_bytes, byte_delta)?;

        if let Some(limit) = report.max_files
            && projected_files > limit
        {
            return Err(CoreFsError::PolicyViolation(format!(
                "quota exceeded: projected files {projected_files} > limit {limit}"
            )));
        }

        if let Some(limit) = report.max_bytes
            && projected_bytes > limit
        {
            return Err(CoreFsError::PolicyViolation(format!(
                "quota exceeded: projected bytes {projected_bytes} > limit {limit}"
            )));
        }

        Ok(())
    }
}

fn apply_delta(current: usize, delta: isize) -> CoreFsResult<usize> {
    if delta.is_negative() {
        current
            .checked_sub(delta.unsigned_abs())
            .ok_or_else(|| CoreFsError::State("quota underflow while applying delta".to_string()))
    } else {
        current
            .checked_add(delta as usize)
            .ok_or_else(|| CoreFsError::State("quota overflow while applying delta".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageTier;
    use crate::domain::metadata::{ContentClass, FileMetadata};
    use std::time::SystemTime;

    fn inode(id: u64, kind: InodeKind, path: &str, size: usize) -> Inode {
        Inode {
            id: crate::domain::inode::InodeId(id),
            kind,
            path: path.to_string(),
            size,
            created_at: SystemTime::now(),
            modified_at: SystemTime::now(),
            metadata: FileMetadata {
                tags: Vec::new(),
                attributes: Vec::new(),
                content_class: ContentClass::Unknown,
                storage_tier: StorageTier::Warm,
                acl: Vec::new(),
                encrypted: false,
                compressed: false,
            },
        }
    }

    #[test]
    fn quota_report_counts_files_and_bytes() {
        let service = QuotaService;
        let report = service.report(
            &QuotaPolicy {
                max_files: Some(4),
                max_bytes: Some(1024),
            },
            &[
                inode(1, InodeKind::Directory, "/data", 0),
                inode(2, InodeKind::File, "/data/a", 100),
                inode(3, InodeKind::Symlink, "/data/b", 8),
            ],
        );

        assert_eq!(report.used_files, 2);
        assert_eq!(report.used_bytes, 108);
    }

    #[test]
    fn quota_enforcement_rejects_excess() {
        let service = QuotaService;
        let result = service.enforce_delta(
            &QuotaPolicy {
                max_files: Some(1),
                max_bytes: Some(16),
            },
            &[inode(1, InodeKind::File, "/a", 10)],
            1,
            8,
        );

        assert!(matches!(result, Err(CoreFsError::PolicyViolation(_))));
    }
}
