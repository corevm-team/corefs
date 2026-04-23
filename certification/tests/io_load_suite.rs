mod common;

use common::{CertTemp, assert_clean_image, deterministic_bytes, maybe_write_evidence};
use corefs::config::CoreFsConfig;
use corefs::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const IO_CAPACITY: u64 = 128 * 1024 * 1024;

#[derive(Debug, Default)]
struct WorkerStats {
    ops: usize,
    bytes: usize,
    snapshots: usize,
}

#[test]
fn cert_072_high_io_multi_access_serialized_odf_writers() {
    let tmp = CertTemp::new("high-io-writers");
    let image = tmp.path("high-io.img");
    let workers = env_usize("COREFS_CERT_IO_WORKERS", 6);
    let ops_per_worker = env_usize("COREFS_CERT_IO_OPS_PER_WORKER", 24);
    let batch_size = env_usize("COREFS_CERT_IO_BATCH_SIZE", 6).max(1);
    let payload_bytes = env_usize("COREFS_CERT_IO_PAYLOAD_BYTES", 8192);
    let min_mib_per_sec = env_f64("COREFS_CERT_MIN_IO_WRITE_MIB_S", 0.01);

    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = IO_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();
    opts.config.versioning.keep_latest = 8;

    let session = Arc::new(Mutex::new(
        OdfFileSession::format_new(&image, &opts).expect("format high-io image"),
    ));
    {
        let mut guard = session.lock().expect("session mutex");
        guard
            .mutate(|fs| {
                fs.create_directory("/load")?;
                for worker in 0..workers {
                    fs.create_directory(&format!("/load/w{worker:02}"))?;
                    fs.create_file(&format!("/load/w{worker:02}/hot.bin"), b"seed", &[])?;
                }
                Ok(())
            })
            .expect("seed worker directories");
    }

    let barrier = Arc::new(Barrier::new(workers));
    let start = Instant::now();
    let mut handles = Vec::with_capacity(workers);
    for worker in 0..workers {
        let session = Arc::clone(&session);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> Result<WorkerStats, String> {
            barrier.wait();
            let mut stats = WorkerStats::default();
            let mut op = 0;
            while op < ops_per_worker {
                let end = (op + batch_size).min(ops_per_worker);
                let (batch_stats, _) = session
                    .lock()
                    .expect("session mutex")
                    .mutate(|fs| {
                        let mut batch_stats = WorkerStats::default();
                        for current in op..end {
                            let len = payload_bytes + ((worker + current) % 5) * 1024;
                            let payload =
                                deterministic_bytes(((worker as u64) << 32) | current as u64, len);
                            let patch = deterministic_bytes(
                                0xFA57_1000 + worker as u64 + current as u64,
                                257,
                            );
                            let dir = format!("/load/w{worker:02}");
                            let tmp_path = format!("{dir}/file-{current:04}.tmp");
                            let final_path = format!("{dir}/file-{current:04}.bin");
                            let hot_path = format!("{dir}/hot.bin");

                            fs.create_file(&tmp_path, &payload, &[])?;
                            fs.write_file_range(&tmp_path, (payload.len() / 2) as u64, &patch)?;
                            fs.rename_entry(&tmp_path, &final_path)?;
                            if current % 3 == 0 {
                                fs.write_file(&hot_path, &payload)?;
                            }
                            if current % 11 == 0 {
                                fs.create_snapshot(&format!("worker-{worker:02}-op-{current:04}"));
                                batch_stats.snapshots += 1;
                            }

                            batch_stats.ops += 1;
                            batch_stats.bytes += payload.len()
                                + patch.len()
                                + if current % 3 == 0 { payload.len() } else { 0 };
                        }
                        Ok(batch_stats)
                    })
                    .map_err(|err| format!("{err:?}"))?;
                stats.ops += batch_stats.ops;
                stats.bytes += batch_stats.bytes;
                stats.snapshots += batch_stats.snapshots;
                op = end;
            }
            Ok(stats)
        }));
    }

    let mut total = WorkerStats::default();
    for handle in handles {
        let stats = handle
            .join()
            .expect("writer thread joined")
            .expect("writer ok");
        total.ops += stats.ops;
        total.bytes += stats.bytes;
        total.snapshots += stats.snapshots;
    }
    let elapsed = start.elapsed();
    let mib_per_sec = mib_per_sec(total.bytes, elapsed);
    assert!(
        mib_per_sec >= min_mib_per_sec,
        "high-IO write throughput {mib_per_sec:.2} MiB/s below gate {min_mib_per_sec:.2} MiB/s"
    );

    {
        let guard = session.lock().expect("session mutex");
        let paths = guard.service().list_paths();
        let expected_paths = 1 + workers + workers + workers * ops_per_worker;
        assert_eq!(paths.len(), expected_paths);
        for worker in 0..workers {
            let sample = format!("/load/w{worker:02}/file-{:04}.bin", ops_per_worker - 1);
            assert!(
                guard
                    .service()
                    .read_file(&sample)
                    .expect("sample read")
                    .len()
                    >= payload_bytes
            );
        }
    }
    drop(session);

    assert_clean_image(&image);
    let reopened = OdfFileSession::open(&image).expect("reopen high-io image");
    assert_eq!(
        reopened.service().list_paths().len(),
        1 + workers + workers + workers * ops_per_worker
    );

    maybe_write_evidence(
        "cert_072_high_io_multi_access_writers",
        &format!(
            "workers={workers}\nops_per_worker={ops_per_worker}\nbatch_size={batch_size}\ntotal_ops={}\nlogical_bytes={}\nelapsed_ms={}\nthroughput_mib_s={:.2}\nsnapshots={}\nfsck_clean=true\nreopen_paths={}\n",
            total.ops,
            total.bytes,
            elapsed.as_millis(),
            mib_per_sec,
            total.snapshots,
            reopened.service().list_paths().len(),
        ),
    );
}

#[test]
fn cert_073_parallel_reopen_readers_under_io_load_image() {
    let tmp = CertTemp::new("parallel-readers");
    let image = tmp.path("readers.img");
    let reader_workers = env_usize("COREFS_CERT_IO_READERS", 8);
    let file_count = env_usize("COREFS_CERT_IO_READ_FILES", 96);
    let payload_bytes = env_usize("COREFS_CERT_IO_READ_PAYLOAD_BYTES", 4096);
    let min_mib_per_sec = env_f64("COREFS_CERT_MIN_IO_READ_MIB_S", 0.5);

    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = IO_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();
    {
        let mut session = OdfFileSession::format_new(&image, &opts).expect("format reader image");
        session
            .mutate(|fs| {
                fs.create_directory("/read-set")?;
                for index in 0..file_count {
                    fs.create_file(
                        &format!("/read-set/file-{index:04}.bin"),
                        &deterministic_bytes(index as u64, payload_bytes),
                        &[],
                    )?;
                }
                Ok(())
            })
            .expect("seed read files");
    }
    assert_clean_image(&image);

    let image = Arc::new(image);
    let barrier = Arc::new(Barrier::new(reader_workers));
    let start = Instant::now();
    let mut handles = Vec::with_capacity(reader_workers);
    for worker in 0..reader_workers {
        let image: Arc<PathBuf> = Arc::clone(&image);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> Result<usize, String> {
            barrier.wait();
            let session = OdfFileSession::open(&*image).map_err(|err| format!("{err:?}"))?;
            let mut bytes_read = 0usize;
            for index in 0..file_count {
                let path = format!("/read-set/file-{index:04}.bin");
                let bytes = session
                    .service()
                    .read_file(&path)
                    .map_err(|err| format!("{err:?}"))?;
                let expected = deterministic_bytes(index as u64, payload_bytes);
                if bytes != expected {
                    return Err(format!(
                        "reader {worker} observed corrupted payload at {path}"
                    ));
                }
                bytes_read += bytes.len();
            }
            Ok(bytes_read)
        }));
    }

    let mut total_read = 0usize;
    for handle in handles {
        total_read += handle
            .join()
            .expect("reader thread joined")
            .expect("reader ok");
    }
    let elapsed = start.elapsed();
    let mib_per_sec = mib_per_sec(total_read, elapsed);
    assert!(
        mib_per_sec >= min_mib_per_sec,
        "parallel read throughput {mib_per_sec:.2} MiB/s below gate {min_mib_per_sec:.2} MiB/s"
    );

    maybe_write_evidence(
        "cert_073_parallel_reopen_readers",
        &format!(
            "readers={reader_workers}\nfile_count={file_count}\npayload_bytes={payload_bytes}\ntotal_read_bytes={total_read}\nelapsed_ms={}\nthroughput_mib_s={:.2}\nfsck_clean=true\n",
            elapsed.as_millis(),
            mib_per_sec,
        ),
    );
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

fn mib_per_sec(bytes: usize, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(0.001);
    (bytes as f64 / (1024.0 * 1024.0)) / secs
}
