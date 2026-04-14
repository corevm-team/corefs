use super::*;

#[test]
fn benchmark_runs_and_produces_metrics() {
    let result = run_benchmark(BenchmarkConfig {
        profile: BenchmarkProfile::Balanced,
        file_count: 4,
        payload_size: 32,
        snapshot_count: 2,
        persist_runs: 2,
    })
    .expect("benchmark should succeed");

    assert_eq!(result.profile, "balanced");
    assert_eq!(result.file_count, 4);
    assert_eq!(result.payload_size, 32);
    assert_eq!(result.snapshot_count, 2);
    assert_eq!(result.persist_runs, 2);
    assert_eq!(result.total_bytes, 128);
    assert!(result.timestamp_unix_ms > 0);
}

#[test]
fn profile_configs_expose_different_load_shapes() {
    let small = BenchmarkConfig::from_profile(BenchmarkProfile::SmallFiles);
    let metadata = BenchmarkConfig::from_profile(BenchmarkProfile::MetadataHeavy);
    let snapshot = BenchmarkConfig::from_profile(BenchmarkProfile::SnapshotHeavy);
    let persist = BenchmarkConfig::from_profile(BenchmarkProfile::PersistHeavy);

    assert!(small.payload_size < 1024);
    assert!(metadata.file_count > small.file_count);
    assert!(snapshot.snapshot_count > 1);
    assert!(persist.persist_runs > 1);
}

#[test]
fn profile_parser_handles_known_and_unknown_profiles() {
    assert_eq!(
        BenchmarkProfile::from_str("small-files").expect("profile should parse"),
        BenchmarkProfile::SmallFiles
    );
    assert!(matches!(
        BenchmarkProfile::from_str("unknown"),
        Err(CoreFsError::InvalidInput(_))
    ));
}

#[test]
fn benchmark_log_is_written_in_markdown() {
    let path = std::env::temp_dir().join(format!(
        "corefs-perf-log-{}-{}.md",
        std::process::id(),
        now_unix_ms()
    ));
    let result = BenchmarkResult {
        timestamp_unix_ms: 1,
        profile: "balanced".to_string(),
        file_count: 10,
        payload_size: 64,
        snapshot_count: 1,
        persist_runs: 1,
        create_ms: 2,
        read_ms: 3,
        snapshot_ms: 1,
        save_ms: 4,
        total_bytes: 640,
    };

    append_benchmark_markdown(&path, &result).expect("markdown log should be written");
    let contents = std::fs::read_to_string(&path).expect("log should exist");
    assert!(contents.contains("# CoreFS Performance Log"));
    assert!(contents.contains("| Timestamp | Profile |"));
    assert!(contents.contains("1970-01-01 00:00:00 UTC"));
    assert!(contents.contains("| balanced | 10 | 64 | 1 | 1 | 2 | 3 | 1 | 4 |"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn human_timestamp_is_rendered_readably() {
    assert_eq!(human_timestamp(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(human_timestamp(86_400_000), "1970-01-02 00:00:00 UTC");
}
