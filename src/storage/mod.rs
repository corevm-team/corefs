// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

// Re-exports of platform-neutral storage primitives that now live in
// corefs-core. Existing call sites such as `crate::storage::allocator::*`,
// `crate::storage::catalog::*` and `crate::storage::block_store::*` continue
// to compile unchanged.
pub use corefs_core::storage::{allocator, block_store, catalog};

pub mod block_device;
pub mod device_volume;
pub mod ondisk;
pub mod volume_image;
pub mod volume_session;
pub mod volume_wal;
