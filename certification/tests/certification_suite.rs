mod common;

use common::{CertTemp, assert_clean_image, deterministic_bytes, maybe_write_evidence};
use corefs::app::CoreFsService;
use corefs::config::CoreFsConfig;
use corefs::platform::performance::{BenchmarkConfig, BenchmarkProfile, run_benchmark};
use corefs::storage::ondisk::benchmark::{OdfBenchConfig, run_odf_bench};
use corefs::storage::ondisk::checksum::Crc32c;
use corefs::storage::ondisk::layout::{BLOCK_SIZE, FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS};
use corefs::storage::ondisk::property;
use corefs::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
use corefs_cli::{ExitStatus, dispatch};
use corefs_core::domain::inode::InodeId;
use corefs_core::domain::inode::InodeKind;
use corefs_tools::Report;
use corefs_tools::backup;
use corefs_tools::defrag;
use corefs_tools::dump;
use corefs_tools::fsck;
use corefs_tools::keys;
use corefs_tools::mkfs::{self, FormatImageOptions, LayoutMode};
use corefs_tools::repair;
use corefs_tools::scrub::{self, ScrubMode};
use corefs_tools::snapshot::{self, CreateOptions};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CERT_CAPACITY: u64 = 64 * 1024 * 1024;

#[test]
fn cert_001_crc32c_vectors_streaming_and_throughput() {
    let vectors: &[(&[u8], u32)] = &[
        (b"", 0x0000_0000),
        (b"123456789", 0xe306_9283),
        (b"The quick brown fox jumps over the lazy dog", 0x2262_0404),
    ];

    for (input, expected) in vectors {
        assert_eq!(Crc32c::hash(input), *expected, "CRC32C vector failed");
    }

    let payload = deterministic_bytes(0xC0DE_CAFE, 256 * 1024);
    let one_shot = Crc32c::hash(&payload);
    let mut seed = !0u32;
    for chunk in payload.chunks(777) {
        seed = Crc32c::update(seed, chunk);
    }
    assert_eq!(!seed, one_shot, "streaming CRC must equal one-shot CRC");

    let big = deterministic_bytes(0xBADC_0FFE, 8 * 1024 * 1024);
    let start = Instant::now();
    let crc = Crc32c::hash(&big);
    let elapsed = start.elapsed();
    let mib_per_sec = mib_per_sec(big.len(), elapsed);
    let min_mib_per_sec = env_f64("COREFS_CERT_MIN_CRC_MIB_S", 25.0);
    assert!(
        mib_per_sec >= min_mib_per_sec,
        "CRC throughput {mib_per_sec:.2} MiB/s below gate {min_mib_per_sec:.2} MiB/s (crc={crc:#x})"
    );

    maybe_write_evidence(
        "cert_001_crc32c",
        &format!("crc={crc:#x}\nthroughput_mib_s={mib_per_sec:.2}\n"),
    );
}

#[test]
fn cert_010_mkfs_geometry_feature_flags_and_cli_reports() {
    let tmp = CertTemp::new("mkfs");
    let image = tmp.path("geometry.img");
    let opts = FormatImageOptions {
        label: "cert-native".to_string(),
        uuid: [0xAB; 16],
        inode_count: Some(2048),
        journal_blocks: Some(32),
        layout_mode: LayoutMode::Native,
        ..Default::default()
    };

    let report = mkfs::format_image(&image, CERT_CAPACITY, &opts).expect("mkfs native image");
    assert_eq!(report.capacity_bytes, CERT_CAPACITY);
    assert_eq!(report.inode_count, 2048);
    assert_eq!(report.journal_blocks, 32);
    assert_eq!(report.uuid_hex, "abababababababababababababababab");

    let sb = dump::superblock(&image).expect("dump superblock");
    assert!(sb.magic_ok);
    assert_eq!(sb.layout_mode, "native");
    assert_eq!(sb.label, "cert-native");
    assert_eq!(sb.total_inodes, 2048);
    assert_ne!(sb.feature_compat & FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS, 0);
    assert_clean_image(&image);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = vec![
        "fsck".to_string(),
        image.display().to_string(),
        "--json".to_string(),
    ];
    assert_eq!(dispatch(&args, &mut out, &mut err), ExitStatus::Ok);
    assert!(
        err.is_empty(),
        "CLI fsck stderr: {}",
        String::from_utf8_lossy(&err)
    );
    assert!(
        String::from_utf8(out)
            .expect("utf8")
            .contains("\"is_clean\": true")
    );

    maybe_write_evidence(
        "cert_010_mkfs_geometry",
        &format!("{}\n{}\n", report.render_json(), sb.render_json()),
    );
}

#[test]
fn cert_020_file_folder_rename_overwrite_delete_restore_reopen_matrix() {
    let tmp = CertTemp::new("ops");
    let image = tmp.path("ops.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format session");
        sess.mutate(|fs| {
            fs.create_directory("/alpha")?;
            fs.create_directory("/alpha/beta")?;
            fs.create_file("/alpha/beta/a.txt", b"hello", &[String::from("cert")])?;
            fs.create_file("/alpha/beta/blob.bin", &deterministic_bytes(1, 8192), &[])?;
            fs.create_symlink("/alpha/link-a", "/alpha/beta/a.txt")?;
            fs.write_file_range("/alpha/beta/a.txt", 2, b"YY")?;
            assert_eq!(fs.read_file("/alpha/beta/a.txt")?, b"heYYo");
            fs.extend_file("/alpha/beta/a.txt", b"-tail")?;
            fs.truncate_file("/alpha/beta/a.txt", 7)?;
            assert_eq!(fs.read_file_range("/alpha/beta/a.txt", 0, 32)?, b"heYYo-t");
            fs.set_owner("/alpha/beta/a.txt", Some(1000), Some(1001))?;
            fs.set_mode("/alpha/beta/a.txt", 0o640)?;
            fs.clone_file("/alpha/beta/a.txt", "/alpha/beta/a.clone")?;
            fs.write_file("/alpha/beta/a.clone", b"clone diverged")?;
            fs.rename_entry("/alpha/beta", "/alpha/renamed")?;
            fs.create_file("/alpha/target.txt", b"old target", &[])?;
            fs.rename_entry("/alpha/renamed/a.clone", "/alpha/target.txt")?;
            fs.delete_file("/alpha/renamed/a.txt", false)?;
            assert!(
                fs.recoverable_paths()
                    .contains(&"/alpha/renamed/a.txt".to_string())
            );
            fs.restore_file("/alpha/renamed/a.txt")?;
            Ok(())
        })
        .expect("mutate file matrix");
    }

    let sess = OdfFileSession::open(&image).expect("reopen matrix image");
    let fs = sess.service();
    assert_eq!(fs.read_file("/alpha/renamed/a.txt").unwrap(), b"heYYo-t");
    assert_eq!(
        fs.read_file("/alpha/target.txt").unwrap(),
        b"clone diverged"
    );
    assert_eq!(fs.read_file("/alpha/link-a").unwrap(), b"/alpha/beta/a.txt");
    assert!(!fs.list_paths().contains(&"/alpha/beta/a.txt".to_string()));

    let inode = fs.get_inode("/alpha/renamed/a.txt").expect("inode");
    assert_eq!(inode.kind, InodeKind::File);
    assert_eq!(inode.metadata.uid, 1000);
    assert_eq!(inode.metadata.gid, 1001);
    assert_eq!(inode.metadata.mode, 0o640);
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_020_file_folder_matrix",
        &format!(
            "paths={:?}\nrestored=/alpha/renamed/a.txt\noverwrite=/alpha/target.txt\nmode=0o{:o}\nuid={}\ngid={}\n",
            fs.list_paths(),
            inode.metadata.mode,
            inode.metadata.uid,
            inode.metadata.gid,
        ),
    );
}

#[test]
fn cert_030_encryption_compression_versioning_and_snapshot_semantics() {
    let tmp = CertTemp::new("crypto");
    let image = tmp.path("crypto.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::default();
    opts.config.versioning.keep_latest = 4;
    opts.config.versioning.max_version_bytes = None;
    opts.config.performance.compression_enabled = true;
    opts.config.security.encryption_at_rest = true;

    let plain_v1 = vec![b'A'; 16 * 1024];
    let plain_v2 = vec![b'B'; 16 * 1024];
    let snap_id;
    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format encrypted");
        let (id, _) = sess
            .mutate(|fs| {
                fs.create_directory("/secure")?;
                fs.create_file("/secure/data.bin", &plain_v1, &[])?;
                let snap = fs.create_snapshot("before-overwrite");
                fs.write_file("/secure/data.bin", &plain_v2)?;
                Ok(snap.id)
            })
            .expect("write encrypted data");
        snap_id = id;
    }

    let mut sess = OdfFileSession::open(&image).expect("open encrypted image");
    assert_eq!(
        sess.service().read_file("/secure/data.bin").unwrap(),
        plain_v2
    );
    assert_active_file_extents_do_not_contain(&image, &sess, "/secure/data.bin", &plain_v2);
    assert_eq!(sess.service().snapshots().len(), 1);
    let versions = sess.service().file_version_ids("/secure/data.bin");
    assert!(
        versions.len() >= 2,
        "create and overwrite should both be versioned, got {versions:?}"
    );
    assert_eq!(
        sess.service()
            .version_bytes_by_id("/secure/data.bin", versions[0].0)
            .unwrap(),
        plain_v1
    );

    sess.mutate(|fs| {
        let restore = fs.restore_snapshot(snap_id)?;
        assert_eq!(restore.restored_files, 1);
        Ok(())
    })
    .expect("restore encrypted snapshot");
    drop(sess);

    let sess = OdfFileSession::open(&image).expect("reopen after restore");
    assert_eq!(
        sess.service().read_file("/secure/data.bin").unwrap(),
        plain_v1
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_030_encryption_versioning",
        &format!(
            "snap_id={snap_id}\nversions={:?}\nactive_plaintext_bytes={}\nrestored_plaintext_bytes={}\n",
            versions,
            plain_v2.len(),
            plain_v1.len(),
        ),
    );
}

#[test]
fn cert_040_snapshot_backup_defrag_scrub_and_repair_toolchain() {
    let tmp = CertTemp::new("toolchain");
    let image = tmp.path("source.img");
    let restored = tmp.path("restored.img");
    let backup_path = tmp.path("full.corefsbk");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format source");
        sess.mutate(|fs| {
            fs.create_directory("/tree")?;
            for i in 0..96 {
                let payload = deterministic_bytes(i as u64 + 10, 257 + (i % 17) * 31);
                fs.create_file(&format!("/tree/file-{i:03}.bin"), &payload, &[])?;
            }
            for i in (0..96).step_by(3) {
                fs.delete_file(&format!("/tree/file-{i:03}.bin"), false)?;
            }
            fs.create_snapshot("after-churn");
            Ok(())
        })
        .expect("populate source");
    }

    let list_before = snapshot::list(&image).expect("list snapshots");
    assert_eq!(list_before.snapshots.len(), 1);
    let scoped = snapshot::create(
        &image,
        &CreateOptions {
            name: "scoped-tree".to_string(),
            scope_root: Some("/tree".to_string()),
        },
    )
    .expect("create scoped snapshot");
    assert!(scoped.file_data_count >= 64);

    {
        let mut sess = OdfFileSession::open(&image).expect("open source for destructive phase");
        sess.mutate(|fs| {
            fs.write_file("/tree/file-001.bin", b"mutated after snapshot")?;
            Ok(())
        })
        .expect("mutate after snapshot");
    }
    let restored_snapshot = snapshot::restore(&image, scoped.id).expect("restore scoped snapshot");
    assert!(restored_snapshot.restored_files >= 64);

    let dump = backup::dump(&image, Some(&backup_path), None).expect("backup dump");
    assert!(dump.bytes_written > 0);
    assert!(dump.entries_written > 0);

    OdfFileSession::format_new(&restored, &opts).expect("format restore target");
    let restore = backup::restore(&restored, Some(&backup_path)).expect("backup restore");
    assert!(restore.entries_read > 0);

    let defrag_report = defrag::defrag_image(&image).expect("defrag source");
    let repair_report = repair::repair_image(&image).expect("repair clean source");
    let scrub_report = scrub::scrub_image(&image, ScrubMode::Full).expect("scrub source");
    assert!(repair_report.fully_repaired);
    assert!(
        scrub_report.is_clean,
        "scrub issues: {:?}",
        scrub_report.residual_issues
    );
    assert_clean_image(&image);
    assert_clean_image(&restored);

    let source_session = OdfFileSession::open(&image).expect("reopen source");
    let restored_session = OdfFileSession::open(&restored).expect("reopen restored");
    assert_eq!(
        source_session
            .service()
            .read_file("/tree/file-001.bin")
            .unwrap(),
        restored_session
            .service()
            .read_file("/tree/file-001.bin")
            .unwrap()
    );

    maybe_write_evidence(
        "cert_040_toolchain",
        &format!(
            "{}\n{}\n{}\n{}",
            dump.render_json(),
            restore.render_json(),
            defrag_report.render_json(),
            scrub_report.render_json()
        ),
    );
}

#[test]
fn cert_050_data_corruption_is_detected_by_scrub() {
    let tmp = CertTemp::new("corruption");
    let image = tmp.path("corrupt.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    let victim = deterministic_bytes(0x5151, 12 * 1024);
    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format corruption");
        sess.mutate(|fs| {
            fs.create_file("/victim.bin", &victim, &[])?;
            Ok(())
        })
        .expect("write victim");
    }

    let sess = OdfFileSession::open(&image).expect("reopen victim");
    let state = sess.service().persisted_state();
    let inode = sess
        .service()
        .inode_for_path("/victim.bin")
        .expect("victim inode");
    let physical_block = state
        .block_records
        .iter()
        .find(|r| r.inode == inode)
        .and_then(|r| r.extents.first())
        .map(|e| e.physical_block)
        .expect("victim physical block");
    drop(sess);

    flip_byte(&image, physical_block * BLOCK_SIZE + 37);
    let report = scrub::scrub_image(&image, ScrubMode::ReadOnly).expect("read-only scrub");
    assert!(
        !report.data_corruptions.is_empty(),
        "scrub must detect injected data corruption"
    );
    assert!(!report.is_clean);

    maybe_write_evidence("cert_050_corruption", &report.render_json());
}

#[test]
fn cert_060_redundant_superblock_survives_primary_block_loss() {
    let tmp = CertTemp::new("superblock-fallback");
    let image = tmp.path("fallback.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format fallback");
        sess.mutate(|fs| {
            fs.create_file("/survivor.txt", b"redundancy matters", &[])?;
            Ok(())
        })
        .expect("write survivor");
    }

    zero_range(&image, BLOCK_SIZE, BLOCK_SIZE as usize);
    let sess = OdfFileSession::open(&image).expect("open via redundant superblock fallback");
    assert_eq!(
        sess.service().read_file("/survivor.txt").unwrap(),
        b"redundancy matters"
    );

    maybe_write_evidence(
        "cert_060_superblock_fallback",
        "primary_superblock_zeroed=true\nfallback_open=true\nsurvivor=/survivor.txt\n",
    );
}

#[test]
fn cert_070_deterministic_property_matrix_across_seeds() {
    let quick_seeds = [1, 2, 3, 5, 8, 13, 21, 34];
    property::fuzz_many_seeds(&quick_seeds, 48).expect("property matrix");
    maybe_write_evidence(
        "cert_070_property_matrix",
        &format!("seeds={quick_seeds:?}\nsequence_len=48\n"),
    );
}

#[test]
fn cert_080_odf_benchmark_regression_gate() {
    let cfg = OdfBenchConfig {
        volume_blocks: 4096,
        inode_count: 1024,
        file_count: 96,
        payload_size: 2048,
    };
    let result = run_odf_bench(cfg).expect("run odf bench");
    assert_duration_under(
        "format",
        result.format,
        env_ms("COREFS_CERT_MAX_FORMAT_MS", 750),
    );
    assert_duration_under(
        "native_save",
        result.native_save,
        env_ms("COREFS_CERT_MAX_NATIVE_SAVE_MS", 1500),
    );
    assert_duration_under(
        "native_load",
        result.native_load,
        env_ms("COREFS_CERT_MAX_NATIVE_LOAD_MS", 1500),
    );

    maybe_write_evidence(
        "cert_080_odf_benchmark",
        &format!(
            "format_ms={}\nblob_save_ms={}\nblob_load_ms={}\nnative_save_ms={}\nnative_load_ms={}\nfiles={}\npayload={}\n",
            result.format.as_millis(),
            result.blob_save.as_millis(),
            result.blob_load.as_millis(),
            result.native_save.as_millis(),
            result.native_load.as_millis(),
            result.files_populated,
            result.bytes_per_file,
        ),
    );
}

#[test]
fn cert_011_blob_and_native_layout_modes_are_identifiable() {
    let tmp = CertTemp::new("layout-modes");
    let native = tmp.path("native.img");
    let blob = tmp.path("blob.img");

    mkfs::format_image(
        &native,
        CERT_CAPACITY,
        &FormatImageOptions {
            label: "cert-native".to_string(),
            layout_mode: LayoutMode::Native,
            ..Default::default()
        },
    )
    .expect("mkfs native");
    mkfs::format_image(
        &blob,
        CERT_CAPACITY,
        &FormatImageOptions {
            label: "cert-blob".to_string(),
            layout_mode: LayoutMode::Blob,
            ..Default::default()
        },
    )
    .expect("mkfs blob");

    let native_sb = dump::superblock(&native).expect("native sb");
    let blob_sb = dump::superblock(&blob).expect("blob sb");
    assert_eq!(native_sb.layout_mode, "native");
    assert_eq!(blob_sb.layout_mode, "blob");
    assert_clean_image(&native);
    assert_clean_image(&blob);

    maybe_write_evidence(
        "cert_011_layout_modes",
        &format!(
            "native={}\nblob={}\nnative_clean=true\nblob_clean=true\n",
            native_sb.render_json(),
            blob_sb.render_json()
        ),
    );
}

#[test]
fn cert_021_inode_ids_distribution_extents_and_dump_inode() {
    let tmp = CertTemp::new("inode-distribution");
    let image = tmp.path("inodes.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format inodes");
        sess.mutate(|fs| {
            fs.create_directory_with_inode("/slots", InodeId(16))?;
            fs.create_file_with_inode(
                "/slots/a.bin",
                &deterministic_bytes(100, 7000),
                &[],
                InodeId(32),
            )?;
            fs.create_file_with_inode(
                "/slots/b.bin",
                &deterministic_bytes(101, 9000),
                &[],
                InodeId(128),
            )?;
            fs.create_file_with_inode(
                "/slots/c.bin",
                &deterministic_bytes(102, 11_000),
                &[],
                InodeId(512),
            )?;
            Ok(())
        })
        .expect("write specific inodes");
    }

    let sess = OdfFileSession::open(&image).expect("reopen inodes");
    let state = sess.service().persisted_state();
    let mut ids: Vec<u64> = state.active_inodes.iter().map(|inode| inode.id.0).collect();
    ids.sort_unstable();
    assert!(ids.contains(&16));
    assert!(ids.contains(&32));
    assert!(ids.contains(&128));
    assert!(ids.contains(&512));
    for record in state
        .block_records
        .iter()
        .filter(|record| !record.extents.is_empty())
    {
        assert!(
            record.content_crc != 0,
            "file records must carry content CRC"
        );
        for extent in &record.extents {
            assert!(extent.length_blocks > 0);
            assert!(extent.physical_block > 0);
        }
    }
    let inode_dump = dump::inode(&image, 1).expect("dump first user inode slot");
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_021_inode_distribution",
        &format!(
            "active_inode_ids={ids:?}\nblock_records={:?}\nslot1={}\n",
            state.block_records,
            inode_dump.render_json()
        ),
    );
}

#[test]
fn cert_022_range_writes_sparse_growth_truncate_and_boundaries() {
    let tmp = CertTemp::new("range");
    let image = tmp.path("range.img");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    {
        let mut sess = OdfFileSession::format_new(&image, &opts).expect("format range");
        sess.mutate(|fs| {
            fs.create_file("/range.bin", b"abcd", &[])?;
            fs.write_file_range("/range.bin", 8, b"XYZ")?;
            let bytes = fs.read_file("/range.bin")?;
            assert_eq!(&bytes[..4], b"abcd");
            assert_eq!(&bytes[4..8], &[0, 0, 0, 0]);
            assert_eq!(&bytes[8..], b"XYZ");
            fs.write_file_range("/range.bin", 2, b"12")?;
            assert_eq!(fs.read_file_range("/range.bin", 0, 11)?, b"ab12\0\0\0\0XYZ");
            fs.truncate_file("/range.bin", 5)?;
            assert_eq!(fs.read_file("/range.bin")?, b"ab12\0");
            fs.truncate_file("/range.bin", 9)?;
            assert_eq!(fs.read_file("/range.bin")?, b"ab12\0\0\0\0\0");
            Ok(())
        })
        .expect("range matrix");
    }

    let sess = OdfFileSession::open(&image).expect("reopen range");
    assert_eq!(
        sess.service().read_file("/range.bin").unwrap(),
        b"ab12\0\0\0\0\0"
    );
    assert_clean_image(&image);
    maybe_write_evidence(
        "cert_022_range_truncate",
        "sparse_gap_zero_filled=true\ntruncate_shrink=true\ntruncate_grow=true\nreopen=true\n",
    );
}

#[test]
fn cert_023_quota_and_validation_failures_are_enforced() {
    let mut config = CoreFsConfig::performance_profile();
    config.quotas.max_files = Some(2);
    config.quotas.max_bytes = Some(16);
    let mut fs = CoreFsService::format(config);
    fs.create_file("/a.txt", b"12345678", &[])
        .expect("a within quota");
    fs.create_file("/b.txt", b"12345678", &[])
        .expect("b within quota");
    assert!(
        fs.create_file("/c.txt", b"x", &[]).is_err(),
        "max_files must reject third file"
    );
    assert!(
        fs.write_file("/a.txt", b"123456789").is_err(),
        "max_bytes must reject growth"
    );
    assert!(
        fs.create_file("relative.txt", b"x", &[]).is_err(),
        "relative paths must fail"
    );
    assert!(
        fs.create_file("/a.txt", b"duplicate", &[]).is_err(),
        "duplicate path must fail"
    );

    maybe_write_evidence(
        "cert_023_quota_validation",
        "max_files=2\nmax_bytes=16\nthird_file_rejected=true\ngrowth_rejected=true\nrelative_path_rejected=true\nduplicate_rejected=true\n",
    );
}

#[test]
fn cert_024_secure_delete_is_not_recoverable() {
    let mut fs = CoreFsService::format(CoreFsConfig::performance_profile());
    fs.create_file("/secret.bin", b"classified", &[])
        .expect("create secret");
    fs.delete_file("/secret.bin", true).expect("secure delete");
    assert!(!fs.list_paths().contains(&"/secret.bin".to_string()));
    assert!(fs.restore_file("/secret.bin").is_err());
    assert!(!fs.recoverable_paths().contains(&"/secret.bin".to_string()));

    maybe_write_evidence(
        "cert_024_secure_delete",
        "secure_delete=true\nrestore_rejected=true\nrecoverable=false\n",
    );
}

#[test]
fn cert_031_keystore_init_verify_rotate_and_wrong_key_rejection() {
    let tmp = CertTemp::new("keys");
    let keystore = tmp.path("volume.ks");
    let old_master = tmp.path("old.master");
    let new_master = tmp.path("new.master");
    let wrong_master = tmp.path("wrong.master");
    std::fs::write(&old_master, deterministic_bytes(0xA1, 32)).expect("old master");
    std::fs::write(&new_master, deterministic_bytes(0xA2, 32)).expect("new master");
    std::fs::write(&wrong_master, deterministic_bytes(0xA3, 32)).expect("wrong master");

    let init = keys::init(&keystore, &old_master, "00112233445566778899aabbccddeeff")
        .expect("keystore init");
    let verify_old = keys::verify(&keystore, &old_master).expect("verify old");
    assert!(verify_old.magic_ok && verify_old.version_ok && verify_old.unwrap_ok);
    let wrong_before = keys::verify(&keystore, &wrong_master).expect("verify wrong before rotate");
    assert!(!wrong_before.unwrap_ok);
    let rotate = keys::rotate(&keystore, &old_master, &new_master).expect("rotate key");
    let verify_new = keys::verify(&keystore, &new_master).expect("verify new");
    assert!(verify_new.unwrap_ok);
    let old_after = keys::verify(&keystore, &old_master).expect("verify old after rotate");
    assert!(!old_after.unwrap_ok);

    maybe_write_evidence(
        "cert_031_keystore",
        &format!(
            "{}\n{}\n{}\n{}\nwrong_before_unwrap={}\nold_after_unwrap={}\n",
            init.render_json(),
            verify_old.render_json(),
            rotate.render_json(),
            verify_new.render_json(),
            wrong_before.unwrap_ok,
            old_after.unwrap_ok,
        ),
    );
}

#[test]
fn cert_041_incremental_backup_and_truncated_stream_detection() {
    let tmp = CertTemp::new("incremental-backup");
    let source = tmp.path("source.img");
    let target = tmp.path("target.img");
    let full_stream = tmp.path("full.bk");
    let inc_stream = tmp.path("inc.bk");
    let truncated_stream = tmp.path("truncated.bk");
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = CERT_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();

    let base_id = {
        let mut sess = OdfFileSession::format_new(&source, &opts).expect("source format");
        let (base_id, _) = sess
            .mutate(|fs| {
                fs.create_file("/keep.txt", b"base", &[])?;
                fs.create_file("/delete.txt", b"remove me", &[])?;
                Ok(fs.create_snapshot("base").id)
            })
            .expect("base state");
        base_id
    };

    let full = backup::dump(&source, Some(&full_stream), None).expect("full dump");
    {
        let mut sess = OdfFileSession::open(&source).expect("open source mutate");
        sess.mutate(|fs| {
            fs.write_file("/keep.txt", b"incremental value")?;
            fs.delete_file("/delete.txt", false)?;
            fs.create_file("/new.txt", b"new", &[])?;
            fs.create_snapshot("after-inc");
            Ok(())
        })
        .expect("incremental mutation");
    }
    let inc = backup::dump(&source, Some(&inc_stream), Some(base_id)).expect("incremental dump");
    assert!(inc.incremental);
    assert!(inc.delete_markers >= 1);

    OdfFileSession::format_new(&target, &opts).expect("format target");
    backup::restore(&target, Some(&full_stream)).expect("restore full");
    backup::restore(&target, Some(&inc_stream)).expect("restore incremental");
    let target_sess = OdfFileSession::open(&target).expect("open target");
    assert_eq!(
        target_sess.service().read_file("/keep.txt").unwrap(),
        b"incremental value"
    );
    assert!(target_sess.service().read_file("/delete.txt").is_err());
    assert_eq!(target_sess.service().read_file("/new.txt").unwrap(), b"new");

    let mut bytes = std::fs::read(&inc_stream).expect("read inc");
    bytes.truncate(bytes.len().saturating_sub(8));
    std::fs::write(&truncated_stream, bytes).expect("write truncated");
    let truncated_result = backup::restore(&target, Some(&truncated_stream));
    assert!(
        truncated_result.is_err(),
        "truncated backup stream must fail"
    );

    maybe_write_evidence(
        "cert_041_incremental_backup",
        &format!(
            "base_id={base_id}\nfull={}\ninc={}\ntruncated_restore_failed=true\n",
            full.render_json(),
            inc.render_json()
        ),
    );
}

#[test]
fn cert_042_cli_admin_json_surface_smoke() {
    let tmp = CertTemp::new("cli");
    let image = tmp.path("cli.img");
    let image_s = image.display().to_string();
    run_cli_ok(&[
        "mkfs",
        &image_s,
        "--capacity",
        &CERT_CAPACITY.to_string(),
        "--label",
        "cli-cert",
    ]);
    let fsck_json = run_cli_ok(&["fsck", &image_s, "--json"]);
    let dump_json = run_cli_ok(&["dump-superblock", &image_s, "--json"]);
    let snap_json = run_cli_ok(&[
        "snapshot", "create", &image_s, "--name", "cli-snap", "--json",
    ]);
    let list_json = run_cli_ok(&["snapshot", "list", &image_s, "--json"]);
    let defrag_json = run_cli_ok(&["defrag", &image_s, "--json"]);
    let scrub_json = run_cli_ok(&["scrub", &image_s, "--mode", "full", "--json"]);
    assert!(fsck_json.contains("\"is_clean\": true"));
    assert!(dump_json.contains("\"label\": \"cli-cert\""));
    assert!(snap_json.contains("\"name\": \"cli-snap\""));
    assert!(list_json.contains("\"snapshots\""));
    assert!(defrag_json.contains("\"final_device_blocks\""));
    assert!(scrub_json.contains("\"is_clean\": true"));

    maybe_write_evidence(
        "cert_042_cli_surface",
        &format!(
            "{fsck_json}\n{dump_json}\n{snap_json}\n{list_json}\n{defrag_json}\n{scrub_json}\n"
        ),
    );
}

#[test]
fn cert_051_structural_corruption_is_reported_by_fsck_and_repair() {
    let tmp = CertTemp::new("structural-corruption");
    let image = tmp.path("structural.img");
    mkfs::format_image(&image, CERT_CAPACITY, &FormatImageOptions::default()).expect("mkfs");
    let sb = dump::superblock(&image).expect("superblock before corruption");
    zero_range(
        &image,
        sb.secondary_superblock_block * BLOCK_SIZE,
        BLOCK_SIZE as usize,
    );

    let fsck_report = fsck::check_image(&image).expect("fsck with secondary missing");
    assert!(
        !fsck_report.issues.is_empty(),
        "missing secondary superblock should be reported"
    );
    let repair_report = repair::repair_image(&image).expect("repair stale/missing secondary");
    assert!(repair_report.fully_repaired);
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_051_structural_repair",
        &format!(
            "before={}\nafter={}\nclean_after=true\n",
            fsck_report.render_json(),
            repair_report.render_json()
        ),
    );
}

#[test]
fn cert_071_threaded_metadata_and_file_mutation_model() {
    let fs = Arc::new(Mutex::new(CoreFsService::format(
        CoreFsConfig::performance_profile(),
    )));
    fs.lock()
        .unwrap()
        .create_directory("/parallel")
        .expect("root dir");

    let mut handles = Vec::new();
    for worker in 0..4 {
        let fs = Arc::clone(&fs);
        handles.push(thread::spawn(move || {
            for index in 0..25 {
                let path = format!("/parallel/w{worker}-{index:02}.bin");
                let payload = deterministic_bytes((worker * 100 + index) as u64, 128);
                fs.lock()
                    .unwrap()
                    .create_file(&path, &payload, &[])
                    .expect("parallel create");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker joined");
    }

    let fs = fs.lock().unwrap();
    assert_eq!(fs.list_paths().len(), 101);
    assert_eq!(fs.read_file("/parallel/w3-24.bin").unwrap().len(), 128);
    assert_eq!(fs.scrub().invalid_blocks, 0);
    maybe_write_evidence(
        "cert_071_threaded_mutation",
        "workers=4\nfiles_per_worker=25\nexpected_paths=101\nscrub_invalid_blocks=0\n",
    );
}

#[test]
fn cert_081_service_benchmark_profiles_have_measurable_output() {
    let cfg = BenchmarkConfig {
        profile: BenchmarkProfile::SmallFiles,
        file_count: 120,
        payload_size: 256,
        snapshot_count: 2,
        persist_runs: 1,
    };
    let result = run_benchmark(cfg).expect("service benchmark");
    assert_eq!(result.file_count, 120);
    assert!(result.total_bytes > 0);
    assert!(result.create_ops_per_sec() > 0.0);
    assert!(result.read_ops_per_sec() > 0.0);

    maybe_write_evidence(
        "cert_081_service_benchmark",
        &format!(
            "profile={}\nfile_count={}\npayload_size={}\ncreate_ms={}\nread_ms={}\nsnapshot_ms={}\nsave_ms={}\ncreate_ops_s={:.2}\nread_ops_s={:.2}\n",
            result.profile,
            result.file_count,
            result.payload_size,
            result.create_ms,
            result.read_ms,
            result.snapshot_ms,
            result.save_ms,
            result.create_ops_per_sec(),
            result.read_ops_per_sec(),
        ),
    );
}

#[test]
fn cert_090_cross_platform_command_manifest_is_current() {
    let manifest = include_str!("../MATRIX.md");
    assert!(manifest.contains("Windows"));
    assert!(manifest.contains("Linux"));
    assert!(manifest.contains("AnyOS"));
    assert!(manifest.contains("cargo test -p corefs-certification"));
    assert!(manifest.contains("cargo check -p corefs-core --no-default-features"));

    maybe_write_evidence("cert_090_cross_platform_manifest", manifest);
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn assert_active_file_extents_do_not_contain(
    image: &Path,
    sess: &OdfFileSession,
    path: &str,
    plaintext: &[u8],
) {
    let state = sess.service().persisted_state();
    let inode = sess.service().inode_for_path(path).expect("inode for path");
    let record = state
        .block_records
        .iter()
        .find(|record| record.inode == inode)
        .expect("block record for path");
    let mut file = OpenOptions::new()
        .read(true)
        .open(image)
        .expect("open image for extent inspection");
    let mut extent_bytes = Vec::new();
    for extent in &record.extents {
        let len = u64::from(extent.length_blocks) * BLOCK_SIZE;
        let mut buf = vec![0u8; len as usize];
        file.seek(SeekFrom::Start(extent.physical_block * BLOCK_SIZE))
            .expect("seek extent");
        file.read_exact(&mut buf).expect("read extent");
        extent_bytes.extend_from_slice(&buf);
    }
    assert!(
        !contains_subslice(&extent_bytes, plaintext),
        "active encrypted file extents must not contain plaintext for {path}"
    );
}

fn flip_byte(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open image for corruption");
    file.seek(SeekFrom::Start(offset)).expect("seek corruption");
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).expect("read byte to corrupt");
    byte[0] ^= 0x5A;
    file.seek(SeekFrom::Start(offset)).expect("seek rewrite");
    file.write_all(&byte).expect("write corrupted byte");
    file.sync_all().expect("sync corrupted byte");
}

fn zero_range(path: &Path, offset: u64, len: usize) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open image for zeroing");
    file.seek(SeekFrom::Start(offset)).expect("seek zero range");
    file.write_all(&vec![0; len]).expect("write zero range");
    file.sync_all().expect("sync zero range");
}

fn mib_per_sec(bytes: usize, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(0.000_001);
    (bytes as f64 / (1024.0 * 1024.0)) / secs
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_ms(name: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(default),
    )
}

fn assert_duration_under(name: &str, actual: Duration, max: Duration) {
    assert!(
        actual <= max,
        "{name} took {} ms, above gate {} ms",
        actual.as_millis(),
        max.as_millis()
    );
}

fn run_cli_ok(args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = dispatch(&args, &mut out, &mut err);
    assert_eq!(
        status,
        ExitStatus::Ok,
        "CLI failed: args={args:?} stderr={}",
        String::from_utf8_lossy(&err)
    );
    String::from_utf8(out).expect("cli stdout utf8")
}
