// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::ondisk::layout::BLOCK_SIZE;

fn mem(blocks: u64) -> MemoryDevice {
    MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap()
}

// ---------------------------------------------------------------------------
// TierMap
// ---------------------------------------------------------------------------

#[test]
fn tier_map_returns_default_for_unset_blocks() {
    let m = TierMap::new(Tier::Cold);
    assert_eq!(m.get(0), Tier::Cold);
    assert_eq!(m.get(12345), Tier::Cold);
}

#[test]
fn tier_map_set_overrides_default() {
    let mut m = TierMap::new(Tier::Cold);
    m.set(10, Tier::Hot);
    assert_eq!(m.get(10), Tier::Hot);
    assert_eq!(m.get(11), Tier::Cold);
    assert_eq!(m.explicit_entries(), 1);
}

#[test]
fn tier_map_setting_default_removes_entry() {
    let mut m = TierMap::new(Tier::Cold);
    m.set(10, Tier::Hot);
    m.set(10, Tier::Cold); // back to default → entry should drop out
    assert_eq!(m.explicit_entries(), 0);
}

#[test]
fn tier_map_blocks_in_tier_lists_non_default_only() {
    let mut m = TierMap::new(Tier::Cold);
    m.set(5, Tier::Hot);
    m.set(8, Tier::Hot);
    let hot = m.blocks_in_tier(Tier::Hot);
    assert_eq!(hot, vec![5, 8]);
    let cold = m.blocks_in_tier(Tier::Cold);
    assert!(cold.is_empty()); // default tier isn't enumerated
}

// ---------------------------------------------------------------------------
// HotnessTracker
// ---------------------------------------------------------------------------

#[test]
fn hotness_tracker_counts_reads_and_writes() {
    let mut t = HotnessTracker::new();
    t.note_read(1);
    t.note_read(1);
    t.note_write(1);
    t.note_read(2);
    assert_eq!(t.heat(1), 3);
    assert_eq!(t.heat(2), 1);
    assert_eq!(t.heat(999), 0);
}

#[test]
fn hottest_returns_blocks_in_descending_heat_order() {
    let mut t = HotnessTracker::new();
    for _ in 0..5 {
        t.note_read(10);
    }
    for _ in 0..2 {
        t.note_read(20);
    }
    for _ in 0..10 {
        t.note_write(30);
    }
    let top = t.hottest(3);
    assert_eq!(top, vec![(30, 10), (10, 5), (20, 2)]);
}

#[test]
fn reset_clears_all_counters() {
    let mut t = HotnessTracker::new();
    t.note_read(1);
    t.reset();
    assert_eq!(t.heat(1), 0);
    assert!(t.hottest(10).is_empty());
}

// ---------------------------------------------------------------------------
// TieredDevice
// ---------------------------------------------------------------------------

fn tiered() -> TieredDevice<MemoryDevice, MemoryDevice> {
    let hot = mem(16);
    let cold = mem(16);
    let map = TierMap::new(Tier::Cold);
    TieredDevice::new(hot, cold, map).unwrap()
}

#[test]
fn mismatched_capacities_rejected() {
    let hot = mem(16);
    let cold = mem(8);
    let map = TierMap::new(Tier::Cold);
    assert!(TieredDevice::new(hot, cold, map).is_err());
}

#[test]
fn write_lands_on_default_tier() {
    let mut dev = tiered();
    let payload = vec![0x11u8; BLOCK_SIZE as usize];
    dev.write_at(0, &payload).unwrap();
    // Default is Cold — the byte should be on the cold device.
    assert_eq!(dev.cold().data()[0], 0x11);
    assert_eq!(dev.hot().data()[0], 0x00);
    // Reads route correctly.
    let read = dev.read_at(0, BLOCK_SIZE).unwrap();
    assert_eq!(read, payload);
}

#[test]
fn write_lands_on_hot_tier_when_mapped() {
    let mut dev = tiered();
    dev.map_mut().set(2, Tier::Hot);
    let payload = vec![0xCCu8; BLOCK_SIZE as usize];
    dev.write_at(2 * BLOCK_SIZE, &payload).unwrap();
    assert_eq!(dev.hot().data()[2 * BLOCK_SIZE as usize], 0xCC);
    assert_eq!(dev.cold().data()[2 * BLOCK_SIZE as usize], 0x00);
}

#[test]
fn cross_tier_write_is_rejected() {
    let mut dev = tiered();
    dev.map_mut().set(0, Tier::Hot);
    // Block 0 is hot, block 1 stays cold — a 2-block write would cross.
    let payload = vec![0u8; 2 * BLOCK_SIZE as usize];
    let err = dev.write_at(0, &payload).unwrap_err();
    assert!(format!("{err}").contains("tier boundary"));
}

#[test]
fn cross_tier_read_is_rejected() {
    let mut dev = tiered();
    dev.map_mut().set(0, Tier::Hot);
    let err = dev.read_at(0, 2 * BLOCK_SIZE).unwrap_err();
    assert!(format!("{err}").contains("tier boundary"));
}

#[test]
fn migrate_block_moves_data_between_tiers() {
    let mut dev = tiered();
    let payload = vec![0xABu8; BLOCK_SIZE as usize];
    dev.write_at(5 * BLOCK_SIZE, &payload).unwrap();
    // Live on cold.
    assert_eq!(dev.cold().data()[5 * BLOCK_SIZE as usize], 0xAB);
    assert_eq!(dev.hot().data()[5 * BLOCK_SIZE as usize], 0x00);

    dev.migrate_block(5, Tier::Hot).unwrap();
    assert_eq!(dev.hot().data()[5 * BLOCK_SIZE as usize], 0xAB);
    assert_eq!(dev.map().get(5), Tier::Hot);

    // Reads now come from hot.
    let read = dev.read_at(5 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    assert_eq!(read, payload);
}

#[test]
fn migrate_to_same_tier_is_noop() {
    let mut dev = tiered();
    dev.migrate_block(0, Tier::Cold).unwrap(); // already cold
    assert_eq!(dev.map().get(0), Tier::Cold);
}

#[test]
fn sync_forwards_to_both_inner_devices() {
    let mut dev = tiered();
    // Smoke test: no inner device errors out on sync.
    dev.sync().unwrap();
}

// ---------------------------------------------------------------------------
// Migrator
// ---------------------------------------------------------------------------

#[test]
fn rebalance_promotes_hot_blocks_to_hot_tier() {
    let mut dev = tiered();
    // Populate blocks 0,1,2 on cold (default).
    for b in 0..3 {
        let payload = vec![b as u8; BLOCK_SIZE as usize];
        dev.write_at(b * BLOCK_SIZE, &payload).unwrap();
    }
    // Block 1 gets a lot of reads.
    let mut tracker = HotnessTracker::new();
    for _ in 0..20 {
        tracker.note_read(1);
    }

    let policy = TierPolicy::balanced();
    let report = Migrator::rebalance(&mut dev, &tracker, &policy).unwrap();
    assert_eq!(report.promoted, 1);
    assert_eq!(dev.map().get(1), Tier::Hot);
    // Block 1's content moved.
    assert_eq!(dev.hot().data()[BLOCK_SIZE as usize], 1);
}

#[test]
fn rebalance_demotes_cold_blocks_from_hot_tier() {
    let mut dev = tiered();
    // Pre-seed block 7 on hot.
    dev.map_mut().set(7, Tier::Hot);
    let payload = vec![0x77u8; BLOCK_SIZE as usize];
    dev.write_at(7 * BLOCK_SIZE, &payload).unwrap();
    assert_eq!(dev.hot().data()[7 * BLOCK_SIZE as usize], 0x77);

    // Empty tracker — block 7 has heat 0 → below demote threshold.
    let tracker = HotnessTracker::new();
    let policy = TierPolicy::balanced();
    let report = Migrator::rebalance(&mut dev, &tracker, &policy).unwrap();
    assert_eq!(report.demoted, 1);
    assert_eq!(dev.map().get(7), Tier::Cold);
    assert_eq!(dev.cold().data()[7 * BLOCK_SIZE as usize], 0x77);
}

#[test]
fn rebalance_respects_max_migrations_budget() {
    let mut dev = tiered();
    // Put 10 blocks on hot that will all qualify for demotion.
    for b in 0..10 {
        dev.map_mut().set(b, Tier::Hot);
    }
    let tracker = HotnessTracker::new();
    let mut policy = TierPolicy::balanced();
    policy.max_migrations_per_pass = 3;
    let report = Migrator::rebalance(&mut dev, &tracker, &policy).unwrap();
    assert_eq!(report.demoted, 3, "budget should cap demotions");
}

#[test]
fn balanced_policy_has_reasonable_defaults() {
    let p = TierPolicy::balanced();
    assert!(p.promote_heat_threshold > p.demote_heat_threshold);
    assert!(p.max_migrations_per_pass > 0);
}
