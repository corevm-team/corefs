// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

fn e(inode: u64, name: &str, kind: DirEntryKind) -> DirEntry {
    DirEntry {
        inode,
        kind,
        name: name.to_string(),
    }
}

#[test]
fn single_entry_roundtrips() {
    let blk = DirBlock {
        next_dir_block: 0,
        entries: vec![e(7, "hello.txt", DirEntryKind::File)],
    };
    let enc = blk.encode().unwrap();
    let dec = DirBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
}

#[test]
fn multiple_entries_preserve_order() {
    let blk = DirBlock {
        next_dir_block: 99,
        entries: vec![
            e(1, "a", DirEntryKind::File),
            e(2, "bb", DirEntryKind::Directory),
            e(3, "ccc", DirEntryKind::Symlink),
            e(4, "system", DirEntryKind::System),
        ],
    };
    let enc = blk.encode().unwrap();
    let dec = DirBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
}

#[test]
fn entry_length_is_8_byte_aligned() {
    let e1 = e(1, "a", DirEntryKind::File);
    let e2 = e(1, "abcdefg", DirEntryKind::File);
    // name=1: header 16 + 1 = 17 → 24
    // name=7: header 16 + 7 = 23 → 24
    assert_eq!(e1.encoded_len().unwrap(), 24);
    assert_eq!(e2.encoded_len().unwrap(), 24);
}

#[test]
fn long_name_rejected() {
    let name = "a".repeat(300);
    let entry = e(1, &name, DirEntryKind::File);
    assert!(entry.encoded_len().is_err());
}

#[test]
fn checksum_detects_corruption() {
    let blk = DirBlock {
        next_dir_block: 0,
        entries: vec![e(1, "x", DirEntryKind::File)],
    };
    let mut enc = blk.encode().unwrap();
    enc[20] ^= 0x40;
    assert!(DirBlock::decode(&enc).is_err());
}

#[test]
fn bad_magic_is_rejected() {
    let blk = DirBlock::empty();
    let mut enc = blk.encode().unwrap();
    enc[0..4].copy_from_slice(&0u32.to_le_bytes());
    let mut zeroed = enc.clone();
    zeroed[4092..4096].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(&zeroed);
    enc[4092..4096].copy_from_slice(&csum.to_le_bytes());
    let err = DirBlock::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("magic"));
}

#[test]
fn overflow_is_rejected_on_encode() {
    let mut blk = DirBlock::empty();
    // 4076 usable bytes / 24 per 7-byte name = 169 entries fit.
    for i in 0..400 {
        blk.entries.push(e(i as u64, "name123", DirEntryKind::File));
    }
    assert!(blk.encode().is_err());
}

#[test]
fn pack_fits_entries_into_reserve() {
    let entries: Vec<DirEntry> = (0..300)
        .map(|i| e(i as u64 + 100, "entry-0x", DirEntryKind::File))
        .collect();
    let blocks_needed = DirBlock::blocks_needed(&entries).unwrap();
    assert!(blocks_needed >= 2);
    let reserve: Vec<u64> = (1000..1000 + blocks_needed as u64).collect();
    let packed = DirBlock::pack(&entries, &reserve).unwrap();
    assert_eq!(packed.len(), blocks_needed);
    // Every block except the last carries a next_dir_block pointer.
    for i in 0..packed.len() - 1 {
        assert_eq!(packed[i].next_dir_block, reserve[i + 1]);
    }
    assert_eq!(packed.last().unwrap().next_dir_block, 0);
    // Total entries match.
    let total: usize = packed.iter().map(|b| b.entries.len()).sum();
    assert_eq!(total, 300);
}

#[test]
fn pack_rejects_insufficient_reserve() {
    let entries: Vec<DirEntry> = (0..500)
        .map(|i| e(i as u64, "longerName", DirEntryKind::File))
        .collect();
    assert!(DirBlock::pack(&entries, &[1000]).is_err());
}

#[test]
fn empty_block_roundtrips() {
    let enc = DirBlock::empty().encode().unwrap();
    let dec = DirBlock::decode(&enc).unwrap();
    assert_eq!(dec, DirBlock::empty());
}

#[test]
fn blocks_needed_returns_at_least_one() {
    assert_eq!(DirBlock::blocks_needed(&[]).unwrap(), 1);
}
