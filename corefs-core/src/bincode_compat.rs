// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Bincode wire-compat layer.
//!
//! Mappt den klassischen `bincode` 1.x API-Stil (`serialize`/`deserialize`)
//! auf `bincode` 2.x mit [`bincode::config::legacy`].  Die produzierten
//! Wire-Bytes sind **byte-identisch** zur Default-Konfiguration von
//! `bincode` 1.x — bestehende On-Disk-Volumes bleiben damit lesbar.
//!
//! Diese Indirektion erlaubt es, `corefs-core` unabhängig vom `std`-Feature
//! gegen `bincode` 2 (no_std-fähig) zu linken.

use alloc::vec::Vec;
use bincode::config::{Configuration, Fixint, LittleEndian, NoLimit, legacy};

/// Liefert die `bincode` 2.x Konfiguration, die bytewise identisch zu
/// `bincode` 1.x Default ist (Little Endian, Fixed-Int, ohne Längenlimit).
#[inline]
const fn cfg() -> Configuration<LittleEndian, Fixint, NoLimit> {
    legacy()
}

/// Serialisiert einen Wert in einen `Vec<u8>` — byte-identisch zu
/// `bincode::serialize` aus `bincode` 1.x.
pub fn serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(value, cfg())
}

/// Deserialisiert einen Wert aus einem Byte-Slice — byte-identisch zu
/// `bincode::deserialize` aus `bincode` 1.x.
pub fn deserialize<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice::<T, _>(bytes, cfg()).map(|(v, _)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CoreFsConfig;
    use crate::domain::volume::VolumeDescriptor;
    use crate::platform::Timestamp;
    use crate::services::journal::JournalRuntimeState;
    use crate::storage::block_store::AllocatorPolicy;
    use crate::storage::persisted_state::PersistedState;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        a: u64,
        b: String,
        c: Vec<u8>,
    }

    /// Hartcodierter Erwartungs-Vector, erzeugt mit `bincode` 1.3.3 Default.
    ///
    /// Layout für `Sample { a: 0xDEADBEEF, b: "hi", c: vec![1,2,3] }`:
    ///
    /// - `a`: 8 Bytes Little-Endian u64 → `EF BE AD DE 00 00 00 00`
    /// - `b`: 8 Bytes Length (u64 LE = 2) + UTF-8 Bytes → `02 00 00 00 00 00 00 00 68 69`
    /// - `c`: 8 Bytes Length (u64 LE = 3) + Bytes → `03 00 00 00 00 00 00 00 01 02 03`
    const SAMPLE_BINCODE1_BYTES: &[u8] = &[
        0xEF, 0xBE, 0xAD, 0xDE, 0x00, 0x00, 0x00, 0x00, // a
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // b length
        0x68, 0x69, // "hi"
        0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // c length
        0x01, 0x02, 0x03, // c content
    ];

    #[test]
    fn wire_bytes_match_bincode1_legacy_default() {
        let s = Sample {
            a: 0xDEAD_BEEFu64,
            b: "hi".to_string(),
            c: vec![1, 2, 3],
        };
        let bytes = serialize(&s).expect("serialize");
        assert_eq!(
            bytes.as_slice(),
            SAMPLE_BINCODE1_BYTES,
            "bincode 2 + legacy() muss byte-identische Ausgabe zu bincode 1.x default liefern",
        );
    }

    #[test]
    fn round_trip_sample() {
        let s = Sample {
            a: 42,
            b: "hello world".to_string(),
            c: vec![9, 8, 7, 6],
        };
        let bytes = serialize(&s).expect("serialize");
        let decoded: Sample = deserialize(&bytes).expect("deserialize");
        assert_eq!(s, decoded);
    }

    #[test]
    fn deserialize_known_bincode1_bytes() {
        // Wir können die hartcodierten bincode-1-Bytes aus dem
        // Wire-Compat-Test wieder einlesen.
        let decoded: Sample =
            deserialize(SAMPLE_BINCODE1_BYTES).expect("deserialize bincode-1 bytes");
        assert_eq!(
            decoded,
            Sample {
                a: 0xDEAD_BEEFu64,
                b: "hi".to_string(),
                c: vec![1, 2, 3],
            }
        );
    }

    #[test]
    fn persisted_state_round_trip() {
        let state = PersistedState {
            config: CoreFsConfig::default(),
            volume: VolumeDescriptor::from_config_at(&CoreFsConfig::default(), Timestamp::EPOCH),
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
        let bytes = serialize(&state).expect("serialize PersistedState");
        let decoded: PersistedState = deserialize(&bytes).expect("deserialize PersistedState");
        assert_eq!(state, decoded);
    }
}
