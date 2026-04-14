// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattformneutrale Storage-Primitive.
//!
//! Aktueller Migrationsstand (Phase 5.2 / 5-cont.): das Modul enthält
//! die `BlockDevice`-Trait-Definition, die `DeviceGeometry`-Beschreibung,
//! Alignment-/Bounds-Helfer und die `MemoryDevice`-Referenz-Implementierung.
//! Datei- und Linux-spezifische Block-Device-Backends bleiben weiterhin
//! im main `corefs` crate (bzw. wandern langfristig in `corefs-std`).

pub mod allocator;
pub mod block_device;
pub mod block_store;
pub mod catalog;
pub mod ondisk;
pub mod persisted_state;
pub mod volume_wal;
