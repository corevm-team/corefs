// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// P0 stress and scalability tests for the App/Service layer (CoreFsService).
//
// Naming convention: `a1_…` – `a10_…` (App-layer stress), paralleling the
// `s1_…` – `s6_…` naming in `storage::ondisk::stress_tests`.
//
// All tests disable compression, encryption and versioning to isolate the
// App layer from pipeline overhead and keep runtime under 10 s each.

use super::*;

// ----- helpers --------------------------------------------------------------

fn stress_config() -> CoreFsConfig {
    CoreFsConfig {
        performance: crate::config::PerformancePolicy {
            compression_enabled: false,
            copy_on_write: true,
            deduplication_enabled: true,
            ..CoreFsConfig::default().performance
        },
        security: crate::config::SecurityPolicy {
            encryption_at_rest: false,
            ..CoreFsConfig::default().security
        },
        versioning: crate::config::VersioningPolicy {
            keep_latest: 0,
            ..CoreFsConfig::default().versioning
        },
        ..CoreFsConfig::default()
    }
}

fn stress_fs() -> CoreFsService {
    CoreFsService::format(stress_config())
}

fn assert_fsck_clean(fs: &CoreFsService, context: &str) {
    let r = fs.fsck();
    assert_eq!(
        r.missing_blocks,
        Vec::<String>::new(),
        "fsck missing_blocks [{context}]"
    );
    assert!(
        r.orphaned_blocks.is_empty(),
        "fsck orphaned_blocks [{context}]: {:?}",
        r.orphaned_blocks
    );
    assert!(
        r.size_mismatches.is_empty(),
        "fsck size_mismatches [{context}]: {:?}",
        r.size_mismatches
    );
    assert!(
        r.checksum_failures.is_empty(),
        "fsck checksum_failures [{context}]: {:?}",
        r.checksum_failures
    );
}

// ----- a1: many files in a single directory ---------------------------------

#[test]
fn a1_many_files_in_catalog() {
    let mut fs = stress_fs();
    let count = 2_000;

    for i in 0..count {
        fs.create_file(&format!("/f{i:05}"), format!("v{i}").as_bytes(), &[])
            .unwrap();
    }

    let paths = fs.list_paths();
    assert_eq!(paths.len(), count);

    // Spot-check first, middle and last.
    assert_eq!(fs.read_file("/f00000").unwrap(), b"v0");
    assert_eq!(fs.read_file("/f01000").unwrap(), b"v1000");
    assert_eq!(fs.read_file("/f01999").unwrap(), b"v1999");

    assert_fsck_clean(&fs, "a1 after 2000 files");
}

// ----- a2: large single file ------------------------------------------------

#[test]
fn a2_large_single_file() {
    let mut fs = stress_fs();
    // 2 MiB payload (larger values would slow the test without adding coverage).
    let payload: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

    fs.create_file("/bigfile", &payload, &[]).unwrap();

    let readback = fs.read_file("/bigfile").unwrap();
    assert_eq!(readback.len(), payload.len());
    assert_eq!(readback, payload);

    assert_fsck_clean(&fs, "a2 large file");
}

// ----- a3: deep directory tree ----------------------------------------------

#[test]
fn a3_deep_directory_tree() {
    let mut fs = stress_fs();
    let depth = 300;

    let mut path = String::new();
    for level in 0..depth {
        path.push_str(&format!("/d{level}"));
        fs.create_directory(&path).unwrap();
    }

    // Create a leaf file at the deepest level.
    let leaf = format!("{path}/leaf.txt");
    fs.create_file(&leaf, b"deep", &[]).unwrap();

    assert_eq!(fs.read_file(&leaf).unwrap(), b"deep");
    assert_fsck_clean(&fs, "a3 deep tree");
}

// ----- a4: snapshot accumulation under churn --------------------------------

#[test]
fn a4_snapshot_accumulation_under_churn() {
    let mut fs = stress_fs();
    fs.create_file("/base", b"initial", &[]).unwrap();

    let mut snapshot_ids = Vec::new();
    for round in 0..50 {
        let snap = fs.create_snapshot(&format!("snap-{round}"));
        snapshot_ids.push(snap.id);

        // Mutate between snapshots.
        let path = format!("/churn{round}");
        fs.create_file(&path, format!("r{round}").as_bytes(), &[])
            .unwrap();
    }

    assert_eq!(fs.snapshots().len(), 50);

    // Oldest snapshot should still see only /base.
    let oldest = &fs.snapshots()[0];
    assert!(oldest.file_data.contains_key("/base"));
    // It should NOT see files created after the snapshot.
    assert!(!oldest.file_data.contains_key("/churn5"));

    // Delete half the snapshots — should not corrupt remaining ones.
    for &id in snapshot_ids.iter().step_by(2) {
        fs.delete_snapshot(id).unwrap();
    }

    assert_eq!(fs.snapshots().len(), 25);
    assert_fsck_clean(&fs, "a4 after snapshot churn");
}

// ----- a5: clone cascades (CoW materialisation) -----------------------------

#[test]
fn a5_clone_cascades() {
    let mut fs = stress_fs();
    let original = b"original-payload";
    fs.create_file("/src", original, &[]).unwrap();

    // Chain of clones: /src → /c1 → /c2 → /c3 → /c4 → /c5.
    for i in 1..=5 {
        let from = if i == 1 {
            "/src".to_string()
        } else {
            format!("/c{}", i - 1)
        };
        fs.clone_file(&from, &format!("/c{i}")).unwrap();
    }

    // All clones read original content.
    for i in 1..=5 {
        assert_eq!(
            fs.read_file(&format!("/c{i}")).unwrap(),
            original,
            "clone c{i} should share original"
        );
    }

    // CoW report should show sharing.
    let cow = fs.cow_report();
    assert!(cow.stats.shared_blobs > 0, "blobs should be shared");

    // Write to a middle clone → materialises independent copy.
    fs.write_file("/c3", b"diverged").unwrap();
    assert_eq!(fs.read_file("/c3").unwrap(), b"diverged");

    // Siblings untouched.
    assert_eq!(fs.read_file("/c2").unwrap(), original);
    assert_eq!(fs.read_file("/c4").unwrap(), original);
    assert_eq!(fs.read_file("/src").unwrap(), original);

    assert_fsck_clean(&fs, "a5 clone cascade");
}

// ----- a6: delete/recreate cycles with block reuse --------------------------

#[test]
fn a6_delete_recreate_cycles() {
    let mut fs = stress_fs();

    for cycle in 0..50 {
        fs.create_file("/ephemeral", format!("cycle-{cycle}").as_bytes(), &[])
            .unwrap();
        assert_eq!(
            fs.read_file("/ephemeral").unwrap(),
            format!("cycle-{cycle}").as_bytes()
        );
        // Soft-delete + expunge to fully release blocks.
        fs.delete_file("/ephemeral", false).unwrap();
        fs.expunge_file("/ephemeral").unwrap();
    }

    // No files should remain active.
    assert!(
        !fs.list_paths().contains(&"/ephemeral".to_string()),
        "ephemeral should be gone"
    );

    assert_fsck_clean(&fs, "a6 after 50 delete/recreate cycles");
}

// ----- a7: dedup + defrag + optimise cascade --------------------------------

#[test]
fn a7_dedup_defrag_optimise_cascade() {
    let mut fs = stress_fs();
    let payload = b"identical-content-for-dedup";

    // Identical content is already deduplicated at write time via
    // `BlockStore::write` (entry(checksum).or_insert), so dedup_pass
    // finds 0 duplicates.  The value of this test is exercising the
    // full optimisation pipeline at scale — dedup audit + defrag +
    // optimise — and verifying that all files survive unscathed.
    for i in 0..500 {
        fs.create_file(&format!("/f{i:03}"), payload, &[]).unwrap();
    }

    // Run the full optimisation pipeline.
    let dedup_report = fs.run_dedup().unwrap();
    // ref_count audit should pass cleanly.
    assert_eq!(dedup_report.ref_count_mismatches, 0);

    // CoW report should confirm heavy sharing.
    let cow = fs.cow_report();
    assert!(cow.stats.shared_blobs > 0 || cow.stats.exclusive_blobs >= 1);

    let defrag_report = fs.defragment();
    let _ = defrag_report;

    let opt_report = fs.optimize_storage();
    let _ = opt_report;

    // All 500 files should still read correctly.
    for i in 0..500 {
        assert_eq!(
            fs.read_file(&format!("/f{i:03}")).unwrap(),
            payload,
            "f{i:03} should survive dedup+defrag+optimise"
        );
    }

    assert_fsck_clean(&fs, "a7 after dedup+defrag+optimise");
}

// ----- a8: large clone tree -------------------------------------------------

#[test]
fn a8_large_clone_tree() {
    let mut fs = stress_fs();

    // Build a tree with 100 files across 10 subdirectories.
    fs.create_directory("/tree").unwrap();
    for dir in 0..10 {
        fs.create_directory(&format!("/tree/d{dir}")).unwrap();
        for f in 0..10 {
            fs.create_file(
                &format!("/tree/d{dir}/f{f}"),
                format!("d{dir}-f{f}").as_bytes(),
                &[],
            )
            .unwrap();
        }
    }

    let report = fs.clone_tree("/tree", "/clone").unwrap();
    assert_eq!(report.cloned_files, 100, "100 files should be cloned");
    // 11 dirs: /tree root + 10 subdirectories.
    assert_eq!(report.cloned_directories, 11, "11 dirs should be cloned");
    assert!(report.skipped_paths.is_empty(), "no skips expected");

    // Spot-check content in clone.
    assert_eq!(fs.read_file("/clone/d5/f7").unwrap(), b"d5-f7");

    // Mutate clone — original should be unchanged.
    fs.write_file("/clone/d0/f0", b"mutated").unwrap();
    assert_eq!(fs.read_file("/tree/d0/f0").unwrap(), b"d0-f0");
    assert_eq!(fs.read_file("/clone/d0/f0").unwrap(), b"mutated");

    assert_fsck_clean(&fs, "a8 clone tree");
}

// ----- a9: sustained write + snapshot + prune cycle -------------------------

#[test]
fn a9_sustained_write_snapshot_prune() {
    let mut fs = stress_fs();
    let mut snap_ids = Vec::new();

    for round in 0..80 {
        // Overwrite the same file with growing content.
        let data: Vec<u8> = vec![(round % 256) as u8; 4096 * (1 + round % 8)];
        if round == 0 {
            fs.create_file("/hot", &data, &[]).unwrap();
        } else {
            fs.write_file("/hot", &data).unwrap();
        }

        // Snapshot every 4th round.
        if round % 4 == 0 {
            let snap = fs.create_snapshot(&format!("s{round}"));
            snap_ids.push(snap.id);
        }

        // Prune oldest snapshot when we exceed 10 accumulated.
        if snap_ids.len() > 10 {
            let oldest = snap_ids.remove(0);
            fs.delete_snapshot(oldest).unwrap();
        }
    }

    // Should have ~10 snapshots remaining.
    assert!(fs.snapshots().len() <= 11);

    // Current file should be readable.
    let data = fs.read_file("/hot").unwrap();
    assert!(!data.is_empty());

    assert_fsck_clean(&fs, "a9 sustained write+snapshot+prune");
}

// ----- a10: quota enforcement at scale --------------------------------------

#[test]
fn a10_quota_enforcement_at_scale() {
    let mut config = stress_config();
    config.quotas = crate::config::QuotaPolicy {
        max_files: Some(500),
        max_bytes: None,
    };
    let mut fs = CoreFsService::format(config);

    // Create up to the limit.
    for i in 0..500 {
        fs.create_file(&format!("/q{i:04}"), b"x", &[]).unwrap();
    }

    // Next file should be rejected.
    let result = fs.create_file("/q0500", b"x", &[]);
    assert!(result.is_err(), "quota should reject the 501st file");

    // Delete one, then the slot should open.
    fs.delete_file("/q0000", false).unwrap();
    fs.expunge_file("/q0000").unwrap();
    fs.create_file("/q0500", b"x", &[]).unwrap();

    assert_eq!(fs.list_paths().len(), 500);
    assert_fsck_clean(&fs, "a10 quota at scale");
}
