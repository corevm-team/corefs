// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::storage::block_device::MemoryDevice;
use crate::storage::ondisk::layout::BLOCK_SIZE;

fn mem(blocks: u64) -> MemoryDevice {
    MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap()
}

#[test]
fn passthrough_with_empty_plan_writes_and_reads_normally() {
    let mut dev = FaultyDevice::new(mem(4), FaultPlan::new());
    dev.write_at(0, &[0xAAu8; BLOCK_SIZE as usize]).unwrap();
    let read = dev.read_at(0, BLOCK_SIZE).unwrap();
    assert!(read.iter().all(|b| *b == 0xAA));
    let s = dev.stats();
    assert_eq!(s.writes_attempted, 1);
    assert_eq!(s.writes_persisted, 1);
    assert_eq!(s.writes_failed, 0);
    assert_eq!(s.bytes_written, BLOCK_SIZE);
}

#[test]
fn fail_after_n_writes_stops_subsequent_writes() {
    let mut dev = FaultyDevice::new(mem(8), FaultPlan::new().fail_after_writes(2));
    let buf = vec![1u8; BLOCK_SIZE as usize];
    dev.write_at(0, &buf).unwrap();
    dev.write_at(BLOCK_SIZE, &buf).unwrap();
    // 3rd write fails.
    assert!(dev.write_at(2 * BLOCK_SIZE, &buf).is_err());
    let s = dev.stats();
    assert_eq!(s.writes_persisted, 2);
    assert_eq!(s.writes_failed, 1);
}

#[test]
fn fail_after_bytes_simulates_enospc() {
    let mut dev = FaultyDevice::new(
        mem(16),
        FaultPlan::new().fail_after_bytes_written(2 * BLOCK_SIZE),
    );
    let buf = vec![0u8; BLOCK_SIZE as usize];
    dev.write_at(0, &buf).unwrap();
    dev.write_at(BLOCK_SIZE, &buf).unwrap();
    let err = dev.write_at(2 * BLOCK_SIZE, &buf).unwrap_err();
    assert!(format!("{err}").contains("byte budget"));
}

#[test]
fn silent_corruption_flips_a_byte_but_write_succeeds() {
    let mut dev = FaultyDevice::new(
        mem(4),
        FaultPlan::new().silent_corrupt_writes(0, BLOCK_SIZE, 42),
    );
    let buf = vec![0xFFu8; BLOCK_SIZE as usize];
    dev.write_at(0, &buf).unwrap();
    let read = dev.read_at(0, BLOCK_SIZE).unwrap();
    // Byte 42 was flipped.
    assert_eq!(read[42], 0x00);
    // Every other byte is intact.
    assert!(read[0..42].iter().all(|b| *b == 0xFF));
    assert!(read[43..].iter().all(|b| *b == 0xFF));
    assert_eq!(dev.stats().writes_corrupted, 1);
}

#[test]
fn bad_sector_range_returns_error_and_preserves_device() {
    let mut dev = FaultyDevice::new(
        mem(8),
        FaultPlan::new().fail_writes_in_range(2 * BLOCK_SIZE, 4 * BLOCK_SIZE),
    );
    let buf = vec![1u8; BLOCK_SIZE as usize];
    dev.write_at(0, &buf).unwrap(); // outside bad range → ok
    let err = dev.write_at(2 * BLOCK_SIZE, &buf).unwrap_err(); // inside bad range
    assert!(format!("{err}").contains("bad sector"));
    // Underlying device was never touched for the failed write.
    let read = dev.read_at(2 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(read.iter().all(|b| *b == 0));
}

#[test]
fn fail_sync_consumes_only_next_call() {
    let mut dev = FaultyDevice::new(mem(4), FaultPlan::new().fail_sync());
    let err = dev.sync().unwrap_err();
    assert!(format!("{err}").contains("sync"));
    // Second sync works.
    dev.sync().unwrap();
    let s = dev.stats();
    assert_eq!(s.syncs_attempted, 2);
    assert_eq!(s.syncs_failed, 1);
}

#[test]
fn stop_after_n_writes_silently_drops_later_writes() {
    let mut dev = FaultyDevice::new(mem(16), FaultPlan::new().stop_after_n_writes(2));
    let buf = vec![0xBBu8; BLOCK_SIZE as usize];
    dev.write_at(0, &buf).unwrap();
    dev.write_at(BLOCK_SIZE, &buf).unwrap();
    // 3rd write returns Ok but is silently dropped.
    dev.write_at(2 * BLOCK_SIZE, &buf).unwrap();
    let read = dev.read_at(2 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert!(
        read.iter().all(|b| *b == 0),
        "write should have been dropped"
    );
    let s = dev.stats();
    assert_eq!(s.writes_persisted, 2);
    assert_eq!(s.writes_silently_dropped, 1);
}

#[test]
fn set_plan_swaps_the_active_plan_mid_run() {
    let mut dev = FaultyDevice::new(mem(8), FaultPlan::new());
    let buf = vec![0u8; BLOCK_SIZE as usize];
    dev.write_at(0, &buf).unwrap();
    dev.set_plan(FaultPlan::new().fail_after_writes(0));
    assert!(dev.write_at(BLOCK_SIZE, &buf).is_err());
}

#[test]
fn into_inner_returns_underlying_device() {
    let dev = FaultyDevice::new(mem(4), FaultPlan::new());
    let inner = dev.into_inner();
    assert_eq!(inner.capacity(), 4 * BLOCK_SIZE);
}

#[test]
fn geometry_is_delegated() {
    let dev = FaultyDevice::new(mem(8), FaultPlan::new());
    assert_eq!(dev.geometry().sector_size, 4096);
    assert_eq!(dev.capacity(), 8 * BLOCK_SIZE);
}
