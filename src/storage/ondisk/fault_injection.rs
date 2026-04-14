// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Fault-injecting [`BlockDevice`] wrapper for enterprise-grade
//! resilience tests.
//!
//! `FaultyDevice<D>` wraps an arbitrary [`BlockDevice`] and exposes
//! configurable failure modes for write paths, read paths and fsync
//! barriers.  Tests opt in to one or more fault modes via
//! [`FaultPlan`] and observe the fired-fault counters via
//! [`FaultyDevice::stats`].
//!
//! ## Failure modes
//!
//! * [`FaultPlan::fail_after_writes`] — return [`CoreFsError::State`]
//!   ("ODF-FAULT: write fault") on the *N*-th call into `write_at`.
//!   Models a sudden controller error at a specific point in time.
//! * [`FaultPlan::fail_after_bytes_written`] — fail when the
//!   cumulative-bytes-written counter reaches the threshold.  Used to
//!   simulate ENOSPC roughly proportional to volume usage.
//! * [`FaultPlan::silent_corrupt_writes`] — accept the write but XOR
//!   one byte at the configured offset before persisting.  Models bit
//!   rot.
//! * [`FaultPlan::fail_writes_in_range`] — every write whose
//!   `[offset, offset+len)` intersects `[start, end)` returns an
//!   error.  Models a permanently bad sector.
//! * [`FaultPlan::fail_sync` — the next [`BlockDevice::sync`] call
//!   fails.  Models a power-loss between the last write and the sync
//!   barrier.
//! * [`FaultPlan::stop_after_n_writes` — silently drop subsequent
//!   writes.  Models a power-loss after `N` writes were durable.
//!
//! All counters are exposed via [`FaultStats`].  Tests can
//! `#[derive(Clone)]`-snapshot the stats before and after to assert
//! exactly which faults fired.

use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::{BlockDevice, DeviceGeometry};

/// Behaviour configuration for a [`FaultyDevice`].
#[derive(Debug, Clone, Default)]
pub struct FaultPlan {
    /// If `Some(n)`, the (n+1)-th write returns an error.
    pub fail_after_writes: Option<usize>,
    /// If `Some(b)`, writes start failing once `bytes_written >= b`.
    pub fail_after_bytes_written: Option<u64>,
    /// `(start, length, byte_index_within_write)` — XOR a single byte of
    /// every write that overlaps `[start, start+length)`.
    pub silent_corrupt_writes: Option<(u64, u64, usize)>,
    /// All writes touching `[start, end)` fail with a clear error.
    pub fail_writes_in_range: Option<(u64, u64)>,
    /// The next [`BlockDevice::sync`] call fails.
    pub fail_sync: bool,
    /// After `N` successful writes silently drop subsequent writes
    /// without persisting them — models a power-loss after `N` writes.
    pub stop_after_n_writes: Option<usize>,
}

impl FaultPlan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_after_writes(mut self, n: usize) -> Self {
        self.fail_after_writes = Some(n);
        self
    }
    pub fn fail_after_bytes_written(mut self, n: u64) -> Self {
        self.fail_after_bytes_written = Some(n);
        self
    }
    pub fn silent_corrupt_writes(mut self, start: u64, length: u64, byte: usize) -> Self {
        self.silent_corrupt_writes = Some((start, length, byte));
        self
    }
    pub fn fail_writes_in_range(mut self, start: u64, end: u64) -> Self {
        self.fail_writes_in_range = Some((start, end));
        self
    }
    pub fn fail_sync(mut self) -> Self {
        self.fail_sync = true;
        self
    }
    pub fn stop_after_n_writes(mut self, n: usize) -> Self {
        self.stop_after_n_writes = Some(n);
        self
    }
}

/// Counters maintained by a [`FaultyDevice`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaultStats {
    pub writes_attempted: usize,
    pub writes_persisted: usize,
    pub writes_failed: usize,
    pub writes_silently_dropped: usize,
    pub writes_corrupted: usize,
    pub reads_attempted: usize,
    pub syncs_attempted: usize,
    pub syncs_failed: usize,
    pub bytes_written: u64,
}

/// `BlockDevice` wrapper that injects faults.
#[derive(Debug)]
pub struct FaultyDevice<D: BlockDevice> {
    inner: D,
    plan: FaultPlan,
    stats: FaultStats,
}

impl<D: BlockDevice> FaultyDevice<D> {
    pub fn new(inner: D, plan: FaultPlan) -> Self {
        Self {
            inner,
            plan,
            stats: FaultStats::default(),
        }
    }

    /// Replace the active fault plan.  Useful in tests that progress
    /// through phases (set up cleanly, then arm a fault for the next
    /// operation).
    pub fn set_plan(&mut self, plan: FaultPlan) {
        self.plan = plan;
    }

    /// Snapshot of the counters.
    pub fn stats(&self) -> FaultStats {
        self.stats.clone()
    }

    /// Borrow the underlying device — for tests that want to inspect
    /// the post-fault state.
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// Consume the wrapper and return the inner device.
    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: BlockDevice> BlockDevice for FaultyDevice<D> {
    fn geometry(&self) -> &DeviceGeometry {
        self.inner.geometry()
    }

    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        // Reads aren't currently a configurable fault target — adding
        // one would just be more flag plumbing.  Tests that need a
        // failing read can pre-corrupt the bytes via the underlying
        // MemoryDevice / FileImageDevice.
        self.inner.read_at(offset, length)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        self.stats.writes_attempted += 1;

        // 1) hard write-count threshold
        if let Some(n) = self.plan.fail_after_writes {
            if self.stats.writes_attempted > n {
                self.stats.writes_failed += 1;
                return Err(CoreFsError::State(format!(
                    "ODF-FAULT: write #{} exceeded fail_after_writes={n}",
                    self.stats.writes_attempted
                )));
            }
        }

        // 2) bytes-written threshold (ENOSPC-like)
        if let Some(b) = self.plan.fail_after_bytes_written {
            if self.stats.bytes_written.saturating_add(data.len() as u64) > b {
                self.stats.writes_failed += 1;
                return Err(CoreFsError::State(format!(
                    "ODF-FAULT: write would exceed byte budget ({} + {} > {b})",
                    self.stats.bytes_written,
                    data.len()
                )));
            }
        }

        // 3) bad-sector range
        if let Some((start, end)) = self.plan.fail_writes_in_range {
            let req_end = offset + data.len() as u64;
            if offset < end && req_end > start {
                self.stats.writes_failed += 1;
                return Err(CoreFsError::State(format!(
                    "ODF-FAULT: write at offset {offset} hits bad sector [{start}, {end})"
                )));
            }
        }

        // 4) silent power-loss (drop writes once threshold hit)
        if let Some(n) = self.plan.stop_after_n_writes {
            if self.stats.writes_persisted >= n {
                self.stats.writes_silently_dropped += 1;
                return Ok(()); // pretend it worked
            }
        }

        // 5) silent corruption (write happens but we flip a byte)
        let mut payload = data.to_vec();
        if let Some((start, length, byte)) = self.plan.silent_corrupt_writes {
            let req_end = offset + payload.len() as u64;
            if offset < start + length && req_end > start && byte < payload.len() {
                payload[byte] ^= 0xFF;
                self.stats.writes_corrupted += 1;
            }
        }

        self.inner.write_at(offset, &payload)?;
        self.stats.writes_persisted += 1;
        self.stats.bytes_written = self.stats.bytes_written.saturating_add(data.len() as u64);
        Ok(())
    }

    fn sync(&mut self) -> CoreFsResult<()> {
        self.stats.syncs_attempted += 1;
        if self.plan.fail_sync {
            self.plan.fail_sync = false;
            self.stats.syncs_failed += 1;
            return Err(CoreFsError::State(
                "ODF-FAULT: sync barrier interrupted".into(),
            ));
        }
        self.inner.sync()
    }

    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
        self.inner.trim(offset, length)
    }

    fn supports_trim(&self) -> bool {
        self.inner.supports_trim()
    }
}

#[cfg(test)]
#[path = "fault_injection_tests.rs"]
mod tests;
