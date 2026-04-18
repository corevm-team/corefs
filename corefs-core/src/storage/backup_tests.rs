// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Tests für das Stream-Backup-Format.

use super::*;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::snapshot::{Snapshot, SnapshotInode};
use crate::domain::volume::VolumeDescriptor;
use crate::platform::Timestamp;
use crate::services::versioning::FileVersion;
use crate::storage::persisted_state::PersistedState;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

fn t(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs)
}

fn mk_state() -> PersistedState {
    let cfg = CoreFsConfig::default();
    let vol = VolumeDescriptor::from_config_at(&cfg, t(1));
    let mut s = PersistedState {
        config: cfg,
        volume: vol,
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: Default::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: Default::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    };
    s.active_inodes.push(Inode {
        id: InodeId(1),
        kind: InodeKind::Directory,
        path: "/".to_string(),
        size: 0,
        created_at: t(1),
        modified_at: t(1),
        changed_at: t(1),
        accessed_at: t(1),
        metadata: FileMetadata::default(),
    });
    s.active_inodes.push(Inode {
        id: InodeId(2),
        kind: InodeKind::File,
        path: "/a.txt".to_string(),
        size: 5,
        created_at: t(10),
        modified_at: t(10),
        changed_at: t(10),
        accessed_at: t(10),
        metadata: FileMetadata::default(),
    });
    s
}

fn add_snapshot_with_data(state: &mut PersistedState, id: u64, created: Timestamp) {
    let mut file_data = BTreeMap::new();
    file_data.insert("/a.txt".to_string(), b"hello".to_vec());
    let mut inodes = BTreeMap::new();
    inodes.insert(
        "/a.txt".to_string(),
        SnapshotInode {
            kind: InodeKind::File,
            size: 5,
            created_at: created,
            modified_at: created,
            changed_at: created,
            metadata: FileMetadata::default(),
            symlink_target: None,
        },
    );
    state.snapshots.push(Snapshot {
        id,
        name: format!("snap-{id}"),
        scope_root: "/".to_string(),
        created_at: created,
        paths: vec!["/a.txt".to_string()],
        file_data,
        inodes,
    });
    if id >= state.next_snapshot_id {
        state.next_snapshot_id = id + 1;
    }
}

#[test]
fn full_dump_roundtrip_produces_equal_state_shape() {
    let mut state = mk_state();
    add_snapshot_with_data(&mut state, 0, t(20));

    let mut buf: Vec<u8> = Vec::new();
    let report = stream_dump(&state, None, &mut buf, t(100)).expect("dump");
    assert!(!report.incremental);
    assert!(report.inode_records >= 2);
    assert_eq!(report.snapshot_records, 1);

    // Restore on empty
    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    let r = stream_restore(&mut target, &mut reader).expect("restore");
    assert!(!r.incremental);
    assert_eq!(r.inodes_applied, report.inode_records);
    assert_eq!(r.snapshots_applied, 1);
    // Mindestens der Snapshot + die beiden Inodes müssen übernommen sein.
    assert!(target.active_inodes.iter().any(|i| i.path == "/a.txt"));
    assert_eq!(target.snapshots.len(), 1);
}

#[test]
fn magic_mismatch_is_rejected() {
    let mut buf: Vec<u8> = Vec::new();
    // Fälsche einen Header mit falschem Magic:
    let bad = BackupHeader {
        magic: 0xDEAD_BEEF_DEAD_BEEF,
        version: BACKUP_VERSION,
        volume_id: [0; 16],
        base_snapshot_id: None,
        created_at: t(0),
        entry_count: 0,
    };
    let bytes = bincode_compat::serialize(&bad).unwrap();
    let len = (bytes.len() as u32).to_le_bytes();
    buf.extend_from_slice(&len);
    buf.extend_from_slice(&bytes);

    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    let err = stream_restore(&mut target, &mut reader).unwrap_err();
    match err {
        CoreFsError::InvalidInput(msg) => assert!(msg.contains("bad magic")),
        e => panic!("unexpected error: {e:?}"),
    }
}

#[test]
fn version_mismatch_is_rejected() {
    let mut buf: Vec<u8> = Vec::new();
    let bad = BackupHeader {
        magic: BACKUP_MAGIC,
        version: 999,
        volume_id: [0; 16],
        base_snapshot_id: None,
        created_at: t(0),
        entry_count: 0,
    };
    let bytes = bincode_compat::serialize(&bad).unwrap();
    let len = (bytes.len() as u32).to_le_bytes();
    buf.extend_from_slice(&len);
    buf.extend_from_slice(&bytes);

    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    let err = stream_restore(&mut target, &mut reader).unwrap_err();
    match err {
        CoreFsError::InvalidInput(msg) => assert!(msg.contains("unsupported version")),
        e => panic!("unexpected error: {e:?}"),
    }
}

#[test]
fn crc_mismatch_is_detected() {
    let mut state = mk_state();
    add_snapshot_with_data(&mut state, 0, t(20));

    let mut buf: Vec<u8> = Vec::new();
    stream_dump(&state, None, &mut buf, t(100)).unwrap();

    // Korrumpiere ein paar Bytes in der Mitte, aber außerhalb des Header-Frames
    // (wir kennen die Header-Länge nicht exakt, also korrumpieren wir nahe am Ende).
    if buf.len() > 30 {
        let idx = buf.len() - 20;
        buf[idx] ^= 0xFF;
    }

    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    let err = stream_restore(&mut target, &mut reader).unwrap_err();
    // Entweder CRC-Mismatch (State) oder Decode-Fehler (InvalidInput):
    match err {
        CoreFsError::State(_) | CoreFsError::InvalidInput(_) => {}
        e => panic!("unexpected error: {e:?}"),
    }
}

#[test]
fn incremental_dump_only_includes_changed() {
    let mut state = mk_state();
    // Basis-Snapshot zum Zeitpunkt 50:
    add_snapshot_with_data(&mut state, 0, t(50));
    // Einen neuen Inode nach dem Basis-Snapshot hinzufügen:
    state.active_inodes.push(Inode {
        id: InodeId(3),
        kind: InodeKind::File,
        path: "/new.txt".to_string(),
        size: 3,
        created_at: t(60),
        modified_at: t(60),
        changed_at: t(60),
        accessed_at: t(60),
        metadata: FileMetadata::default(),
    });

    let mut buf: Vec<u8> = Vec::new();
    let report = stream_dump(&state, Some(0), &mut buf, t(100)).expect("dump");
    assert!(report.incremental);
    // /new.txt (changed=60>50) + evtl. Root/a.txt (changed=1<50): nur /new.txt.
    assert_eq!(report.inode_records, 1);
}

#[test]
fn incremental_delete_markers_cover_missing_paths() {
    let mut state = mk_state();
    // Snapshot enthält /a.txt
    add_snapshot_with_data(&mut state, 0, t(50));
    // Entferne /a.txt aus aktivem State:
    state.active_inodes.retain(|i| i.path != "/a.txt");

    let mut buf: Vec<u8> = Vec::new();
    let report = stream_dump(&state, Some(0), &mut buf, t(100)).expect("dump");
    assert!(report.incremental);
    assert_eq!(report.delete_markers, 1);
}

#[test]
fn incremental_restore_applies_delete() {
    let mut state = mk_state();
    add_snapshot_with_data(&mut state, 0, t(50));
    state.active_inodes.retain(|i| i.path != "/a.txt");

    let mut buf: Vec<u8> = Vec::new();
    stream_dump(&state, Some(0), &mut buf, t(100)).unwrap();

    // Ziel hat /a.txt, das jetzt weggelöscht werden soll:
    let mut target = mk_state();
    assert!(target.active_inodes.iter().any(|i| i.path == "/a.txt"));
    let mut reader = SliceReader::new(&buf);
    let r = stream_restore(&mut target, &mut reader).unwrap();
    assert!(r.deletes_applied >= 1);
    assert!(!target.active_inodes.iter().any(|i| i.path == "/a.txt"));
}

#[test]
fn restore_overwrites_existing_inode_by_path() {
    let mut state = mk_state();
    // Modifiziere a.txt: setze size=999
    state.active_inodes.iter_mut().find(|i| i.path == "/a.txt").unwrap().size = 999;
    state.active_inodes.iter_mut().find(|i| i.path == "/a.txt").unwrap().changed_at = t(1000);

    let mut buf: Vec<u8> = Vec::new();
    stream_dump(&state, None, &mut buf, t(1000)).unwrap();

    let mut target = mk_state();
    // Ziel hat ursprüngliche Größe 5.
    assert_eq!(
        target.active_inodes.iter().find(|i| i.path == "/a.txt").unwrap().size,
        5
    );
    let mut reader = SliceReader::new(&buf);
    stream_restore(&mut target, &mut reader).unwrap();
    assert_eq!(
        target.active_inodes.iter().find(|i| i.path == "/a.txt").unwrap().size,
        999
    );
}

#[test]
fn wire_format_stable_header_magic() {
    // Regressionstest: Magic-Bytes sind "COREFSBK"
    assert_eq!(BACKUP_MAGIC.to_le_bytes(), *b"COREFSBK");
}

#[test]
fn volume_id_is_stable_for_same_input() {
    let a = derive_volume_id("myvol", t(42));
    let b = derive_volume_id("myvol", t(42));
    assert_eq!(a, b);
    let c = derive_volume_id("othervol", t(42));
    assert_ne!(a, c);
    let d = derive_volume_id("myvol", t(43));
    assert_ne!(a, d);
}

#[test]
fn full_dump_roundtrip_with_versions() {
    let mut state = mk_state();
    state.versions.push(FileVersion {
        version_id: 1,
        path: "/a.txt".to_string(),
        created_at: t(15),
        bytes: b"old".to_vec(),
    });

    let mut buf: Vec<u8> = Vec::new();
    let report = stream_dump(&state, None, &mut buf, t(100)).unwrap();
    assert_eq!(report.version_records, 1);

    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    let r = stream_restore(&mut target, &mut reader).unwrap();
    assert_eq!(r.versions_applied, 1);
    assert_eq!(target.versions.len(), 1);
    assert_eq!(target.versions[0].bytes, b"old");
}

#[test]
fn incremental_with_unknown_base_is_rejected() {
    let state = mk_state();
    let mut buf: Vec<u8> = Vec::new();
    let err = stream_dump(&state, Some(999), &mut buf, t(100)).unwrap_err();
    match err {
        CoreFsError::NotFound(msg) => assert!(msg.contains("999")),
        e => panic!("unexpected: {e:?}"),
    }
}

#[test]
fn blob_provider_contributes_active_content() {
    struct FakeProvider;
    impl BlobProvider for FakeProvider {
        fn read_inode(&mut self, id: InodeId) -> Option<Vec<u8>> {
            if id == InodeId(2) {
                Some(b"ACTIVE".to_vec())
            } else {
                None
            }
        }
    }

    let state = mk_state();
    let mut buf: Vec<u8> = Vec::new();
    let mut p = FakeProvider;
    let report =
        stream_dump_with_blobs(&state, None, &mut buf, t(100), &mut p).unwrap();
    assert_eq!(report.blob_records, 1);

    // Restore: Blob wird in einem neuen Snapshot installiert.
    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    stream_restore(&mut target, &mut reader).unwrap();
    let snap = target
        .snapshots
        .iter()
        .find(|s| s.name == "restore-blobs")
        .expect("restore-blobs snapshot installed");
    assert_eq!(snap.file_data.get("/a.txt").map(|v| v.as_slice()), Some(&b"ACTIVE"[..]));
}

#[test]
fn truncated_stream_is_detected() {
    let mut state = mk_state();
    add_snapshot_with_data(&mut state, 0, t(20));
    let mut buf: Vec<u8> = Vec::new();
    stream_dump(&state, None, &mut buf, t(100)).unwrap();
    // abgeschnitten — mindestens 10 Byte am Ende entfernen
    buf.truncate(buf.len().saturating_sub(10));
    let mut target = PersistedState::empty_at(CoreFsConfig::default(), t(0));
    let mut reader = SliceReader::new(&buf);
    let err = stream_restore(&mut target, &mut reader).unwrap_err();
    match err {
        CoreFsError::InvalidInput(_) | CoreFsError::State(_) => {}
        e => panic!("unexpected error: {e:?}"),
    }
}
