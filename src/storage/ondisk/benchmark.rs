// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Lightweight micro-benchmark for the ODF driver.
//!
//! Exercises format → save (blob + native) → load cycles and reports
//! wall-clock durations.  Uses a [`MemoryDevice`] so no disk I/O skews
//! the numbers.  The intent is regression-detection for ODF internals,
//! not an enterprise-grade benchmarking suite — the existing
//! [`crate::platform::performance`] framework remains the canonical
//! harness for whole-system profiles.

use std::time::{Duration, Instant};

use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::domain::metadata::FileMetadata;
use crate::domain::volume::VolumeDescriptor;
use crate::error::CoreFsResult;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::MemoryDevice;
use crate::storage::block_store::{AllocatorPolicy, BlockRecord};

use super::layout::BLOCK_SIZE;
use super::native::{load_state_native, save_state_native};
use super::volume::{FormatOptions, format_device, load_state, save_state};

/// Parameters controlling a single benchmark run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdfBenchConfig {
    pub volume_blocks: u64,
    pub inode_count: u64,
    pub file_count: usize,
    pub payload_size: usize,
}

impl Default for OdfBenchConfig {
    fn default() -> Self {
        Self {
            volume_blocks: 4096,
            inode_count: 1024,
            file_count: 32,
            payload_size: 1024,
        }
    }
}

/// Timings produced by a single benchmark run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdfBenchResult {
    pub format: Duration,
    pub blob_save: Duration,
    pub blob_load: Duration,
    pub native_save: Duration,
    pub native_load: Duration,
    pub files_populated: usize,
    pub bytes_per_file: usize,
}

/// Run the micro-benchmark and return a [`OdfBenchResult`].  Errors
/// propagate — the benchmark aborts early on any I/O / format failure.
pub fn run_odf_bench(cfg: OdfBenchConfig) -> CoreFsResult<OdfBenchResult> {
    let mut dev = MemoryDevice::new(cfg.volume_blocks * BLOCK_SIZE, 4096)?;
    let opts = FormatOptions {
        label: "bench".into(),
        uuid: [0x42u8; 16],
        inode_count: cfg.inode_count,
        journal_blocks: 32,
    };
    let t0 = Instant::now();
    format_device(&mut dev, &opts)?;
    let format = t0.elapsed();

    let state = synthetic_state(cfg.file_count, cfg.payload_size);

    let t1 = Instant::now();
    save_state(&mut dev, &state)?;
    let blob_save = t1.elapsed();

    let t2 = Instant::now();
    let _ = load_state(&dev)?;
    let blob_load = t2.elapsed();

    let t3 = Instant::now();
    save_state_native(&mut dev, &state)?;
    let native_save = t3.elapsed();

    let t4 = Instant::now();
    let _ = load_state_native(&dev)?;
    let native_load = t4.elapsed();

    Ok(OdfBenchResult {
        format,
        blob_save,
        blob_load,
        native_save,
        native_load,
        files_populated: cfg.file_count,
        bytes_per_file: cfg.payload_size,
    })
}

fn synthetic_state(files: usize, payload: usize) -> PersistedState {
    let config = CoreFsConfig::default();
    let mut state = PersistedState {
        volume: VolumeDescriptor::from_config(&config),
        config,
        clean_unmount: true,
        pending_wal: None,
        active_inodes: Vec::new(),
        deleted_inodes: Vec::new(),
        allocator_policy: AllocatorPolicy::default(),
        free_extents: Vec::new(),
        hot_path_records: Vec::new(),
        block_records: Vec::new(),
        journal_entries: Vec::new(),
        journal_runtime: JournalRuntimeState::default(),
        versions: Vec::new(),
        sync_statuses: Vec::new(),
        snapshots: Vec::new(),
        next_snapshot_id: 0,
    };
    let now = corefs_core::platform::Timestamp::EPOCH;
    for i in 0..files {
        let id = InodeId(1000 + i as u64);
        state.active_inodes.push(Inode {
            id,
            kind: InodeKind::File,
            path: format!("/bench/{i}.bin"),
            size: payload,
            created_at: now,
            modified_at: now,
            changed_at: now,
            metadata: FileMetadata::default(),
        });
        state.block_records.push(BlockRecord {
            inode: id,
            bytes: vec![(i % 256) as u8; payload],
            checksum: 0,
            device_block: 0,
            allocated_blocks: 0,
        });
    }
    state
}

#[cfg(test)]
#[path = "benchmark_tests.rs"]
mod tests;
