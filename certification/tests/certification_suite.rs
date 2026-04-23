mod common;

use common::{CertTemp, assert_clean_image, deterministic_bytes, maybe_write_evidence};
use corefs::config::CoreFsConfig;
use corefs::storage::ondisk::benchmark::{OdfBenchConfig, run_odf_bench};
use corefs::storage::ondisk::checksum::Crc32c;
use corefs::storage::ondisk::layout::{BLOCK_SIZE, FEATURE_COMPAT_REDUNDANT_SUPERBLOCKS};
use corefs::storage::ondisk::property;
use corefs::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
use corefs_cli::{ExitStatus, dispatch};
use corefs_core::domain::inode::InodeKind;
use corefs_tools::Report;
use corefs_tools::backup;
use corefs_tools::defrag;
use corefs_tools::dump;
use corefs_tools::mkfs::{self, FormatImageOptions, LayoutMode};
use corefs_tools::repair;
use corefs_tools::scrub::{self, ScrubMode};
use corefs_tools::snapshot::{self, CreateOptions};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
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
}

#[test]
fn cert_070_deterministic_property_matrix_across_seeds() {
    let quick_seeds = [1, 2, 3, 5, 8, 13, 21, 34];
    property::fuzz_many_seeds(&quick_seeds, 48).expect("property matrix");
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
