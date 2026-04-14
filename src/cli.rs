// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::error::{CoreFsError, CoreFsResult};
#[cfg(target_os = "linux")]
use crate::platform::diagnostics;
#[cfg(target_os = "linux")]
use crate::platform::linux_fuse;
#[cfg(target_os = "linux")]
use crate::platform::linux_fuse::LinuxMountOptions;
use crate::platform::performance::{
    BenchmarkConfig, BenchmarkProfile, append_benchmark_markdown, run_benchmark,
};
use crate::services::integrity::IntegrityService;

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
        "status" => {
            let report = fs.admin_report();
            let fragmentation = fs.fragmentation_report();
            println!("volume: {}", report.volume.name);
            println!("block_size: {}", report.volume.block_size);
            println!("features: {}", report.volume.feature_flags.join(", "));
            println!("files: {}", report.stats.files);
            println!("snapshots: {}", report.stats.snapshots);
            println!("journal_entries: {}", report.stats.journal_entries);
            println!(
                "fragmentation: {}% free_extents={} total_free_blocks={}",
                fragmentation.fragmentation_percent,
                fragmentation.free_extents,
                fragmentation.total_free_blocks
            );
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
                "snapshot {} created with {} paths",
                snapshot.name,
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
        "defrag" => {
            let report = fs.defragment();
            println!(
                "defrag moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                report.moved_entries, report.reclaimed_gaps, report.final_device_blocks
            );
        }
        "optimize" => {
            let report = fs.optimize_storage();
            println!(
                "optimize before={} after={} heat_reallocated={} compacted={}",
                report.before.fragmentation_percent,
                report.after.fragmentation_percent,
                report.heat_reallocation.is_some(),
                report.defragmentation.is_some()
            );
            if let Some(heat) = report.heat_reallocation {
                println!(
                    "prioritized_inodes={} promoted_hot_inodes={} moved_entries={} final_device_blocks={}",
                    heat.prioritized_inodes,
                    heat.promoted_hot_inodes,
                    heat.moved_entries,
                    heat.final_device_blocks
                );
            }
            if let Some(defrag) = report.defragmentation {
                println!(
                    "moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                    defrag.moved_entries, defrag.reclaimed_gaps, defrag.final_device_blocks
                );
            }
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
        "save-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for save-image".to_string())
            })?;
            fs.save_image_to_path(path)?;
            println!("saved volume image to {path}");
        }
        "mkfs-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for mkfs-image".to_string())
            })?;
            let include_demo = args.iter().any(|arg| arg == "--demo");
            #[cfg(target_os = "linux")]
            {
                linux_fuse::create_image(path, include_demo)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = include_demo;
                return Err(CoreFsError::InvalidCommand(
                    "mkfs-image is only available on Linux builds".to_string(),
                ));
            }
            println!("created CoreFS image at {path}");
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
            if args.iter().any(|arg| arg == "--repair") {
                let repaired = IntegrityService.repair_image(path)?;
                println!("fsck-image repaired: {path}");
                println!("repaired_superblocks: {}", repaired.repaired_superblocks);
                println!("selected_generation: {}", repaired.selected_generation);
                println!(
                    "resulting_valid_superblocks: {}",
                    repaired.resulting_valid_superblocks
                );
                println!(
                    "layout_repair: recovered_without_valid_superblock={} reconstructed_segment_directory={} reconstructed_block_descriptors={}",
                    repaired.recovered_without_valid_superblock,
                    repaired.reconstructed_segment_directory,
                    repaired.reconstructed_block_descriptors
                );
                println!(
                    "journal_repair: moved_to_deleted={} restored_to_active={} purged_deleted={} removed_orphan_blocks={} resized_inodes={} snapshot_id_adjusted={}",
                    repaired.moved_to_deleted,
                    repaired.restored_to_active,
                    repaired.purged_deleted,
                    repaired.removed_orphan_blocks,
                    repaired.resized_inodes,
                    repaired.snapshot_id_adjusted
                );
            } else {
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
        }
        "defrag-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for defrag-image".to_string())
            })?;
            let mut loaded = CoreFsService::load_image_from_path(path)?;
            let report = loaded.defragment();
            loaded.save_image_to_path(path)?;
            println!("defrag-image ok: {path}");
            println!(
                "moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                report.moved_entries, report.reclaimed_gaps, report.final_device_blocks
            );
        }
        "optimize-image" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for optimize-image".to_string())
            })?;
            let mut loaded = CoreFsService::load_image_from_path(path)?;
            let report = loaded.optimize_storage();
            loaded.save_image_to_path(path)?;
            println!("optimize-image ok: {path}");
            println!(
                "before={} after={} heat_reallocated={} compacted={}",
                report.before.fragmentation_percent,
                report.after.fragmentation_percent,
                report.heat_reallocation.is_some(),
                report.defragmentation.is_some()
            );
            if let Some(heat) = report.heat_reallocation {
                println!(
                    "prioritized_inodes={} promoted_hot_inodes={} moved_entries={} final_device_blocks={}",
                    heat.prioritized_inodes,
                    heat.promoted_hot_inodes,
                    heat.moved_entries,
                    heat.final_device_blocks
                );
            }
            if let Some(defrag) = report.defragmentation {
                println!(
                    "moved_entries={} reclaimed_gaps={} final_device_blocks={}",
                    defrag.moved_entries, defrag.reclaimed_gaps, defrag.final_device_blocks
                );
            }
        }
        "mount-image" => {
            let image_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image path for mount-image".to_string())
            })?;
            let mount_point = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing mount point for mount-image".to_string())
            })?;
            #[cfg(target_os = "linux")]
            {
                linux_fuse::mount_image(image_path, mount_point)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "mount-image is only available on Linux builds".to_string(),
                ));
            }
        }
        "mount-image-rw" => {
            let image_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image path for mount-image-rw".to_string())
            })?;
            let mount_point = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing mount point for mount-image-rw".to_string())
            })?;
            #[cfg(target_os = "linux")]
            {
                linux_fuse::mount_image_rw(image_path, mount_point)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "mount-image-rw is only available on Linux builds".to_string(),
                ));
            }
        }
        "probe-device" => {
            let device_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing device path for probe-device".to_string())
            })?;
            #[cfg(target_os = "linux")]
            {
                let info = crate::storage::block_device::raw::probe_device(device_path)?;
                println!("device: {}", info.path.display());
                println!("capacity: {} bytes", info.capacity_bytes);
                println!(
                    "sector_size: logical={} physical={}",
                    info.logical_sector_size, info.physical_sector_size
                );
                println!("read_only: {}", info.read_only);
                println!("whole_disk: {}", info.is_whole_disk);
                println!("mounted: {}", info.is_mounted);
                println!("safe_to_format: {}", info.is_safe_to_format());
                for blocker in info.format_blockers() {
                    println!("  blocker: {blocker}");
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = device_path;
                return Err(CoreFsError::InvalidCommand(
                    "probe-device is only available on Linux builds".to_string(),
                ));
            }
        }
        "mkfs-device" => {
            let device_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing device path for mkfs-device".to_string())
            })?;
            #[cfg(target_os = "linux")]
            {
                crate::storage::block_device::raw::check_device_permissions(device_path)?;
                let info = crate::storage::block_device::raw::probe_device(device_path)?;
                if !info.is_safe_to_format() {
                    for blocker in info.format_blockers() {
                        eprintln!("error: {blocker}");
                    }
                    return Err(CoreFsError::PolicyViolation(
                        "device is not safe to format — see errors above".to_string(),
                    ));
                }
                let config = crate::config::CoreFsConfig::default();
                let mut device =
                    crate::storage::block_device::raw::RawBlockDevice::open(device_path, false)?;
                linux_fuse::format_device(&mut device, config)?;
                println!("formatted CoreFS volume on {device_path}");
                println!(
                    "capacity: {} bytes ({} sectors of {} bytes)",
                    info.capacity_bytes,
                    info.capacity_bytes / u64::from(info.logical_sector_size),
                    info.logical_sector_size
                );

                // Quick fake-stick sanity check (unless --skip-check).
                if !args.iter().any(|a| a == "--skip-check") {
                    println!("running quick writability check at distributed offsets ...");
                    // Leave the first 16 MiB untouched (volume image region).
                    let report = crate::storage::block_device::sanity_check_writable(
                        &mut device,
                        16 * 1024 * 1024,
                    )?;
                    if report.is_honest() {
                        println!(
                            "sanity-check ok: {} probes succeeded across {} bytes",
                            report.probed_offsets.len(),
                            report.advertised_bytes
                        );
                    } else {
                        eprintln!(
                            "warning: device appears to be fake or failing — {} of {} probes failed",
                            report.failed_offsets.len(),
                            report.probed_offsets.len()
                        );
                        eprintln!(
                            "  advertised capacity: {} bytes",
                            report.advertised_bytes
                        );
                        eprintln!(
                            "  estimated usable:    {} bytes ({}% appears fake)",
                            report.estimated_usable_bytes,
                            report.fake_ratio_percent()
                        );
                        eprintln!("  failed offsets (bytes):");
                        for off in &report.failed_offsets {
                            eprintln!("    - {off}");
                        }
                        return Err(CoreFsError::State(
                            "device failed writability check — data loss likely, aborting"
                                .to_string(),
                        ));
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = device_path;
                return Err(CoreFsError::InvalidCommand(
                    "mkfs-device is only available on Linux builds".to_string(),
                ));
            }
        }
        "mount-device-rw" => {
            let device_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand(
                    "missing device path for mount-device-rw".to_string(),
                )
            })?;
            let mount_point = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand(
                    "missing mount point for mount-device-rw".to_string(),
                )
            })?;
            #[cfg(target_os = "linux")]
            {
                crate::storage::block_device::raw::check_device_permissions(device_path)?;
                let device =
                    crate::storage::block_device::raw::RawBlockDevice::open(device_path, false)?;
                linux_fuse::mount_device_rw(Box::new(device), mount_point)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (device_path, mount_point);
                return Err(CoreFsError::InvalidCommand(
                    "mount-device-rw is only available on Linux builds".to_string(),
                ));
            }
        }
        "verify-device" => {
            let device_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand(
                    "missing device path for verify-device".to_string(),
                )
            })?;
            #[cfg(target_os = "linux")]
            {
                if !args.iter().any(|a| a == "--destructive") {
                    return Err(CoreFsError::InvalidCommand(
                        "verify-device overwrites all data on the device. \
                         Re-run with --destructive to confirm."
                            .to_string(),
                    ));
                }
                let chunk_count: u64 = parse_flag_u64(&args, "--chunks").unwrap_or(200);
                let chunk_size: u64 =
                    parse_flag_u64(&args, "--chunk-size").unwrap_or(64 * 1024);
                let info = crate::storage::block_device::raw::probe_device(device_path)?;
                if info.is_mounted {
                    return Err(CoreFsError::PolicyViolation(format!(
                        "refusing to verify: {device_path} is currently mounted"
                    )));
                }
                crate::storage::block_device::raw::check_device_permissions(device_path)?;
                let mut device =
                    crate::storage::block_device::raw::RawBlockDevice::open(device_path, false)?;
                println!(
                    "verifying {device_path}: {chunk_count} chunks × {chunk_size} bytes across {} bytes",
                    info.capacity_bytes
                );
                let report = crate::storage::block_device::verify_device_capacity(
                    &mut device,
                    chunk_size,
                    chunk_count,
                )?;
                println!("advertised_bytes: {}", report.advertised_bytes);
                println!(
                    "probed_offsets:   {} distinct positions",
                    report.probed_offsets.len()
                );
                println!("failed_probes:    {}", report.failed_offsets.len());
                println!(
                    "highest_verified: {} bytes",
                    report.highest_verified_offset
                );
                println!(
                    "estimated_usable: {} bytes",
                    report.estimated_usable_bytes
                );
                if report.is_honest() {
                    println!("verdict: ok — device appears to be honest");
                } else {
                    println!(
                        "verdict: FAKE — roughly {}% of advertised capacity is unusable",
                        report.fake_ratio_percent()
                    );
                    return Err(CoreFsError::State(
                        "device failed capacity verification".to_string(),
                    ));
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = device_path;
                return Err(CoreFsError::InvalidCommand(
                    "verify-device is only available on Linux builds".to_string(),
                ));
            }
        }
        "fsck-device" => {
            let device_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing device path for fsck-device".to_string())
            })?;
            #[cfg(target_os = "linux")]
            {
                // Read-only access is enough for fsck.
                let device =
                    crate::storage::block_device::raw::RawBlockDevice::open(device_path, true)?;
                let report = IntegrityService.fsck_device(&device)?;
                println!("fsck-device ok: {device_path}");
                println!("format_version: {}", report.format_version);
                println!("segment_count: {}", report.segment_count);
                println!("valid_superblocks: {}", report.valid_superblocks);
                println!("selected_generation: {}", report.selected_generation);
                println!(
                    "checksums: directory={} payload={}",
                    report.directory_checksum_valid, report.payload_checksum_valid
                );
                println!("block_descriptors: {}", report.block_descriptors);
                if !report.directory_checksum_valid || !report.payload_checksum_valid {
                    return Err(CoreFsError::State(
                        "integrity check failed — checksums do not match".to_string(),
                    ));
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = device_path;
                return Err(CoreFsError::InvalidCommand(
                    "fsck-device is only available on Linux builds".to_string(),
                ));
            }
        }
        "diagnose-mount" => {
            let image_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image path for diagnose-mount".to_string())
            })?;
            let mount_point = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing mount point for diagnose-mount".to_string())
            })?;
            #[cfg(target_os = "linux")]
            {
                let options = linux_mount_options_from_args(&args[4..]);
                let report = diagnostics::diagnose_mount(image_path, mount_point, &options);
                for line in diagnostics::render_mount_diagnosis(&report) {
                    println!("{line}");
                }
                diagnostics::ensure_mount_ready(&report)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "diagnose-mount is only available on Linux builds".to_string(),
                ));
            }
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
    fs.create_directory("/etc")?;
    fs.create_directory("/var")?;
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
    fs.create_symlink("/etc/corefs-current", "/etc/corefs.conf")?;
    Ok(fs)
}

fn print_usage() {
    println!("corefs commands:");
    println!("  mkfs");
    println!("  status");
    println!("  ls");
    println!("  snapshot [name]");
    println!("  scrub");
    println!("  defrag");
    println!("  optimize");
    println!("  delete <path> [--secure]");
    println!("  restore <path>");
    println!("  write <path> <payload>");
    println!("  read <path>");
    println!("  save-image <path>");
    println!("  mkfs-image <path> [--demo]");
    println!("  load-image <path>");
    println!("  fsck-image <path> [--repair]");
    println!("  defrag-image <path>");
    println!("  optimize-image <path>");
    println!("  mount-image <image-path> <mount-point>");
    println!("  mount-image-rw <image-path> <mount-point>");
    println!("  probe-device <device-path>");
    println!("  mkfs-device <device-path> [--skip-check]");
    println!("  fsck-device <device-path>");
    println!("  verify-device <device-path> --destructive [--chunks <n>] [--chunk-size <bytes>]");
    println!("  mount-device-rw <device-path> <mount-point>");
    println!("  diagnose-mount <image-path> <mount-point> [--create]");
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

fn parse_flag_u64(args: &[String], flag: &str) -> Option<u64> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).and_then(|v| v.parse::<u64>().ok())
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

#[cfg(target_os = "linux")]
fn linux_mount_options_from_args(args: &[String]) -> LinuxMountOptions {
    let mut options = LinuxMountOptions::default();

    for arg in args {
        if arg == "--create" {
            options.create_if_missing = true;
        }
    }

    options
}

fn parse_usize_flag(args: &[String], index: usize, flag: &str) -> CoreFsResult<usize> {
    let value = args
        .get(index + 1)
        .ok_or_else(|| CoreFsError::InvalidCommand(format!("missing value for {flag}")))?;
    value.parse::<usize>().map_err(|error| {
        CoreFsError::InvalidInput(format!("invalid numeric value for {flag}: {error}"))
    })
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
