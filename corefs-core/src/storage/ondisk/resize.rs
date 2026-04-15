// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Grow-Operation für ein formatiertes ODF-Volume.
//!
//! [`grow_device`] erweitert ein bestehendes Volume um zusätzliche Blöcke:
//!
//! * Lädt den primären Superblock.
//! * Validiert, dass die neue Kapazität echt größer ist als die alte und
//!   dass das Device physisch genug Platz hat.
//! * Erweitert die Block-Allokations-Bitmap: alle neuen Bits werden als
//!   `free` markiert (zusätzlich reservieren wir die neuen Positionen der
//!   redundanten Superblock-Kopien).
//! * Aktualisiert Superblock-Felder (`total_blocks`, `free_blocks`,
//!   `secondary_superblock_block`, `tertiary_superblock_block`,
//!   `block_bitmap_crc`) und schreibt Primary + beide Redundanzen zurück.
//! * Flusht das Device.
//!
//! Bewusste Einschränkungen:
//!
//! * **Nur Grow**, kein Shrink.
//! * Die Inode-Tabelle und die Journal-Region werden **nicht** erweitert.
//! * Die Block-Bitmap muss in ihrer bestehenden Ausdehnung (`block_bitmap_blocks`)
//!   genug Reserve haben, um die neue Block-Anzahl aufzunehmen. Ist das
//!   nicht der Fall, schlägt die Operation mit [`CoreFsError::State`] fehl —
//!   das Bitmap selbst zu vergrößern wäre ein Layout-Umbau und kommt in
//!   einem separaten Feature.

use alloc::format;
use alloc::string::ToString;

use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;

use super::bitmap::Bitmap;
use super::checksum::Crc32c;
use super::layout::{BITS_PER_BITMAP_BLOCK, BLOCK_SIZE, PRIMARY_SUPERBLOCK_BLOCK};
use super::superblock::Superblock;
use super::volume::read_sb_with_fallbacks;

/// Report einer erfolgreichen [`grow_device`]-Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrowReport {
    /// Volumengröße in Blöcken vor dem Grow.
    pub old_total_blocks: u64,
    /// Volumengröße in Blöcken nach dem Grow.
    pub new_total_blocks: u64,
    /// Anzahl hinzugefügter Blöcke (`new - old`).
    pub added_blocks: u64,
}

/// Erweitert ein formatiertes ODF-Volume auf `new_capacity_blocks` Blöcke.
///
/// Siehe Modul-Doku für Semantik und Einschränkungen.
pub fn grow_device(
    device: &mut dyn BlockDevice,
    new_capacity_blocks: u64,
) -> CoreFsResult<GrowReport> {
    let mut sb = read_sb_with_fallbacks(device)?;
    let old_total = sb.total_blocks;

    if new_capacity_blocks <= old_total {
        return Err(CoreFsError::InvalidInput(format!(
            "grow: new_capacity_blocks {new_capacity_blocks} must exceed current total_blocks {old_total}"
        )));
    }
    let required_bytes = new_capacity_blocks
        .checked_mul(BLOCK_SIZE)
        .ok_or_else(|| CoreFsError::InvalidInput("grow: capacity overflow".to_string()))?;
    if required_bytes > device.capacity() {
        return Err(CoreFsError::InvalidInput(format!(
            "grow: device physical capacity {} < required {}",
            device.capacity(),
            required_bytes
        )));
    }

    // The existing bitmap region must be wide enough to address every new bit.
    let bitmap_capacity_bits = sb.block_bitmap_blocks * BITS_PER_BITMAP_BLOCK;
    if new_capacity_blocks > bitmap_capacity_bits {
        return Err(CoreFsError::State(format!(
            "grow: block bitmap holds {} bits, need {} — bitmap extension is not yet supported",
            bitmap_capacity_bits, new_capacity_blocks
        )));
    }

    // Load the current bitmap buffer, re-wrap with the new (larger) capacity.
    let bitmap_bytes = device.read_at(
        sb.block_bitmap_start * BLOCK_SIZE,
        sb.block_bitmap_blocks * BLOCK_SIZE,
    )?;
    let mut bitmap = Bitmap::from_bytes(bitmap_bytes, new_capacity_blocks)?;

    // New redundant-superblock positions.  The old ones (inside the former
    // data region) remain free — they are simply no longer pinned.
    let new_secondary = new_capacity_blocks - 1;
    let new_tertiary = new_capacity_blocks / 2;

    // Mark the new redundancy-block positions as allocated.  Ranges above
    // the old capacity come in as naturally-zero bits from `from_bytes`
    // (the bitmap buffer was zero-padded beyond the used prefix), which
    // is exactly what we want for "free".
    bitmap.set(new_secondary)?;
    bitmap.set(new_tertiary)?;

    // Persist the (possibly mutated) bitmap bytes.
    let bitmap_bytes = bitmap.as_bytes();
    let bitmap_crc = Crc32c::hash(bitmap_bytes);
    device.write_at(sb.block_bitmap_start * BLOCK_SIZE, bitmap_bytes)?;

    // Superblock updates.  `free_blocks` gains:
    //  + (new_capacity_blocks - old_total)   — all newly added blocks
    //  + 2                                   — old redundancy slots freed
    //  - 2                                   — new redundancy slots reserved
    // so net gain == added_blocks.
    let added = new_capacity_blocks - old_total;
    sb.total_blocks = new_capacity_blocks;
    sb.free_blocks = sb.free_blocks.saturating_add(added);
    sb.secondary_superblock_block = new_secondary;
    sb.tertiary_superblock_block = new_tertiary;
    sb.block_bitmap_crc = bitmap_crc;

    let sb_block = sb.encode_block();
    device.write_at(PRIMARY_SUPERBLOCK_BLOCK * BLOCK_SIZE, &sb_block)?;
    device.write_at(new_tertiary * BLOCK_SIZE, &sb_block)?;
    device.write_at(new_secondary * BLOCK_SIZE, &sb_block)?;
    device.sync()?;

    Ok(GrowReport {
        old_total_blocks: old_total,
        new_total_blocks: new_capacity_blocks,
        added_blocks: added,
    })
}

/// Hilfs-Alias für `read_sb_with_fallbacks` — dokumentiert, dass das
/// Resize-Modul den primären Superblock explizit lesen darf.
#[allow(dead_code)]
fn read_primary(device: &dyn BlockDevice) -> CoreFsResult<Superblock> {
    read_sb_with_fallbacks(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::block_device::MemoryDevice;
    use crate::storage::ondisk::volume::{format_device, FormatOptions};
    use alloc::boxed::Box;

    fn make_volume(device_capacity_bytes: u64, fs_capacity_blocks: u64) -> Box<dyn BlockDevice> {
        // The device is sized for the *final* capacity; we first format it to a
        // smaller nominal capacity by only formatting within a sub-window.
        // However, `format_device` uses `device.capacity() / BLOCK_SIZE`, so we
        // model this by creating a device exactly at the initial size, running
        // format, and then return a **larger** device for grow tests by passing
        // `device_capacity_bytes`. We satisfy both via a two-step approach:
        // create a device at fs_capacity, format, then copy its bytes into a
        // larger device.
        let mut small = MemoryDevice::new(fs_capacity_blocks * BLOCK_SIZE, 4096).unwrap();
        format_device(&mut small, &FormatOptions::default()).unwrap();
        let mut big_buf = alloc::vec![0u8; device_capacity_bytes as usize];
        big_buf[..small.data().len()].copy_from_slice(small.data());
        Box::new(MemoryDevice::from_bytes(big_buf, 4096).unwrap())
    }

    #[test]
    fn grow_rejects_shrink_or_equal() {
        let mut dev = make_volume(4096 * BLOCK_SIZE, 1024);
        let err = grow_device(dev.as_mut(), 1024).expect_err("equal must fail");
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
        let err = grow_device(dev.as_mut(), 512).expect_err("shrink must fail");
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn grow_rejects_beyond_physical_capacity() {
        let mut dev = make_volume(2048 * BLOCK_SIZE, 1024);
        let err = grow_device(dev.as_mut(), 4096).expect_err("beyond device must fail");
        assert!(matches!(err, CoreFsError::InvalidInput(_)));
    }

    #[test]
    fn grow_updates_superblock_and_bitmap() {
        let mut dev = make_volume(4096 * BLOCK_SIZE, 1024);
        let report = grow_device(dev.as_mut(), 2048).expect("grow ok");
        assert_eq!(report.old_total_blocks, 1024);
        assert_eq!(report.new_total_blocks, 2048);
        assert_eq!(report.added_blocks, 1024);

        let sb = read_sb_with_fallbacks(dev.as_ref()).expect("sb readable");
        assert_eq!(sb.total_blocks, 2048);
        assert_eq!(sb.secondary_superblock_block, 2047);
        assert_eq!(sb.tertiary_superblock_block, 1024);
    }

    #[test]
    fn grow_free_blocks_increases_by_added() {
        let mut dev = make_volume(4096 * BLOCK_SIZE, 1024);
        let sb_before = read_sb_with_fallbacks(dev.as_ref()).unwrap();
        let free_before = sb_before.free_blocks;
        let _ = grow_device(dev.as_mut(), 1536).expect("grow ok");
        let sb_after = read_sb_with_fallbacks(dev.as_ref()).unwrap();
        assert_eq!(sb_after.free_blocks, free_before + 512);
    }

    #[test]
    fn grow_writes_redundant_superblocks_at_new_positions() {
        let mut dev = make_volume(4096 * BLOCK_SIZE, 1024);
        let _ = grow_device(dev.as_mut(), 2048).expect("grow ok");
        // Decode superblock at new secondary and tertiary positions.
        let block_sec = dev.read_at(2047 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
        let sb_sec = Superblock::decode_block(&block_sec).expect("secondary SB decodes");
        assert_eq!(sb_sec.total_blocks, 2048);
        let block_tert = dev.read_at(1024 * BLOCK_SIZE, BLOCK_SIZE).unwrap();
        let sb_tert = Superblock::decode_block(&block_tert).expect("tertiary SB decodes");
        assert_eq!(sb_tert.total_blocks, 2048);
    }

    #[test]
    fn grow_rejects_when_bitmap_too_small() {
        // Default geometry gives block_bitmap_blocks=1 -> 32768 bits fit.
        let mut dev = make_volume(200_000 * BLOCK_SIZE, 1024);
        let err = grow_device(dev.as_mut(), 40_000).expect_err("bitmap overflow must fail");
        assert!(matches!(err, CoreFsError::State(_)));
    }
}
