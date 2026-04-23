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
#[cfg(target_os = "windows")]
use crate::platform::windows;
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
            let config = corefs_config_from_args(&args[3..])?;
            let fs = if include_demo {
                bootstrap_demo_fs_with_config(config)?
            } else {
                CoreFsService::format(config)
            };
            fs.save_image_to_path(path)?;
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
            #[cfg(target_os = "windows")]
            {
                println!("mounting CoreFS image on {mount_point}; keep this process running");
                let report = windows::mount_image_as_drive(
                    image_path,
                    mount_point,
                    false,
                    windows_staging_from_args(&args[4..]),
                )?;
                println!(
                    "unmounted CoreFS image from {}: mount_point={}",
                    report.drive_letter,
                    report.mount_point.display()
                );
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "mount-image is only available on Linux and Windows builds".to_string(),
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
            #[cfg(target_os = "windows")]
            {
                println!("mounting CoreFS image read-write on {mount_point}; keep this process running");
                let report = windows::mount_image_as_drive(
                    image_path,
                    mount_point,
                    true,
                    windows_staging_from_args(&args[4..]),
                )?;
                println!(
                    "unmounted CoreFS image read-write from {}: mount_point={}",
                    report.drive_letter,
                    report.mount_point.display()
                );
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "mount-image-rw is only available on Linux and Windows builds".to_string(),
                ));
            }
        }
        "unmount-image-win" => {
            let drive_letter = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand(
                    "missing drive letter for unmount-image-win".to_string(),
                )
            })?;
            let commit = !args.iter().any(|arg| arg == "--discard");
            #[cfg(target_os = "windows")]
            {
                let report = windows::unmount_image_drive(drive_letter, commit)?;
                println!(
                    "unmounted {}: committed={} image={}",
                    report.drive_letter,
                    report.committed,
                    report.image_path.display()
                );
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = (drive_letter, commit);
                return Err(CoreFsError::InvalidCommand(
                    "unmount-image-win is only available on Windows builds".to_string(),
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
                let config = corefs_config_from_args(&args[3..])?;
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
                        eprintln!("  advertised capacity: {} bytes", report.advertised_bytes);
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
                CoreFsError::InvalidCommand("missing device path for mount-device-rw".to_string())
            })?;
            let mount_point = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing mount point for mount-device-rw".to_string())
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
                CoreFsError::InvalidCommand("missing device path for verify-device".to_string())
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
                let chunk_size: u64 = parse_flag_u64(&args, "--chunk-size").unwrap_or(64 * 1024);
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
                println!("highest_verified: {} bytes", report.highest_verified_offset);
                println!("estimated_usable: {} bytes", report.estimated_usable_bytes);
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
        "mkfs-odf" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image path for mkfs-odf".to_string())
            })?;
            let capacity_bytes = parse_flag_u64(&args[3..], "--size").unwrap_or(64 * 1024 * 1024);
            odf_mkfs_image(path, capacity_bytes)?;
            println!("odf volume formatted at {path} ({capacity_bytes} bytes)");
        }
        "fsck-odf" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image path for fsck-odf".to_string())
            })?;
            let report = odf_fsck_image(path)?;
            println!(
                "fsck-odf: {} inode(s), {} extent(s), {} issue(s)",
                report.inodes_checked,
                report.extents_checked,
                report.issues.len()
            );
            for issue in &report.issues {
                println!("  [{:?}] {}: {}", issue.severity, issue.code, issue.message);
            }
            if !report.is_clean() {
                return Err(CoreFsError::State(
                    "fsck-odf: volume has Error-level issues".to_string(),
                ));
            }
        }
        "inspect-odf" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing image path for inspect-odf".to_string())
            })?;
            let info = odf_inspect_image(path)?;
            println!("label: {}", info.label);
            println!(
                "uuid: {}",
                info.uuid
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            println!("total_blocks: {}", info.total_blocks);
            println!("free_blocks: {}", info.free_blocks);
            println!("total_inodes: {}", info.total_inodes);
            println!("free_inodes: {}", info.free_inodes);
            println!("generation: {}", info.generation);
            println!("state: {}", info.state);
            println!(
                "superblocks: primary={} tertiary={} secondary={}",
                info.primary_ok, info.tertiary_ok, info.secondary_ok
            );
        }
        "mount-odf" => {
            #[cfg(target_os = "linux")]
            {
                let image_path = args.get(2).ok_or_else(|| {
                    CoreFsError::InvalidCommand("mount-odf <image-path> <mount-point>".to_string())
                })?;
                let mount_point = args.get(3).ok_or_else(|| {
                    CoreFsError::InvalidCommand("mount-odf <image-path> <mount-point>".to_string())
                })?;
                linux_fuse::mount_odf_image(image_path, mount_point)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "mount-odf is only available on Linux".to_string(),
                ));
            }
        }
        "mount-odf-rw" => {
            #[cfg(target_os = "linux")]
            {
                let image_path = args.get(2).ok_or_else(|| {
                    CoreFsError::InvalidCommand(
                        "mount-odf-rw <image-path> <mount-point>".to_string(),
                    )
                })?;
                let mount_point = args.get(3).ok_or_else(|| {
                    CoreFsError::InvalidCommand(
                        "mount-odf-rw <image-path> <mount-point>".to_string(),
                    )
                })?;
                linux_fuse::mount_odf_image_rw(image_path, mount_point)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(CoreFsError::InvalidCommand(
                    "mount-odf-rw is only available on Linux".to_string(),
                ));
            }
        }
        "odf-session-demo" => {
            // End-to-end CLI demo: format a fresh ODF image, populate it
            // via OdfFileSession, list inodes via OdfReader, print results.
            let image_path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("odf-session-demo <image-path>".to_string())
            })?;
            let capacity = parse_flag_u64(&args[3..], "--size").unwrap_or(16 * 1024 * 1024);
            odf_session_demo(image_path, capacity)?;
        }
        "migrate-to-odf" => {
            let src = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("migrate-to-odf <src.img> <dst.odf> [--size N]".into())
            })?;
            let dst = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("migrate-to-odf <src.img> <dst.odf> [--size N]".into())
            })?;
            let capacity_bytes = parse_flag_u64(&args[4..], "--size").unwrap_or(64 * 1024 * 1024);
            let report = odf_migrate_from_volume_image(src, dst, capacity_bytes)?;
            println!(
                "migrated {src} → {dst}: {} inode(s), generation {}",
                report.active_slots, report.generation
            );
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
    bootstrap_demo_fs_with_config(CoreFsConfig::default())
}

fn bootstrap_demo_fs_with_config(config: CoreFsConfig) -> CoreFsResult<CoreFsService> {
    let mut fs = CoreFsService::format(config);
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

// ---------------------------------------------------------------------------
// ODF (On-Disk Format v1) helpers
// ---------------------------------------------------------------------------

fn odf_mkfs_image(path: &str, capacity_bytes: u64) -> CoreFsResult<()> {
    use crate::storage::block_device::{BlockDevice, FileImageDevice};
    use crate::storage::ondisk::volume::{FormatOptions, format_device};
    let mut dev = FileImageDevice::create(path, capacity_bytes, 4096)?;
    let opts = FormatOptions {
        label: "corefs".to_string(),
        uuid: generate_uuid(),
        inode_count: crate::storage::ondisk::layout::DEFAULT_INODE_COUNT,
        journal_blocks: crate::storage::ondisk::layout::DEFAULT_JOURNAL_BLOCKS,
    };
    format_device(&mut dev, &opts)?;
    let _ = dev.sync();
    Ok(())
}

fn odf_fsck_image(path: &str) -> CoreFsResult<crate::storage::ondisk::fsck::FsckReport> {
    use crate::storage::block_device::FileImageDevice;
    use crate::storage::ondisk::fsck;
    let dev = FileImageDevice::open(path, false)?;
    fsck::check(&dev)
}

fn odf_inspect_image(path: &str) -> CoreFsResult<crate::storage::ondisk::volume::VolumeInfo> {
    use crate::storage::block_device::FileImageDevice;
    use crate::storage::ondisk::volume::inspect;
    let dev = FileImageDevice::open(path, false)?;
    inspect(&dev)
}

fn odf_session_demo(image_path: &str, capacity: u64) -> CoreFsResult<()> {
    use crate::storage::ondisk::reader::OdfReader;
    use crate::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = capacity;
    opts.inode_count = 512;
    opts.journal_blocks = 32;
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    let image = std::path::Path::new(image_path);
    let _ = std::fs::remove_file(image);
    let mut sess = OdfFileSession::format_new(image, &opts)?;
    let (_, report) = sess.mutate(|fs| {
        fs.create_directory("/demo")?;
        fs.create_file("/demo/readme.txt", b"hello from odf", &[])?;
        fs.create_file("/demo/data.bin", &vec![0x42u8; 512], &[])?;
        fs.create_symlink("/demo/link", "/demo/readme.txt")?;
        Ok(())
    })?;
    println!(
        "flush: created={} updated={} removed={} unchanged={}",
        report.incremental.created,
        report.incremental.updated,
        report.incremental.removed,
        report.incremental.unchanged
    );
    drop(sess);

    let device = crate::storage::block_device::FileImageDevice::open(image, true)?;
    let reader = OdfReader::open(&device)?;
    println!("allocated user inodes: {}", reader.allocated_user_inodes());
    for summary in reader.list_inodes()? {
        println!(
            "  slot={} id={} kind={:?} size={} flags=0x{:X}",
            summary.slot, summary.domain_id, summary.kind, summary.size_bytes, summary.flags
        );
    }
    Ok(())
}

struct MigrateReport {
    active_slots: usize,
    generation: u64,
}

fn odf_migrate_from_volume_image(
    src: &str,
    dst: &str,
    capacity_bytes: u64,
) -> CoreFsResult<MigrateReport> {
    use crate::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};
    use crate::storage::volume_image::load_volume_image_with_bytes;

    // Load the legacy volume_image with file bytes.
    let (state, block_bytes) = load_volume_image_with_bytes(std::path::Path::new(src))?;

    // Restore bytes into a CoreFsService.
    let mut svc = crate::app::CoreFsService::from_persisted_state(state.clone());
    svc.restore_block_bytes(block_bytes);
    let active_slots = svc.list_paths().len();

    // Format a new ODF image file and flush via the ODF session which handles
    // writing bytes to the device correctly.
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = capacity_bytes;
    opts.label = state.volume.name.clone();
    opts.uuid = generate_uuid();
    opts.config = state.config.clone();

    let mut sess = OdfFileSession::format_new(dst, &opts)?;
    // Replace the session's service with the migrated one.
    *sess.service_mut() = svc;
    let report = sess.flush()?;

    Ok(MigrateReport {
        active_slots,
        generation: report.incremental.generation,
    })
}

fn generate_uuid() -> [u8; 16] {
    // Simple time-based pseudo-UUID (not cryptographic — just unique-ish).
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let hi = (nanos >> 64) as u64;
    let lo = nanos as u64;
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&lo.to_le_bytes());
    out[8..16].copy_from_slice(&hi.to_le_bytes());
    out
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
    println!("  mkfs-image <path> [--demo] [--profile default|performance]");
    println!("  load-image <path>");
    println!("  fsck-image <path> [--repair]");
    println!("  defrag-image <path>");
    println!("  optimize-image <path>");
    println!("  mount-image <image-path> <mount-point>");
    println!("  mount-image-rw <image-path> <mount-point>");
    println!("  unmount-image-win <drive-letter> [--discard]  (Windows only)");
    println!("  probe-device <device-path>");
    println!("  mkfs-device <device-path> [--skip-check] [--profile default|performance]");
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
    println!("  mkfs-odf <path> [--size <bytes>]");
    println!("  fsck-odf <path>");
    println!("  inspect-odf <path>");
    println!("  migrate-to-odf <src.img> <dst.odf> [--size <bytes>]");
    println!("  mount-odf <image-path> <mount-point>  (Linux only, read-only)");
    println!("  mount-odf-rw <image-path> <mount-point>  (Linux only, read-write)");
    println!("  odf-session-demo <image-path> [--size <bytes>]");
}

fn parse_flag_u64(args: &[String], flag: &str) -> Option<u64> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).and_then(|v| v.parse::<u64>().ok())
}

fn corefs_config_from_args(args: &[String]) -> CoreFsResult<CoreFsConfig> {
    let mut config = CoreFsConfig::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CoreFsError::InvalidCommand("missing value for --profile".to_string())
                })?;
                config = corefs_config_from_profile(value)?;
                index += 2;
            }
            "--demo" | "--skip-check" => {
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    Ok(config)
}

fn corefs_config_from_profile(profile: &str) -> CoreFsResult<CoreFsConfig> {
    match profile {
        "default" | "enterprise" | "secure" => Ok(CoreFsConfig::default()),
        "performance" | "bench" | "raw" => Ok(CoreFsConfig::performance_profile()),
        other => Err(CoreFsError::InvalidInput(format!(
            "unknown CoreFS profile: {other}"
        ))),
    }
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

#[cfg(target_os = "windows")]
fn windows_staging_from_args(args: &[String]) -> Option<String> {
    let idx = args.iter().position(|arg| arg == "--staging")?;
    args.get(idx + 1).cloned()
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
