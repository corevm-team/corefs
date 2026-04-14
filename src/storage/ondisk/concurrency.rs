// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Concurrency-safety scenarios for the ODF layer.
//!
//! `BlockDevice` is `Send` but not `Sync`: the trait explicitly
//! models ownership of mutable state (the underlying file descriptor
//! or block cache), so any sharing across threads must route through
//! `Arc<Mutex<…>>` or the equivalent.  These tests codify what works
//! today, and provide a foundation for future lock-free paths.
//!
//! Scope of what each test demonstrates:
//!
//! * pure primitives ([`Crc32c`], [`Bitmap`] operating on disjoint
//!   owned instances) are trivially thread-safe because they don't
//!   share state — verified by exercising them across real
//!   `std::thread` spawns.
//! * [`OdfReader`]: an `Arc<dyn BlockDevice>` wrapped in a `Mutex` can
//!   service many concurrent read-only consumers (serialised on the
//!   mutex, correct under contention).
//! * [`OdfDeviceSession`]: mutate is **not** safe to run concurrently
//!   on the same volume; the test documents this by showing a safe
//!   pattern (one writer, N readers via periodic flush + reopen).
//!
//! No module data here — just the sibling test file.

#[cfg(test)]
#[path = "concurrency_tests.rs"]
mod tests;
