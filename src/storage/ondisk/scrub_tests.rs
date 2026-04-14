// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::CoreFsConfig;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

fn populated_device() -> Box<dyn BlockDevice> {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 16 * 1024 * 1024;
    opts.inode_count = 256;
    opts.journal_blocks = 32;
    opts.config = CoreFsConfig::default();
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| {
        fs.create_file("/a.txt", b"alpha one", &[])?;
        fs.create_file("/b.txt", b"beta one two", &[])?;
        fs.create_file("/c.bin", &[0x42u8; 512], &[])?;
        Ok(())
    })
    .unwrap();
    sess.into_device()
}

#[test]
fn scrub_plan_variants_carry_expected_flags() {
    let full = ScrubPlan::full();
    assert!(full.auto_repair && full.verify_data_crc);
    let structural = ScrubPlan::structural_only();
    assert!(structural.auto_repair && !structural.verify_data_crc);
    let read_only = ScrubPlan::read_only();
    assert!(!read_only.auto_repair && read_only.verify_data_crc);
}

#[test]
fn scrub_on_clean_volume_reports_clean() {
    let mut dev = populated_device();
    let report = run(dev.as_mut(), &ScrubPlan::full()).unwrap();
    assert!(report.is_clean(), "report: {report:?}");
    assert!(report.data_corruptions.is_empty());
    assert!(report.extents_verified > 0);
    assert!(report.blocks_verified > 0);
}

#[test]
fn scrub_detects_silent_data_corruption() {
    let mut dev = populated_device();
    // Locate the first file's data block and flip a byte.
    let reader = crate::storage::ondisk::reader::OdfReader::open(dev.as_ref()).unwrap();
    let summaries = reader.list_inodes().unwrap();
    let victim = summaries
        .iter()
        .find(|s| s.size_bytes > 0)
        .expect("at least one file with content");
    let victim_slot = victim.slot;
    let on_disk = reader.read_on_disk_inode(victim_slot).unwrap();
    let data_block = on_disk.extents[0].physical_block;
    drop(reader);

    let mut raw = dev.read_at(data_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    raw[0] ^= 0xFF;
    dev.write_at(data_block * BLOCK_SIZE, &raw).unwrap();

    let report = run(dev.as_mut(), &ScrubPlan::full()).unwrap();
    assert_eq!(report.data_corruptions.len(), 1);
    assert_eq!(report.data_corruptions[0].0, victim_slot);
    assert!(!report.is_clean());
}

#[test]
fn scrub_auto_repairs_stale_tertiary_superblock() {
    let mut dev = populated_device();
    // Corrupt the tertiary superblock.
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let tert = sb.tertiary_superblock_block;
    let mut buf = dev.read_at(tert * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    buf[10] ^= 0xFF;
    dev.write_at(tert * BLOCK_SIZE, &buf).unwrap();

    let report = run(dev.as_mut(), &ScrubPlan::full()).unwrap();
    assert!(report.fsck_issues_before >= 1);
    assert!(report.repair_ops_committed >= 1);
    // After the scrub, the tertiary warning must be gone.
    let has_tertiary_issue = report
        .residual_issues
        .iter()
        .any(|i| i.code == "ODF.SB.TERTIARY_UNREADABLE" || i.code == "ODF.SB.TERTIARY_STALE");
    assert!(
        !has_tertiary_issue,
        "residual: {:?}",
        report.residual_issues
    );
}

#[test]
fn structural_only_plan_skips_data_verification() {
    let mut dev = populated_device();
    // Corrupt a data block — structural-only scrub should NOT detect it.
    let reader = crate::storage::ondisk::reader::OdfReader::open(dev.as_ref()).unwrap();
    let summaries = reader.list_inodes().unwrap();
    let victim = summaries.iter().find(|s| s.size_bytes > 0).unwrap();
    let on_disk = reader.read_on_disk_inode(victim.slot).unwrap();
    let data_block = on_disk.extents[0].physical_block;
    drop(reader);

    let mut raw = dev.read_at(data_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    raw[5] ^= 0x80;
    dev.write_at(data_block * BLOCK_SIZE, &raw).unwrap();

    let report = run(dev.as_mut(), &ScrubPlan::structural_only()).unwrap();
    // Structural scrub reports it clean because it didn't read data.
    assert!(report.data_corruptions.is_empty());
    assert_eq!(report.blocks_verified, 0);
    assert_eq!(report.extents_verified, 0);
}

#[test]
fn read_only_plan_performs_no_writes() {
    let mut dev = populated_device();
    // Snapshot the device bytes before scrub.
    let before = dev.read_at(0, dev.capacity()).unwrap();

    // Corrupt tertiary SB so read-only scrub would *want* to repair.
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let mut buf = dev
        .read_at(sb.tertiary_superblock_block * BLOCK_SIZE, BLOCK_SIZE)
        .unwrap();
    buf[0] ^= 0xFF;
    dev.write_at(sb.tertiary_superblock_block * BLOCK_SIZE, &buf)
        .unwrap();

    let snapshot_after_corruption = dev.read_at(0, dev.capacity()).unwrap();
    let report = run(dev.as_mut(), &ScrubPlan::read_only()).unwrap();
    assert_eq!(report.repair_ops_committed, 0);
    // Residual issues still contain the corruption.
    assert!(
        report
            .residual_issues
            .iter()
            .any(|i| i.code == "ODF.SB.TERTIARY_UNREADABLE")
    );
    // Device wasn't modified by the read-only scrub.
    let after = dev.read_at(0, dev.capacity()).unwrap();
    assert_eq!(after, snapshot_after_corruption);
    let _ = before;
}

#[test]
fn is_clean_reports_true_only_when_both_structural_and_data_clean() {
    let report = ScrubReport {
        extents_verified: 0,
        blocks_verified: 0,
        data_corruptions: Vec::new(),
        residual_issues: Vec::new(),
        repair_ops_committed: 0,
        fsck_issues_before: 0,
    };
    assert!(report.is_clean());

    let mut with_data_corruption = report.clone();
    with_data_corruption.data_corruptions.push((10, 100));
    assert!(!with_data_corruption.is_clean());

    let mut with_residual_error = report.clone();
    with_residual_error
        .residual_issues
        .push(crate::storage::ondisk::fsck::FsckIssue {
            severity: crate::storage::ondisk::fsck::Severity::Error,
            code: "ODF.TEST",
            message: "e".into(),
        });
    assert!(!with_residual_error.is_clean());

    let mut with_only_warning = report.clone();
    with_only_warning
        .residual_issues
        .push(crate::storage::ondisk::fsck::FsckIssue {
            severity: crate::storage::ondisk::fsck::Severity::Warning,
            code: "ODF.TEST.WARN",
            message: "w".into(),
        });
    assert!(with_only_warning.is_clean()); // warnings alone don't flip clean→dirty
}

#[test]
fn scrub_telemetry_reflects_inode_count() {
    let mut dev = populated_device();
    let report = run(dev.as_mut(), &ScrubPlan::full()).unwrap();
    // 3 files populated: expect ≥ 3 extents (each is a single contiguous extent
    // in this test).
    assert!(report.extents_verified >= 3);
}
