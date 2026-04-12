use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::error::{CoreFsError, CoreFsResult};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchmarkProfile {
    Balanced,
    SmallFiles,
    MetadataHeavy,
    SnapshotHeavy,
    PersistHeavy,
}

impl BenchmarkProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::SmallFiles => "small-files",
            Self::MetadataHeavy => "metadata-heavy",
            Self::SnapshotHeavy => "snapshot-heavy",
            Self::PersistHeavy => "persist-heavy",
        }
    }

    pub fn from_str(value: &str) -> CoreFsResult<Self> {
        match value {
            "balanced" => Ok(Self::Balanced),
            "small-files" => Ok(Self::SmallFiles),
            "metadata-heavy" => Ok(Self::MetadataHeavy),
            "snapshot-heavy" => Ok(Self::SnapshotHeavy),
            "persist-heavy" => Ok(Self::PersistHeavy),
            other => Err(CoreFsError::InvalidInput(format!(
                "unknown benchmark profile: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkConfig {
    pub profile: BenchmarkProfile,
    pub file_count: usize,
    pub payload_size: usize,
    pub snapshot_count: usize,
    pub persist_runs: usize,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            profile: BenchmarkProfile::Balanced,
            file_count: 250,
            payload_size: 4096,
            snapshot_count: 1,
            persist_runs: 1,
        }
    }
}

impl BenchmarkConfig {
    pub fn from_profile(profile: BenchmarkProfile) -> Self {
        match profile {
            BenchmarkProfile::Balanced => Self::default(),
            BenchmarkProfile::SmallFiles => Self {
                profile,
                file_count: 2000,
                payload_size: 256,
                snapshot_count: 1,
                persist_runs: 1,
            },
            BenchmarkProfile::MetadataHeavy => Self {
                profile,
                file_count: 5000,
                payload_size: 64,
                snapshot_count: 1,
                persist_runs: 1,
            },
            BenchmarkProfile::SnapshotHeavy => Self {
                profile,
                file_count: 400,
                payload_size: 1024,
                snapshot_count: 10,
                persist_runs: 1,
            },
            BenchmarkProfile::PersistHeavy => Self {
                profile,
                file_count: 800,
                payload_size: 4096,
                snapshot_count: 2,
                persist_runs: 5,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkResult {
    pub timestamp_unix_ms: u128,
    pub profile: String,
    pub file_count: usize,
    pub payload_size: usize,
    pub snapshot_count: usize,
    pub persist_runs: usize,
    pub create_ms: u128,
    pub read_ms: u128,
    pub snapshot_ms: u128,
    pub save_ms: u128,
    pub total_bytes: usize,
}

impl BenchmarkResult {
    pub fn timestamp_human(&self) -> String {
        human_timestamp(self.timestamp_unix_ms)
    }

    pub fn create_ops_per_sec(&self) -> f64 {
        ops_per_sec(self.file_count, self.create_ms)
    }

    pub fn read_ops_per_sec(&self) -> f64 {
        ops_per_sec(self.file_count, self.read_ms)
    }

    pub fn write_mib(&self) -> f64 {
        mib(self.total_bytes)
    }

    pub fn to_markdown_row(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.2} | {:.2} | {:.2} |",
            self.timestamp_human(),
            self.profile,
            self.file_count,
            self.payload_size,
            self.snapshot_count,
            self.persist_runs,
            self.create_ms,
            self.read_ms,
            self.snapshot_ms,
            self.save_ms,
            self.write_mib(),
            self.create_ops_per_sec(),
            self.read_ops_per_sec()
        )
    }
}

pub fn run_benchmark(config: BenchmarkConfig) -> CoreFsResult<BenchmarkResult> {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/bench")?;

    let payload = vec![b'x'; config.payload_size];
    let total_bytes = config.file_count.saturating_mul(config.payload_size);

    let create_start = Instant::now();
    for index in 0..config.file_count {
        let path = format!("/bench/file-{index:05}.bin");
        fs.create_file(&path, &payload, &["bench".to_string()])?;
    }
    let create_ms = create_start.elapsed().as_millis();

    let read_start = Instant::now();
    for index in 0..config.file_count {
        let path = format!("/bench/file-{index:05}.bin");
        let _ = fs.read_file(&path)?;
    }
    let read_ms = read_start.elapsed().as_millis();

    let snapshot_start = Instant::now();
    for index in 0..config.snapshot_count {
        let _ = fs.create_snapshot(&format!("benchmark-{index:03}"));
    }
    let snapshot_ms = snapshot_start.elapsed().as_millis();

    let state_path = std::env::temp_dir().join(format!(
        "corefs-benchmark-{}-{}.json",
        std::process::id(),
        now_unix_ms()
    ));

    let save_start = Instant::now();
    for _ in 0..config.persist_runs {
        fs.save_to_path(&state_path)?;
    }
    let save_ms = save_start.elapsed().as_millis();

    let _ = std::fs::remove_file(state_path);

    Ok(BenchmarkResult {
        timestamp_unix_ms: now_unix_ms(),
        profile: config.profile.as_str().to_string(),
        file_count: config.file_count,
        payload_size: config.payload_size,
        snapshot_count: config.snapshot_count,
        persist_runs: config.persist_runs,
        create_ms,
        read_ms,
        snapshot_ms,
        save_ms,
        total_bytes,
    })
}

pub fn append_benchmark_markdown(
    path: impl AsRef<Path>,
    result: &BenchmarkResult,
) -> CoreFsResult<()> {
    let path = path.as_ref();
    let exists = path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            CoreFsError::State(format!(
                "failed to open benchmark log {}: {error}",
                path.display()
            ))
        })?;

    if !exists {
        writeln!(file, "# CoreFS Performance Log\n")
            .and_then(|_| {
                writeln!(
                    file,
                    "| Timestamp | Profile | Files | Payload (B) | Snapshots | Saves | Create (ms) | Read (ms) | Snapshot (ms) | Save (ms) | MiB | Create ops/s | Read ops/s |"
                )
            })
            .and_then(|_| {
                writeln!(
                    file,
                    "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
                )
            })
            .map_err(|error| {
                CoreFsError::State(format!(
                    "failed to initialize benchmark log {}: {error}",
                    path.display()
                ))
            })?;
    }

    writeln!(file, "{}", result.to_markdown_row()).map_err(|error| {
        CoreFsError::State(format!(
            "failed to append benchmark log {}: {error}",
            path.display()
        ))
    })?;

    Ok(())
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_millis()
}

fn human_timestamp(timestamp_unix_ms: u128) -> String {
    let total_seconds = (timestamp_unix_ms / 1000) as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m, d)
}

fn ops_per_sec(ops: usize, duration_ms: u128) -> f64 {
    if duration_ms == 0 {
        ops as f64
    } else {
        (ops as f64 / duration_ms as f64) * 1000.0
    }
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
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
}
