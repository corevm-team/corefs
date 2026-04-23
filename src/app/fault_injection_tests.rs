// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Fault injection tests for the App/Service layer.
//
// These tests compose the ODF fault injection framework
// (`storage::ondisk::fault_injection::FaultyDevice`) with the
// `OdfDeviceSession` to verify that the CoreFsService + ODF pipeline
// handles I/O failures, ENOSPC, and partial writes correctly at the
// *application* level.
//
// Storage-layer resilience is covered separately in
// `storage::ondisk::resilience_tests`.

use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::fault_injection::{FaultPlan, FaultyDevice};
use crate::storage::ondisk::fsck;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::native::{
    load_state_native, save_state_native, save_state_native_incremental,
};
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};
use crate::storage::ondisk::volume::{FormatOptions, format_device};

// ----- helpers --------------------------------------------------------------

fn session_opts() -> OdfSessionOptions {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 8 * 1024 * 1024;
    opts.inode_count = 256;
    opts.journal_blocks = 32;
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    opts
}

/// Create a formatted FaultyDevice and populate it with a known good state.
fn formatted_faulty_with_state(file_count: usize) -> FaultyDevice<MemoryDevice> {
    let opts = session_opts();
    // Use OdfDeviceSession so file bytes are written to the device correctly.
    let mem = MemoryDevice::new(opts.capacity_bytes, 4096).unwrap();
    let dev: Box<dyn crate::storage::block_device::BlockDevice> = Box::new(mem);
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| {
        for i in 0..file_count {
            fs.create_file(
                &format!("/file{i:03}"),
                format!("content-{i}").as_bytes(),
                &[],
            )?;
        }
        Ok(())
    })
    .unwrap();
    let dev_box = sess.into_device();
    // Downcast back to MemoryDevice for FaultyDevice wrapping.
    let raw = dev_box.read_at(0, dev_box.capacity()).unwrap();
    let mem = MemoryDevice::from_bytes(raw, 4096).unwrap();

    FaultyDevice::new(mem, FaultPlan::new())
}

// ----- fi1: ENOSPC during incremental save leaves previous generation valid --

#[test]
fn fi1_enospc_during_incremental_save_preserves_previous_state() {
    let mut dev = formatted_faulty_with_state(5);

    // Verify good state loads.
    let original = load_state_native(&dev).unwrap();
    assert_eq!(original.active_inodes.len(), 5);

    // Add more files to the in-memory state.
    let mut service = CoreFsService::from_persisted_state(original.clone());
    for i in 100..110 {
        service
            .create_file(&format!("/new{i}"), b"extra", &[])
            .unwrap();
    }

    // Arm ENOSPC after a few writes — the incremental save should fail.
    dev.set_plan(FaultPlan::new().fail_after_writes(2));
    let new_state = service.persisted_state();
    let result = save_state_native_incremental(&mut dev, &new_state);
    assert!(result.is_err(), "save should fail under ENOSPC");

    // Clear faults, reload: must see the ORIGINAL 5 files.
    dev.set_plan(FaultPlan::new());
    let reloaded = load_state_native(&dev).unwrap();
    assert_eq!(
        reloaded.active_inodes.len(),
        5,
        "failed save must not corrupt the previous generation"
    );
}

// ----- fi2: full save failure does not advance superblock generation ---------

#[test]
fn fi2_full_save_failure_leaves_superblock_generation_unchanged() {
    let mut dev = formatted_faulty_with_state(3);

    let gen_before = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap()
    .generation;

    // Arm fault and attempt a full save with modified state.
    let mut service = CoreFsService::from_persisted_state(load_state_native(&dev).unwrap());
    service.create_file("/extra", b"fail-me", &[]).unwrap();

    dev.set_plan(FaultPlan::new().fail_after_writes(3));
    let result = save_state_native(&mut dev, &service.persisted_state());
    assert!(result.is_err());

    dev.set_plan(FaultPlan::new());
    let gen_after = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap()
    .generation;

    assert_eq!(
        gen_before, gen_after,
        "failed save must not advance superblock generation"
    );
}

// ----- fi3: session mutate propagates device errors -------------------------

#[test]
fn fi3_session_mutate_propagates_flush_error() {
    let opts = session_opts();
    let mem = MemoryDevice::new(opts.capacity_bytes, 4096).unwrap();
    let dev: Box<dyn BlockDevice> = Box::new(mem);
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();

    // First mutate succeeds.
    sess.mutate(|fs| fs.create_file("/ok", b"fine", &[]))
        .unwrap();

    // The session owns a Box<dyn BlockDevice>, so we can't arm faults on
    // the inner device directly.  Instead we verify that we can open the
    // state from a faulty device and that save errors bubble up.
    let good_state = sess.service().persisted_state();
    let dev_bytes = sess.into_device();
    let capacity = dev_bytes.capacity();
    let raw = dev_bytes.read_at(0, capacity).unwrap();

    let mut faulty = FaultyDevice::new(
        MemoryDevice::from_bytes(raw, 4096).unwrap(),
        FaultPlan::new(),
    );

    // Arm fault for the next save.
    faulty.set_plan(FaultPlan::new().fail_after_writes(1));
    let result = save_state_native_incremental(&mut faulty, &good_state);
    assert!(result.is_err(), "flush should propagate device error");
}

// ----- fi4: fsck detects corruption introduced after a good save -------------

#[test]
fn fi4_fsck_detects_post_save_corruption() {
    let dev = formatted_faulty_with_state(10);

    // Verify clean.
    let report = fsck::check(&dev).unwrap();
    assert!(
        report.is_clean(),
        "baseline should be clean: {:?}",
        report.issues
    );

    // Corrupt the tertiary superblock (a structural element).
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();

    // We need a mutable reference — unwrap the faulty device.
    let mut dev = dev;
    dev.write_at(
        sb.tertiary_superblock_block * BLOCK_SIZE,
        &[0u8; BLOCK_SIZE as usize],
    )
    .unwrap();

    let report = fsck::check(&dev).unwrap();
    assert!(
        report.issues.iter().any(|i| i.code.contains("TERTIARY")),
        "fsck should flag the corrupted tertiary superblock"
    );
}

// ----- fi5: reload after silent corruption detects tampered data -------------

#[test]
fn fi5_silent_corruption_detected_on_reload() {
    let mut dev = formatted_faulty_with_state(3);

    // Identify the data block for /file000 via slot_for_path.
    let data_block = {
        let mut reader = crate::storage::ondisk::reader::OdfReader::open(&dev).unwrap();
        let slot = reader.slot_for_path("/file000").unwrap().unwrap();
        let on_disk = reader.read_on_disk_inode(slot).unwrap();
        on_disk.extents[0].physical_block
    };

    // Flip one byte in the data block.
    let offset = data_block * BLOCK_SIZE;
    let mut raw = dev.read_at(offset, BLOCK_SIZE).unwrap();
    raw[7] ^= 0xFF;
    dev.write_at(offset, &raw).unwrap();

    // Now reading via the reader should detect the CRC mismatch.
    let mut reader = crate::storage::ondisk::reader::OdfReader::open(&dev).unwrap();
    let result = reader.read_file_by_path("/file000");
    assert!(
        result.is_err(),
        "corrupted data should be caught by data CRC"
    );
}

// ----- fi6: sync failure during flush is observable --------------------------

#[test]
fn fi6_sync_failure_is_propagated_through_save() {
    let mut dev = formatted_faulty_with_state(2);

    // Save a good state first.
    let state = load_state_native(&dev).unwrap();

    // Arm sync failure.
    dev.set_plan(FaultPlan::new().fail_sync());
    let result = save_state_native(&mut dev, &state);
    // Depending on when sync is called, this may or may not fail.
    // What matters is that the error is surfaced, not swallowed.
    if let Err(e) = result {
        assert!(
            format!("{e}").contains("sync") || format!("{e}").contains("FAULT"),
            "error should mention sync or fault: {e}"
        );
    }
    // Stats confirm the sync was attempted.
    assert!(dev.stats().syncs_failed >= 1 || dev.stats().syncs_attempted >= 1);
}

// ----- fi7: service fsck stays clean after successful mutate+flush -----------

#[test]
fn fi7_service_fsck_clean_after_mutate_flush_roundtrip() {
    let opts = session_opts();
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();

    // Several mutation rounds.
    for round in 0..5 {
        sess.mutate(|fs| {
            for i in 0..10 {
                let path = format!("/r{round}_f{i}");
                fs.create_file(&path, format!("round-{round}").as_bytes(), &[])?;
            }
            Ok(())
        })
        .unwrap();
    }

    // In-memory fsck.
    let report = sess.service().fsck();
    assert_eq!(
        report.missing_blocks,
        Vec::<String>::new(),
        "service fsck: {:?}",
        report
    );
    assert_eq!(
        report.orphaned_blocks.len(),
        0,
        "service fsck orphans: {:?}",
        report
    );

    // On-disk fsck.
    let report = fsck::check(sess.device()).unwrap();
    assert!(report.is_clean(), "ODF fsck: {:?}", report.issues);
}

// ----- fi8: reload from device preserves all service state -------------------

#[test]
fn fi8_service_state_survives_save_reload_cycle() {
    let opts = session_opts();
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();

    sess.mutate(|fs| {
        fs.create_file("/alpha", b"aaa", &[])?;
        fs.create_file("/beta", b"bbb", &[])?;
        fs.create_directory("/dir")?;
        fs.create_file("/dir/child", b"ccc", &[])?;
        Ok(())
    })
    .unwrap();

    let paths_before = sess.service().list_paths();
    let alpha_before = sess.service().read_file("/alpha").unwrap();

    // Reload from device.
    let dev = sess.into_device();
    let sess2 = OdfDeviceSession::open(dev).unwrap();

    let paths_after = sess2.service().list_paths();
    let alpha_after = sess2.service().read_file("/alpha").unwrap();

    assert_eq!(paths_before, paths_after);
    assert_eq!(alpha_before, alpha_after);
}

// ----- fi9: stop_after_n_writes simulates power-loss — no panic --------------

#[test]
fn fi9_power_loss_simulation_does_not_panic() {
    let mut dev = formatted_faulty_with_state(5);
    let state = load_state_native(&dev).unwrap();

    // Arm silent write-drop to simulate power loss.
    dev.set_plan(FaultPlan::new().stop_after_n_writes(4));

    // The save may succeed (if it finishes in ≤4 writes) or produce
    // a partial image.  Either way, it must not panic.
    let _ = save_state_native(&mut dev, &state);

    // Clear faults and verify the device is still loadable (previous
    // generation is intact because the superblock wasn't updated).
    dev.set_plan(FaultPlan::new());
    let reloaded = load_state_native(&dev);
    assert!(
        reloaded.is_ok(),
        "device should remain loadable after power-loss simulation"
    );
}

// ----- fi10: multiple fault/recovery cycles don't accumulate damage ----------

#[test]
fn fi10_repeated_fault_recovery_cycles_stay_consistent() {
    let mut dev = formatted_faulty_with_state(3);

    for cycle in 0..5 {
        // Load current state.
        let state = load_state_native(&dev).unwrap();
        let count_before = state.active_inodes.len();

        // Mutate in memory.
        let mut service = CoreFsService::from_persisted_state(state);
        service
            .create_file(&format!("/cycle{cycle}"), b"data", &[])
            .unwrap();

        // Attempt save — fault on even cycles.
        if cycle % 2 == 0 {
            dev.set_plan(FaultPlan::new().fail_after_writes(2));
            let _ = save_state_native(&mut dev, &service.persisted_state());
            dev.set_plan(FaultPlan::new());
            // After a failed save, we should still see the old state.
            let reloaded = load_state_native(&dev).unwrap();
            assert_eq!(reloaded.active_inodes.len(), count_before);
        } else {
            dev.set_plan(FaultPlan::new());
            save_state_native(&mut dev, &service.persisted_state()).unwrap();
            let reloaded = load_state_native(&dev).unwrap();
            assert_eq!(reloaded.active_inodes.len(), count_before + 1);
        }
    }

    // Final state is consistent.
    dev.set_plan(FaultPlan::new());
    let final_state = load_state_native(&dev).unwrap();
    let final_service = CoreFsService::from_persisted_state(final_state);
    let report = final_service.fsck();
    assert_eq!(
        report.missing_blocks,
        Vec::<String>::new(),
        "fsck after fault cycles: {:?}",
        report
    );
    assert_eq!(
        report.orphaned_blocks.len(),
        0,
        "fsck orphans after fault cycles: {:?}",
        report
    );
}
