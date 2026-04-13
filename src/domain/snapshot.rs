use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::SystemTime;

/// A point-in-time consistent snapshot of the filesystem.
///
/// Beyond recording which paths existed at snapshot time, the snapshot retains
/// the **uncompressed byte content** of every regular file in `file_data`.  This
/// makes snapshot restoration self-contained and independent of whether the
/// active blocks have since been overwritten or compacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: u64,
    pub name: String,
    pub scope_root: String,
    pub created_at: SystemTime,
    /// All paths (files, directories, symlinks) that existed at snapshot time.
    pub paths: Vec<String>,
    /// Captured uncompressed content for every regular file at snapshot time,
    /// keyed by absolute path.  Directories and symlinks are not included.
    pub file_data: BTreeMap<String, Vec<u8>>,
}
