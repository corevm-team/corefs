// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::block_device::MemoryDevice;
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

fn populated_device() -> Box<dyn crate::storage::block_device::BlockDevice> {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 16 * 1024 * 1024;
    opts.inode_count = 256;
    opts.journal_blocks = 32;
    // Disable compression + encryption so the reader sees raw bytes
    // that round-trip identically.  (Compression + encryption are
    // service-level concerns; OdfReader returns bytes as they live on
    // disk.)
    opts.config.performance.compression_enabled = false;
    opts.config.security.encryption_at_rest = false;
    let dev: Box<dyn crate::storage::block_device::BlockDevice> =
        Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| {
        fs.create_file("/alpha.txt", b"alpha content", &[])?;
        fs.create_file("/beta.bin", &[0xABu8; 2048], &[])?;
        fs.create_file("/sub/gamma.log", b"gamma!", &[])?;
        Ok(())
    })
    .unwrap();
    sess.into_device()
}

#[test]
fn open_reads_superblock_and_bitmap() {
    let dev = populated_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    assert_eq!(reader.superblock().layout_mode, LAYOUT_MODE_NATIVE);
    // At least 3 user inodes were created.
    assert!(reader.allocated_user_inodes() >= 3);
}

#[test]
fn open_rejects_blob_mode_volume() {
    let mut dev = MemoryDevice::new(4 * 1024 * 1024, 4096).unwrap();
    crate::storage::ondisk::volume::format_device(
        &mut dev,
        &crate::storage::ondisk::volume::FormatOptions {
            label: "b".into(),
            uuid: [0u8; 16],
            inode_count: 128,
            journal_blocks: 32,
        },
    )
    .unwrap();
    let err = match OdfReader::open(&dev) {
        Ok(_) => panic!("expected error"),
        Err(e) => e,
    };
    assert!(format!("{err}").contains("NATIVE"));
}

#[test]
fn list_inodes_returns_summary_per_slot() {
    let dev = populated_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    let summaries = reader.list_inodes().unwrap();
    assert!(summaries.len() >= 3);
    for s in &summaries {
        assert!(s.slot >= crate::storage::ondisk::native::FIRST_USER_INODE_SLOT);
        assert!(!s.is_deleted);
    }
    // File extents are >= 1 for non-empty files.
    let any_with_extents = summaries.iter().any(|s| s.extent_count > 0);
    assert!(any_with_extents);
}

#[test]
fn slot_for_domain_id_is_cached() {
    let dev = populated_device();
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let first = reader.list_inodes().unwrap()[0].clone();
    let slot = reader.slot_for_domain_id(first.domain_id).unwrap().unwrap();
    assert_eq!(slot, first.slot);
    // Missing domain-id returns None.
    assert_eq!(reader.slot_for_domain_id(999_999).unwrap(), None);
}

#[test]
fn slot_for_path_locates_existing_file() {
    let dev = populated_device();
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let slot = reader.slot_for_path("/alpha.txt").unwrap();
    assert!(slot.is_some());
    assert_eq!(reader.slot_for_path("/does-not-exist.txt").unwrap(), None);
}

#[test]
fn read_file_content_matches_original_bytes() {
    let dev = populated_device();
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let bytes = reader.read_file_by_path("/alpha.txt").unwrap();
    assert_eq!(bytes, b"alpha content");
    let big = reader.read_file_by_path("/beta.bin").unwrap();
    assert_eq!(big.len(), 2048);
    assert!(big.iter().all(|b| *b == 0xAB));
}

#[test]
fn read_file_content_verifies_data_crc() {
    let mut dev = populated_device();
    // Find the data block of /alpha.txt and corrupt it.
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    let summaries = reader.list_inodes().unwrap();
    let alpha_slot = {
        let mut r = OdfReader::open(dev.as_ref()).unwrap();
        r.slot_for_path("/alpha.txt").unwrap().unwrap()
    };
    let rec = reader.read_on_disk_inode(alpha_slot).unwrap();
    drop(reader);
    let data_block = rec.extents[0].physical_block;
    // Now tamper.
    let mut raw = dev.read_at(data_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    raw[0] ^= 0xFF;
    dev.write_at(data_block * BLOCK_SIZE, &raw).unwrap();

    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let slot = reader.slot_for_path("/alpha.txt").unwrap().unwrap();
    let err = reader.read_file_content(slot).unwrap_err();
    assert!(format!("{err}").contains("data CRC"));
    let _ = summaries;
}

#[test]
fn read_on_disk_inode_rejects_unallocated_slots() {
    let dev = populated_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    let err = reader
        .read_on_disk_inode(crate::storage::ondisk::native::FIRST_USER_INODE_SLOT + 100)
        .unwrap_err();
    assert!(matches!(err, crate::error::CoreFsError::NotFound(_)));
}

#[test]
fn read_on_disk_inode_rejects_out_of_range_slot() {
    let dev = populated_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    let err = reader.read_on_disk_inode(999_999).unwrap_err();
    assert!(matches!(err, crate::error::CoreFsError::InvalidInput(_)));
}

#[test]
fn read_inode_metadata_roundtrips_path() {
    let dev = populated_device();
    let mut reader = OdfReader::open(dev.as_ref()).unwrap();
    let slot = reader.slot_for_path("/sub/gamma.log").unwrap().unwrap();
    let inode = reader.read_inode_metadata(slot).unwrap();
    assert_eq!(inode.path, "/sub/gamma.log");
}

#[test]
fn summaries_indicate_deleted_inodes() {
    // Make a fresh volume with a deleted entry.
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = 16 * 1024 * 1024;
    opts.inode_count = 128;
    opts.journal_blocks = 32;
    let dev: Box<dyn crate::storage::block_device::BlockDevice> =
        Box::new(MemoryDevice::new(opts.capacity_bytes, 4096).unwrap());
    let mut sess = OdfDeviceSession::format_new(dev, &opts).unwrap();
    sess.mutate(|fs| {
        fs.create_file("/keeper", b"a", &[])?;
        fs.create_file("/victim", b"b", &[])?;
        fs.delete_file("/victim", false)?;
        Ok(())
    })
    .unwrap();
    let dev = sess.into_device();
    let reader = OdfReader::open(dev.as_ref()).unwrap();
    let summaries = reader.list_inodes().unwrap();
    assert!(summaries.iter().any(|s| s.is_deleted));
}
