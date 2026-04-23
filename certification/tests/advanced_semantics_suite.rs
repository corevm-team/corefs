mod common;

use common::{CertTemp, assert_clean_image, deterministic_bytes, maybe_write_evidence};
use corefs::app::CoreFsService;
use corefs::config::{CoreFsConfig, PerformancePolicy};
use corefs::error::CoreFsError;
use corefs::storage::block_store::AllocatorPolicy;
use corefs::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
use corefs::storage::volume_wal::{VolumeWal, WalOperation};

const SEM_CAPACITY: u64 = 64 * 1024 * 1024;

#[test]
fn cert_100_snapshot_diff_scoped_restore_and_lifecycle_persist() {
    let tmp = CertTemp::new("snapshot-lifecycle");
    let image = tmp.path("snapshots.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = SEM_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    let (snap_a_id, snap_b_id, scoped_id) = {
        let mut session = OdfFileSession::format_new(&image, &opts).expect("format snapshots");
        session
            .mutate(|fs| {
                fs.create_directory("/scope")?;
                fs.create_file("/stable.txt", b"same", &[])?;
                fs.create_file("/changed.txt", b"old", &[])?;
                fs.create_file("/removed.txt", b"gone", &[])?;
                fs.create_file("/scope/inside.txt", b"inside-v1", &[])?;
                fs.create_file("/outside.txt", b"outside-v1", &[])?;
                let snap_a = fs.create_snapshot("before");

                fs.write_file("/changed.txt", b"new")?;
                fs.delete_file("/removed.txt", false)?;
                fs.create_file("/added.txt", b"fresh", &[])?;
                fs.write_file("/scope/inside.txt", b"inside-v2")?;
                fs.write_file("/outside.txt", b"outside-v2")?;
                let scoped = fs.create_snapshot_scoped("scope-only", "/scope");
                let snap_b = fs.create_snapshot("after");
                Ok((snap_a.id, snap_b.id, scoped.id))
            })
            .expect("create snapshot lifecycle")
            .0
    };

    {
        let mut session = OdfFileSession::open(&image).expect("reopen snapshots");
        let diff = session
            .service()
            .diff_snapshots(snap_a_id, snap_b_id)
            .expect("diff snapshots");
        assert_eq!(diff.added, vec!["/added.txt".to_string()]);
        assert_eq!(diff.removed, vec!["/removed.txt".to_string()]);
        assert_eq!(
            diff.modified,
            vec![
                "/changed.txt".to_string(),
                "/outside.txt".to_string(),
                "/scope/inside.txt".to_string()
            ]
        );
        assert_eq!(diff.unchanged, vec!["/stable.txt".to_string()]);

        session
            .mutate(|fs| {
                fs.restore_snapshot(scoped_id)?;
                fs.delete_snapshot(snap_a_id)?;
                Ok(())
            })
            .expect("restore scoped and delete snapshot");
    }

    let session = OdfFileSession::open(&image).expect("reopen after scoped restore");
    assert_eq!(
        session.service().read_file("/scope/inside.txt").unwrap(),
        b"inside-v2"
    );
    assert_eq!(
        session.service().read_file("/outside.txt").unwrap(),
        b"outside-v2",
        "scoped restore must not touch out-of-scope paths"
    );
    assert!(
        session
            .service()
            .snapshots()
            .iter()
            .all(|snapshot| snapshot.id != snap_a_id)
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_100_snapshot_diff_lifecycle",
        &format!(
            "snap_a_deleted=true\nsnap_b_id={snap_b_id}\nscoped_id={scoped_id}\nremaining_snapshots={}\nfsck_clean=true\n",
            session.service().snapshots().len()
        ),
    );
}

#[test]
fn cert_101_metadata_mode_owner_timestamps_and_versions_persist() {
    let tmp = CertTemp::new("metadata");
    let image = tmp.path("metadata.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = SEM_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();
    opts.config.versioning.keep_latest = 8;

    {
        let mut session = OdfFileSession::format_new(&image, &opts).expect("format metadata");
        session
            .mutate(|fs| {
                fs.create_directory("/meta")?;
                fs.create_file("/meta/doc.txt", b"v1", &[String::from("audit")])?;
                fs.write_file("/meta/doc.txt", b"v2")?;
                let versions_after_write = fs.file_version_ids("/meta/doc.txt").len();
                fs.set_owner("/meta/doc.txt", Some(1234), Some(5678))?;
                fs.set_mode("/meta/doc.txt", 0o177777)?;
                assert_eq!(
                    fs.file_version_ids("/meta/doc.txt").len(),
                    versions_after_write
                );
                Ok(())
            })
            .expect("write metadata");
    }

    let session = OdfFileSession::open(&image).expect("reopen metadata");
    let inode = session
        .service()
        .get_inode("/meta/doc.txt")
        .expect("metadata inode");
    assert_eq!(inode.metadata.uid, 1234);
    assert_eq!(inode.metadata.gid, 5678);
    assert_eq!(inode.metadata.mode, 0o7777);
    assert_eq!(session.service().read_file("/meta/doc.txt").unwrap(), b"v2");
    let versions = session.service().file_version_ids("/meta/doc.txt");
    assert!(!versions.is_empty());
    assert_eq!(
        session
            .service()
            .version_bytes_by_id("/meta/doc.txt", versions[0].0)
            .unwrap(),
        b"v1"
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_101_metadata_versions",
        &format!(
            "uid={}\ngid={}\nmode=0o{:o}\nversions={versions:?}\njournal_entries={}\nfsck_clean=true\n",
            inode.metadata.uid,
            inode.metadata.gid,
            inode.metadata.mode,
            session.service().journal_entries()
        ),
    );
}

#[test]
fn cert_102_clone_tree_cow_dedup_and_divergence_persist() {
    let tmp = CertTemp::new("cow-dedup");
    let image = tmp.path("cow.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = SEM_CAPACITY;
    opts.config = CoreFsConfig {
        performance: PerformancePolicy {
            deduplication_enabled: true,
            copy_on_write: true,
            journaling_enabled: true,
            ..CoreFsConfig::performance_profile().performance
        },
        ..CoreFsConfig::performance_profile()
    };

    {
        let mut session = OdfFileSession::format_new(&image, &opts).expect("format cow");
        session
            .mutate(|fs| {
                fs.create_directory("/src")?;
                fs.create_directory("/src/sub")?;
                fs.create_file("/src/a.bin", &deterministic_bytes(10, 4096), &[])?;
                fs.create_file("/src/sub/b.bin", b"duplicate", &[])?;
                fs.create_file("/other.bin", b"duplicate", &[])?;
                let clone_report = fs.clone_tree("/src", "/copy")?;
                assert_eq!(clone_report.cloned_directories, 2);
                assert_eq!(clone_report.cloned_files, 2);
                fs.write_file("/copy/a.bin", b"copy diverged")?;
                let dedup = fs.run_dedup()?;
                assert!(dedup.blobs_scanned >= 3);
                Ok(())
            })
            .expect("cow/dedup mutate");
    }

    let session = OdfFileSession::open(&image).expect("reopen cow");
    assert_eq!(
        session.service().read_file("/copy/a.bin").unwrap(),
        b"copy diverged"
    );
    assert_ne!(
        session.service().read_file("/src/a.bin").unwrap(),
        session.service().read_file("/copy/a.bin").unwrap(),
        "copy-on-write clone must diverge independently"
    );
    let cow = session.service().cow_report();
    assert!(cow.copy_on_write_enabled);
    assert!(
        session.service().journal_entries() >= 1,
        "journaling must persist clone/dedup evidence"
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_102_clone_cow_dedup",
        &format!(
            "copy_on_write_enabled={}\nshared_blobs={}\nbytes_saved_by_sharing={}\njournal_entries={}\nfsck_clean=true\n",
            cow.copy_on_write_enabled,
            cow.stats.shared_blobs,
            cow.stats.bytes_saved_by_sharing,
            session.service().journal_entries()
        ),
    );
}

#[test]
fn cert_103_fragmentation_hot_path_optimize_and_legacy_roundtrip() {
    let tmp = CertTemp::new("fragmentation");
    let image = tmp.path("fragmented.corefs");
    let mut fs = CoreFsService::format(CoreFsConfig {
        block_size: 4,
        performance: PerformancePolicy {
            deduplication_enabled: true,
            ..CoreFsConfig::performance_profile().performance
        },
        ..CoreFsConfig::performance_profile()
    });
    fs.set_allocator_policy(AllocatorPolicy {
        background_compaction_enabled: false,
        fragmentation_threshold_percent: 25,
        coalesce_on_release: false,
        ..AllocatorPolicy::default()
    });

    for name in ["a", "b", "c", "d", "e", "f"] {
        fs.create_file(&format!("/{name}.bin"), name.as_bytes(), &[])
            .expect("fragment file");
    }
    fs.delete_file("/b.bin", true).expect("delete b");
    fs.delete_file("/d.bin", true).expect("delete d");
    for _ in 0..8 {
        fs.write_file("/c.bin", b"cccc-hot").expect("hot write");
    }
    let before = fs.fragmentation_report();
    let optimized = fs.optimize_storage();
    assert!(before.fragmentation_percent >= 25);
    assert_eq!(optimized.after.fragmentation_percent, 0);
    assert!(optimized.heat_reallocation.is_some() || optimized.defragmentation.is_some());

    fs.save_image_to_path(&image).expect("save legacy image");
    let loaded = CoreFsService::load_image_from_path(&image).expect("load legacy image");
    assert_eq!(loaded.read_file("/c.bin").unwrap(), b"cccc-hot");
    assert_eq!(loaded.fragmentation_report().fragmentation_percent, 0);

    maybe_write_evidence(
        "cert_103_fragmentation_optimize",
        &format!(
            "before={before:?}\nafter={:?}\nlegacy_roundtrip=true\njournal_entries={}\n",
            optimized.after,
            loaded.journal_entries()
        ),
    );
}

#[test]
fn cert_104_aborted_odf_mutation_does_not_persist_partial_changes() {
    let tmp = CertTemp::new("abort");
    let image = tmp.path("abort.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = SEM_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    {
        let mut session = OdfFileSession::format_new(&image, &opts).expect("format abort");
        session
            .mutate(|fs| {
                fs.create_file("/committed.txt", b"durable", &[])?;
                Ok(())
            })
            .expect("commit initial");

        let result = session.mutate(|fs| {
            fs.create_file("/partial.txt", b"must not hit disk", &[])?;
            Err::<(), CoreFsError>(CoreFsError::InvalidInput("intentional abort".to_string()))
        });
        assert!(result.is_err());
    }

    let session = OdfFileSession::open(&image).expect("reopen aborted image");
    assert_eq!(
        session.service().read_file("/committed.txt").unwrap(),
        b"durable"
    );
    assert!(session.service().read_file("/partial.txt").is_err());
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_104_aborted_mutation",
        "committed_present=true\npartial_absent_after_reopen=true\nfsck_clean=true\n",
    );
}

#[test]
fn cert_105_pending_wal_replays_after_legacy_image_reload() {
    let tmp = CertTemp::new("wal");
    let image = tmp.path("wal.corefs");
    let mut fs = CoreFsService::format(CoreFsConfig {
        performance: PerformancePolicy {
            journaling_enabled: true,
            ..CoreFsConfig::performance_profile().performance
        },
        ..CoreFsConfig::performance_profile()
    });
    fs.create_file("/wal-src.txt", b"wal-data", &[])
        .expect("seed wal file");
    let mut wal = VolumeWal::new(42, "cert-wal-rename");
    wal.push(WalOperation::RenamePath {
        from: "/wal-src.txt".to_string(),
        to: "/wal-dst.txt".to_string(),
    });
    fs.set_pending_wal(wal);
    fs.save_image_to_path(&image)
        .expect("save pending wal image");

    let loaded = CoreFsService::load_image_from_path(&image).expect("load and replay wal");
    assert!(!loaded.has_pending_wal());
    assert!(loaded.read_file("/wal-src.txt").is_err());
    assert_eq!(loaded.read_file("/wal-dst.txt").unwrap(), b"wal-data");

    let loaded_again = CoreFsService::load_image_from_path(&image).expect("load replayed image");
    assert!(!loaded_again.has_pending_wal());
    assert_eq!(loaded_again.read_file("/wal-dst.txt").unwrap(), b"wal-data");

    maybe_write_evidence(
        "cert_105_pending_wal_replay",
        &format!(
            "transaction_id=42\nsrc_absent=true\ndst_present=true\npending_wal_after_load={}\n",
            loaded_again.has_pending_wal()
        ),
    );
}
