// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! `BlockDevice` wrapper that **captures** all writes into an in-memory
//! table instead of forwarding them to the underlying device.  Callers can
//! then commit the captured set as a single journal transaction via
//! [`JournalCapture::commit_journaled`], giving crash-consistent semantics
//! to existing save pipelines without refactoring them.
//!
//! Reads transparently merge the captured view over the device state so
//! that code doing read-after-write within the same save operation sees
//! its own pending writes.
//!
//! Only writes at block-aligned offsets with block-sized lengths are
//! captured.  Sub-block (sector-aligned but not block-aligned) writes
//! fall back to a write-through on the device — these paths are rare in
//! the ODF code base and are not crash-critical.
//!
//! ```text
//!   +---------------------------+        +-----------------------+
//!   |  save_state_native_*(dev) | -----> |  JournalCapture<'d>   |
//!   +---------------------------+        |  captured: BTree<blk>  |
//!                                        +-----------+-----------+
//!                                                    | commit_journaled()
//!                                                    v
//!                                +-----------------------+
//!                                |  Journal::begin → txn |
//!                                |  push BlockWrite …    |
//!                                |  commit → replay → cp |
//!                                +-----------------------+
//! ```

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;
use core::fmt;

use super::journal::{Journal, Op};
use super::layout::BLOCK_SIZE;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::{BlockDevice, DeviceGeometry};

/// Captures all block-aligned writes into an in-memory map.  Sub-block
/// writes fall back to write-through against the underlying device.
pub struct JournalCapture<'d> {
    inner: &'d mut dyn BlockDevice,
    captured: BTreeMap<u64, Vec<u8>>,
}

impl<'d> JournalCapture<'d> {
    pub fn new(inner: &'d mut dyn BlockDevice) -> Self {
        Self {
            inner,
            captured: BTreeMap::new(),
        }
    }

    /// Number of block-sized writes buffered so far.
    pub fn staged_blocks(&self) -> usize {
        self.captured.len()
    }

    /// Consume the capture and commit all buffered writes as one
    /// journal transaction on the underlying device.  Journal replay
    /// and checkpoint both happen here, so after return the writes are
    /// fully durable.
    ///
    /// If nothing was captured this is a no-op.
    pub fn commit_journaled(self) -> CoreFsResult<usize> {
        if self.captured.is_empty() {
            return Ok(0);
        }
        let JournalCapture { inner, captured } = self;
        // Flush any write-through side-effects before the journal commit
        // so the eventual replay can safely idempotently reapply on top.
        inner.sync()?;
        let sb = super::volume::read_sb_with_fallbacks(inner)?;
        let mut journal = Journal::open(inner, &sb)?;
        let mut txn = journal.begin();
        let count = captured.len();
        for (block, data) in captured {
            txn.push(Op::BlockWrite { block, data });
        }
        txn.commit(&mut journal)?;
        let _applied = journal.replay()?;
        journal.checkpoint()?;
        Ok(count)
    }

    /// Discard all captured writes without committing.  Write-through
    /// side-effects (sub-block writes) remain on disk.
    pub fn abort(self) {
        drop(self);
    }
}

impl fmt::Debug for JournalCapture<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalCapture")
            .field("staged_blocks", &self.captured.len())
            .finish()
    }
}

impl<'d> BlockDevice for JournalCapture<'d> {
    fn geometry(&self) -> &DeviceGeometry {
        self.inner.geometry()
    }

    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        let mut out = self.inner.read_at(offset, length)?;
        if self.captured.is_empty() {
            return Ok(out);
        }
        let start_block = offset / BLOCK_SIZE;
        let end_byte = offset + length;
        let end_block = end_byte.div_ceil(BLOCK_SIZE);
        for block in start_block..end_block {
            if let Some(buf) = self.captured.get(&block) {
                let block_byte_start = block * BLOCK_SIZE;
                // Intersect [block_byte_start, block_byte_start+BLOCK_SIZE)
                // with [offset, offset+length).
                let copy_start_abs = block_byte_start.max(offset);
                let copy_end_abs = (block_byte_start + BLOCK_SIZE).min(end_byte);
                if copy_end_abs <= copy_start_abs {
                    continue;
                }
                let dst_off = (copy_start_abs - offset) as usize;
                let src_off = (copy_start_abs - block_byte_start) as usize;
                let copy_len = (copy_end_abs - copy_start_abs) as usize;
                out[dst_off..dst_off + copy_len]
                    .copy_from_slice(&buf[src_off..src_off + copy_len]);
            }
        }
        Ok(out)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        // Must be block-aligned AND block-sized to be captured.
        let block_sized = (data.len() as u64) % BLOCK_SIZE == 0;
        let block_aligned = offset % BLOCK_SIZE == 0;
        if !block_sized || !block_aligned {
            return self.inner.write_at(offset, data);
        }
        let start_block = offset / BLOCK_SIZE;
        let num_blocks = data.len() as u64 / BLOCK_SIZE;
        for i in 0..num_blocks {
            let block = start_block + i;
            let from = (i * BLOCK_SIZE) as usize;
            let to = ((i + 1) * BLOCK_SIZE) as usize;
            let mut buf = alloc::vec![0u8; BLOCK_SIZE as usize];
            buf.copy_from_slice(&data[from..to]);
            self.captured.insert(block, buf);
        }
        Ok(())
    }

    fn sync(&mut self) -> CoreFsResult<()> {
        // No-op: capture layer holds writes until commit_journaled().
        // We still forward sync to the device so that any pre-existing
        // write-through chunks (sub-block writes) are persisted.
        self.inner.sync()
    }

    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
        self.inner.trim(offset, length)
    }

    fn supports_trim(&self) -> bool {
        self.inner.supports_trim()
    }

    fn capacity(&self) -> u64 {
        self.inner.capacity()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn resize(&mut self, new_capacity: u64) -> CoreFsResult<()> {
        self.inner.resize(new_capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::block_device::{MemoryDevice, SECTOR_SIZE_4K};

    #[test]
    fn captures_block_aligned_writes_and_serves_them_on_read() {
        let mut dev = MemoryDevice::new(16 * BLOCK_SIZE, SECTOR_SIZE_4K).unwrap();
        let mut cap = JournalCapture::new(&mut dev);
        let data = alloc::vec![0xAB; BLOCK_SIZE as usize];
        cap.write_at(3 * BLOCK_SIZE, &data).unwrap();
        // Read back through capture — sees new bytes.
        let read = cap.read_at(3 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
        assert_eq!(read, data);
        // Abort: underlying device still has zeros.
        cap.abort();
        let direct = dev.read_at(3 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
        assert!(direct.iter().all(|b| *b == 0));
    }
}
