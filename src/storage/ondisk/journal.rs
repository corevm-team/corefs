// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! On-disk write-ahead log (WAL) for ODF v1.
//!
//! The volume reserves a contiguous block range (`journal_blocks` in the
//! superblock geometry) as the journal region.  The first block holds a
//! [`JournalHeader`] describing the log state; all following blocks form
//! a byte-addressable append-only record area:
//!
//! ```text
//! ┌─ block 0 ──────────────┐ ┌─ blocks 1..N-1 ─────────────────────────────┐
//! │ JournalHeader           │ │ Record 1 | Record 2 | ... | Record k        │
//! └────────────────────────┘ └─────────────────────────────────────────────┘
//! ```
//!
//! ## Record format
//!
//! Every record starts with a 40-byte header followed by a variable-length
//! payload and a trailing 4-byte CRC32C of the entire record:
//!
//! | field           | size | meaning                                       |
//! |-----------------|------|-----------------------------------------------|
//! | `magic`         | 4    | `0xCAFE_50ED`                                 |
//! | `kind`          | 2    | record kind (see [`RecordKind`])              |
//! | `flags`         | 2    | reserved, currently zero                      |
//! | `txn_id`        | 8    | transaction this record belongs to            |
//! | `seq`           | 8    | monotonic sequence number                     |
//! | `payload_len`   | 4    | bytes in the payload that follows             |
//! | `header_crc`    | 4    | CRC32C over the preceding 32 header bytes     |
//! | `reserved`      | 8    | future use, must be zero                      |
//! | `payload`       | var  | opaque bytes — see [`Op`] for op records      |
//! | `record_crc`    | 4    | CRC32C over header + payload (stored at end)  |
//!
//! ## Transactional semantics
//!
//! 1. [`Journal::begin`] returns a [`TxnBuilder`].
//! 2. The caller accumulates [`Op`] values describing intended writes.
//! 3. [`TxnBuilder::commit`] writes all op records *then* a commit record
//!    to disk, with `sync` calls enforcing ordering.  After the commit
//!    record has landed the transaction is durable.
//! 4. [`Journal::replay`] scans from `head` to `tail` on mount, collects
//!    every complete committed transaction and invokes each op's `apply`.
//!    Partial transactions without a commit record are dropped.
//! 5. [`Journal::checkpoint`] resets `head`/`tail` to zero after the
//!    caller has confirmed that the replayed effects have reached the
//!    main filesystem areas.

use super::checksum::Crc32c;
use super::layout::BLOCK_SIZE;
use super::superblock::Superblock;
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::block_device::BlockDevice;

/// Journal magic — `b"COREJRNL"` interpreted as little-endian u64.
pub const JOURNAL_MAGIC: u64 = u64::from_le_bytes(*b"COREJRNL");
/// Record magic — fixed for easy scanning / resync.
pub const RECORD_MAGIC: u32 = 0xCAFE_50ED;
const RECORD_HEADER_BYTES: usize = 40;
const RECORD_TRAILER_BYTES: usize = 4;
const HEADER_STRUCT_BYTES: usize = 128;
const HEADER_CHECKSUM_OFFSET: usize = HEADER_STRUCT_BYTES - 4;
/// Journal states.
pub const JSTATE_CLEAN: u32 = 0;
pub const JSTATE_DIRTY: u32 = 1;

/// Kind of a journal record.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Op = 1,
    Commit = 2,
    Abort = 3,
}

impl RecordKind {
    fn from_u16(v: u16) -> CoreFsResult<Self> {
        match v {
            1 => Ok(Self::Op),
            2 => Ok(Self::Commit),
            3 => Ok(Self::Abort),
            x => Err(CoreFsError::State(format!("unknown journal record kind {x}"))),
        }
    }
}

/// On-disk journal header (first block of the journal region).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalHeader {
    pub magic: u64,
    pub version: u16,
    pub flags: u16,
    pub head_offset: u64,
    pub tail_offset: u64,
    pub next_seq: u64,
    pub next_txn_id: u64,
    pub state: u32,
    pub record_area_bytes: u64,
}

impl JournalHeader {
    pub fn fresh(record_area_bytes: u64) -> Self {
        Self {
            magic: JOURNAL_MAGIC,
            version: 1,
            flags: 0,
            head_offset: 0,
            tail_offset: 0,
            next_seq: 0,
            next_txn_id: 1,
            state: JSTATE_CLEAN,
            record_area_bytes,
        }
    }

    fn encode_block(&self) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE as usize];
        let mut p = 0usize;
        fn put(buf: &mut [u8], p: &mut usize, bytes: &[u8]) {
            buf[*p..*p + bytes.len()].copy_from_slice(bytes);
            *p += bytes.len();
        }
        put(&mut block, &mut p, &self.magic.to_le_bytes());
        put(&mut block, &mut p, &self.version.to_le_bytes());
        put(&mut block, &mut p, &self.flags.to_le_bytes());
        put(&mut block, &mut p, &self.head_offset.to_le_bytes());
        put(&mut block, &mut p, &self.tail_offset.to_le_bytes());
        put(&mut block, &mut p, &self.next_seq.to_le_bytes());
        put(&mut block, &mut p, &self.next_txn_id.to_le_bytes());
        put(&mut block, &mut p, &self.state.to_le_bytes());
        put(&mut block, &mut p, &self.record_area_bytes.to_le_bytes());
        // Checksum at HEADER_CHECKSUM_OFFSET over the full block with
        // the checksum slot zeroed.
        let csum = Crc32c::hash(&block);
        block[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4]
            .copy_from_slice(&csum.to_le_bytes());
        block
    }

    fn decode_block(block: &[u8]) -> CoreFsResult<Self> {
        if block.len() != BLOCK_SIZE as usize {
            return Err(CoreFsError::InvalidInput("journal header: wrong length".into()));
        }
        let stored = u32::from_le_bytes(
            block[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        let mut zeroed = block.to_vec();
        zeroed[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4].fill(0);
        let expected = Crc32c::hash(&zeroed);
        if stored != expected {
            return Err(CoreFsError::State(
                "journal header CRC mismatch".into(),
            ));
        }
        let mut p = 0usize;
        fn take<const N: usize>(buf: &[u8], p: &mut usize) -> [u8; N] {
            let mut out = [0u8; N];
            out.copy_from_slice(&buf[*p..*p + N]);
            *p += N;
            out
        }
        let magic = u64::from_le_bytes(take::<8>(block, &mut p));
        if magic != JOURNAL_MAGIC {
            return Err(CoreFsError::State("journal header: bad magic".into()));
        }
        let version = u16::from_le_bytes(take::<2>(block, &mut p));
        let flags = u16::from_le_bytes(take::<2>(block, &mut p));
        let head_offset = u64::from_le_bytes(take::<8>(block, &mut p));
        let tail_offset = u64::from_le_bytes(take::<8>(block, &mut p));
        let next_seq = u64::from_le_bytes(take::<8>(block, &mut p));
        let next_txn_id = u64::from_le_bytes(take::<8>(block, &mut p));
        let state = u32::from_le_bytes(take::<4>(block, &mut p));
        let record_area_bytes = u64::from_le_bytes(take::<8>(block, &mut p));
        Ok(Self {
            magic,
            version,
            flags,
            head_offset,
            tail_offset,
            next_seq,
            next_txn_id,
            state,
            record_area_bytes,
        })
    }
}

/// Abstract filesystem operation carried by a journal record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Overwrite the contents of a single 4-KiB block with `data`.
    BlockWrite { block: u64, data: Vec<u8> },
    /// Install a new version of the on-disk inode record for `index`.
    InodeUpdate { index: u64, record: Vec<u8> },
}

impl Op {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Op::BlockWrite { block, data } => {
                buf.push(1u8);
                buf.extend_from_slice(&block.to_le_bytes());
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(data);
            }
            Op::InodeUpdate { index, record } => {
                buf.push(2u8);
                buf.extend_from_slice(&index.to_le_bytes());
                buf.extend_from_slice(&(record.len() as u32).to_le_bytes());
                buf.extend_from_slice(record);
            }
        }
        buf
    }

    fn decode(payload: &[u8]) -> CoreFsResult<Self> {
        if payload.is_empty() {
            return Err(CoreFsError::State("journal op: empty payload".into()));
        }
        let tag = payload[0];
        match tag {
            1 => {
                if payload.len() < 1 + 8 + 4 {
                    return Err(CoreFsError::State("journal op: truncated BlockWrite".into()));
                }
                let block = u64::from_le_bytes(payload[1..9].try_into().unwrap());
                let len = u32::from_le_bytes(payload[9..13].try_into().unwrap()) as usize;
                if payload.len() != 13 + len {
                    return Err(CoreFsError::State("journal op: bad BlockWrite length".into()));
                }
                Ok(Op::BlockWrite {
                    block,
                    data: payload[13..].to_vec(),
                })
            }
            2 => {
                if payload.len() < 1 + 8 + 4 {
                    return Err(CoreFsError::State("journal op: truncated InodeUpdate".into()));
                }
                let index = u64::from_le_bytes(payload[1..9].try_into().unwrap());
                let len = u32::from_le_bytes(payload[9..13].try_into().unwrap()) as usize;
                if payload.len() != 13 + len {
                    return Err(CoreFsError::State("journal op: bad InodeUpdate length".into()));
                }
                Ok(Op::InodeUpdate {
                    index,
                    record: payload[13..].to_vec(),
                })
            }
            x => Err(CoreFsError::State(format!("journal op: unknown tag {x}"))),
        }
    }
}

/// Handle onto an ODF journal region.  Holds a copy of the header and
/// lets callers append transactions.
pub struct Journal<'d> {
    device: &'d mut dyn BlockDevice,
    header_block: u64,
    record_start_block: u64,
    header: JournalHeader,
}

impl<'d> Journal<'d> {
    /// Initialise a fresh journal region.  Overwrites the header block and
    /// zero-fills the first sector of the record area (to invalidate any
    /// stale records).
    pub fn format(device: &'d mut dyn BlockDevice, sb: &Superblock) -> CoreFsResult<Self> {
        if sb.journal_blocks < 2 {
            return Err(CoreFsError::InvalidInput(
                "journal region must be at least 2 blocks".into(),
            ));
        }
        let record_area_bytes = (sb.journal_blocks - 1) * BLOCK_SIZE;
        let header = JournalHeader::fresh(record_area_bytes);
        let header_block = sb.journal_start;
        let record_start_block = header_block + 1;
        device.write_at(header_block * BLOCK_SIZE, &header.encode_block())?;
        // Zero the first record block to prevent stale magic.
        device.write_at(
            record_start_block * BLOCK_SIZE,
            &vec![0u8; BLOCK_SIZE as usize],
        )?;
        device.sync()?;
        Ok(Self {
            device,
            header_block,
            record_start_block,
            header,
        })
    }

    /// Open an existing journal.  Loads the header block and validates it.
    pub fn open(device: &'d mut dyn BlockDevice, sb: &Superblock) -> CoreFsResult<Self> {
        let header_block = sb.journal_start;
        let block = device.read_at(header_block * BLOCK_SIZE, BLOCK_SIZE)?;
        let header = JournalHeader::decode_block(&block)?;
        Ok(Self {
            device,
            header_block,
            record_start_block: header_block + 1,
            header,
        })
    }

    /// Open for read-only inspection (does not allow appends).
    pub fn inspect(device: &dyn BlockDevice, sb: &Superblock) -> CoreFsResult<JournalHeader> {
        let block = device.read_at(sb.journal_start * BLOCK_SIZE, BLOCK_SIZE)?;
        JournalHeader::decode_block(&block)
    }

    /// Start a new transaction.
    pub fn begin(&mut self) -> TxnBuilder {
        let txn_id = self.header.next_txn_id;
        self.header.next_txn_id = self.header.next_txn_id.saturating_add(1);
        TxnBuilder {
            txn_id,
            ops: Vec::new(),
        }
    }

    /// Replay all committed transactions between head and tail and apply
    /// each op's effect directly to the device.  Partial (uncommitted)
    /// transactions at the tail are discarded.  Returns a list of the
    /// transactions that were applied.
    pub fn replay(&mut self) -> CoreFsResult<Vec<ReplayedTxn>> {
        let mut cursor = self.header.head_offset;
        let tail = self.header.tail_offset;
        let mut pending: std::collections::BTreeMap<u64, Vec<Op>> =
            std::collections::BTreeMap::new();
        let mut applied: Vec<ReplayedTxn> = Vec::new();
        while cursor < tail {
            let rec = match self.read_record_at(cursor)? {
                Some(r) => r,
                None => break, // Corrupted record — stop replay.
            };
            cursor += rec.encoded_len;
            match rec.kind {
                RecordKind::Op => {
                    let op = Op::decode(&rec.payload)?;
                    pending.entry(rec.txn_id).or_default().push(op);
                }
                RecordKind::Commit => {
                    if let Some(ops) = pending.remove(&rec.txn_id) {
                        for op in &ops {
                            self.apply_op(op)?;
                        }
                        applied.push(ReplayedTxn {
                            txn_id: rec.txn_id,
                            ops: ops.len(),
                        });
                    }
                }
                RecordKind::Abort => {
                    pending.remove(&rec.txn_id);
                }
            }
        }
        if !applied.is_empty() {
            self.device.sync()?;
        }
        Ok(applied)
    }

    /// Reset head/tail to zero and flush the header.  Callers invoke this
    /// once they have verified that all replayed effects are on stable
    /// storage (typically after an fsync of the main image).
    pub fn checkpoint(&mut self) -> CoreFsResult<()> {
        self.header.head_offset = 0;
        self.header.tail_offset = 0;
        self.header.state = JSTATE_CLEAN;
        // Zero the first record block so stale records cannot be scanned.
        self.device.write_at(
            self.record_start_block * BLOCK_SIZE,
            &vec![0u8; BLOCK_SIZE as usize],
        )?;
        self.flush_header()
    }

    /// Current header snapshot.
    pub fn header(&self) -> &JournalHeader {
        &self.header
    }

    // -----------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------

    fn flush_header(&mut self) -> CoreFsResult<()> {
        self.device
            .write_at(self.header_block * BLOCK_SIZE, &self.header.encode_block())?;
        self.device.sync()
    }

    fn apply_op(&mut self, op: &Op) -> CoreFsResult<()> {
        match op {
            Op::BlockWrite { block, data } => {
                if data.len() as u64 != BLOCK_SIZE {
                    return Err(CoreFsError::State(
                        "replay: BlockWrite payload is not one full block".into(),
                    ));
                }
                self.device.write_at(block * BLOCK_SIZE, data)
            }
            Op::InodeUpdate { index, record } => {
                if record.len() != super::inode::INODE_RECORD_SIZE {
                    return Err(CoreFsError::State(
                        "replay: InodeUpdate record has wrong length".into(),
                    ));
                }
                // Read-modify-write of the containing block.
                // We need geometry for the inode location — pull from
                // the superblock that is always at PRIMARY_SUPERBLOCK_BLOCK.
                let sb_bytes = self
                    .device
                    .read_at(super::layout::PRIMARY_SUPERBLOCK_BLOCK * BLOCK_SIZE, BLOCK_SIZE)?;
                let sb = Superblock::decode_block(&sb_bytes)?;
                let geom = sb.geometry();
                let (block, offset_in_block) = geom.inode_record_location(*index)?;
                let mut buf = self.device.read_at(block * BLOCK_SIZE, BLOCK_SIZE)?;
                buf[offset_in_block as usize
                    ..offset_in_block as usize + super::inode::INODE_RECORD_SIZE]
                    .copy_from_slice(record);
                self.device.write_at(block * BLOCK_SIZE, &buf)
            }
        }
    }

    fn read_record_at(&self, offset: u64) -> CoreFsResult<Option<DecodedRecord>> {
        // Read the 40-byte header into a sector-aligned chunk.
        let header_bytes = self.read_bytes(offset, RECORD_HEADER_BYTES as u64)?;
        if header_bytes.iter().all(|b| *b == 0) {
            return Ok(None); // Untouched region.
        }
        let magic = u32::from_le_bytes(header_bytes[0..4].try_into().unwrap());
        if magic != RECORD_MAGIC {
            return Ok(None);
        }
        let kind = u16::from_le_bytes(header_bytes[4..6].try_into().unwrap());
        let _flags = u16::from_le_bytes(header_bytes[6..8].try_into().unwrap());
        let txn_id = u64::from_le_bytes(header_bytes[8..16].try_into().unwrap());
        let _seq = u64::from_le_bytes(header_bytes[16..24].try_into().unwrap());
        let payload_len = u32::from_le_bytes(header_bytes[24..28].try_into().unwrap()) as u64;
        let header_crc = u32::from_le_bytes(header_bytes[28..32].try_into().unwrap());

        let mut hdr_for_crc = header_bytes[..32].to_vec();
        hdr_for_crc[28..32].fill(0);
        let expected_hdr_crc = Crc32c::hash(&hdr_for_crc);
        if expected_hdr_crc != header_crc {
            return Ok(None);
        }
        let payload =
            self.read_bytes(offset + RECORD_HEADER_BYTES as u64, payload_len)?;
        let trailer = self.read_bytes(
            offset + RECORD_HEADER_BYTES as u64 + payload_len,
            RECORD_TRAILER_BYTES as u64,
        )?;
        let stored_rec_crc = u32::from_le_bytes(trailer.as_slice().try_into().unwrap());
        let mut full = Vec::with_capacity(RECORD_HEADER_BYTES + payload.len());
        full.extend_from_slice(&header_bytes);
        full.extend_from_slice(&payload);
        let expected_rec_crc = Crc32c::hash(&full);
        if expected_rec_crc != stored_rec_crc {
            return Ok(None);
        }
        let kind = RecordKind::from_u16(kind)?;
        let encoded_len = RECORD_HEADER_BYTES as u64 + payload_len + RECORD_TRAILER_BYTES as u64;
        Ok(Some(DecodedRecord {
            kind,
            txn_id,
            payload,
            encoded_len,
        }))
    }

    fn read_bytes(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        // Sector-align the read and slice out the wanted range.
        let sector = u64::from(self.device.sector_size());
        let device_offset = self.record_start_block * BLOCK_SIZE + offset;
        let aligned_start = (device_offset / sector) * sector;
        let end = device_offset + length;
        let aligned_end = end.div_ceil(sector) * sector;
        let raw = self.device.read_at(aligned_start, aligned_end - aligned_start)?;
        let start_in = (device_offset - aligned_start) as usize;
        Ok(raw[start_in..start_in + length as usize].to_vec())
    }

    fn write_bytes(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let sector = u64::from(self.device.sector_size());
        let device_offset = self.record_start_block * BLOCK_SIZE + offset;
        let aligned_start = (device_offset / sector) * sector;
        let end = device_offset + data.len() as u64;
        let aligned_end = end.div_ceil(sector) * sector;
        let mut buf = self.device.read_at(aligned_start, aligned_end - aligned_start)?;
        let start_in = (device_offset - aligned_start) as usize;
        buf[start_in..start_in + data.len()].copy_from_slice(data);
        self.device.write_at(aligned_start, &buf)
    }

    fn append_record(
        &mut self,
        kind: RecordKind,
        txn_id: u64,
        payload: &[u8],
    ) -> CoreFsResult<()> {
        let total = (RECORD_HEADER_BYTES + payload.len() + RECORD_TRAILER_BYTES) as u64;
        if self.header.tail_offset + total > self.header.record_area_bytes {
            return Err(CoreFsError::State(
                "journal: record area full — checkpoint required".into(),
            ));
        }
        let mut header = [0u8; RECORD_HEADER_BYTES];
        header[0..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
        header[4..6].copy_from_slice(&(kind as u16).to_le_bytes());
        header[6..8].copy_from_slice(&0u16.to_le_bytes());
        header[8..16].copy_from_slice(&txn_id.to_le_bytes());
        header[16..24].copy_from_slice(&self.header.next_seq.to_le_bytes());
        header[24..28].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        // header_crc at 28..32 (over first 32 bytes with this slot zero)
        let hdr_crc = {
            let mut hdr_for_crc = header[..32].to_vec();
            hdr_for_crc[28..32].fill(0);
            Crc32c::hash(&hdr_for_crc)
        };
        header[28..32].copy_from_slice(&hdr_crc.to_le_bytes());
        // reserved 32..40 stays zero.

        let mut full = Vec::with_capacity(RECORD_HEADER_BYTES + payload.len());
        full.extend_from_slice(&header);
        full.extend_from_slice(payload);
        let rec_crc = Crc32c::hash(&full);
        full.extend_from_slice(&rec_crc.to_le_bytes());

        let tail = self.header.tail_offset;
        self.write_bytes(tail, &full)?;
        self.header.tail_offset += total;
        self.header.next_seq += 1;
        Ok(())
    }
}

/// Builder that accumulates ops belonging to one transaction.
pub struct TxnBuilder {
    txn_id: u64,
    ops: Vec<Op>,
}

impl TxnBuilder {
    pub fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    pub fn txn_id(&self) -> u64 {
        self.txn_id
    }

    /// Append every op record plus a commit record, with a sync between the
    /// last op record and the commit record so a crash cannot leave a
    /// "committed" state without the preceding ops being durable.
    pub fn commit(self, journal: &mut Journal) -> CoreFsResult<u64> {
        if self.ops.is_empty() {
            return Err(CoreFsError::InvalidInput(
                "journal: cannot commit an empty transaction".into(),
            ));
        }
        journal.header.state = JSTATE_DIRTY;
        journal.flush_header()?;
        for op in &self.ops {
            let bytes = op.encode();
            journal.append_record(RecordKind::Op, self.txn_id, &bytes)?;
        }
        journal.device.sync()?;
        journal.append_record(RecordKind::Commit, self.txn_id, &[])?;
        journal.device.sync()?;
        journal.flush_header()?;
        Ok(self.txn_id)
    }

    /// Write an abort record — the transaction is logically discarded.
    pub fn abort(self, journal: &mut Journal) -> CoreFsResult<()> {
        journal.append_record(RecordKind::Abort, self.txn_id, &[])?;
        journal.device.sync()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct DecodedRecord {
    kind: RecordKind,
    txn_id: u64,
    payload: Vec<u8>,
    encoded_len: u64,
}

/// Summary of a replayed transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayedTxn {
    pub txn_id: u64,
    pub ops: usize,
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
