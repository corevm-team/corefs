mod common;

use common::{deterministic_bytes, maybe_write_evidence};
use corefs::app::CoreFsService;
use corefs::config::CoreFsConfig;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static SCALE_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn cert_140_single_directory_many_files_portable_load() {
    let _guard = SCALE_LOCK.lock().expect("scale test lock");
    let file_count = env_usize("COREFS_CERT_MASS_FILES", 10_000);
    run_single_directory_file_load(file_count, "cert_140_single_directory_many_files");
}

#[test]
#[ignore = "heavy lab certification: creates more than 100,000 files in one directory"]
fn cert_144_heavy_single_directory_over_100k_files_lab_load() {
    let _guard = SCALE_LOCK.lock().expect("scale test lock");
    let file_count = env_usize("COREFS_CERT_HEAVY_MASS_FILES", 100_001).max(100_001);
    run_single_directory_file_load(
        file_count,
        "cert_144_heavy_single_directory_over_100k_files",
    );
}

fn run_single_directory_file_load(file_count: usize, evidence_id: &str) {
    let min_creates_per_sec = env_f64("COREFS_CERT_MIN_MASS_CREATE_OPS_S", 1.0);
    let mut fs = CoreFsService::format(CoreFsConfig::performance_profile());

    fs.create_directory("/wide").expect("wide directory");
    let start = Instant::now();
    for index in 0..file_count {
        let payload = [(index & 0xff) as u8];
        fs.create_file(&format!("/wide/file-{index:06}.dat"), &payload, &[])
            .expect("mass file create");
    }
    let elapsed = start.elapsed();
    let create_ops_per_sec = ops_per_sec(file_count, elapsed);
    assert!(
        create_ops_per_sec >= min_creates_per_sec,
        "mass create rate {create_ops_per_sec:.2} ops/s below gate {min_creates_per_sec:.2}"
    );

    let paths = fs.list_paths();
    assert_eq!(paths.len(), file_count + 1);
    assert_eq!(fs.stats().files, file_count + 1);
    assert_eq!(fs.read_file("/wide/file-000000.dat").unwrap(), [0]);
    assert_eq!(
        fs.read_file(&format!("/wide/file-{:06}.dat", file_count - 1))
            .unwrap(),
        [((file_count - 1) & 0xff) as u8]
    );
    assert!(
        fs.create_file("/wide/file-000000.dat", b"duplicate", &[])
            .is_err()
    );
    assert_service_clean(&fs);

    maybe_write_evidence(
        evidence_id,
        &format!(
            "files_in_one_directory={file_count}\npaths={}\nelapsed_ms={}\ncreate_ops_per_sec={create_ops_per_sec:.2}\nfsck_clean=true\n",
            paths.len(),
            elapsed.as_millis(),
        ),
    );
}

#[test]
fn cert_141_many_folders_breadth_depth_inventory_load() {
    let _guard = SCALE_LOCK.lock().expect("scale test lock");
    let flat_dirs = env_usize("COREFS_CERT_MASS_DIRS", 2_500);
    let deep_dirs = env_usize("COREFS_CERT_DEEP_DIRS", 128);
    let min_dirs_per_sec = env_f64("COREFS_CERT_MIN_DIR_CREATE_OPS_S", 1.0);
    let mut fs = CoreFsService::format(CoreFsConfig::performance_profile());

    fs.create_directory("/folders").expect("folder root");
    let start = Instant::now();
    for index in 0..flat_dirs {
        fs.create_directory(&format!("/folders/d-{index:06}"))
            .expect("flat dir create");
    }

    let mut path = String::from("/deep");
    fs.create_directory(&path).expect("deep root");
    for index in 0..deep_dirs {
        path.push_str(&format!("/d{index:03}"));
        fs.create_directory(&path).expect("deep dir create");
    }
    let leaf = format!("{path}/leaf.txt");
    fs.create_file(&leaf, b"leaf", &[]).expect("deep leaf");
    let elapsed = start.elapsed();
    let dir_ops = flat_dirs + deep_dirs + 2;
    let dir_ops_per_sec = ops_per_sec(dir_ops, elapsed);
    assert!(
        dir_ops_per_sec >= min_dirs_per_sec,
        "directory create rate {dir_ops_per_sec:.2} ops/s below gate {min_dirs_per_sec:.2}"
    );

    assert_eq!(fs.read_file(&leaf).unwrap(), b"leaf");
    assert_eq!(fs.stats().files, flat_dirs + deep_dirs + 3);
    assert_service_clean(&fs);

    maybe_write_evidence(
        "cert_141_many_folders_breadth_depth",
        &format!(
            "flat_directories={flat_dirs}\ndeep_directories={deep_dirs}\npaths={}\nelapsed_ms={}\ndir_ops_per_sec={dir_ops_per_sec:.2}\nfsck_clean=true\n",
            fs.stats().files,
            elapsed.as_millis(),
        ),
    );
}

#[test]
fn cert_142_very_large_file_range_truncate_throughput_load() {
    let _guard = SCALE_LOCK.lock().expect("scale test lock");
    let large_bytes = env_usize("COREFS_CERT_LARGE_FILE_BYTES", 16 * 1024 * 1024);
    let min_mib_per_sec = env_f64("COREFS_CERT_MIN_LARGE_FILE_MIB_S", 1.0);
    let mut fs = CoreFsService::format(CoreFsConfig::performance_profile());
    let payload = deterministic_bytes(0xC0DE_0142, large_bytes);

    let start = Instant::now();
    fs.create_file("/large.bin", &payload, &[])
        .expect("large file create");
    let write_elapsed = start.elapsed();
    let write_mib_per_sec = mib_per_sec(large_bytes, write_elapsed);
    assert!(
        write_mib_per_sec >= min_mib_per_sec,
        "large file write throughput {write_mib_per_sec:.2} MiB/s below gate {min_mib_per_sec:.2}"
    );

    let midpoint = (large_bytes / 2) as u64;
    fs.write_file_range("/large.bin", midpoint, b"RANGE-PATCH")
        .expect("large range write");
    assert_eq!(
        fs.read_file_range("/large.bin", midpoint, "RANGE-PATCH".len())
            .unwrap(),
        b"RANGE-PATCH"
    );
    fs.truncate_file("/large.bin", (large_bytes / 2) as u64)
        .expect("large shrink");
    assert_eq!(fs.read_file("/large.bin").unwrap().len(), large_bytes / 2);
    fs.truncate_file("/large.bin", (large_bytes / 2 + 4096) as u64)
        .expect("large grow");
    assert_eq!(
        fs.read_file_range("/large.bin", (large_bytes / 2) as u64, 4096)
            .unwrap(),
        vec![0; 4096]
    );
    assert_service_clean(&fs);

    maybe_write_evidence(
        "cert_142_very_large_file_range_truncate",
        &format!(
            "large_file_bytes={large_bytes}\nwrite_elapsed_ms={}\nwrite_mib_per_sec={write_mib_per_sec:.2}\nrange_patch=true\ntruncate_shrink=true\ntruncate_grow_zero_fill=true\nfsck_clean=true\n",
            write_elapsed.as_millis(),
        ),
    );
}

#[test]
fn cert_143_mass_dedup_identical_files_and_cow_clone_load() {
    let _guard = SCALE_LOCK.lock().expect("scale test lock");
    let identical_files = env_usize("COREFS_CERT_DEDUP_IDENTICAL_FILES", 1_000);
    let cow_clones = env_usize("COREFS_CERT_DEDUP_COW_CLONES", 1_000);
    let mut config = CoreFsConfig::performance_profile();
    config.performance.copy_on_write = true;
    config.performance.deduplication_enabled = true;
    let mut fs = CoreFsService::format(config);
    let payload = deterministic_bytes(0xDED0_0143, 4096);

    fs.create_directory("/dedup").expect("dedup root");
    fs.create_file("/dedup/base.bin", &payload, &[])
        .expect("dedup base");
    let start = Instant::now();
    for index in 0..identical_files {
        fs.create_file(&format!("/dedup/same-{index:06}.bin"), &payload, &[])
            .expect("identical file");
    }
    for index in 0..cow_clones {
        fs.clone_file("/dedup/base.bin", &format!("/dedup/clone-{index:06}.bin"))
            .expect("cow clone");
    }
    let load_elapsed = start.elapsed();
    let dedup_report = fs.run_dedup().expect("dedup pass");
    let cow = fs.cow_report();

    assert_eq!(dedup_report.hash_collisions, 0);
    assert_eq!(dedup_report.ref_count_mismatches, 0);
    assert!(
        cow.stats.max_ref_count >= identical_files + cow_clones + 1,
        "expected all identical payload owners to share one blob: {cow:?}"
    );
    assert!(cow.stats.bytes_saved_by_sharing >= 4096 * (identical_files + cow_clones));
    fs.write_file("/dedup/clone-000000.bin", b"diverged")
        .expect("clone divergence");
    assert_eq!(fs.read_file("/dedup/base.bin").unwrap(), payload);
    assert_eq!(
        fs.read_file("/dedup/clone-000000.bin").unwrap(),
        b"diverged"
    );
    assert_service_clean(&fs);

    maybe_write_evidence(
        "cert_143_mass_dedup_cow_clone",
        &format!(
            "identical_files={identical_files}\ncow_clones={cow_clones}\nelapsed_ms={}\nmax_ref_count={}\nbytes_saved_by_sharing={}\ndedup_hash_collisions={}\ndedup_ref_count_mismatches={}\nclone_divergence=true\nfsck_clean=true\n",
            load_elapsed.as_millis(),
            cow.stats.max_ref_count,
            cow.stats.bytes_saved_by_sharing,
            dedup_report.hash_collisions,
            dedup_report.ref_count_mismatches,
        ),
    );
}

fn assert_service_clean(fs: &CoreFsService) {
    let report = fs.fsck();
    assert_eq!(report.checked_inodes, report.valid_inodes, "{report:?}");
    assert!(report.orphaned_blocks.is_empty(), "{report:?}");
    assert!(report.missing_blocks.is_empty(), "{report:?}");
    assert!(report.size_mismatches.is_empty(), "{report:?}");
    assert!(report.ref_count_errors.is_empty(), "{report:?}");
    assert!(report.compression_errors.is_empty(), "{report:?}");
    assert!(report.encryption_errors.is_empty(), "{report:?}");
    assert!(report.checksum_failures.is_empty(), "{report:?}");
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(default)
}

fn ops_per_sec(ops: usize, elapsed: Duration) -> f64 {
    ops as f64 / elapsed.as_secs_f64().max(0.001)
}

fn mib_per_sec(bytes: usize, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(0.001);
    (bytes as f64 / (1024.0 * 1024.0)) / secs
}
