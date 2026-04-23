// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! End-to-end roundtrip tests for a FUSE-style handler running on a fresh
//! [`OdfDeviceSession`]. Mirrors the behaviour of `corefsd::CoreFsHandler`
//! in the AnyOS tree, but exercised via host-runnable tests so regressions
//! can be caught in CI without an AnyOS kernel image.
//!
//! The local `MiniHandler` replicates the same `PersistedState` mutation
//! patterns used by the daemon: FUSE-Inode interning, path resolution,
//! Write/Read/Create/Unlink/Mkdir/Rmdir/Rename/Setattr/Symlink/Readlink.
//! If the daemon's handler ever diverges from these semantics, that is a
//! bug in the daemon — this file is the authoritative host-side oracle.

use std::boxed::Box;
use std::collections::{BTreeMap, HashMap};
use std::string::{String, ToString};
use std::vec::Vec;

use corefs_core::domain::inode::{Inode, InodeId, InodeKind};
use corefs_core::domain::metadata::FileMetadata;
use corefs_core::error::CoreFsError;
use corefs_core::platform::Timestamp;
use corefs_core::storage::block_device::{BlockDevice, MemoryDevice};
use corefs_core::storage::block_store::BlockRecord;
use corefs_core::storage::ondisk::session::{OdfDeviceSession, OdfSessionOptions};

type InodeNo = u64;

const ENOENT: i32 = 2;
const ENOTEMPTY: i32 = 39;

#[derive(Debug)]
enum HandlerErr {
    Errno(i32),
}

struct MiniHandler {
    session: OdfDeviceSession,
    inode_by_no: BTreeMap<InodeNo, String>,
    no_by_id: BTreeMap<InodeId, InodeNo>,
    next_no: InodeNo,
    next_fh: u64,
    open_files: BTreeMap<u64, InodeNo>,
    /// Byte store for file/symlink data (separate from BlockRecord metadata).
    byte_store: HashMap<InodeId, Vec<u8>>,
}

impl MiniHandler {
    fn new(session: OdfDeviceSession) -> Self {
        let mut by_no = BTreeMap::new();
        by_no.insert(1u64, "/".to_string());
        Self {
            session,
            inode_by_no: by_no,
            no_by_id: BTreeMap::new(),
            next_no: 2,
            next_fh: 1,
            open_files: BTreeMap::new(),
            byte_store: HashMap::new(),
        }
    }

    fn intern(&mut self, inode: &Inode) -> InodeNo {
        if let Some(&no) = self.no_by_id.get(&inode.id) {
            self.inode_by_no.insert(no, inode.path.clone());
            return no;
        }
        let no = self.next_no;
        self.next_no += 1;
        self.inode_by_no.insert(no, inode.path.clone());
        self.no_by_id.insert(inode.id, no);
        no
    }

    fn path_of(&self, ino: InodeNo) -> Option<String> {
        self.inode_by_no.get(&ino).cloned()
    }

    fn child_path(parent: &str, name: &str) -> String {
        if parent == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent, name)
        }
    }

    fn lookup(&mut self, parent: InodeNo, name: &str) -> Result<InodeNo, HandlerErr> {
        let parent_path = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let full = Self::child_path(&parent_path, name);
        let inode = {
            let st = self.session.state();
            st.active_inodes.iter().find(|i| i.path == full).cloned()
        };
        match inode {
            Some(i) => Ok(self.intern(&i)),
            None => Err(HandlerErr::Errno(ENOENT)),
        }
    }

    fn create(
        &mut self,
        parent: InodeNo,
        name: &str,
        kind: InodeKind,
    ) -> Result<(InodeNo, u64), HandlerErr> {
        let parent_path = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let child = Self::child_path(&parent_path, name);
        let now = Timestamp::EPOCH;
        let res = self.session.mutate(|st| {
            if st.active_inodes.iter().any(|i| i.path == child) {
                return Err(CoreFsError::AlreadyExists(child.clone()));
            }
            let next_id = st
                .active_inodes
                .iter()
                .map(|i| i.id.0)
                .chain(st.deleted_inodes.iter().map(|i| i.id.0))
                .max()
                .unwrap_or(0)
                + 1;
            let mut meta = FileMetadata::default();
            meta.mode = match kind {
                InodeKind::Directory => 0o755,
                InodeKind::File => 0o644,
                InodeKind::Symlink => 0o777,
            };
            let inode = Inode::new_at(InodeId(next_id), kind, child.clone(), meta, now);
            st.active_inodes.push(inode);
            Ok(InodeId(next_id))
        });
        let (id, _) = res.map_err(|_| HandlerErr::Errno(5))?;
        let inode = {
            let st = self.session.state();
            st.active_inodes
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .unwrap()
        };
        let no = self.intern(&inode);
        let fh = self.next_fh;
        self.next_fh += 1;
        self.open_files.insert(fh, no);
        Ok((no, fh))
    }

    fn write(&mut self, ino: InodeNo, offset: u64, data: &[u8]) -> Result<u32, HandlerErr> {
        let path = self.path_of(ino).ok_or(HandlerErr::Errno(ENOENT))?;
        let start = offset as usize;
        let end = start + data.len();
        // Look up inode id first (immutable borrow).
        let id = {
            let st = self.session.state();
            st.active_inodes
                .iter()
                .find(|i| i.path == path)
                .map(|i| i.id)
                .ok_or(HandlerErr::Errno(ENOENT))?
        };
        // Update byte store.
        let bytes = self.byte_store.entry(id).or_default();
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(data);
        let new_len = bytes.len();
        // Update PersistedState metadata.
        let res = self.session.mutate(|st| {
            if st.block_records.iter().all(|r| r.inode != id) {
                st.block_records.push(BlockRecord {
                    inode: id,
                    logical_size: new_len as u64,
                    extents: vec![],
                    content_crc: 0,
                    flags: 0,
                });
            } else if let Some(rec) = st.block_records.iter_mut().find(|r| r.inode == id) {
                rec.logical_size = new_len as u64;
            }
            if let Some(inode) = st.active_inodes.iter_mut().find(|i| i.id == id) {
                if end > inode.size {
                    inode.size = end;
                }
                inode.touch_modified_at(Timestamp::EPOCH);
            }
            Ok(())
        });
        res.map_err(|_| HandlerErr::Errno(5))?;
        Ok(data.len() as u32)
    }

    fn read(&self, ino: InodeNo, offset: u64, size: u32) -> Result<Vec<u8>, HandlerErr> {
        let path = self.path_of(ino).ok_or(HandlerErr::Errno(ENOENT))?;
        let id = {
            let st = self.session.state();
            st.active_inodes
                .iter()
                .find(|i| i.path == path)
                .map(|i| i.id)
                .ok_or(HandlerErr::Errno(ENOENT))?
        };
        let empty = Vec::new();
        let bytes = self.byte_store.get(&id).unwrap_or(&empty);
        let start = offset as usize;
        let end = start.saturating_add(size as usize).min(bytes.len());
        Ok(if start >= bytes.len() {
            Vec::new()
        } else {
            bytes[start..end].to_vec()
        })
    }

    fn unlink(&mut self, parent: InodeNo, name: &str) -> Result<(), HandlerErr> {
        let parent_path = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let child = Self::child_path(&parent_path, name);
        let res = self.session.mutate(|st| {
            let idx = st
                .active_inodes
                .iter()
                .position(|i| i.path == child)
                .ok_or_else(|| CoreFsError::NotFound(child.clone()))?;
            let removed = st.active_inodes.remove(idx);
            st.block_records.retain(|r| r.inode != removed.id);
            st.deleted_inodes.push(removed);
            Ok(())
        });
        res.map_err(|_| HandlerErr::Errno(ENOENT))?;
        Ok(())
    }

    fn rmdir(&mut self, parent: InodeNo, name: &str) -> Result<(), HandlerErr> {
        let parent_path = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let child = Self::child_path(&parent_path, name);
        let prefix = format!("{}/", child);
        let res = self.session.mutate(|st| {
            let idx = st
                .active_inodes
                .iter()
                .position(|i| i.path == child)
                .ok_or_else(|| CoreFsError::NotFound(child.clone()))?;
            if st.active_inodes[idx].kind != InodeKind::Directory {
                return Err(CoreFsError::InvalidCommand("not a directory".into()));
            }
            if st
                .active_inodes
                .iter()
                .any(|i| i.path.starts_with(prefix.as_str()))
            {
                return Err(CoreFsError::InvalidCommand("directory not empty".into()));
            }
            let removed = st.active_inodes.remove(idx);
            st.block_records.retain(|r| r.inode != removed.id);
            st.deleted_inodes.push(removed);
            Ok(())
        });
        match res {
            Ok(_) => Ok(()),
            Err(CoreFsError::InvalidCommand(ref m)) if m == "directory not empty" => {
                Err(HandlerErr::Errno(ENOTEMPTY))
            }
            Err(_) => Err(HandlerErr::Errno(5)),
        }
    }

    fn rename(
        &mut self,
        parent: InodeNo,
        old: &str,
        new_parent: InodeNo,
        new: &str,
    ) -> Result<(), HandlerErr> {
        let op = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let np = self.path_of(new_parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let old_path = Self::child_path(&op, old);
        let new_path = Self::child_path(&np, new);
        let old_prefix = format!("{}/", old_path);
        let new_prefix = format!("{}/", new_path);
        let op_owned = old_path.clone();
        let np_owned = new_path.clone();
        let res = self.session.mutate(|st| {
            if !st.active_inodes.iter().any(|i| i.path == op_owned) {
                return Err(CoreFsError::NotFound(op_owned.clone()));
            }
            if st.active_inodes.iter().any(|i| i.path == np_owned) {
                return Err(CoreFsError::AlreadyExists(np_owned.clone()));
            }
            for inode in st.active_inodes.iter_mut() {
                if inode.path == op_owned {
                    inode.path = np_owned.clone();
                    inode.touch_changed_at(Timestamp::EPOCH);
                } else if inode.path.starts_with(old_prefix.as_str()) {
                    let suf = &inode.path[old_prefix.len()..];
                    inode.path = format!("{}{}", new_prefix, suf);
                    inode.touch_changed_at(Timestamp::EPOCH);
                }
            }
            Ok(())
        });
        res.map_err(|_| HandlerErr::Errno(5))?;
        // Refresh inode_by_no for affected entries so subsequent lookups
        // via the old FUSE-InodeNo continue to resolve.
        let updates: Vec<(InodeNo, String)> = {
            let st = self.session.state();
            self.inode_by_no
                .iter()
                .filter_map(|(no, old)| {
                    let id = self
                        .no_by_id
                        .iter()
                        .find(|(_, n)| *n == no)
                        .map(|(id, _)| *id)?;
                    let cur = st
                        .active_inodes
                        .iter()
                        .find(|i| i.id == id)
                        .map(|i| i.path.clone())?;
                    if cur != *old { Some((*no, cur)) } else { None }
                })
                .collect()
        };
        for (no, p) in updates {
            self.inode_by_no.insert(no, p);
        }
        Ok(())
    }

    fn setattr_size(&mut self, ino: InodeNo, new_size: u64) -> Result<(), HandlerErr> {
        let path = self.path_of(ino).ok_or(HandlerErr::Errno(ENOENT))?;
        let ns = new_size as usize;
        let id = {
            let st = self.session.state();
            st.active_inodes
                .iter()
                .find(|i| i.path == path)
                .map(|i| i.id)
                .ok_or(HandlerErr::Errno(ENOENT))?
        };
        // Update byte store.
        let bytes = self.byte_store.entry(id).or_default();
        if bytes.len() < ns {
            bytes.resize(ns, 0);
        } else {
            bytes.truncate(ns);
        }
        // Update PersistedState metadata.
        let res = self.session.mutate(|st| {
            if let Some(rec) = st.block_records.iter_mut().find(|r| r.inode == id) {
                rec.logical_size = new_size;
            }
            if let Some(inode) = st.active_inodes.iter_mut().find(|i| i.id == id) {
                inode.size = ns;
                inode.touch_modified_at(Timestamp::EPOCH);
            }
            Ok(())
        });
        res.map_err(|_| HandlerErr::Errno(5))?;
        Ok(())
    }

    fn symlink(
        &mut self,
        parent: InodeNo,
        name: &str,
        target: &str,
    ) -> Result<InodeNo, HandlerErr> {
        let parent_path = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let child = Self::child_path(&parent_path, name);
        let target_bytes = target.as_bytes().to_vec();
        let len = target_bytes.len();
        let res = self.session.mutate(|st| {
            if st.active_inodes.iter().any(|i| i.path == child) {
                return Err(CoreFsError::AlreadyExists(child.clone()));
            }
            let next_id = st
                .active_inodes
                .iter()
                .map(|i| i.id.0)
                .chain(st.deleted_inodes.iter().map(|i| i.id.0))
                .max()
                .unwrap_or(0)
                + 1;
            let mut meta = FileMetadata::default();
            meta.mode = 0o777;
            let mut inode = Inode::new_at(
                InodeId(next_id),
                InodeKind::Symlink,
                child.clone(),
                meta,
                Timestamp::EPOCH,
            );
            inode.size = len;
            st.active_inodes.push(inode);
            st.block_records.push(BlockRecord {
                inode: InodeId(next_id),
                logical_size: len as u64,
                extents: vec![],
                content_crc: 0,
                flags: 0,
            });
            Ok(InodeId(next_id))
        });
        let (id, _) = res.map_err(|_| HandlerErr::Errno(5))?;
        // Store target bytes in the byte store.
        self.byte_store.insert(id, target_bytes);
        let inode = {
            let st = self.session.state();
            st.active_inodes
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .unwrap()
        };
        Ok(self.intern(&inode))
    }

    fn readlink(&self, ino: InodeNo) -> Result<String, HandlerErr> {
        let path = self.path_of(ino).ok_or(HandlerErr::Errno(ENOENT))?;
        let id = {
            let st = self.session.state();
            let inode = st
                .active_inodes
                .iter()
                .find(|i| i.path == path)
                .ok_or(HandlerErr::Errno(ENOENT))?;
            if inode.kind != InodeKind::Symlink {
                return Err(HandlerErr::Errno(22));
            }
            inode.id
        };
        let empty = Vec::new();
        let bytes = self.byte_store.get(&id).unwrap_or(&empty);
        Ok(String::from_utf8(bytes.clone()).unwrap())
    }

    fn readdir(&self, parent: InodeNo) -> Result<Vec<String>, HandlerErr> {
        let parent_path = self.path_of(parent).ok_or(HandlerErr::Errno(ENOENT))?;
        let st = self.session.state();
        let mut out = Vec::new();
        for inode in st.active_inodes.iter() {
            if let Some(last) = inode.path.rfind('/') {
                let p = if last == 0 { "/" } else { &inode.path[..last] };
                if p == parent_path {
                    out.push(inode.path[last + 1..].to_string());
                }
            }
        }
        Ok(out)
    }

    fn stat_size(&self, ino: InodeNo) -> Result<usize, HandlerErr> {
        let path = self.path_of(ino).ok_or(HandlerErr::Errno(ENOENT))?;
        let st = self.session.state();
        Ok(st
            .active_inodes
            .iter()
            .find(|i| i.path == path)
            .unwrap()
            .size)
    }
}

fn mk_handler() -> MiniHandler {
    let opts = OdfSessionOptions {
        capacity_bytes: 4 * 1024 * 1024,
        ..OdfSessionOptions::with_defaults()
    };
    let dev: Box<dyn BlockDevice> = Box::new(MemoryDevice::new(4 * 1024 * 1024, 4096).unwrap());
    let sess = OdfDeviceSession::format_new_at(dev, &opts, Timestamp::EPOCH).unwrap();
    MiniHandler::new(sess)
}

#[test]
fn e2e_file_create_write_read_unlink_roundtrip() {
    let mut h = mk_handler();
    // Create /file.txt.
    let (ino, _fh) = h.create(1, "file.txt", InodeKind::File).unwrap();
    // Write 11 bytes.
    let n = h.write(ino, 0, b"hello world").unwrap();
    assert_eq!(n, 11);
    // Lookup /file.txt.
    let ino2 = h.lookup(1, "file.txt").unwrap();
    assert_eq!(ino, ino2);
    assert_eq!(h.stat_size(ino2).unwrap(), 11);
    // Read 11 bytes.
    let bytes = h.read(ino2, 0, 11).unwrap();
    assert_eq!(&bytes, b"hello world");
    // Unlink.
    h.unlink(1, "file.txt").unwrap();
    // Follow-up lookup fails with ENOENT.
    match h.lookup(1, "file.txt") {
        Err(HandlerErr::Errno(e)) => assert_eq!(e, ENOENT),
        _ => panic!("expected ENOENT"),
    }
}

#[test]
fn e2e_directory_mkdir_create_readdir_rmdir_cycle() {
    let mut h = mk_handler();
    // Mkdir /subdir.
    let (sub_ino, _) = h.create(1, "subdir", InodeKind::Directory).unwrap();
    // Create /subdir/inner.txt.
    let (_inner, _) = h.create(sub_ino, "inner.txt", InodeKind::File).unwrap();
    // Readdir /subdir should include "inner.txt".
    let entries = h.readdir(sub_ino).unwrap();
    assert!(
        entries.iter().any(|n| n == "inner.txt"),
        "entries: {:?}",
        entries
    );
    // Rmdir with a child → ENOTEMPTY.
    match h.rmdir(1, "subdir") {
        Err(HandlerErr::Errno(e)) => assert_eq!(e, ENOTEMPTY),
        _ => panic!("expected ENOTEMPTY"),
    }
    // Remove the child, then rmdir succeeds.
    h.unlink(sub_ino, "inner.txt").unwrap();
    h.rmdir(1, "subdir").unwrap();
}

#[test]
fn e2e_rename_preserves_content_and_size() {
    let mut h = mk_handler();
    let (ino, _fh) = h.create(1, "a.txt", InodeKind::File).unwrap();
    h.write(ino, 0, b"persistent payload").unwrap();
    assert_eq!(h.stat_size(ino).unwrap(), 18);
    // Rename a.txt → b.txt at the same parent.
    h.rename(1, "a.txt", 1, "b.txt").unwrap();
    // Old name gone.
    match h.lookup(1, "a.txt") {
        Err(HandlerErr::Errno(e)) => assert_eq!(e, ENOENT),
        _ => panic!("a.txt must be gone"),
    }
    // New name resolves; size preserved.
    let ino_b = h.lookup(1, "b.txt").unwrap();
    assert_eq!(h.stat_size(ino_b).unwrap(), 18);
    // Content still readable.
    let data = h.read(ino_b, 0, 18).unwrap();
    assert_eq!(&data, b"persistent payload");
}

#[test]
fn e2e_rename_rewrites_descendants() {
    let mut h = mk_handler();
    let (src, _) = h.create(1, "src", InodeKind::Directory).unwrap();
    let (inner, _) = h.create(src, "inside", InodeKind::File).unwrap();
    h.write(inner, 0, b"xx").unwrap();
    h.rename(1, "src", 1, "dst").unwrap();
    // /dst/inside is reachable with the original FUSE-InodeNo because the
    // inode_by_no map follows the rename.
    let dst = h.lookup(1, "dst").unwrap();
    let inside = h.lookup(dst, "inside").unwrap();
    assert_eq!(h.stat_size(inside).unwrap(), 2);
}

#[test]
fn e2e_setattr_truncate_shrinks_payload() {
    let mut h = mk_handler();
    let (ino, _) = h.create(1, "t.bin", InodeKind::File).unwrap();
    h.write(ino, 0, b"hello world").unwrap();
    h.setattr_size(ino, 5).unwrap();
    assert_eq!(h.stat_size(ino).unwrap(), 5);
    let data = h.read(ino, 0, 100).unwrap();
    assert_eq!(&data, b"hello");
}

#[test]
fn e2e_symlink_roundtrip() {
    let mut h = mk_handler();
    let ino = h.symlink(1, "link", "/some/where").unwrap();
    let target = h.readlink(ino).unwrap();
    assert_eq!(target, "/some/where");
}
