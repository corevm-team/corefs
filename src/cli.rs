use crate::app::CoreFsService;
use crate::config::{CoreFsConfig, StorageTier};
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::linux_fuse::{LinuxMountOptions, mount_volume};
use crate::platform::performance::{
    BenchmarkConfig, BenchmarkProfile, append_benchmark_markdown, run_benchmark,
};
use crate::services::integrity::IntegrityService;
use crate::storage::volume_session::VolumeSession;

pub fn run<I>(args: I) -> CoreFsResult<()>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args.len() <= 1 {
        print_usage();
        return Ok(());
    }

    let mut fs = bootstrap_demo_fs()?;

    match args[1].as_str() {
        "mkfs" => {
            println!("formatted volume {}", fs.volume_name());
        }
        "mkfs-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for mkfs-image".to_string())
            })?;
            let config = config_from_args(&args[3..])?;
            let bootstrap = args.iter().any(|arg| arg == "--bootstrap");
            let mut session = VolumeSession::format_new(path, config)?;
            if bootstrap {
                seed_enterprise_volume(session.service_mut())?;
                session.flush()?;
            }
            println!("formatted volume image {path}");
        }
        "status-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for status-image".to_string())
            })?;
            let session = VolumeSession::open(path)?;
            let report = session.service().admin_report();
            println!("image: {}", session.image_path().display());
            println!("volume: {}", report.volume.name);
            println!("block_size: {}", report.volume.block_size);
            println!("features: {}", report.volume.feature_flags.join(", "));
            println!("files: {}", report.stats.files);
            println!("snapshots: {}", report.stats.snapshots);
            println!("journal_entries: {}", report.stats.journal_entries);
        }
        "mount-image" => {
            let image = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image for mount-image".to_string())
            })?;
            let mountpoint = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing mountpoint for mount-image".to_string())
            })?;
            let options = mount_options_from_args(&args[4..])?;
            mount_volume(image, mountpoint, options)?;
        }
        "status" => {
            let report = fs.admin_report();
            println!("volume: {}", report.volume.name);
            println!("block_size: {}", report.volume.block_size);
            println!("features: {}", report.volume.feature_flags.join(", "));
            println!("files: {}", report.stats.files);
            println!("snapshots: {}", report.stats.snapshots);
            println!("journal_entries: {}", report.stats.journal_entries);
        }
        "ls" => {
            for path in fs.list_paths() {
                println!("{path}");
            }
        }
        "snapshot" => {
            let name = args.get(2).cloned().unwrap_or_else(|| "manual".to_string());
            let snapshot = fs.create_snapshot(&name);
            println!(
                "snapshot {} created for {} with {} paths",
                snapshot.name,
                snapshot.scope_root,
                snapshot.paths.len()
            );
        }
        "snapshot-tree" => {
            let name = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing name for snapshot-tree".to_string())
            })?;
            let root = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing root path for snapshot-tree".to_string())
            })?;
            let snapshot = fs.create_snapshot_for_subtree(name, root)?;
            println!(
                "snapshot {} created for {} with {} paths",
                snapshot.name,
                snapshot.scope_root,
                snapshot.paths.len()
            );
        }
        "scrub" => {
            let report = fs.scrub();
            println!(
                "scrub checked={} valid={} invalid={}",
                report.checked_paths, report.valid_blocks, report.invalid_blocks
            );
        }
        "delete" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for delete".to_string())
            })?;
            let secure = args.iter().any(|arg| arg == "--secure");
            fs.delete_file(path, secure)?;
            println!("deleted {path}");
        }
        "restore" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for restore".to_string())
            })?;
            fs.restore_file(path)?;
            println!("restored {path}");
        }
        "write" => {
            let path = args
                .get(2)
                .ok_or_else(|| CoreFsError::InvalidCommand("missing path for write".to_string()))?;
            let payload = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing payload for write".to_string())
            })?;
            fs.write_file(path, payload.as_bytes())?;
            println!("written {path}");
        }
        "read" => {
            let path = args
                .get(2)
                .ok_or_else(|| CoreFsError::InvalidCommand("missing path for read".to_string()))?;
            let bytes = fs.read_file(path)?;
            println!("{}", String::from_utf8_lossy(&bytes));
        }
        "versions" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for versions".to_string())
            })?;
            for version in fs.list_versions_for_path(path)? {
                println!(
                    "version_id={} timestamp={:?} bytes={}",
                    version.version_id,
                    version.created_at,
                    version.bytes.len()
                );
            }
        }
        "read-version" => {
            let selector = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing selector for read-version".to_string())
            })?;
            let bytes = fs.read_version_selector(selector)?;
            println!("{}", String::from_utf8_lossy(&bytes));
        }
        "tag-add" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for tag-add".to_string())
            })?;
            let tag = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing tag for tag-add".to_string())
            })?;
            fs.add_tag(path, tag)?;
            println!("tagged {path} with {tag}");
        }
        "tag-remove" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for tag-remove".to_string())
            })?;
            let tag = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing tag for tag-remove".to_string())
            })?;
            fs.remove_tag(path, tag)?;
            println!("removed tag {tag} from {path}");
        }
        "find-tag" => {
            let tag = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing tag for find-tag".to_string())
            })?;
            for path in fs.find_by_tag(tag) {
                println!("{path}");
            }
        }
        "attr-set" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for attr-set".to_string())
            })?;
            let key = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing key for attr-set".to_string())
            })?;
            let value = args.get(4).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing value for attr-set".to_string())
            })?;
            fs.set_attribute(path, key, value)?;
            println!("set attribute {key} on {path}");
        }
        "attr-get" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for attr-get".to_string())
            })?;
            let metadata = fs.metadata_for_path(path)?;
            println!("path: {}", metadata.path);
            println!("tier: {}", storage_tier_name(&metadata.storage_tier));
            println!("tags: {}", metadata.tags.join(", "));
            for (key, value) in metadata.attributes {
                println!("attr {key}={value}");
            }
        }
        "set-tier" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for set-tier".to_string())
            })?;
            let tier = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing tier for set-tier".to_string())
            })?;
            fs.set_storage_tier(path, parse_storage_tier(tier)?)?;
            println!("set storage tier for {path} to {tier}");
        }
        "quota" => {
            let report = fs.quota_report();
            println!(
                "quota files={}/{} bytes={}/{}",
                report.used_files,
                report
                    .max_files
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unlimited".to_string()),
                report.used_bytes,
                report
                    .max_bytes
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unlimited".to_string())
            );
        }
        "save" => {
            let path = args
                .get(2)
                .ok_or_else(|| CoreFsError::InvalidCommand("missing path for save".to_string()))?;
            fs.save_to_path(path)?;
            println!("saved state to {path}");
        }
        "save-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for save-image".to_string())
            })?;
            fs.save_image_to_path(path)?;
            println!("saved volume image to {path}");
        }
        "load" => {
            let path = args
                .get(2)
                .ok_or_else(|| CoreFsError::InvalidCommand("missing path for load".to_string()))?;
            let loaded = CoreFsService::load_from_path(path)?;
            let report = loaded.admin_report();
            println!("loaded volume: {}", report.volume.name);
            println!("files: {}", report.stats.files);
            println!("snapshots: {}", report.stats.snapshots);
            println!("journal_entries: {}", report.stats.journal_entries);
        }
        "load-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for load-image".to_string())
            })?;
            let loaded = CoreFsService::load_image_from_path(path)?;
            let report = loaded.admin_report();
            println!("loaded volume image: {}", report.volume.name);
            println!("files: {}", report.stats.files);
            println!("snapshots: {}", report.stats.snapshots);
            println!("journal_entries: {}", report.stats.journal_entries);
        }
        "fsck-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for fsck-image".to_string())
            })?;
            let report = IntegrityService.fsck_image(path)?;
            println!("fsck-image ok: {path}");
            println!("format_version: {}", report.format_version);
            println!("segment_count: {}", report.segment_count);
            println!("valid_superblocks: {}", report.valid_superblocks);
            println!("selected_generation: {}", report.selected_generation);
            println!(
                "checksums: directory={} payload={}",
                report.directory_checksum_valid, report.payload_checksum_valid
            );
            println!("block_descriptors: {}", report.block_descriptors);
        }
        "benchmark" => {
            let config = benchmark_config_from_args(&args[2..])?;
            let result = run_benchmark(config)?;
            println!("benchmark timestamp_ms={}", result.timestamp_unix_ms);
            println!(
                "profile={} files={} payload={} snapshots={} saves={}",
                result.profile,
                result.file_count,
                result.payload_size,
                result.snapshot_count,
                result.persist_runs
            );
            println!(
                "create_ms={} read_ms={} snapshot_ms={} save_ms={}",
                result.create_ms, result.read_ms, result.snapshot_ms, result.save_ms
            );
            println!(
                "throughput_mib={:.2} create_ops_s={:.2} read_ops_s={:.2}",
                result.write_mib(),
                result.create_ops_per_sec(),
                result.read_ops_per_sec()
            );
        }
        "benchmark-log" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for benchmark-log".to_string())
            })?;
            let config = benchmark_config_from_args(&args[3..])?;
            let result = run_benchmark(config)?;
            append_benchmark_markdown(path, &result)?;
            println!("benchmark written to {path}");
        }
        command => {
            return Err(CoreFsError::InvalidCommand(format!(
                "unknown command: {command}"
            )));
        }
    }

    Ok(())
}

fn bootstrap_demo_fs() -> CoreFsResult<CoreFsService> {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    seed_enterprise_volume(&mut fs)?;
    Ok(fs)
}

fn seed_enterprise_volume(fs: &mut CoreFsService) -> CoreFsResult<()> {
    fs.create_directory("/etc")?;
    fs.create_directory("/srv")?;
    fs.create_directory("/var")?;
    fs.create_directory("/srv/corefs")?;
    fs.create_file(
        "/etc/corefs.conf",
        b"volume=corefs\ncompression=on\nencryption=on\n",
        &["config".to_string(), "system".to_string()],
    )?;
    fs.create_file(
        "/var/readme.txt",
        b"CoreFS enterprise bootstrap",
        &["docs".to_string()],
    )?;
    fs.create_file(
        "/srv/corefs/welcome.txt",
        b"Mounted through CoreFS volume session",
        &["docs".to_string(), "bootstrap".to_string()],
    )?;
    fs.create_symlink("/etc/corefs-current", "/etc/corefs.conf")?;
    Ok(())
}

fn print_usage() {
    println!("corefs commands:");
    println!("  mkfs");
    println!("  mkfs-image <path> [--bootstrap] [--volume-name <name>] [--block-size <bytes>]");
    println!("  status-image <path>");
    println!(
        "  mount-image <image> <mountpoint> [--create] [--read-only] [--auto-unmount] [--threads <n>]"
    );
    println!("  status");
    println!("  ls");
    println!("  snapshot [name]");
    println!("  snapshot-tree <name> <root>");
    println!("  scrub");
    println!("  delete <path> [--secure]");
    println!("  restore <path>");
    println!("  write <path> <payload>");
    println!("  read <path>");
    println!("  versions <path>");
    println!("  read-version <path@latest|path@vN|path@YYYY-MM-DD-HH-MM-SS>");
    println!("  tag-add <path> <tag>");
    println!("  tag-remove <path> <tag>");
    println!("  find-tag <tag>");
    println!("  attr-set <path> <key> <value>");
    println!("  attr-get <path>");
    println!("  set-tier <path> <hot|warm|cold>");
    println!("  quota");
    println!("  save <path>");
    println!("  save-image <path>");
    println!("  load <path>");
    println!("  load-image <path>");
    println!("  fsck-image <path>");
    println!(
        "  benchmark [--profile <name>] [--files <n>] [--payload <bytes>] [--snapshots <n>] [--saves <n>]"
    );
    println!(
        "  benchmark-log <path> [--profile <name>] [--files <n>] [--payload <bytes>] [--snapshots <n>] [--saves <n>]"
    );
    println!(
        "  profiles: balanced | small-files | metadata-heavy | snapshot-heavy | persist-heavy"
    );
}

fn benchmark_config_from_args(args: &[String]) -> CoreFsResult<BenchmarkConfig> {
    let mut config = BenchmarkConfig::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CoreFsError::InvalidCommand("missing value for --profile".to_string())
                })?;
                config = BenchmarkConfig::from_profile(BenchmarkProfile::from_str(value)?);
                index += 2;
            }
            "--files" => {
                config.file_count = parse_usize_flag(args, index, "--files")?;
                index += 2;
            }
            "--payload" => {
                config.payload_size = parse_usize_flag(args, index, "--payload")?;
                index += 2;
            }
            "--snapshots" => {
                config.snapshot_count = parse_usize_flag(args, index, "--snapshots")?;
                index += 2;
            }
            "--saves" => {
                config.persist_runs = parse_usize_flag(args, index, "--saves")?;
                index += 2;
            }
            other => {
                return Err(CoreFsError::InvalidCommand(format!(
                    "unknown benchmark option: {other}"
                )));
            }
        }
    }

    Ok(config)
}

fn config_from_args(args: &[String]) -> CoreFsResult<CoreFsConfig> {
    let mut config = CoreFsConfig::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--bootstrap" => {
                index += 1;
            }
            "--volume-name" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CoreFsError::InvalidCommand("missing value for --volume-name".to_string())
                })?;
                config.volume_name = value.clone();
                index += 2;
            }
            "--block-size" => {
                config.block_size = parse_usize_flag(args, index, "--block-size")?;
                index += 2;
            }
            other => {
                return Err(CoreFsError::InvalidCommand(format!(
                    "unknown mkfs-image option: {other}"
                )));
            }
        }
    }

    Ok(config)
}

fn mount_options_from_args(args: &[String]) -> CoreFsResult<LinuxMountOptions> {
    let mut options = LinuxMountOptions::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--create" => {
                options.create_if_missing = true;
                index += 1;
            }
            "--read-only" => {
                options.read_only = true;
                index += 1;
            }
            "--auto-unmount" => {
                options.auto_unmount = true;
                index += 1;
            }
            "--threads" => {
                options.threads = parse_usize_flag(args, index, "--threads")?;
                index += 2;
            }
            other => {
                return Err(CoreFsError::InvalidCommand(format!(
                    "unknown mount-image option: {other}"
                )));
            }
        }
    }

    Ok(options)
}

fn parse_usize_flag(args: &[String], index: usize, flag: &str) -> CoreFsResult<usize> {
    let value = args
        .get(index + 1)
        .ok_or_else(|| CoreFsError::InvalidCommand(format!("missing value for {flag}")))?;
    value.parse::<usize>().map_err(|error| {
        CoreFsError::InvalidInput(format!("invalid numeric value for {flag}: {error}"))
    })
}

fn parse_storage_tier(value: &str) -> CoreFsResult<StorageTier> {
    match value {
        "hot" => Ok(StorageTier::Hot),
        "warm" => Ok(StorageTier::Warm),
        "cold" => Ok(StorageTier::Cold),
        other => Err(CoreFsError::InvalidInput(format!(
            "unknown storage tier: {other}"
        ))),
    }
}

fn storage_tier_name(tier: &StorageTier) -> &'static str {
    match tier {
        StorageTier::Hot => "hot",
        StorageTier::Warm => "warm",
        StorageTier::Cold => "cold",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str, extension: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "corefs-cli-{name}-{}-{}.{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time should be after unix epoch")
                    .as_nanos(),
                extension
            ))
            .display()
            .to_string()
    }

    #[test]
    fn cli_without_command_returns_ok() {
        let result = run(vec!["corefs".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn cli_supports_successful_commands() {
        let fsck_image_path = temp_path("fsck", "img");
        let fs = bootstrap_demo_fs().expect("bootstrap should succeed");
        fs.save_image_to_path(&fsck_image_path)
            .expect("image should be saved");

        let successful = [
            vec!["corefs".to_string(), "mkfs".to_string()],
            vec!["corefs".to_string(), "status".to_string()],
            vec!["corefs".to_string(), "ls".to_string()],
            vec![
                "corefs".to_string(),
                "snapshot".to_string(),
                "nightly".to_string(),
            ],
            vec!["corefs".to_string(), "scrub".to_string()],
            vec![
                "corefs".to_string(),
                "write".to_string(),
                "/etc/corefs.conf".to_string(),
                "updated".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "read".to_string(),
                "/etc/corefs.conf".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "save".to_string(),
                std::env::temp_dir()
                    .join("corefs-cli-save.json")
                    .display()
                    .to_string(),
            ],
            vec![
                "corefs".to_string(),
                "save-image".to_string(),
                temp_path("save", "img"),
            ],
            vec![
                "corefs".to_string(),
                "fsck-image".to_string(),
                fsck_image_path.clone(),
            ],
            vec!["corefs".to_string(), "benchmark".to_string()],
            vec![
                "corefs".to_string(),
                "benchmark".to_string(),
                "--profile".to_string(),
                "small-files".to_string(),
                "--files".to_string(),
                "16".to_string(),
                "--payload".to_string(),
                "128".to_string(),
                "--snapshots".to_string(),
                "2".to_string(),
                "--saves".to_string(),
                "2".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "benchmark-log".to_string(),
                std::env::temp_dir()
                    .join("corefs-cli-benchmark.md")
                    .display()
                    .to_string(),
                "--profile".to_string(),
                "persist-heavy".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "delete".to_string(),
                "/var/readme.txt".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "delete".to_string(),
                "/var/readme.txt".to_string(),
                "--secure".to_string(),
            ],
        ];

        for args in successful {
            assert!(run(args).is_ok());
        }

        let _ = fs::remove_file(fsck_image_path);
    }

    #[test]
    fn cli_returns_errors_for_invalid_commands_and_missing_arguments() {
        let invalid = run(vec!["corefs".to_string(), "nope".to_string()]);
        assert!(matches!(invalid, Err(CoreFsError::InvalidCommand(_))));

        let delete = run(vec!["corefs".to_string(), "delete".to_string()]);
        assert!(matches!(delete, Err(CoreFsError::InvalidCommand(_))));

        let restore = run(vec!["corefs".to_string(), "restore".to_string()]);
        assert!(matches!(restore, Err(CoreFsError::InvalidCommand(_))));

        let write_path = run(vec!["corefs".to_string(), "write".to_string()]);
        assert!(matches!(write_path, Err(CoreFsError::InvalidCommand(_))));

        let write_payload = run(vec![
            "corefs".to_string(),
            "write".to_string(),
            "/etc/corefs.conf".to_string(),
        ]);
        assert!(matches!(write_payload, Err(CoreFsError::InvalidCommand(_))));

        let read = run(vec!["corefs".to_string(), "read".to_string()]);
        assert!(matches!(read, Err(CoreFsError::InvalidCommand(_))));

        let save = run(vec!["corefs".to_string(), "save".to_string()]);
        assert!(matches!(save, Err(CoreFsError::InvalidCommand(_))));

        let load = run(vec!["corefs".to_string(), "load".to_string()]);
        assert!(matches!(load, Err(CoreFsError::InvalidCommand(_))));

        let save_image = run(vec!["corefs".to_string(), "save-image".to_string()]);
        assert!(matches!(save_image, Err(CoreFsError::InvalidCommand(_))));

        let load_image = run(vec!["corefs".to_string(), "load-image".to_string()]);
        assert!(matches!(load_image, Err(CoreFsError::InvalidCommand(_))));

        let fsck_image = run(vec!["corefs".to_string(), "fsck-image".to_string()]);
        assert!(matches!(fsck_image, Err(CoreFsError::InvalidCommand(_))));

        let benchmark_log = run(vec!["corefs".to_string(), "benchmark-log".to_string()]);
        assert!(matches!(benchmark_log, Err(CoreFsError::InvalidCommand(_))));

        let benchmark_profile = run(vec![
            "corefs".to_string(),
            "benchmark".to_string(),
            "--profile".to_string(),
        ]);
        assert!(matches!(
            benchmark_profile,
            Err(CoreFsError::InvalidCommand(_))
        ));

        let benchmark_value = run(vec![
            "corefs".to_string(),
            "benchmark".to_string(),
            "--files".to_string(),
            "abc".to_string(),
        ]);
        assert!(matches!(benchmark_value, Err(CoreFsError::InvalidInput(_))));
    }

    #[test]
    fn bootstrap_demo_fs_creates_expected_layout() {
        let fs = bootstrap_demo_fs().expect("bootstrap should succeed");
        let paths = fs.list_paths();

        assert!(paths.iter().any(|path| path == "/etc"));
        assert!(paths.iter().any(|path| path == "/var"));
        assert!(paths.iter().any(|path| path == "/etc/corefs.conf"));
        assert!(paths.iter().any(|path| path == "/var/readme.txt"));
        assert!(paths.iter().any(|path| path == "/etc/corefs-current"));
    }

    #[test]
    fn benchmark_config_parser_accepts_overrides() {
        let args = vec![
            "--profile".to_string(),
            "snapshot-heavy".to_string(),
            "--files".to_string(),
            "10".to_string(),
            "--payload".to_string(),
            "512".to_string(),
            "--snapshots".to_string(),
            "3".to_string(),
            "--saves".to_string(),
            "2".to_string(),
        ];

        let config = benchmark_config_from_args(&args).expect("config should parse");

        assert_eq!(config.profile, BenchmarkProfile::SnapshotHeavy);
        assert_eq!(config.file_count, 10);
        assert_eq!(config.payload_size, 512);
        assert_eq!(config.snapshot_count, 3);
        assert_eq!(config.persist_runs, 2);
    }
}
