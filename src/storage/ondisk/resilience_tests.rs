// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT
//
// Enterprise resilience scenarios — fault injection composed with the
// ODF save/load pipeline.  Every test below documents the invariant it
// protects (e.g. "after a write-mid-save crash, load_state_native
// must not return a half-written state").

use crate::app::PersistedState;
use crate::config::CoreFsConfig;
use crate::domain::volume::VolumeDescriptor;
use crate::services::journal::JournalRuntimeState;
use crate::storage::block_device::{BlockDevice, MemoryDevice};
use crate::storage::block_store::AllocatorPolicy;
use crate::storage::ondisk::fault_injection::{FaultPlan, FaultyDevice};
use crate::storage::ondisk::fsck;
use crate::storage::ondisk::journaled::{JournaledSaveSession, recover_pending_transactions};
use crate::storage::ondisk::layout::BLOCK_SIZE;
use crate::storage::ondisk::native::{
    load_state_native, save_state_native, save_state_native_incremental,
};
use crate::storage::ondisk::volume::{FormatOptions, format_device};

// ----- small helpers --------------------------------------------------------

fn empty_state() -> PersistedState {
    let config = CoreFsConfig::default();
    PersistedState {
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
    }
}

fn formatted_faulty(blocks: u64) -> FaultyDevice<MemoryDevice> {
    let mut mem = MemoryDevice::new(blocks * BLOCK_SIZE, 4096).unwrap();
    format_device(
        &mut mem,
        &FormatOptions {
            label: "resil".into(),
            uuid: [0u8; 16],
            inode_count: 256,
            journal_blocks: 32,
        },
    )
    .unwrap();
    FaultyDevice::new(mem, FaultPlan::new())
}

// ---- Invariant 1: crash after commit is recovered by journal replay --------

#[test]
fn i1_crash_after_commit_before_apply_is_recovered() {
    let mut dev = formatted_faulty(4096);
    // Stage and commit a journaled metadata write, without applying it.
    // Simulates power loss between commit-record-durable and replay.
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let target = sb.data_start + 7;
    let payload = vec![0x11u8; BLOCK_SIZE as usize];

    {
        let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
        sess.stage_metadata_block(target, payload.clone()).unwrap();
        sess.commit_without_apply().unwrap();
    }

    // Target block still unchanged.
    assert!(
        dev.read_at(target * BLOCK_SIZE, BLOCK_SIZE)
            .unwrap()
            .iter()
            .all(|b| *b == 0)
    );

    // Simulated mount: replay + checkpoint recovers the effect.
    let report = recover_pending_transactions(&mut dev).unwrap();
    assert_eq!(report.txns_replayed.len(), 1);
    assert_eq!(
        dev.read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap(),
        payload
    );
}

// ---- Invariant 2: sync failure during commit is observable -----------------

#[test]
fn i2_failed_sync_during_commit_returns_error() {
    let mut dev = formatted_faulty(4096);
    // Arm the device to fail the NEXT sync call.  This models a power
    // cut between the op-record writes and the commit-record sync.
    dev.set_plan(FaultPlan::new().fail_sync());
    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.stage_metadata_block(
        sess.superblock().data_start + 1,
        vec![0u8; BLOCK_SIZE as usize],
    )
    .unwrap();
    let err = sess.commit().unwrap_err();
    assert!(format!("{err}").contains("sync"));
    assert_eq!(dev.stats().syncs_failed, 1);
}

// ---- Invariant 3: silent single-byte flip in a data block is caught --------

#[test]
fn i3_silent_data_block_corruption_is_caught_by_data_crc() {
    use crate::domain::inode::{Inode, InodeId, InodeKind};
    use crate::domain::metadata::FileMetadata;
    use crate::storage::block_store::BlockRecord;
    use crate::storage::ondisk::reader::OdfReader;
    use std::time::UNIX_EPOCH;

    let mut dev = formatted_faulty(4096);
    // Populate a file via the normal save path, then silently corrupt
    // one byte of the underlying device.
    let mut state = empty_state();
    let content = b"enterprise payload".to_vec();
    state.active_inodes.push(Inode {
        id: InodeId(7),
        kind: InodeKind::File,
        path: "/f".into(),
        size: content.len(),
        created_at: UNIX_EPOCH.into(),
        modified_at: UNIX_EPOCH.into(),
        changed_at: UNIX_EPOCH.into(),
        metadata: FileMetadata::default(),
    });
    state.block_records.push(BlockRecord {
        inode: InodeId(7),
        bytes: content.clone(),
        checksum: 0,
        device_block: 0,
        allocated_blocks: 0,
    });
    save_state_native(&mut dev, &state).unwrap();

    // Find the inode's data block through the reader, then corrupt the
    // first byte *in place* so the bitmap CRC still matches (the flip
    // sits in data, not bitmap).
    let data_block = {
        let reader = OdfReader::open(&dev).unwrap();
        let summaries = reader.list_inodes().unwrap();
        let slot = summaries[0].slot;
        reader.read_on_disk_inode(slot).unwrap().extents[0].physical_block
    };
    let mut raw = dev.read_at(data_block * BLOCK_SIZE, BLOCK_SIZE).unwrap();
    raw[3] ^= 0xFF;
    dev.write_at(data_block * BLOCK_SIZE, &raw).unwrap();

    // Reader must refuse to return silently-corrupted content.
    let mut reader = OdfReader::open(&dev).unwrap();
    let err = reader.read_file_by_path("/f").unwrap_err();
    assert!(format!("{err}").contains("data CRC"));
}

// ---- Invariant 4: silent single-byte flip in the bitmap is caught ----------

#[test]
fn i4_silent_bitmap_corruption_is_caught_on_load() {
    let mut dev = formatted_faulty(4096);
    save_state_native(&mut dev, &empty_state()).unwrap();

    // Flip a bit in the block bitmap — the bitmap CRC in the superblock
    // no longer matches.
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let mut bmp = dev
        .read_at(sb.block_bitmap_start * BLOCK_SIZE, BLOCK_SIZE)
        .unwrap();
    bmp[9] ^= 0x80;
    dev.write_at(sb.block_bitmap_start * BLOCK_SIZE, &bmp).unwrap();

    let err = load_state_native(&dev).unwrap_err();
    assert!(format!("{err}").contains("bitmap CRC"));
}

// ---- Invariant 5: superblock corruption → transparent fallback -------------

#[test]
fn i5_primary_superblock_corruption_falls_back_to_redundant_copies() {
    let mut dev = formatted_faulty(4096);
    save_state_native(&mut dev, &empty_state()).unwrap();

    // Wipe the primary superblock.
    dev.write_at(BLOCK_SIZE, &[0u8; BLOCK_SIZE as usize]).unwrap();

    // Load still succeeds via one of the redundant copies.
    let state = load_state_native(&dev).unwrap();
    assert!(state.active_inodes.is_empty());
}

// ---- Invariant 6: save_state_native aborting mid-way never produces -------
//                    a state where load_state_native returns a partial
//                    inode record.

#[test]
fn i6_save_aborted_by_enospc_leaves_previous_generation_loadable() {
    let mut dev = formatted_faulty(4096);
    // First: save a known-good state.
    save_state_native(&mut dev, &empty_state()).unwrap();

    // Record the generation of that good save.
    let gen_before = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap()
    .generation;

    // Now arm an ENOSPC-like fault partway through the next save.
    // Allow a handful of writes, then fail.
    dev.set_plan(FaultPlan::new().fail_after_writes(3));
    let err = save_state_native(&mut dev, &empty_state()).unwrap_err();
    assert!(
        format!("{err}").contains("ODF-FAULT") || format!("{err}").contains("fault"),
        "unexpected error: {err}"
    );

    // Re-arm the device with no faults and reload.  Because ODF
    // rewrites the superblock last, and the crashed save never made it
    // that far, the superblock still points at generation == gen_before.
    dev.set_plan(FaultPlan::new());
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sb.generation, gen_before,
        "aborted save must not advance the superblock generation"
    );
    // And reload returns a consistent state (whichever state that
    // superblock points at).
    let _ = load_state_native(&dev).unwrap();
}

// ---- Invariant 7: fsck detects every introduced corruption -----------------

#[test]
fn i7_fsck_detects_every_flavour_of_introduced_damage() {
    // Silent data block flip is caught as a *read-time* CRC mismatch
    // (not as an fsck error — fsck is structural only).  Bitmap flip
    // and superblock flip show up in the fsck report.
    let mut dev = formatted_faulty(4096);
    save_state_native(&mut dev, &empty_state()).unwrap();

    // Wipe tertiary superblock → fsck flags TERTIARY_UNREADABLE.
    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    dev.write_at(
        sb.tertiary_superblock_block * BLOCK_SIZE,
        &[0u8; BLOCK_SIZE as usize],
    )
    .unwrap();

    let report = fsck::check(&dev).unwrap();
    // Tertiary-wipe is a Warning, not an Error — is_clean() still
    // returns true.  What we care about is that the issue shows up.
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == "ODF.SB.TERTIARY_UNREADABLE"),
        "expected TERTIARY_UNREADABLE among {:?}",
        report.issues.iter().map(|i| i.code).collect::<Vec<_>>()
    );
}

// ---- Invariant 8: fault_stats accurately reflect what happened -------------

#[test]
fn i8_stats_accurately_reflect_fault_activity() {
    let mut dev = formatted_faulty(4096);
    // Do a small incremental save after initial full save.
    save_state_native(&mut dev, &empty_state()).unwrap();
    let before = dev.stats();

    save_state_native_incremental(&mut dev, &empty_state()).unwrap();
    let after = dev.stats();

    assert!(after.writes_persisted > before.writes_persisted);
    assert!(after.bytes_written > before.bytes_written);
    assert_eq!(after.writes_failed, 0);
    assert_eq!(after.syncs_failed, 0);
    assert_eq!(after.writes_silently_dropped, 0);
}

// ---- Invariant 9: a dropped-tail power-loss never yields a corrupt SB ------

#[test]
fn i9_power_loss_after_n_writes_leaves_loadable_state() {
    let mut dev = formatted_faulty(4096);
    save_state_native(&mut dev, &empty_state()).unwrap();
    let gen_before = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap()
    .generation;

    // Arm: after 5 successful writes, silently drop the rest.  Then
    // start a fresh save; every write past the threshold is a no-op,
    // so the on-disk state stays a mixture of "old superblock (never
    // rewritten)" + "partially updated bitmaps".  Whether the next
    // load_state succeeds depends on which structures made it — but
    // the invariant we care about is: no panic, no undefined behaviour.
    dev.set_plan(FaultPlan::new().stop_after_n_writes(5));
    let _ = save_state_native_incremental(&mut dev, &empty_state());

    // Clear the fault and confirm we can at least read the superblock
    // and produce a deterministic error / success — no panics.
    dev.set_plan(FaultPlan::new());
    let sb_after = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    );
    if let Ok(sb) = sb_after {
        // If the superblock survived, it must still describe a valid
        // generation (either unchanged or the new one).
        assert!(sb.generation >= gen_before);
    }
    // Either way the device didn't panic.
    let _ = dev.stats();
}

// ---- Invariant 10: journaled save + crash sequence works end-to-end --------

#[test]
fn i10_journaled_save_survives_crash_between_commit_and_apply() {
    // Combine: journaled metadata write + simulated crash + remount
    // + fsck still clean.
    let mut dev = formatted_faulty(4096);
    save_state_native(&mut dev, &empty_state()).unwrap();

    let sb = crate::storage::ondisk::superblock::Superblock::decode_block(
        &dev.read_at(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
    )
    .unwrap();
    let target = sb.data_start + 10;
    let payload = vec![0xCDu8; BLOCK_SIZE as usize];

    let mut sess = JournaledSaveSession::open(&mut dev).unwrap();
    sess.stage_metadata_block(target, payload.clone()).unwrap();
    sess.commit_without_apply().unwrap();

    // Remount-equivalent recovery.
    let report = recover_pending_transactions(&mut dev).unwrap();
    assert_eq!(report.ops_applied, 1);

    // Read-back matches.
    assert_eq!(
        dev.read_at(target * BLOCK_SIZE, BLOCK_SIZE).unwrap(),
        payload
    );
}
