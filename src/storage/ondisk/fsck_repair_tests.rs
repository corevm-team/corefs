// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::volume::VolumeDescriptor;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::MemoryDevice;
use crate::storage::block_store::AllocatorPolicy;
use crate::storage::ondisk::fsck::check;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::volume::{FormatOptions, format_device, save_state};

fn make_dev() -> MemoryDevice {
    let mut dev = MemoryDevice::new(4096 * BLOCK_SIZE, 4096).unwrap();
    let opts = FormatOptions {
        label: "rep".into(),
        uuid: [7u8; 16],
        inode_count: 512,
        journal_blocks: 32,
    };
    format_device(&mut dev, &opts).unwrap();
    dev
}

fn empty_state() -> PersistedState {
    let config = CoreFsConfig::default();
    PersistedState {
        volume: VolumeDescriptor::from_config(&config),
        config,
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    }
}

#[test]
fn clean_volume_needs_no_repair() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let report = check(&dev).unwrap();
    assert!(report.is_clean());
    let rep = repair(&mut dev, &report).unwrap();
    // No Error-level issues → no fixes and no commits.  (Some
    // benign Info/Warning entries may be present — repair should
    // ignore them.)
    assert!(
        rep.ops_committed == 0,
        "unexpected repair writes on clean volume: fixed={:?}, issues={:?}",
        rep.fixed, report.issues
    );
}

#[test]
fn repairs_stale_tertiary_superblock() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    // Corrupt the tertiary superblock.
    let tert = sb.tertiary_superblock_block;
    let mut buf = dev.read_at(tert * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    buf[50] ^= 0xFF;
    dev.write_at(tert * BLOCK_SIZE, &buf).unwrap();

    let report = check(&dev).unwrap();
    assert!(
        report.issues.iter().any(|i| i.code == "ODF.SB.TERTIARY_UNREADABLE"),
        "pre-repair report: {:?}",
        report.issues
    );

    let rep = repair(&mut dev, &report).unwrap();
    assert!(rep.fixed.contains(&"ODF.SB.TERTIARY_UNREADABLE"));

    // Post-repair fsck should now consider the tertiary OK.
    let after = check(&dev).unwrap();
    assert!(
        !after.issues.iter().any(|i| i.code == "ODF.SB.TERTIARY_UNREADABLE"),
        "post-repair issues: {:?}",
        after.issues
    );
}

#[test]
fn repairs_stale_secondary_superblock() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let sec = sb.secondary_superblock_block;
    let mut buf = dev.read_at(sec * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    buf[12] ^= 0xAA;
    dev.write_at(sec * BLOCK_SIZE, &buf).unwrap();

    let report = check(&dev).unwrap();
    let rep = repair(&mut dev, &report).unwrap();
    assert!(rep.fixed.contains(&"ODF.SB.SECONDARY_UNREADABLE"));
    let after = check(&dev).unwrap();
    assert!(
        !after
            .issues
            .iter()
            .any(|i| i.code == "ODF.SB.SECONDARY_UNREADABLE"),
        "remaining issues: {:?}", after.issues
    );
}

#[test]
fn repairs_stale_block_bitmap_crc_field() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    // Read the primary SB, mangle its block_bitmap_crc field, rewrite.
    let mut sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let mut sb2 = sb.clone();
    sb2.block_bitmap_crc ^= 0xDEAD_BEEF;
    sb_bytes = sb2.encode_block();
    dev.write_at(BLOCK_SIZE, &sb_bytes).unwrap();
    dev.write_at(sb.tertiary_superblock_block * BLOCK_SIZE, &sb_bytes).unwrap();
    dev.write_at(sb.secondary_superblock_block * BLOCK_SIZE, &sb_bytes).unwrap();

    let report = check(&dev).unwrap();
    assert!(report.issues.iter().any(|i| i.code == "ODF.BBM.CRC"));

    let rep = repair(&mut dev, &report).unwrap();
    assert!(rep.fixed.contains(&"ODF.BBM.CRC"));

    let after = check(&dev).unwrap();
    assert!(
        !after.issues.iter().any(|i| i.code == "ODF.BBM.CRC"),
        "post-repair: {:?}", after.issues
    );
}

#[test]
fn repairs_wrong_free_blocks_counter() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let mut sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let mut sb2 = sb.clone();
    sb2.free_blocks = sb.free_blocks / 2; // deliberately wrong
    sb_bytes = sb2.encode_block();
    dev.write_at(BLOCK_SIZE, &sb_bytes).unwrap();
    dev.write_at(sb.tertiary_superblock_block * BLOCK_SIZE, &sb_bytes).unwrap();
    dev.write_at(sb.secondary_superblock_block * BLOCK_SIZE, &sb_bytes).unwrap();

    let report = check(&dev).unwrap();
    assert!(report.issues.iter().any(|i| i.code == "ODF.SB.FREE_BLOCKS"));

    repair(&mut dev, &report).unwrap();
    let after = check(&dev).unwrap();
    assert!(
        !after.issues.iter().any(|i| i.code == "ODF.SB.FREE_BLOCKS"),
        "post-repair: {:?}", after.issues
    );
}

#[test]
fn leaves_unfixable_issues_untouched() {
    // Manually build a fake report with a code the repair doesn't handle.
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let synthetic = crate::storage::ondisk::fsck::FsckReport {
        issues: vec![crate::storage::ondisk::fsck::FsckIssue {
            severity: crate::storage::ondisk::fsck::Severity::Error,
            code: "ODF.BLOCK.DOUBLE_ALLOCATED",
            message: "data block 42 claimed by both slot 10 and slot 11".into(),
        }],
        inodes_checked: 0,
        extents_checked: 0,
        blocks_referenced: 0,
    };
    let rep = repair(&mut dev, &synthetic).unwrap();
    assert!(rep.fixed.is_empty());
    assert_eq!(rep.unfixable.len(), 1);
    assert_eq!(rep.unfixable[0].code, "ODF.BLOCK.DOUBLE_ALLOCATED");
}

#[test]
fn repair_is_idempotent() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    // Corrupt tertiary SB.
    let tert = sb.tertiary_superblock_block;
    let mut buf = dev.read_at(tert * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    buf[100] ^= 0x5A;
    dev.write_at(tert * BLOCK_SIZE, &buf).unwrap();

    let r1 = check(&dev).unwrap();
    let rep1 = repair(&mut dev, &r1).unwrap();
    assert!(!rep1.fixed.is_empty());

    // Second pass on an already-clean volume: no journaled writes.
    let r2 = check(&dev).unwrap();
    let rep2 = repair(&mut dev, &r2).unwrap();
    assert_eq!(
        rep2.ops_committed, 0,
        "second pass should write nothing — fixed={:?}, remaining={:?}",
        rep2.fixed, r2.issues
    );
}

#[test]
fn parse_slot_from_message_recovers_slot_number() {
    assert_eq!(
        super::parse_slot_from_message("slot 42 marked allocated but record kind = Unused"),
        Some(42)
    );
    assert_eq!(
        super::parse_slot_from_message("slot 0 extent block 100 not marked allocated"),
        Some(0)
    );
    assert_eq!(super::parse_slot_from_message("no slot here"), None);
}

#[test]
fn parse_block_from_message_recovers_block_number() {
    assert_eq!(
        super::parse_block_from_message("slot 5 extent block 123 not marked allocated"),
        Some(123)
    );
    assert_eq!(
        super::parse_block_from_message("slot 7 attr block 99 not marked allocated"),
        Some(99)
    );
    assert_eq!(super::parse_block_from_message("nothing to see"), None);
}

#[test]
fn report_records_committed_op_count() {
    let mut dev = make_dev();
    save_state(&mut dev, &empty_state()).unwrap();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let tert = sb.tertiary_superblock_block;
    let mut buf = dev.read_at(tert * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    buf[0] ^= 0xFF;
    dev.write_at(tert * BLOCK_SIZE, &buf).unwrap();

    let r = check(&dev).unwrap();
    let rep = repair(&mut dev, &r).unwrap();
    assert!(rep.ops_committed >= 1);
}
