// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::layout::{BLOCK_SIZE, LayoutGeometry, LayoutParams};

fn formatted_device() -> (MemoryDevice, Superblock) {
    let mut dev = MemoryDevice::new(4096 * BLOCK_SIZE, 4096).unwrap();
    let geom = LayoutGeometry::plan(LayoutParams {
        total_blocks: 4096,
        inode_count: 256,
        journal_blocks: 32,
    })
    .unwrap();
    let sb = Superblock::for_new_volume(&geom, [0u8; 16], "j", 0);
    let sb_bytes = sb.encode_block();
    dev.write_at(BLOCK_SIZE, &sb_bytes).unwrap();
    (dev, sb)
}

#[test]
fn format_and_open_roundtrip_header() {
    let (mut dev, sb) = formatted_device();
    {
        let mut j = Journal::format(&mut dev, &sb).unwrap();
        assert_eq!(j.header().tail_offset, 0);
        assert_eq!(j.header().state, JSTATE_CLEAN);
    }
    let j2 = Journal::open(&mut dev, &sb).unwrap();
    assert_eq!(j2.header().magic, JOURNAL_MAGIC);
    assert_eq!(j2.header().version, 1);
}

#[test]
fn commit_then_replay_applies_ops() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();

    {
        let mut j = Journal::open(&mut dev, &sb).unwrap();
        let mut txn = j.begin();
        let payload = vec![0xAB; BLOCK_SIZE as usize];
        txn.push(Op::BlockWrite {
            block: sb.data_start,
            data: payload.clone(),
        });
        let txn_id = txn.commit(&mut j).unwrap();
        assert_eq!(txn_id, 1);
        assert!(j.header().tail_offset > 0);
    }

    // Second session: replay the journal, which should apply the BlockWrite.
    {
        let mut j = Journal::open(&mut dev, &sb).unwrap();
        let applied = j.replay().unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].txn_id, 1);
        j.checkpoint().unwrap();
        assert_eq!(j.header().tail_offset, 0);
    }

    let data = dev.read_at(sb.data_start * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(data.iter().all(|b| *b == 0xAB));
}

#[test]
fn abort_discards_ops() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();
    let mut j = Journal::open(&mut dev, &sb).unwrap();
    let mut txn = j.begin();
    txn.push(Op::BlockWrite {
        block: sb.data_start,
        data: vec![0xCC; BLOCK_SIZE as usize],
    });
    txn.abort(&mut j).unwrap();
    let applied = j.replay().unwrap();
    assert!(applied.is_empty());
    let data = dev.read_at(sb.data_start * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(data.iter().all(|b| *b == 0));
}

#[test]
fn partial_uncommitted_txn_is_dropped() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();
    let mut j = Journal::open(&mut dev, &sb).unwrap();
    let mut txn = j.begin();
    txn.push(Op::BlockWrite {
        block: sb.data_start,
        data: vec![0xDD; BLOCK_SIZE as usize],
    });
    // Simulate crash by consuming the builder into ops directly and
    // writing only op records without a commit record.
    let ops = txn.ops;
    let txn_id = txn.txn_id;
    for op in &ops {
        let bytes = op.encode();
        j.append_record(RecordKind::Op, txn_id, &bytes).unwrap();
    }
    // No commit record → replay must not apply.
    let applied = j.replay().unwrap();
    assert!(applied.is_empty());
    let data = dev.read_at(sb.data_start * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(data.iter().all(|b| *b == 0));
}

#[test]
fn multi_transaction_replay_preserves_order() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();
    {
        let mut j = Journal::open(&mut dev, &sb).unwrap();
        for i in 0..3 {
            let mut txn = j.begin();
            txn.push(Op::BlockWrite {
                block: sb.data_start + i,
                data: vec![i as u8 + 1; BLOCK_SIZE as usize],
            });
            txn.commit(&mut j).unwrap();
        }
    }
    let mut j = Journal::open(&mut dev, &sb).unwrap();
    let applied = j.replay().unwrap();
    assert_eq!(applied.len(), 3);
    for i in 0..3 {
        let data = dev
            .read_at((sb.data_start + i) * BLOCK_SIZE, BLOCK_SIZE)
            .unwrap();
        assert!(data.iter().all(|b| *b == i as u8 + 1));
    }
}

#[test]
fn commit_empty_txn_is_rejected() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();
    let mut j = Journal::open(&mut dev, &sb).unwrap();
    let txn = j.begin();
    assert!(txn.commit(&mut j).is_err());
}

#[test]
fn record_corruption_stops_replay_gracefully() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();
    {
        let mut j = Journal::open(&mut dev, &sb).unwrap();
        let mut txn = j.begin();
        txn.push(Op::BlockWrite {
            block: sb.data_start,
            data: vec![0x11; BLOCK_SIZE as usize],
        });
        txn.commit(&mut j).unwrap();
    }
    // Corrupt first byte of the first record (the magic).
    let record_block_offset = (sb.journal_start + 1) * BLOCK_SIZE;
    let mut block = dev.read_at(record_block_offset, BLOCK_SIZE).unwrap();
    block[0] ^= 0xFF;
    dev.write_at(record_block_offset, &block).unwrap();

    let mut j = Journal::open(&mut dev, &sb).unwrap();
    let applied = j.replay().unwrap();
    assert!(applied.is_empty());
}

#[test]
fn inode_update_op_encode_decode_roundtrip() {
    let op = Op::InodeUpdate {
        index: 42,
        record: vec![0xA5; crate::storage::ondisk::inode::INODE_RECORD_SIZE],
    };
    let enc = op.encode();
    let dec = Op::decode(&enc).unwrap();
    assert_eq!(op, dec);
}

#[test]
fn block_write_op_encode_decode_roundtrip() {
    let op = Op::BlockWrite {
        block: 100,
        data: vec![0x55; BLOCK_SIZE as usize],
    };
    let enc = op.encode();
    let dec = Op::decode(&enc).unwrap();
    assert_eq!(op, dec);
}

#[test]
fn inspect_returns_header_without_mutation() {
    let (mut dev, sb) = formatted_device();
    Journal::format(&mut dev, &sb).unwrap();
    let hdr = Journal::inspect(&dev, &sb).unwrap();
    assert_eq!(hdr.tail_offset, 0);
    assert_eq!(hdr.next_txn_id, 1);
}
