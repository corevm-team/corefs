// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Hot / cold storage tiering (D.11).
//!
//! Provides the two primitives an enterprise tiering policy needs:
//!
//! * [`TieredDevice`] — a [`crate::storage::block_device::BlockDevice`] that routes every read and
//!   write to one of two inner devices (hot / cold) based on a
//!   [`TierMap`] block-level assignment.  The outer device looks
//!   exactly like a single flat block address space to upper layers
//!   (volume, native, grouped).
//! * [`HotnessTracker`] + [`Migrator`] — access-frequency counters
//!   plus a policy-driven pass that promotes frequently-read blocks
//!   into the hot tier and demotes cold ones.
//!
//! ## Addressing model
//!
//! Both inner devices are expected to expose the same logical block
//! address space (same capacity, same sector size).  Each block
//! appears on *exactly one* inner device at any given time; the
//! [`TierMap`] records which.  Migration between tiers is a read-
//! from-source + write-to-destination + flip-of-the-map entry,
//! atomic from the caller's perspective since the wrapper serialises
//! all access.
//!
//! ## Persistence
//!
//! [`TierMap`] is in-memory here; persisting the map into an ODF
//! volume would dedicate a reserved region analogous to the refcount
//! table.  The current primitives are the foundation — wiring into
//! [`super::volume::format_device`] is a follow-up.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;
use core::fmt;

use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::{BlockDevice, DeviceGeometry};

use super::layout::BLOCK_SIZE;

/// Storage tier assigned to a data block.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Hot = 0,
    Cold = 1,
}

/// Block-level tier assignment.  Blocks not present in the map
/// default to the [`TierMap::default_tier`] value set at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierMap {
    assignments: BTreeMap<u64, Tier>,
    default_tier: Tier,
}

impl TierMap {
    pub fn new(default_tier: Tier) -> Self {
        Self {
            assignments: BTreeMap::new(),
            default_tier,
        }
    }

    pub fn default_tier(&self) -> Tier {
        self.default_tier
    }

    pub fn get(&self, block: u64) -> Tier {
        self.assignments
            .get(&block)
            .copied()
            .unwrap_or(self.default_tier)
    }

    pub fn set(&mut self, block: u64, tier: Tier) {
        if tier == self.default_tier {
            self.assignments.remove(&block);
        } else {
            self.assignments.insert(block, tier);
        }
    }

    pub fn blocks_in_tier(&self, tier: Tier) -> Vec<u64> {
        if tier == self.default_tier {
            // Default-tier blocks aren't enumerated — callers have to
            // intersect with the valid-block range.
            return Vec::new();
        }
        self.assignments
            .iter()
            .filter(|(_, t)| **t == tier)
            .map(|(b, _)| *b)
            .collect()
    }

    pub fn explicit_entries(&self) -> usize {
        self.assignments.len()
    }
}

/// Access-frequency tracker.  Reads and writes each bump a per-block
/// counter; the [`Migrator`] uses these numbers plus a [`TierPolicy`]
/// to decide what to promote / demote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HotnessTracker {
    reads: BTreeMap<u64, u64>,
    writes: BTreeMap<u64, u64>,
}

impl HotnessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn note_read(&mut self, block: u64) {
        *self.reads.entry(block).or_insert(0) += 1;
    }

    pub fn note_write(&mut self, block: u64) {
        *self.writes.entry(block).or_insert(0) += 1;
    }

    /// Total reads + writes to `block` since the tracker was created
    /// or last reset.
    pub fn heat(&self, block: u64) -> u64 {
        self.reads.get(&block).copied().unwrap_or(0) + self.writes.get(&block).copied().unwrap_or(0)
    }

    /// Top-N hottest blocks by `heat`.
    pub fn hottest(&self, n: usize) -> Vec<(u64, u64)> {
        let mut all: BTreeMap<u64, u64> = BTreeMap::new();
        for (b, c) in &self.reads {
            *all.entry(*b).or_insert(0) += c;
        }
        for (b, c) in &self.writes {
            *all.entry(*b).or_insert(0) += c;
        }
        let mut v: Vec<_> = all.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.truncate(n);
        v
    }

    pub fn reset(&mut self) {
        self.reads.clear();
        self.writes.clear();
    }
}

/// Policy controlling when a block moves between tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPolicy {
    /// Minimum heat for a block to be promoted to the hot tier.
    pub promote_heat_threshold: u64,
    /// Maximum heat for a block to remain on the hot tier.  Blocks
    /// with heat strictly below this threshold get demoted.
    pub demote_heat_threshold: u64,
    /// Hard cap on the number of blocks migrated per rebalance call.
    /// Keeps a single migration pass bounded in cost.
    pub max_migrations_per_pass: usize,
}

impl TierPolicy {
    pub fn balanced() -> Self {
        Self {
            promote_heat_threshold: 8,
            demote_heat_threshold: 1,
            max_migrations_per_pass: 64,
        }
    }
}

/// [`crate::storage::block_device::BlockDevice`] wrapper that routes every I/O to one of two
/// underlying devices based on a [`TierMap`].
pub struct TieredDevice<H: BlockDevice, C: BlockDevice> {
    hot: H,
    cold: C,
    map: TierMap,
    geometry: DeviceGeometry,
}

impl<H: BlockDevice, C: BlockDevice> fmt::Debug for TieredDevice<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TieredDevice")
            .field("hot_capacity", &self.hot.capacity())
            .field("cold_capacity", &self.cold.capacity())
            .field("default_tier", &self.map.default_tier)
            .field("explicit_entries", &self.map.explicit_entries())
            .finish()
    }
}

impl<H: BlockDevice, C: BlockDevice> TieredDevice<H, C> {
    pub fn new(hot: H, cold: C, map: TierMap) -> CoreFsResult<Self> {
        if hot.capacity() != cold.capacity() {
            return Err(CoreFsError::InvalidInput(format!(
                "tiered: hot capacity {} != cold capacity {}",
                hot.capacity(),
                cold.capacity()
            )));
        }
        if hot.sector_size() != cold.sector_size() {
            return Err(CoreFsError::InvalidInput(
                "tiered: hot and cold sector sizes must match".into(),
            ));
        }
        let geometry = DeviceGeometry {
            sector_size: hot.sector_size(),
            sector_count: hot.capacity() / u64::from(hot.sector_size()),
            capacity_bytes: hot.capacity(),
            read_only: hot.is_read_only() || cold.is_read_only(),
        };
        Ok(Self {
            hot,
            cold,
            map,
            geometry,
        })
    }

    pub fn map(&self) -> &TierMap {
        &self.map
    }

    pub fn map_mut(&mut self) -> &mut TierMap {
        &mut self.map
    }

    pub fn hot(&self) -> &H {
        &self.hot
    }

    pub fn cold(&self) -> &C {
        &self.cold
    }

    /// Migrate a single block between tiers.  Reads the block from its
    /// current tier, writes it to the target tier, then updates the
    /// map.  Ordering is deliberate: the destination write is durable
    /// before the tier flip, so a crash mid-migration leaves the map
    /// pointing at the still-intact source.
    pub fn migrate_block(&mut self, block: u64, target: Tier) -> CoreFsResult<()> {
        let current = self.map.get(block);
        if current == target {
            return Ok(());
        }
        let offset = block * BLOCK_SIZE;
        let data = match current {
            Tier::Hot => self.hot.read_at(offset, BLOCK_SIZE)?,
            Tier::Cold => self.cold.read_at(offset, BLOCK_SIZE)?,
        };
        match target {
            Tier::Hot => self.hot.write_at(offset, &data)?,
            Tier::Cold => self.cold.write_at(offset, &data)?,
        }
        self.map.set(block, target);
        Ok(())
    }
}

impl<H: BlockDevice, C: BlockDevice> BlockDevice for TieredDevice<H, C> {
    fn geometry(&self) -> &DeviceGeometry {
        &self.geometry
    }

    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        // Route by first block — we require the read range to live
        // entirely in one tier, because spanning tiers would require
        // splitting and rejoining the read.  ODF always does
        // block-sized I/O, so this is never a hot path in practice.
        if length == 0 {
            return Ok(Vec::new());
        }
        let first_block = offset / BLOCK_SIZE;
        let last_block = (offset + length - 1) / BLOCK_SIZE;
        let tier = self.map.get(first_block);
        for b in first_block + 1..=last_block {
            if self.map.get(b) != tier {
                return Err(CoreFsError::InvalidInput(format!(
                    "tiered read crosses tier boundary between block {} and {}",
                    b - 1,
                    b
                )));
            }
        }
        match tier {
            Tier::Hot => self.hot.read_at(offset, length),
            Tier::Cold => self.cold.read_at(offset, length),
        }
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let first_block = offset / BLOCK_SIZE;
        let last_block = (offset + data.len() as u64 - 1) / BLOCK_SIZE;
        let tier = self.map.get(first_block);
        for b in first_block + 1..=last_block {
            if self.map.get(b) != tier {
                return Err(CoreFsError::InvalidInput(format!(
                    "tiered write crosses tier boundary between block {} and {}",
                    b - 1,
                    b
                )));
            }
        }
        match tier {
            Tier::Hot => self.hot.write_at(offset, data),
            Tier::Cold => self.cold.write_at(offset, data),
        }
    }

    fn sync(&mut self) -> CoreFsResult<()> {
        self.hot.sync()?;
        self.cold.sync()?;
        Ok(())
    }

    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
        // Forward TRIM to the tier that currently owns the range.
        if length == 0 {
            return Ok(());
        }
        let first_block = offset / BLOCK_SIZE;
        let tier = self.map.get(first_block);
        match tier {
            Tier::Hot => self.hot.trim(offset, length),
            Tier::Cold => self.cold.trim(offset, length),
        }
    }

    fn supports_trim(&self) -> bool {
        self.hot.supports_trim() && self.cold.supports_trim()
    }
}

/// Policy-driven migration pass.
pub struct Migrator;

/// Summary of a [`Migrator::rebalance`] invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MigrationReport {
    pub promoted: usize,
    pub demoted: usize,
    pub considered: usize,
}

impl Migrator {
    /// Walk the hotness tracker and migrate blocks that cross the
    /// [`TierPolicy`] thresholds.  Migrations are capped at
    /// `policy.max_migrations_per_pass` to keep one pass bounded.
    pub fn rebalance<H: BlockDevice, C: BlockDevice>(
        tiered: &mut TieredDevice<H, C>,
        tracker: &HotnessTracker,
        policy: &TierPolicy,
    ) -> CoreFsResult<MigrationReport> {
        let mut report = MigrationReport::default();
        let mut budget = policy.max_migrations_per_pass;

        // Promote: visit hottest blocks currently on cold, move to hot.
        let hottest = tracker.hottest(1024);
        for (block, heat) in &hottest {
            if budget == 0 {
                break;
            }
            report.considered += 1;
            if *heat >= policy.promote_heat_threshold && tiered.map.get(*block) == Tier::Cold {
                tiered.migrate_block(*block, Tier::Hot)?;
                report.promoted += 1;
                budget -= 1;
            }
        }

        // Demote: visit currently-hot blocks with heat below demote threshold.
        let hot_blocks: Vec<u64> = tiered.map.blocks_in_tier(Tier::Hot).into_iter().collect();
        for block in hot_blocks {
            if budget == 0 {
                break;
            }
            report.considered += 1;
            if tracker.heat(block) < policy.demote_heat_threshold {
                tiered.migrate_block(block, Tier::Cold)?;
                report.demoted += 1;
                budget -= 1;
            }
        }

        Ok(report)
    }
}

#[cfg(test)]
#[path = "tiering_tests.rs"]
mod tests;
