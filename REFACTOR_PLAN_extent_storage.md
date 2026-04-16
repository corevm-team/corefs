# CoreFS Extent-Based Refactor — Implementation Plan

## 1. Current-state map

### 1.1 Types in `corefs-core/src/storage/block_store.rs`

Full type inventory (file: `corefs-core/src/storage/block_store.rs`, 1020 lines):

| Type | Visibility | Role |
|---|---|---|
| `BlockRecord` | `pub` | **Wire struct** returned by `BlockStore::read` / persisted in `PersistedState::block_records`. Carries `inode`, **`bytes: Vec<u8>` (full file body)**, `checksum: u64`, `device_block: u64`, `allocated_blocks: u64`. This is *the* struct causing the heap blow-up. |
| `BlobRecord` | private | Internal dedup key holder — `bytes: Vec<u8>`, `checksum: u64`, `ref_count: usize`. Keyed by checksum in `BlockStore::blobs: BTreeMap<u64, BlobRecord>`. |
| `BlockEntry` | private | Per-inode mapping from `InodeId` → `(blob_checksum, size, device_block, allocated_blocks)`. Keyed in `BlockStore::blocks: BTreeMap<InodeId, BlockEntry>`. |
| `FreeExtentRecord` | `pub` | Serializable free-list entry `(device_block, allocated_blocks)`. |
| `AllocationStrategy` | `pub` | `BestFit | FirstFit`. |
| `AllocatorPolicy` | `pub` | Policy bundle. Persisted in `PersistedState::allocator_policy`. |
| `FreedExtent` | `pub` | TRIM candidate accumulated in `BlockStore::pending_trims`. |
| `DedupeStats` / `DedupePassReport` / `CowStats` / `DefragmentationReport` / `FragmentationReport` / `OptimizationReport` / `HeatReallocationReport` | `pub` | Reports. |
| `BlockStore` | `pub` | Master struct: `block_size`, `next_device_block`, `policy`, `free_extents`, `blocks`, `blobs`, `pending_trims`. Everything held in RAM. |

Key methods: `write(InodeId, Vec<u8>)`, `read(InodeId) -> Option<BlockRecord>`, `append_to_inode(InodeId, &[u8])`, `remove(InodeId)`, `verify(InodeId)`, `records()`, `from_records*`, `clone_for_inode`, `cow_stats`, `dedup_pass`, `defragment`, `optimize`, `reallocate_prioritized_extents`, `drain_freed_extents`, internal allocator (`allocate_extent`, `insert_free_extent`, `normalize_free_extents`, `trim_free_tail`, `rebuild_free_extents`, `release_inode`). Hash function is a weak FNV-like polynomial fold.

### 1.2 Callsites of `BlockRecord` / `BlockStore` / `BlockRecord::bytes`

**`BlockRecord` constructors / field-access:**
- `corefs-core/src/storage/block_store.rs` — `read`, `from_records*`, tests
- `corefs-core/src/storage/block_store_tests.rs` — heavy test usage
- `corefs-core/src/storage/ondisk/native.rs` — `load_state_native` reconstructs `BlockRecord` from on-disk extents (ll. 306-324); `save_state_native` dereferences `rec.bytes.as_slice()` at ll. 180-181, 437-438. Entire `save_state_native_incremental` path depends on `BlockRecord.bytes`.
- `corefs-core/src/storage/ondisk/native_tests.rs` — test factories
- `corefs-core/src/storage/ondisk/grouped.rs`, `grouped_tests.rs` — grouped variant, same shape
- `corefs-core/src/storage/persisted_state.rs` — `PersistedState::block_records: Vec<BlockRecord>`, `restore_snapshot_at` l. 253, tests produce `BlockRecord`s with `bytes.clone()`
- `corefs/src/storage/volume_image.rs` — legacy blob-layout encoder/decoder, `split_blocks`, `join_blocks`, `reconstruct_block_records_from_data` — serializes `BlockRecord.bytes` as a DATA segment
- `corefs/src/storage/volume_image_tests.rs` — heavy usage
- `corefs/src/storage/ondisk/fsck_tests.rs`, `resilience_tests.rs`, `benchmark.rs` — test scaffolding
- `corefs/src/app/types.rs` — re-exports `block_records: Vec<BlockRecord>` field (shadowed by `pub use corefs_core::...::PersistedState`)
- `corefs/src/services/journal_tests.rs` — fixtures
- `corefs/tests/fuse_handler_e2e.rs` — E2E against userspace daemon
- **AnyOS kernel: `anyos/kernel/src/fs/corefs/driver.rs`** — 10 direct references in `Filesystem::read/write`, `truncate_file`, `create_symlink`, `delete`, plus `block_records.push(BlockRecord {...})` constructor calls. **This driver is the kernel-side consumer that crashes on a 1 GiB write.**

**`BlockStore` direct callers:**
- `corefs-core/src/storage/persisted_state.rs` — `defragment_in_place`
- `corefs/src/app/mod.rs` — `CoreFsService` owns `blocks: BlockStore`; 20+ callsites for `create_file`, `write_file`, `extend_file`, `read_file`, `delete`, scrub, defrag, clone-tree, snapshot-restore
- `corefs/src/services/integrity.rs`, `integrity_tests.rs` — `scrub`, `deep_fsck`
- `corefs/src/app/stress_tests.rs`
- `doc/deduplication.md`, `doc/glossary.md`, `doc/architecture.md`, `doc/features.md`

**`PersistedState::block_records` field access:**
- `corefs/src/storage/volume_image.rs` ll. 156, 1393, 1401, 1995, 2448
- `corefs/src/app/mod.rs` l. 1388 (populate), l. 1424 (hydrate via `BlockStore::from_records_with_allocator`)
- `corefs/src/platform/linux_fuse.rs` ll. 156, 1116 — direct read of `block_records` for FUSE cache hydration

### 1.3 On-disk format today

Source of truth: `corefs-core/src/storage/ondisk/layout.rs` (ODF v1, 4 KiB `BLOCK_SIZE`). Two modes persisted today:

1. **Blob mode (legacy)** — `ondisk/volume.rs::save_state` bincode-serializes the *whole* `PersistedState` (including every `BlockRecord::bytes`) into the system payload inode. Breaks on large volumes.
2. **Native mode** (`ondisk/native.rs`, 977 lines) — per-inode slot layout:
   - slot 0: reserved (blob legacy)
   - slot 1: `ANCILLARY_INODE_SLOT` — bincode of `AncillaryState` (config, volume, free_extents, policy, journal, snapshots, …)
   - slot ≥ 10: one `OnDiskInode` per domain inode + sibling `AttrBlock` for metadata
   - **File content today**: `save_state_native` iterates `state.block_records`, takes `rec.bytes.as_slice()`, allocates one contiguous extent sized `ceil(bytes.len()/BLOCK_SIZE)` via `OndiskAllocator::allocate_contiguous`, writes the *entire file body* as one `device.write_at`. `load_state_native` reads back via `read_all_extent_bytes` which walks `OnDiskInode.extents` (or indirect chain if `FLAG_HAS_EXTENT_INDEX`) and concatenates into a `Vec<u8>` — that `Vec` is re-wrapped into `BlockRecord { bytes, ... }` in RAM. So even native-mode load keeps the full file in memory.
3. Grouped variant (`ondisk/grouped.rs`, 718 lines) — same bytes-in-RAM shape
4. Volume image (`corefs/src/storage/volume_image.rs`, 2569 lines, std-only) encodes `BlockRecord`s as (descriptor_table, byte_blob) segments SUPR/CNFG/VOLM/AINO/DINO/JOUR/VERS/SYNC/HOTP/SNAP/TXNJ/FREE/**BLKD**/**DATA** — same full-bytes-in-RAM shape for `.img` files.

### 1.4 Compression and encryption today

- **Compression**: `corefs-core/src/services/compression.rs` (LZ4-frame, `std`-gated). Operates on the *whole file buffer* at once. Min payload 64 bytes.
- **Encryption**: `corefs-core/src/services/encryption.rs` (ChaCha20-Poly1305, `crypto`-gated; AnyOS disables). Whole-file encrypt/decrypt, emits `nonce (12B) || ciphertext || tag (16B)`.
- **Wiring**: only in `corefs/src/app/mod.rs` (userspace). Pipeline `compress → encrypt → store` in `create_file` / `write_file` (ll. 234-246, 341-360); reverse in `read_file` (ll. 430-450). `inode.metadata.encrypted` / `inode.metadata.compressed` flags. AnyOS kernel does **not** touch these services.
- Per-inode flags on disk: `FLAG_ENCRYPTED = 1 << 1`, `FLAG_COMPRESSED = 1 << 2` in `ondisk/inode.rs:60-62`.

### 1.5 Test coverage today

`corefs-core/src/storage/`:
- `block_store_tests.rs` (548 lines) — 23 tests: write/read/remove, dedupe, existing-alloc reuse, free-list reuse, shrink-tail, allocator metadata roundtrip, defragment, fragmentation report, optimize, prioritized reallocation, CoW (`is_shared`, `clone_for_inode`, write-after-clone materializes, `append_after_clone`, `cow_stats`), TRIM tracking
- `allocator_tests.rs`, `catalog_tests.rs`
- `ondisk/native_tests.rs` (446 lines), `grouped_tests.rs` (291 lines)
- `ondisk/fsck_repair_tests.rs`, `fsck`, `journal`, `journaled`, `refcount`, `extent_tree`, `xattr`, `attr_block`, `bitmap`, `block_group`, `multi_group_allocator`, `checksum`, `dir_entry`, `layout`, `tiering`, `inode`, `superblock`, `volume`

Userspace (`corefs/`):
- `src/app/app_tests.rs` (1639 lines) — strongest behavioral oracle: create/read/write/extend/delete, versioning, snapshots, clone_tree, restore, journal, integrity, quotas, CoW
- `src/app/concurrency_tests.rs`, `stress_tests.rs`, `fault_injection_tests.rs`
- `src/storage/volume_image_tests.rs` (784 lines)
- `src/storage/device_volume_tests.rs`, `volume_session_tests.rs`, `volume_wal_tests.rs`
- `src/services/integrity_tests.rs`, `journal_tests.rs`
- `tests/fuse_handler_e2e.rs`

AnyOS:
- `anyos/kernel/src/fs/corefs/driver.rs` tests module (~500 lines embedded): mount_writable, create/read/write/delete, symlink, truncate, rename, set_mode/set_owner, read-only mount, write+flush+remount roundtrip
- `anyos/kernel/src/fs/corefs/integration_tests.rs` — skeleton, one host smoke test

---

## 2. Target architecture

### 2.1 New `BlockRecord` shape

`BlockRecord` becomes **metadata-only extent descriptor**. Never touches file bytes.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub inode: InodeId,
    /// Logical size in bytes (size userspace sees).
    pub logical_size: u64,
    /// Extents in logical order.
    pub extents: Vec<ExtentRef>,
    /// CRC32C over logical plaintext (before compress+encrypt).
    pub content_crc: u32,
    /// Bit flags: EXTENT_COMPRESSED, EXTENT_ENCRYPTED, HAS_INDIRECT_CHAIN.
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtentRef {
    pub logical_block: u32,
    pub logical_len: u32,
    pub physical_block: u64,
    pub length_blocks: u32,
    pub physical_len: u32,
    pub content_crc: u32,
    /// Per-extent flags: COMPRESSED (LZ4), ENCRYPTED (ChaCha20-Poly1305), HOLE.
    pub flags: u32,
}
```

**Extent size**: same as `BLOCK_SIZE` from `ondisk::layout` (4 KiB). One extent can span many blocks (`length_blocks`) but atomic I/O granularity is one block.

**Rationale**:
- 1:1 compatible with `OnDiskInode.extents` and existing `Extent` type (`ondisk/inode.rs:92-101`) — we add `logical_len`/`physical_len`/`content_crc`/`flags` for per-extent compression + encryption
- `content_crc: u32` replaces weak FNV polynomial with CRC32C (already in ODF via `ondisk::checksum::Crc32c`). Collision risk 2^-32 per pair, acceptable for dedup
- `HOLE` flag represents sparse files without allocating blocks

### 2.2 How `BlockStore` holds the device

**Decision: `BlockStore` holds no device handle. Every mutating method takes `&mut dyn BlockDevice`.**

Justification:
- `corefs-core` is `no_std + alloc` with `#![forbid(unsafe_code)]`. Can't use `std::sync::Mutex`. `spin::Mutex` would force new dep and pollute all call sites; `RefCell` isn't `Send`
- AnyOS driver already holds own `crate::sync::mutex::Mutex<Inner>`. Userspace `CoreFsService` is single-threaded (FUSE serializes). Second wrap redundant
- Explicit I/O boundary eases transactional reasoning and fault-injection tests
- `BlockDevice` trait is already `Send + fmt::Debug`; no further bounds

```rust
pub struct BlockStore {
    block_size: u64,
    policy: AllocatorPolicy,
    free_extents: Vec<FreeExtentRecord>,
    next_device_block: u64,
    /// Metadata only — no bytes.
    records: BTreeMap<InodeId, BlockRecord>,
    /// CRC32C(plaintext extent payload) → DedupEntry. Persisted via ondisk::dedup_table (new).
    dedup_table: HashMap<u32, DedupEntry>,
    pending_trims: Vec<FreedExtent>,
}

struct DedupEntry {
    physical_block: u64,
    length_blocks: u32,
    physical_len: u32,
    ref_count: u32,
}

impl BlockStore {
    pub fn write(&mut self, device: &mut dyn BlockDevice, inode: InodeId, offset: u64, data: &[u8], policies: &WritePolicies) -> CoreFsResult<()>;
    pub fn read(&self, device: &dyn BlockDevice, inode: InodeId, offset: u64, out: &mut [u8]) -> CoreFsResult<usize>;
    pub fn truncate(&mut self, device: &mut dyn BlockDevice, inode: InodeId, new_size: u64) -> CoreFsResult<()>;
    pub fn remove(&mut self, device: &mut dyn BlockDevice, inode: InodeId) -> CoreFsResult<()>;
    pub fn verify(&self, device: &dyn BlockDevice, inode: InodeId) -> CoreFsResult<bool>;
    pub fn clone_for_inode(&mut self, source: InodeId, target: InodeId) -> CoreFsResult<bool>;
    pub fn records(&self) -> Vec<BlockRecord>;
}

pub struct WritePolicies {
    pub compression: CompressionMode,  // Off | Lz4
    pub encryption: EncryptionMode,    // Off | ChaCha20Poly1305 { key: [u8;32] }
    pub dedupe_enabled: bool,
}
```

### 2.3 Read/write routing through the device

**Geometry**: `BLOCK_SIZE = 4096`. `BlockDevice::sector_size()` is 512 or 4096 — all offsets are `physical_block * BLOCK_SIZE`, all lengths `N * BLOCK_SIZE`, always sector-aligned.

**Write path** (`write(device, inode, offset, data, policies)`):
1. Compute affected logical block range: `[first=offset/BLOCK_SIZE, last=(offset+data.len()-1)/BLOCK_SIZE]`
2. **Read-modify-write at edges**: if write starts mid-block or ends mid-block, read existing extent(s), overlay `data`, mark full-block buffer
3. For each full block: reuse extent if inode owns it and blob is not shared; else allocate fresh via `allocate_extent`, TRIM displaced range
4. **Compression** (per-extent, userspace only): if `should_compress`, run `CompressionService::compress`. Set `EXTENT_COMPRESSED`; `physical_len = compressed.len()` padded to sector; `logical_len` stays original. Skip if <10% savings
5. **Encryption** (per-extent, userspace only): `EncryptionService::encrypt_with_rng` — fresh nonce per extent. Set `EXTENT_ENCRYPTED`; `physical_len` grows by `12 + 16`, padded to sector
6. **Dedup** (if `policies.dedupe_enabled`): hash **plaintext, pre-compress payload** with CRC32C. Lookup → hit: `ref_count++`, reuse `physical_block`, skip write. Miss: allocate, write, insert entry
7. `device.write_at(physical_block * BLOCK_SIZE, &sector_aligned_payload)`
8. Update `BlockRecord.extents`, `logical_size`, `content_crc` (CRC32C of whole-file plaintext, maintained by XOR-folding per-extent CRCs)

**Read path** (`read(device, inode, offset, out)`):
1. Find extents covering `[offset, offset+out.len())`
2. For each extent: `device.read_at(physical_block * BLOCK_SIZE, length_blocks * BLOCK_SIZE)`
3. Trim to `physical_len`, decrypt if `EXTENT_ENCRYPTED`, decompress if `EXTENT_COMPRESSED`
4. Copy requested slice into `out`
5. Holes: if `HOLE` flag, skip device read, zero-fill `out`

### 2.4 Dedup

- Key = CRC32C of plaintext extent payload (never compressed/encrypted bytes — supports future per-file keys)
- Table = `HashMap<u32, DedupEntry>` in `BlockStore`
- **Collision-safe**: on hit, re-read candidate extent, `memcmp` before sharing. Mismatch → store as fresh extent
- **Ref-count survival across mounts**: persist in **ancillary inode (slot 1)** alongside `allocator_policy` / `free_extents`. New field `dedup_entries: Vec<DedupEntryRecord>` in `AncillaryState`. Fallback: on corrupt ancillary, rebuild by scanning
- **Write-breaks-sharing (CoW)**: if `ref_count > 1`, allocate fresh, decrement shared entry. Existing `clone_for_inode` semantics preserved
- **Remove/truncate**: `dedup_release(crc, physical_block)` decrements; returns physical range to free list only when `ref_count == 0`

### 2.5 Compression

- **Per-extent** LZ4 frame, userspace only (`#[cfg(feature = "compression")]`). AnyOS kernel never sees `CompressionService`. `EXTENT_COMPRESSED` flag still defined in `corefs-core`, but kernel read encountering it returns `CoreFsError::PolicyViolation("volume uses compression, not supported in kernel mode")`. Write path in kernel never sets the flag.
- Per-extent gives locality: reading bytes 0..4096 of a GB-compressed file needs one extent decompressed, not the whole file
- Signal: `ExtentRef.flags & EXTENT_COMPRESSED`. `logical_len` = plaintext, `physical_len` = compressed frame, `length_blocks` = `ceil(physical_len / BLOCK_SIZE)`
- Threshold: skip when `compressed.len() >= plaintext.len() * 0.9` (avoid re-compression on every read)

### 2.6 Encryption

- Per-extent ChaCha20-Poly1305, userspace only (`#[cfg(feature = "crypto")]`)
- Composition: **`compress → encrypt`** (encrypted output is incompressible). Read: `decrypt → decompress`
- Nonce: 12 bytes from `corefs-core::platform::Rng`, stored as first 12 bytes of ciphertext. Tag at end (16 bytes). Both in `physical_len`
- Key management unchanged — `EncryptionService::set_key` / `derive_key_from` at mount time, held in `CoreFsService` / `OdfDeviceSession`. Kernel never has a key

### 2.7 `save_state_native` / `load_state_native` changes

`save_state_native` shrinks dramatically:
```rust
pub fn save_state_native(device: &mut dyn BlockDevice, state: &PersistedState) -> CoreFsResult<NativeSaveReport> {
    // 1. Write ancillary blob (slot 1) — now includes dedup_entries
    // 2. For each active/deleted domain inode:
    //    - Look up BlockRecord (metadata only). DO NOT read/write file bytes here
    //    - Encode OnDiskInode.extents from BlockRecord.extents
    //    - Set flags (FLAG_COMPRESSED, FLAG_ENCRYPTED, FLAG_HAS_EXTENT_INDEX, FLAG_HAS_XATTRS, FLAG_DELETED)
    //    - Write OnDiskInode slot + AttrBlock
    //    - Extent payloads already written by BlockStore::write at data-mutation time
    // 3. Flush bitmaps + superblock
}
```

Critical change: save body never touches file bytes. `save_state_native_incremental` simplifies similarly — "updated" means "extent list changed", already persisted by `BlockStore::write`. No more re-writing file bodies on every `flush`.

`load_state_native`:
- Walk inode slots
- For each `OnDiskInode`: build `BlockRecord { inode, logical_size: on_disk.size_bytes, extents: on_disk.extents.map(into), content_crc: on_disk.data_crc, flags }`
- **Never call `read_all_extent_bytes`** (deleted)
- Reconstruct `dedup_table` from ancillary or rebuild by scan

### 2.8 On-disk format compatibility

**Decision: bump ODF to v2. ODF v1 volumes fail-closed — migration explicit via `corefs-tools::migrate` subcommand.**

- `ondisk/layout.rs::ODF_VERSION_MAJOR`: 1 → 2
- Add `FEATURE_INCOMPAT_EXTENT_ADDRESSED` (bit 3), `FEATURE_INCOMPAT_DEDUP_TABLE` (bit 4)
- Mounting v1 under v2: `CoreFsError::State("volume format v1 requires migration, run corefs-migrate")`
- `corefs-linux.img` at repo root re-formatted as v2; old file renamed `corefs-linux-v1.img` as migration input

---

## 3. Characterization test suite (write BEFORE the refactor)

All tests exercise **current public API**, must pass against current in-RAM implementation, and must still pass unchanged after refactor. New files beside existing test modules — don't edit existing tests.

### 3.1 `corefs-core/src/storage/block_store_characterization_tests.rs`

Build `BlockStore`, exercise `write` / `read` / `append_to_inode` / `remove` / `verify` / `clone_for_inode`.

| Test | Assertion |
|---|---|
| `char_write_empty_reads_back_empty` | `write(inode, vec![])`, read empty |
| `char_write_one_byte_reads_back_one_byte` | Single byte roundtrip, `verify` true |
| `char_write_exactly_one_block_reads_back_identical` | 4096 bytes roundtrip |
| `char_write_one_block_plus_one_byte_allocates_two_blocks` | `allocated_blocks == 2` |
| `char_write_16kib_multi_block_roundtrip` | 16 KiB byte-equal |
| `char_overwrite_in_place_when_size_fits_preserves_device_block` | Same/smaller second write keeps `device_block` |
| `char_overwrite_larger_moves_allocation` | Larger release + fresh alloc |
| `char_shrinking_write_releases_tail_as_free_extent_and_trim` | `drain_freed_extents` yields tail |
| `char_append_to_exclusive_blob_extends_in_place` | `append_to_inode` on `ref_count=1` |
| `char_append_to_shared_blob_materializes_cow` | `clone_for_inode` then `append_to_inode` — clone untouched |
| `char_dedup_identical_bytes_share_storage` | `unique_blobs == 1`, `deduplicated_blocks == 1` |
| `char_dedup_modifying_one_does_not_affect_other` | Overwrite inode 1; inode 2 unchanged |
| `char_clone_then_write_source_preserves_target` | (existing test moved with `char_` prefix) |
| `char_remove_returns_record_and_frees_extent` | (existing) |
| `char_from_records_roundtrips_with_free_extents` | Roundtrip via `from_records` |
| `char_random_payload_verifies_true` | Seeded PRNG fill, verify + roundtrip |

### 3.2 `corefs-core/src/storage/ondisk/native_characterization_tests.rs`

| Test | Assertion |
|---|---|
| `char_save_load_empty_state_roundtrips` | Empty state identical |
| `char_save_load_one_file_preserves_bytes` | `b"hello world"` roundtrip |
| `char_save_load_large_file_preserves_bytes` | 2 MiB seeded PRNG byte-equal |
| `char_save_load_sparse_file_preserves_zero_fill` | Truncate 8 KiB, read `[0; 8192]` |
| `char_save_load_preserves_allocator_policy_and_free_extents` | Policy + free list preserved |
| `char_save_load_preserves_inode_metadata` | mode, uid, gid, tags, timestamps |
| `char_incremental_save_unchanged_inode_is_unchanged` | `unchanged >= 1` after no-op flush |
| `char_save_load_with_deleted_inodes_preserves_them` | `deleted_inodes` preserved |
| `char_corrupted_inode_crc_returns_state_error` | Flip byte → load returns `CoreFsError::State` |
| `char_corrupted_data_block_detected_by_verify` | Flip byte → `verify(inode)` false |

### 3.3 `corefs-core/src/services/pipeline_characterization_tests.rs`

Gated on `#[cfg(feature = "std")]`.

| Test | Assertion |
|---|---|
| `char_compress_roundtrip_repeated_content` | `b"abcd".repeat(10_000)` compresses; decompress equal |
| `char_compress_random_content_still_decompresses` | 64 KiB PRNG — may or may not shrink; decompress byte-equal |
| `char_encrypt_roundtrip_with_seeded_rng` | Encrypt/decrypt equal; different nonce each call |
| `char_encrypted_ciphertext_changes_with_plaintext` | Bit-flip → ciphertext differs |
| `char_decrypt_with_wrong_key_fails` | Error on wrong key |

### 3.4 `corefs/src/app/content_roundtrip_characterization_tests.rs`

Full pipeline via `CoreFsService` — strongest oracle.

| Test | Assertion |
|---|---|
| `char_service_create_read_empty` | Empty file roundtrip |
| `char_service_create_read_1_byte` | 1 byte |
| `char_service_create_read_1_block` | 4096 bytes |
| `char_service_create_read_1_block_plus_one` | 4097 bytes |
| `char_service_create_read_4MB_random_seeded` | 4 MiB PRNG byte-equal |
| `char_service_partial_overwrite_preserves_surrounding_bytes` | 16 KiB, overwrite `[5000..5100]`, surrounding intact |
| `char_service_extend_file_appends_bytes` | Extend then read = original ++ extra |
| `char_service_truncate_shrinks_size_and_bytes` | 12 → 4 bytes |
| `char_service_truncate_extends_with_zero_fill` | Truncate past EOF, zero-padded read |
| `char_service_truncate_to_zero_empties_file` | Empty read |
| `char_service_dedup_two_identical_files_share_storage` | `cow_stats.shared_blobs == 1` |
| `char_service_modify_one_of_shared_breaks_sharing` | `shared_blobs == 0`; other file unchanged |
| `char_service_compress_roundtrip_compressible_payload` | `metadata.compressed == true`; read original |
| `char_service_compress_roundtrip_incompressible_payload` | Random 64 KiB; read returns original |
| `char_service_encrypt_roundtrip` | `encryption_at_rest = true`, read returns original |
| `char_service_compress_then_encrypt_pipeline` | Both enabled, read = plaintext |
| `char_service_save_then_load_image_preserves_all_files` | 3 files, flush, reopen, byte-equal |
| `char_service_save_then_load_device_preserves_all_files` | Via `DeviceVolumeSession` + `MemoryDevice` |
| `char_service_integrity_scrub_passes_on_clean_image` | `invalid_blocks == 0` |
| `char_service_integrity_scrub_detects_corrupted_byte` | Flip byte in persisted extent, scrub sees it |
| `char_service_sparse_truncate_then_read_returns_zeros` | Sparse read = zeros |

### 3.5 `anyos/kernel/src/fs/corefs/integration_characterization_tests.rs`

| Test | Assertion |
|---|---|
| `char_kernel_driver_write_1MiB_file_roundtrips_through_flush_and_remount` | 1 MiB PRNG through flush + remount |
| `char_kernel_driver_128MiB_file_does_not_panic_on_heap` | 128 MiB — `#[ignore]` pre-refactor: "characterizes failure mode; passes after refactor" |
| `char_kernel_driver_two_identical_files_deduplicate` | Same 64 KiB to `/a` + `/b`, blocks ≤ 1× payload + overhead post-refactor. Pre-refactor: `#[ignore]` |
| `char_kernel_driver_truncate_extends_with_zero_fill_and_remounts` | Truncate past EOF + flush + reload |
| `char_kernel_driver_partial_write_preserves_surrounding` | 10 KiB, overwrite `[3000..3100]`, remount, surrounding bytes intact |

Canary: a pre-refactor green that flips red post-refactor = observable-behavior break.

---

## 4. Migration order (big-bang)

Single branch, single commit series, final state compiles + passes all tests.

### Phase A — foundations

1. **`corefs-core/src/storage/block_store.rs`** — rewrite from scratch. New types. `BlockStore` takes `&mut dyn BlockDevice` at every I/O method. Keep report types (shapes unchanged). Port existing allocator. Delete `BlobRecord` + `BlockEntry` (absorbed). FNV `checksum()` → `Crc32c::hash` (already in `ondisk/checksum.rs`)
2. **`corefs-core/src/storage/persisted_state.rs`** — `PersistedState::block_records: Vec<BlockRecord>` keeps field name, new shape. `restore_snapshot_at` takes `&mut dyn BlockDevice` + `BlockStore`. Propagate to `CoreFsService::restore_snapshot`
3. **`corefs-core/src/storage/ondisk/native.rs`** — rewrite save/load per §2.7. Delete `read_all_extent_bytes_public` + internal. Add `record_from_ondisk_inode`. Bump ODF to v2 in `layout.rs`
4. **`corefs-core/src/storage/ondisk/grouped.rs`** — mirror native
5. **`corefs-core/src/storage/ondisk/layout.rs`** — major version bump + feature flags

### Phase B — services

6. **`corefs-core/src/services/compression.rs`** — `compress_extent` / `decompress_extent` per-extent primitives
7. **`corefs-core/src/services/encryption.rs`** — `encrypt_extent_with_rng` / `decrypt_extent` per-extent

### Phase C — userspace

8. **`corefs/src/app/mod.rs`** — biggest surgery. `CoreFsService` gains `device: Box<dyn BlockDevice>`. Touched methods: `create_file*`, `write_file`, `extend_file`, `read_file`, `delete_path`, `truncate`, `create_symlink`, `clone_tree`, `restore_snapshot`, `persisted_state()`, `from_persisted_state(state)`, `auto_optimize_storage`. `auto_optimize_storage::defragment` now moves blocks on device
9. **`corefs/src/app/types.rs`** — remove duplicate `PersistedState` struct (shadowed). Keep only `FsStats`, `AdminReport`, `DirectoryEntry`, `MetadataView`
10. **`corefs/src/services/integrity.rs`** — `scrub` / `deep_fsck` take `&dyn BlockDevice` + `&BlockStore`. `verify(inode)` becomes `verify(&device, inode)`
11. **`corefs/src/storage/volume_image.rs`** — **delete outright**. Native ODF + `DeviceVolumeSession` covers it. `FileImageDevice` already in `corefs/src/storage/block_device.rs`. Removes 2569 + 784 lines
12. **`corefs/src/storage/volume_session.rs`** — `flush` routes through native ODF via `FileImageDevice`
13. **`corefs/src/storage/device_volume.rs`** — canonical file-backed session
14. **`corefs/src/platform/linux_fuse.rs`** — two `block_records` accesses at ll. 156, 1116 replaced with `service.read_file(path)`

### Phase D — AnyOS kernel driver

15. **`anyos/kernel/src/fs/corefs/driver.rs`** — rewrite `Filesystem::read/write`, `truncate_file`, `create_symlink`, `delete` to route through `BlockStore` with held `BlockDeviceAdapter`. `Inner` gains `blocks: BlockStore`. Each mutating call: `inner.blocks.write(&mut inner.device, id, offset, buf, &WritePolicies::plain_no_dedup())?`. Kernel passes `compression: Off, encryption: Off, dedupe_enabled: false`. **Driver goes from 1300 → ~600 lines**
16. **`anyos/kernel/src/fs/corefs/integration_tests.rs`** — extend per §3.5

### Phase E — tools & CLI

17. `corefs-tools/src/mkfs.rs` — format writes v2 superblock
18. `corefs-tools/src/defrag.rs` — real on-device moves
19. `corefs-tools/src/{fsck,scrub,dump,repair,snapshot}.rs` — adjust for `BlockRecord` field rename
20. **New: `corefs-tools/src/migrate.rs`** — v1 → v2 via old blob-layout decoder (kept behind `legacy-v1` feature), streams through `BlockStore::write`. `tests/migrate_v1_to_v2.rs` with precomputed v1 fixture
21. `corefs-cli/src/lib.rs` — `migrate` subcommand
22. `corefs-fuse-adapter/src/lib.rs` — wire `WritePolicies` through `FuseHandler`

### Phase F — test order

```
cargo test -p corefs-core --no-default-features --features alloc   # kernel-style
cargo test -p corefs-core                                          # std
cargo test -p corefs-core --features compression,crypto            # full
cargo test -p corefs                                                # userspace
cargo test -p corefs-tools
cargo test -p corefs-cli
cargo test -p corefs-fuse-proto -p corefs-fuse-adapter
cargo test --workspace
cd /daten1/development/brian/anyos && cargo test -p anyos_kernel  # host-cfg(test)
```

---

## 5. Risk register

### R1 — Alignment at FUSE edge (read-modify-write)
FUSE: arbitrary `(offset, len)`. `BlockDevice::write_at`: sector-aligned. A 3-byte write at offset 100 = read containing 4 KiB block → patch → write back.

RMW lives in `BlockStore::write`. Unit test: `char_service_partial_overwrite_preserves_surrounding_bytes`. Compression + encryption extend hazard: RMW into encrypted extent = decrypt-merge-reencrypt with **fresh nonce** (nonce reuse catastrophic for ChaCha20-Poly1305).

**Mitigation**: dedicated RMW helper `rmw_block(device, extent, byte_offset_in_block, data)`. Property test: 1000 random `(offset, len, content)` against 64 KiB file vs. in-RAM `Vec<u8>` reference.

### R2 — AnyOS kernel `BlockDevice` semantics
`BlockDeviceAdapter` delegates to synchronous `drivers::storage::{read,write}_sectors_on_disk` — blocking. Multi-MB write = dozens of synchronous sector calls.

**Mitigation**: batch writes inside `BlockStore::write` — coalesce physically-adjacent extents into one `device.write_at`. `supports_trim() == false` for now (freed extents leak old content until overwritten — fine). Follow-up: `blockcache` read-through (non-blocking).

### R3 — Dedup refcount corruption on crash
Kernel crashes between sharing update and ancillary persistence → on-disk refcount under-counts → remove frees still-referenced block.

**Mitigation**: existing `volume_wal::VolumeWal` — add `WalOperation::DedupRefCountDelta { crc: u32, delta: i32 }`. Ancillary rewritten atomically after all apply. `PersistedState::replay_pending_wal` needs new arm. Fallback: unclean mount + no usable WAL → full rebuild via scan. Guarded by characterization test + "unclean-mount recovery" variant.

### R4 — ODF version bump breaks existing fixtures
`corefs-linux.img`, `corefs/tests/*`, `corefs/src/storage/volume_image_tests.rs`, `corefs-tools/tests/` all v1.

**Mitigation**: rename to `*.v1.img`. Add `migrate_v1_fixture_to_v2_preserves_content` per fixture. Delete fixtures only used by deleted `volume_image.rs`.

### R5 — `no_std` lock constraint in `BlockStore`
Addressed by §2.2 design. Invariant (single-threaded access per mount) must be documented and enforced.

**Mitigation**: rustdoc: "Not thread-safe. Callers serialize externally." Userspace: `&mut self` via borrow checker. AnyOS: `crate::sync::mutex::Mutex<Inner>` at runtime.

### R6 — Per-extent encryption RMW and nonce hygiene
Nonce reuse = XOR keystream reuse = wire-level vulnerability.

**Mitigation**: every RMW of encrypted extent generates fresh nonce from `Rng::fill_bytes(12)`. Nonce stored at start of extent physical bytes. Property test: 10000 random RMWs, verify no two extents share a nonce.

### R7 — Incremental save "unchanged" classification
Currently keyed by `data_crc`. Post-refactor: extent list mutations without CRC change (defrag that moves blocks but preserves content) must still persist.

**Mitigation**: classification key = `(data_crc, extent_list_hash)` where `extent_list_hash = CRC32C(serialize(extents))`. Defrag bumps even on same content.

### R8 — `MemoryDevice` vs. `MemSectorIo` alignment laxity
`MemoryDevice` enforces alignment. `MemSectorIo` in `anyos/kernel/src/fs/corefs/block_device.rs` is more permissive. Kernel-test-passing bug might surface on real hardware.

**Mitigation**: add `StrictSectorIo` wrapper in kernel test code asserting alignment before delegating. Run all characterization tests through it.

---

## 6. Open questions

1. **Extent stream cipher**: ChaCha20-Poly1305 per-extent for GB files — acceptable perf, or layered scheme (key per extent, AES-NI)? Current plan: per-extent.
2. **Dedup scope**: same file across volumes? Current: per-volume only (dedup_table in ancillary slot 1).
3. **Hash for dedup**: CRC32C + memcmp-on-hit chosen. Prefer BLAKE3 (128-bit truncated)? `blake3` is `no_std`-friendly but adds a dep. Policy choice.
4. **v1 → v2 migration**: keep v1 reader behind `legacy-v1` feature in `corefs-tools` (current plan), or hard break since no production data?
5. **`BlockStore::defragment`**: real on-device moves (plan) or logical-only with background scrubber doing real moves?
6. **FUSE `Cargo.toml`**: `crypto` feature default-on in `corefs-core` — keep compile-time-optional encryption or always compile in (per-volume policy at runtime)?
