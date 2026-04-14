// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::domain::inode::InodeId;
use crate::storage::block_device::MemoryDevice;
use crate::storage::volume_image;
use crate::storage::volume_wal::{VolumeWal, WalOperation};
use std::time::SystemTime;

fn format_device(capacity: u64) -> Box<MemoryDevice> {
    let mut dev = Box::new(MemoryDevice::new(capacity, 4096).unwrap());
    let service = CoreFsService::format(CoreFsConfig::default());
    let state = service.persisted_state();
    volume_image::save_to_device(dev.as_mut(), &state).unwrap();
    dev
}

fn format_device_with_files(capacity: u64) -> Box<MemoryDevice> {
    let mut dev = Box::new(MemoryDevice::new(capacity, 4096).unwrap());
    let mut service = CoreFsService::format(CoreFsConfig::default());
    service.create_directory("/data").unwrap();
    service
        .create_file("/data/hello.txt", b"Hello, World!", &[])
        .unwrap();
    service
        .create_file("/data/readme.md", b"# CoreFS", &[])
        .unwrap();
    let state = service.persisted_state();
    volume_image::save_to_device(dev.as_mut(), &state).unwrap();
    dev
}

const TWO_MIB: u64 = 2 * 1024 * 1024;

// -----------------------------------------------------------------------
// SegmentIndex
// -----------------------------------------------------------------------

#[test]
fn segment_index_reads_header_and_directory() {
    let dev = format_device(TWO_MIB);
    let index = read_segment_index(dev.as_ref()).unwrap();

    assert_eq!(index.segment_count, 15);
    assert_eq!(index.segments.len(), 15);
    assert!(index.image_end > 0);

    // Check expected segment kinds.
    assert!(index.find(b"SUPR").is_some());
    assert!(index.find(b"SUP2").is_some());
    assert!(index.find(b"CNFG").is_some());
    assert!(index.find(b"DATA").is_some());
    assert!(index.find(b"BLKD").is_some());
}

#[test]
fn segment_index_rejects_unformatted_device() {
    let dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
    let err = read_segment_index(dev.as_ref()).unwrap_err();
    assert!(err.to_string().contains("invalid magic"));
}

#[test]
fn segment_index_find_returns_none_for_unknown() {
    let dev = format_device(TWO_MIB);
    let index = read_segment_index(dev.as_ref()).unwrap();
    assert!(index.find(b"XXXX").is_none());
}

// -----------------------------------------------------------------------
// DeviceVolume — open & read
// -----------------------------------------------------------------------

#[test]
fn device_volume_opens_formatted_device() {
    let dev = format_device(TWO_MIB);
    let vol = DeviceVolume::open(dev).unwrap();

    assert_eq!(vol.index().segment_count, 15);
    assert!(!vol.is_dirty());
    assert_eq!(vol.cached_segment_count(), 0);
}

#[test]
fn device_volume_reads_segment_on_demand() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // No segments cached yet.
    assert_eq!(vol.cached_segment_count(), 0);

    // Read CNFG segment.
    let cnfg = vol.read_segment(b"CNFG").unwrap();
    assert!(!cnfg.is_empty());

    // Now it should be cached.
    assert_eq!(vol.cached_segment_count(), 1);

    // Second read should come from cache.
    let cnfg2 = vol.read_segment(b"CNFG").unwrap();
    assert_eq!(cnfg, cnfg2);
}

#[test]
fn device_volume_reads_data_segment() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    let data = vol.read_segment(b"DATA").unwrap();
    assert!(!data.is_empty());
}

#[test]
fn device_volume_reads_all_segments() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    let kinds: Vec<[u8; 4]> = vol.index().segments.iter().map(|s| s.kind).collect();
    for kind in &kinds {
        let data = vol.read_segment(kind).unwrap();
        // All segments should be readable.
        let _ = data;
    }

    assert_eq!(vol.cached_segment_count(), 15);
}

#[test]
fn device_volume_rejects_unknown_segment() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    let err = vol.read_segment(b"ZZZZ").unwrap_err();
    assert!(matches!(err, CoreFsError::NotFound(_)));
}

// -----------------------------------------------------------------------
// DeviceVolume — write & flush
// -----------------------------------------------------------------------

#[test]
fn device_volume_write_marks_dirty() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    assert!(!vol.is_dirty());
    vol.write_segment(*b"CNFG", vec![1, 2, 3]);
    assert!(vol.is_dirty());
    assert_eq!(vol.dirty_segment_count(), 1);
}

#[test]
fn device_volume_write_read_returns_dirty_data() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    let original = vol.read_segment(b"CNFG").unwrap();
    vol.write_segment(*b"CNFG", vec![0xAA; 10]);

    // Should return dirty data, not original.
    let dirty = vol.read_segment(b"CNFG").unwrap();
    assert_eq!(dirty, vec![0xAA; 10]);
    assert_ne!(dirty, original);
}

#[test]
fn device_volume_flush_writes_and_clears_dirty() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Read a segment, modify it, flush.
    let _original = vol.read_segment(b"CNFG").unwrap();
    vol.write_segment(*b"CNFG", vec![0xBB; 20]);

    vol.flush().unwrap();
    assert!(!vol.is_dirty());

    // Re-read from device to verify persistence.
    vol.invalidate_cache();
    let reread = vol.read_segment(b"CNFG").unwrap();
    assert_eq!(reread, vec![0xBB; 20]);
}

#[test]
fn device_volume_flush_preserves_other_segments() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Read DATA before modification.
    let original_data = vol.read_segment(b"DATA").unwrap();

    // Modify only CNFG.
    vol.write_segment(*b"CNFG", vec![0xCC; 15]);
    vol.flush().unwrap();

    // DATA should be unchanged.
    vol.invalidate_cache();
    let after_data = vol.read_segment(b"DATA").unwrap();
    assert_eq!(original_data, after_data);
}

#[test]
fn device_volume_invalidate_cache_forces_device_read() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    let _first = vol.read_segment(b"CNFG").unwrap();
    assert_eq!(vol.cached_segment_count(), 1);

    vol.invalidate_cache();
    assert_eq!(vol.cached_segment_count(), 0);
}

#[test]
fn device_volume_multiple_flush_cycles() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Cycle 1
    vol.write_segment(*b"CNFG", vec![1; 10]);
    vol.flush().unwrap();

    // Cycle 2
    vol.write_segment(*b"CNFG", vec![2; 20]);
    vol.flush().unwrap();

    vol.invalidate_cache();
    let final_data = vol.read_segment(b"CNFG").unwrap();
    assert_eq!(final_data, vec![2; 20]);
}

#[test]
fn device_volume_flush_no_dirty_is_noop() {
    let dev = format_device(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Flush with nothing dirty should succeed.
    vol.flush().unwrap();
    assert!(!vol.is_dirty());
}

// -----------------------------------------------------------------------
// DeviceVolume — with 512-byte sectors
// -----------------------------------------------------------------------

#[test]
fn device_volume_works_with_512_byte_sectors() {
    let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 512).unwrap());
    let service = CoreFsService::format(CoreFsConfig::default());
    let state = service.persisted_state();
    volume_image::save_to_device(dev.as_mut(), &state).unwrap();

    let mut vol = DeviceVolume::open(dev).unwrap();
    let cnfg = vol.read_segment(b"CNFG").unwrap();
    assert!(!cnfg.is_empty());

    vol.write_segment(*b"CNFG", vec![0xDD; 30]);
    vol.flush().unwrap();
    vol.invalidate_cache();
    let reread = vol.read_segment(b"CNFG").unwrap();
    assert_eq!(reread, vec![0xDD; 30]);
}

// -----------------------------------------------------------------------
// DeviceJournal — basic operations
// -----------------------------------------------------------------------

fn make_test_wal() -> VolumeWal {
    VolumeWal {
        transaction_id: 42,
        label: "test-wal".to_string(),
        created_at: SystemTime::now(),
        operations: vec![
            WalOperation::CreateDirectory {
                path: "/test".to_string(),
                inode: InodeId(1),
            },
            WalOperation::CreateFile {
                path: "/test/file.txt".to_string(),
                inode: InodeId(2),
            },
        ],
    }
}

#[test]
fn device_journal_opens_empty_on_uninitialized_device() {
    let dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
    let journal = DeviceJournal::open(dev.as_ref(), 1024 * 1024).unwrap();

    assert!(!journal.has_entries());
    assert_eq!(journal.generation(), 0);
}

#[test]
fn device_journal_commit_and_read_back() {
    let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
    let offset = 1024 * 1024; // 1 MiB

    let wal = make_test_wal();
    {
        let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();
        journal.commit(dev.as_mut(), &wal).unwrap();
        assert!(journal.has_entries());
        assert_eq!(journal.generation(), 1);
    }

    // Re-open and verify persistence.
    let journal2 = DeviceJournal::open(dev.as_ref(), offset).unwrap();
    assert!(journal2.has_entries());
    let recovered = journal2.entries().unwrap();
    assert_eq!(recovered.transaction_id, 42);
    assert_eq!(recovered.operations.len(), 2);
}

#[test]
fn device_journal_clear_removes_entries() {
    let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
    let offset = 1024 * 1024;

    let wal = make_test_wal();
    let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();
    journal.commit(dev.as_mut(), &wal).unwrap();
    assert!(journal.has_entries());

    journal.clear(dev.as_mut()).unwrap();
    assert!(!journal.has_entries());

    // Re-open should see empty journal.
    let journal2 = DeviceJournal::open(dev.as_ref(), offset).unwrap();
    assert!(!journal2.has_entries());
}

#[test]
fn device_journal_generation_increments() {
    let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
    let offset = 1024 * 1024;

    let wal = make_test_wal();
    let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();

    journal.commit(dev.as_mut(), &wal).unwrap();
    assert_eq!(journal.generation(), 1);

    journal.clear(dev.as_mut()).unwrap();
    assert_eq!(journal.generation(), 2);

    journal.commit(dev.as_mut(), &wal).unwrap();
    assert_eq!(journal.generation(), 3);
}

#[test]
fn device_journal_take_entries_consumes() {
    let mut dev = Box::new(MemoryDevice::new(TWO_MIB, 4096).unwrap());
    let offset = 1024 * 1024;

    let wal = make_test_wal();
    let mut journal = DeviceJournal::open(dev.as_ref(), offset).unwrap();
    journal.commit(dev.as_mut(), &wal).unwrap();

    let taken = journal.take_entries();
    assert!(taken.is_some());
    assert!(!journal.has_entries());
}

#[test]
fn device_journal_no_space_returns_error() {
    // Device too small for journal.
    let dev = Box::new(MemoryDevice::new(4096, 4096).unwrap());
    let mut journal = DeviceJournal::open(dev.as_ref(), 4096).unwrap();

    let wal = make_test_wal();
    let err = journal.commit(&mut *Box::new(MemoryDevice::new(4096, 4096).unwrap()), &wal);
    assert!(err.is_err());
}

// -----------------------------------------------------------------------
// DeviceVolume + DeviceJournal integration
// -----------------------------------------------------------------------

#[test]
fn device_volume_and_journal_coexist() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Journal should be after the image data.
    assert!(vol.journal_offset() > 0);
    assert!(vol.journal_offset() >= vol.index().image_end);

    // Open journal.
    let journal = vol.open_journal().unwrap();
    assert!(!journal.has_entries());
}

#[test]
fn device_volume_journal_commit_survives_flush() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Commit to journal.
    let wal = make_test_wal();
    {
        let mut journal = vol.open_journal().unwrap();
        journal.commit(vol.device_mut(), &wal).unwrap();
    }

    // Modify and flush a segment.
    vol.write_segment(*b"CNFG", vec![0xEE; 12]);
    vol.flush().unwrap();

    // Journal should still be readable (it's in a separate region).
    let journal2 = vol.open_journal().unwrap();
    assert!(journal2.has_entries());
    assert_eq!(journal2.entries().unwrap().transaction_id, 42);
}

#[test]
fn device_volume_journal_clear_after_successful_flush() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Typical workflow: commit journal → flush → clear journal.
    let wal = make_test_wal();
    {
        let mut journal = vol.open_journal().unwrap();
        journal.commit(vol.device_mut(), &wal).unwrap();
    }

    vol.flush().unwrap();

    {
        let mut journal = vol.open_journal().unwrap();
        journal.clear(vol.device_mut()).unwrap();
    }

    let journal_final = vol.open_journal().unwrap();
    assert!(!journal_final.has_entries());
}

// -----------------------------------------------------------------------
// Full round-trip: format → open DeviceVolume → read individual segments
// -----------------------------------------------------------------------

#[test]
fn full_round_trip_format_then_on_demand_read() {
    let dev = format_device_with_files(TWO_MIB);
    let mut vol = DeviceVolume::open(dev).unwrap();

    // Read individual segments on demand.
    let supr = vol.read_segment(b"SUPR").unwrap();
    assert_eq!(supr.len(), SUPERBLOCK_SIZE);

    let cnfg = vol.read_segment(b"CNFG").unwrap();
    assert!(!cnfg.is_empty());

    let blkd = vol.read_segment(b"BLKD").unwrap();
    assert!(!blkd.is_empty());

    let data = vol.read_segment(b"DATA").unwrap();
    assert!(!data.is_empty());

    // Only 4 segments should be cached (the ones we read).
    assert_eq!(vol.cached_segment_count(), 4);
}
