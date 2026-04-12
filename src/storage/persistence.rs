use crate::app::PersistedState;
use crate::error::{CoreFsError, CoreFsResult};
use std::fs;
use std::path::Path;

pub fn save_state(path: impl AsRef<Path>, state: &PersistedState) -> CoreFsResult<()> {
    let path = path.as_ref();
    let payload = serde_json::to_vec_pretty(state).map_err(|error| {
        CoreFsError::State(format!(
            "failed to serialize CoreFS state for {}: {error}",
            path.display()
        ))
    })?;

    fs::write(path, payload).map_err(|error| {
        CoreFsError::State(format!(
            "failed to write CoreFS state to {}: {error}",
            path.display()
        ))
    })?;
    Ok(())
}

pub fn load_state(path: impl AsRef<Path>) -> CoreFsResult<PersistedState> {
    let path = path.as_ref();
    let payload = fs::read(path).map_err(|error| {
        CoreFsError::State(format!(
            "failed to read CoreFS state from {}: {error}",
            path.display()
        ))
    })?;

    serde_json::from_slice(&payload).map_err(|error| {
        CoreFsError::State(format!(
            "failed to deserialize CoreFS state from {}: {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PersistedState;
    use crate::config::CoreFsConfig;
    use crate::domain::snapshot::Snapshot;
    use crate::domain::volume::VolumeDescriptor;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn save_and_load_state_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "corefs-persistence-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));

        let state = PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config(&CoreFsConfig::default()),
            active_inodes: Vec::new(),
            deleted_inodes: Vec::new(),
            block_records: Vec::new(),
            journal_entries: Vec::new(),
            versions: Vec::new(),
            sync_statuses: Vec::new(),
            snapshots: vec![Snapshot {
                id: 1,
                name: "baseline".to_string(),
                scope_root: "/".to_string(),
                created_at: SystemTime::now(),
                paths: vec!["/".to_string()],
            }],
            next_snapshot_id: 1,
        };

        save_state(&path, &state).expect("state should be saved");
        let loaded = load_state(&path).expect("state should be loaded");
        assert_eq!(loaded.config, state.config);
        assert_eq!(loaded.next_snapshot_id, 1);
        assert_eq!(loaded.snapshots.len(), 1);

        let _ = fs::remove_file(path);
    }
}
