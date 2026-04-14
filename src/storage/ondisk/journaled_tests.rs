// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::block_device::MemoryDevice;
use crate::storage::ondisk::inode::{INODE_RECORD_SIZE, OnDiskInode};
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::volume::{FormatOptions, format_device};

fn formatted_device() -> MemoryDevice {
    let mut dev = MemoryDevice::new(4096 * BLOCK_SIZE, 4096).unwrap();
    let opts = FormatOptions {
        label: "j".into(),
        uuid: [0u8; 16],
        inode_count: 256,
        journal_blocks: 32,
    };
    format_device(&mut dev, &opts).unwrap();
    dev
}

fn sample_metadata_block() -> Vec<u8> {
    let mut b = vec![0u8; BLOCK_SIZE as usize];
    b[100..104].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    b
}

#[test]
fn commit_applies_metadata_ops() {
    let mut dev = formatted_device();
    let sb_bytes = dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let sb =
        crate::storage::ondisk::superblock::Superblock::decode_block(&sb_bytes).unwrap();
    let target_block = sb.data_start;
    let payload = sample_metadata_block();

    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.stage_metadata_block(target_block, payload.clone()).unwrap();
    let report = sess.commit().unwrap();
    assert_eq!(report.ops_applied, 1);
    let read_back = dev.read_at(target_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert_eq!(read_back, payload);
}

#[test]
fn data_writes_bypass_journal() {
    let mut dev = formatted_device();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let data_block = sb.data_start + 5;
    let data = vec![0xA5u8; BLOCK_SIZE as usize];

    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.write_data(data_block, &data).unwrap();
    // Still need at least one metadata op to commit.
    sess.stage_metadata_block(sb.data_start, sample_metadata_block())
        .unwrap();
    assert_eq!(sess.staged_ops(), 1);
    sess.commit().unwrap();

    let read = dev.read_at(data_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert_eq!(read, data);
}

#[test]
fn crash_between_commit_and_apply_is_recovered_by_replay() {
    let mut dev = formatted_device();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let target = sb.data_start + 1;
    let new_content = vec![0xFEu8; BLOCK_SIZE as usize];

    // Phase A: open session, stage metadata op, commit but DO NOT apply.
    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.stage_metadata_block(target, new_content.clone()).unwrap();
    let txn = sess.commit_without_apply().unwrap();
    assert_eq!(txn, 1);

    // Target block has NOT yet been updated — it's still zero-filled.
    let before = dev.read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(before.iter().all(|b| *b == 0));

    // Phase B: simulated next mount — recovery replays + checkpoints.
    let report = recover_pending_transactions(&mut dev).unwrap();
    assert_eq!(report.txns_replayed, vec![1]);
    assert_eq!(report.ops_applied, 1);

    // After recovery the metadata write is durable.
    let after = dev.read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert_eq!(after, new_content);

    // Second recovery call is a no-op (journal is clean).
    let second = recover_pending_transactions(&mut dev).unwrap();
    assert!(second.txns_replayed.is_empty());
    assert_eq!(second.ops_applied, 0);
}

#[test]
fn abort_discards_staged_ops() {
    let mut dev = formatted_device();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let target = sb.data_start;
    let payload = sample_metadata_block();

    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.stage_metadata_block(target, payload.clone()).unwrap();
    sess.abort().unwrap();

    // Target block is unchanged.
    let read = dev.read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(read.iter().all(|b| *b == 0));
    // Journal is still clean.
    let report = recover_pending_transactions(&mut dev).unwrap();
    assert!(report.txns_replayed.is_empty());
}

#[test]
fn stage_inode_update_writes_exact_record() {
    let mut dev = formatted_device();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let geom = sb.geometry();

    let mut inode = OnDiskInode::unused();
    inode.kind = crate::storage::ondisk::inode::OnDiskKind::File;
    inode.size_bytes = 42;
    inode.domain_inode_id = 9001;
    let encoded = inode.encode().unwrap();

    let slot = crate::storage::ondisk::native::FIRST_USER_INODE_SLOT;
    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.stage_inode_update(slot, encoded.to_vec()).unwrap();
    sess.commit().unwrap();

    let (block, offset) = geom.inode_record_location(slot).unwrap();
    let buf = dev.read_at(block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    let read_inode = OnDiskInode::decode(
        &buf[offset as usize..offset as usize + INODE_RECORD_SIZE],
    )
    .unwrap();
    assert_eq!(read_inode.domain_inode_id, 9001);
    assert_eq!(read_inode.size_bytes, 42);
}

#[test]
fn wrong_block_size_payload_is_rejected() {
    let mut dev = formatted_device();
    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    assert!(sess.stage_metadata_block(100, vec![0u8; 2048]).is_err());
}

#[test]
fn wrong_inode_record_size_rejected() {
    let mut dev = formatted_device();
    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    assert!(sess.stage_inode_update(10, vec![0u8; 64]).is_err());
}

#[test]
fn commit_without_staged_ops_errors() {
    let mut dev = formatted_device();
    let sess = JournaledSaveSession::open(&mut dev).unwrap();
    assert!(sess.commit().is_err());
}

#[test]
fn multiple_sessions_each_get_their_own_txn_id() {
    let mut dev = formatted_device();
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let b1 = sb.data_start;
    let b2 = sb.data_start + 1;

    let mut s1 = JournaledSaveSession::open(&mut dev).unwrap();
    s1.stage_metadata_block(b1, sample_metadata_block()).unwrap();
    let r1 = s1.commit().unwrap();

    let mut s2 = JournaledSaveSession::open(&mut dev).unwrap();
    s2.stage_metadata_block(b2, sample_metadata_block()).unwrap();
    let r2 = s2.commit().unwrap();

    assert!(r2.txn_id > r1.txn_id);
}

#[test]
fn recover_on_clean_journal_is_noop() {
    let mut dev = formatted_device();
    let report = recover_pending_transactions(&mut dev).unwrap();
    assert!(report.txns_replayed.is_empty());
    assert_eq!(report.ops_applied, 0);
}
