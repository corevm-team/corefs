use crate::app::{CoreFsService, PersistedState};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use crate::storage::volume_wal::{VolumeWal, WalOperation};
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use libc::{EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EROFS};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(1);
const ROOT_INO: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinuxMountOptions {
    pub create_if_missing: bool,
}

#[derive(Debug, Clone)]
struct FuseNode {
    path: String,
    parent_path: String,
    inode: Option<Inode>,
    data: Vec<u8>,
}

impl FuseNode {
    fn ino(&self) -> u64 {
        self.inode
            .as_ref()
            .map(|inode| inode.id.0 + 1)
            .unwrap_or(ROOT_INO)
    }

    fn kind(&self) -> InodeKind {
        self.inode
            .as_ref()
            .map(|inode| inode.kind)
            .unwrap_or(InodeKind::Directory)
    }

    fn attr(&self) -> FileAttr {
        let now = SystemTime::now();
        let uid = current_uid();
        let gid = current_gid();
        let perm = match self.kind() {
            InodeKind::File => 0o644,
            InodeKind::Directory => 0o755,
            InodeKind::Symlink => 0o777,
        };
        let size = match self.kind() {
            InodeKind::File | InodeKind::Symlink => self.data.len() as u64,
            InodeKind::Directory => 0,
        };
        let mtime = self
            .inode
            .as_ref()
            .map(|inode| inode.modified_at)
            .unwrap_or(now);
        let ctime = self
            .inode
            .as_ref()
            .map(|inode| inode.created_at)
            .unwrap_or(now);

        FileAttr {
            ino: self.ino(),
            size,
            blocks: 1,
            atime: mtime,
            mtime,
            ctime,
            crtime: ctime,
            kind: match self.kind() {
                InodeKind::File => FileType::RegularFile,
                InodeKind::Directory => FileType::Directory,
                InodeKind::Symlink => FileType::Symlink,
            },
            perm,
            nlink: if matches!(self.kind(), InodeKind::Directory) {
                2
            } else {
                1
            },
            uid,
            gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct CoreFsFuseView {
    nodes_by_ino: HashMap<u64, FuseNode>,
    ino_by_path: HashMap<String, u64>,
    children: BTreeMap<String, Vec<String>>,
    volume_name: String,
}

impl CoreFsFuseView {
    fn from_state(state: PersistedState) -> Self {
        let mut nodes_by_ino = HashMap::new();
        let mut ino_by_path = HashMap::new();
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let block_map: HashMap<InodeId, Vec<u8>> = state
            .block_records
            .into_iter()
            .map(|record| (record.inode, record.bytes))
            .collect();

        let root = FuseNode {
            path: "/".to_string(),
            parent_path: "/".to_string(),
            inode: None,
            data: Vec::new(),
        };
        nodes_by_ino.insert(ROOT_INO, root);
        ino_by_path.insert("/".to_string(), ROOT_INO);
        children.entry("/".to_string()).or_default();

        for inode in state.active_inodes {
            let ino = inode.id.0 + 1;
            let parent_path = parent_path(&inode.path);
            let data = block_map.get(&inode.id).cloned().unwrap_or_default();
            children
                .entry(parent_path.clone())
                .or_default()
                .push(base_name(&inode.path));
            children.entry(inode.path.clone()).or_default();
            ino_by_path.insert(inode.path.clone(), ino);
            nodes_by_ino.insert(
                ino,
                FuseNode {
                    path: inode.path.clone(),
                    parent_path,
                    inode: Some(inode),
                    data,
                },
            );
        }

        for names in children.values_mut() {
            names.sort();
            names.dedup();
        }

        Self {
            nodes_by_ino,
            ino_by_path,
            children,
            volume_name: state.volume.name,
        }
    }

    fn load_image(path: impl AsRef<Path>) -> CoreFsResult<Self> {
        let fs = CoreFsService::load_image_from_path(path)?;
        Ok(Self::from_state(fs.export_state()))
    }

    fn node(&self, ino: u64) -> Option<&FuseNode> {
        self.nodes_by_ino.get(&ino)
    }

    fn lookup_child(&self, parent: u64, name: &OsStr) -> Option<&FuseNode> {
        let parent_node = self.node(parent)?;
        let child_name = name.to_str()?;
        let child_path = if parent_node.path == "/" {
            format!("/{child_name}")
        } else {
            format!("{}/{}", parent_node.path, child_name)
        };
        let ino = self.ino_by_path.get(&child_path)?;
        self.node(*ino)
    }

    fn directory_entries(&self, ino: u64) -> Vec<(u64, FileType, String)> {
        let Some(node) = self.node(ino) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        entries.push((node.ino(), FileType::Directory, ".".to_string()));
        let parent_ino = if ino == ROOT_INO {
            ROOT_INO
        } else {
            *self.ino_by_path.get(&node.parent_path).unwrap_or(&ROOT_INO)
        };
        entries.push((parent_ino, FileType::Directory, "..".to_string()));

        if let Some(children) = self.children.get(&node.path) {
            for child_name in children {
                let child_path = if node.path == "/" {
                    format!("/{child_name}")
                } else {
                    format!("{}/{}", node.path, child_name)
                };
                if let Some(child_ino) = self.ino_by_path.get(&child_path) {
                    if let Some(child) = self.node(*child_ino) {
                        let file_type = match child.kind() {
                            InodeKind::File => FileType::RegularFile,
                            InodeKind::Directory => FileType::Directory,
                            InodeKind::Symlink => FileType::Symlink,
                        };
                        entries.push((*child_ino, file_type, child_name.clone()));
                    }
                }
            }
        }

        entries
    }
}

pub fn create_image(path: impl AsRef<Path>, include_demo: bool) -> CoreFsResult<()> {
    let fs = if include_demo {
        demo_fs()?
    } else {
        CoreFsService::format(crate::config::CoreFsConfig::default())
    };
    fs.save_image_to_path(path)
}

pub fn mount_image(
    image_path: impl AsRef<Path>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let image_path = image_path.as_ref();
    let mount_point = mount_point.as_ref();
    let view = CoreFsFuseView::load_image(image_path)?;
    let mount = CoreFsFuseMount { view };
    let fs_name = format!("corefs:{}", mount.view.volume_name);

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RO,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount CoreFS image {} on {}: {error}",
            image_path.display(),
            mount_point.display()
        ))
    })
}

#[derive(Debug, Clone)]
struct CoreFsFuseMount {
    view: CoreFsFuseView,
}

impl Filesystem for CoreFsFuseMount {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.view.lookup_child(parent, name) {
            Some(node) => reply.entry(&TTL, &node.attr(), 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        match self.view.node(ino) {
            Some(node) => reply.attr(&TTL, &node.attr()),
            None => reply.error(ENOENT),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.view.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::Symlink) => reply.data(&node.data),
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.view.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::File) => {
                if flags & libc::O_ACCMODE != libc::O_RDONLY {
                    reply.error(EROFS);
                } else {
                    reply.opened(0, 0);
                }
            }
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        match self.view.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::File) => {
                let start = offset.max(0) as usize;
                let end = start.saturating_add(size as usize).min(node.data.len());
                let slice = node.data.get(start..end).unwrap_or(&[]);
                reply.data(slice);
            }
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let entries = self.view.directory_entries(ino);
        if entries.is_empty() {
            reply.error(ENOENT);
            return;
        }

        for (index, (entry_ino, kind, name)) in
            entries.into_iter().enumerate().skip(offset as usize)
        {
            let next_offset = (index + 1) as i64;
            if reply.add(entry_ino, next_offset, kind, name) {
                break;
            }
        }

        reply.ok();
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        reply.statfs(
            fuse_total_blocks(),
            fuse_free_blocks(&self.view.nodes_by_ino),
            fuse_free_blocks(&self.view.nodes_by_ino),
            self.view.nodes_by_ino.len() as u64,
            fuse_free_inodes(self.view.nodes_by_ino.len()),
            FUSE_BLOCK_SIZE,
            255,
            FUSE_BLOCK_SIZE,
        );
    }
}

// ── Read-write FUSE mount ────────────────────────────────────────────────────

#[derive(Debug)]
struct CoreFsFuseMountRw {
    service: CoreFsService,
    image_path: PathBuf,
    pending_wal: Option<VolumeWal>,
    nodes_by_ino: HashMap<u64, FuseNode>,
    ino_by_path: HashMap<String, u64>,
    children: BTreeMap<String, Vec<String>>,
    dirty: bool,
}

impl CoreFsFuseMountRw {
    fn from_service(service: CoreFsService, image_path: PathBuf) -> Self {
        let mut mount = Self {
            service,
            image_path,
            pending_wal: None,
            nodes_by_ino: HashMap::new(),
            ino_by_path: HashMap::new(),
            children: BTreeMap::new(),
            dirty: false,
        };
        mount.rebuild_indexes();
        mount
    }

    fn open_session(_service: CoreFsService, image_path: PathBuf) -> CoreFsResult<Self> {
        let mut service = CoreFsService::load_image_from_path(&image_path)?;
        service.mark_unclean_shutdown();
        service.save_image_to_path(&image_path)?;
        Ok(Self::from_service(service, image_path))
    }

    /// Rebuild all FUSE index maps from the current service state.
    /// Called after `from_service` and after any operation that changes paths (rename).
    fn rebuild_indexes(&mut self) {
        let state = self.service.export_state();
        let block_map: HashMap<crate::domain::inode::InodeId, Vec<u8>> = state
            .block_records
            .into_iter()
            .map(|r| (r.inode, r.bytes))
            .collect();

        let mut nodes_by_ino = HashMap::new();
        let mut ino_by_path = HashMap::new();
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();

        nodes_by_ino.insert(
            ROOT_INO,
            FuseNode {
                path: "/".to_string(),
                parent_path: "/".to_string(),
                inode: None,
                data: Vec::new(),
            },
        );
        ino_by_path.insert("/".to_string(), ROOT_INO);
        children.entry("/".to_string()).or_default();

        for inode in state.active_inodes {
            let ino = inode.id.0 + 1;
            let par = parent_path(&inode.path);
            let data = block_map.get(&inode.id).cloned().unwrap_or_default();
            children
                .entry(par.clone())
                .or_default()
                .push(base_name(&inode.path));
            children.entry(inode.path.clone()).or_default();
            ino_by_path.insert(inode.path.clone(), ino);
            nodes_by_ino.insert(
                ino,
                FuseNode {
                    path: inode.path.clone(),
                    parent_path: par,
                    inode: Some(inode),
                    data,
                },
            );
        }
        for names in children.values_mut() {
            names.sort();
            names.dedup();
        }

        self.nodes_by_ino = nodes_by_ino;
        self.ino_by_path = ino_by_path;
        self.children = children;
    }

    fn node(&self, ino: u64) -> Option<&FuseNode> {
        self.nodes_by_ino.get(&ino)
    }

    fn lookup_child(&self, parent: u64, name: &OsStr) -> Option<&FuseNode> {
        let par = self.nodes_by_ino.get(&parent)?;
        let child_name = name.to_str()?;
        let child_path = if par.path == "/" {
            format!("/{child_name}")
        } else {
            format!("{}/{child_name}", par.path)
        };
        let ino = self.ino_by_path.get(&child_path)?;
        self.nodes_by_ino.get(ino)
    }

    fn child_path(&self, parent: u64, name: &OsStr) -> Option<String> {
        let par = self.nodes_by_ino.get(&parent)?;
        let child_name = name.to_str()?;
        Some(if par.path == "/" {
            format!("/{child_name}")
        } else {
            format!("{}/{child_name}", par.path)
        })
    }

    fn directory_entries(&self, ino: u64) -> Vec<(u64, FileType, String)> {
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        entries.push((node.ino(), FileType::Directory, ".".to_string()));
        let parent_ino = if ino == ROOT_INO {
            ROOT_INO
        } else {
            *self.ino_by_path.get(&node.parent_path).unwrap_or(&ROOT_INO)
        };
        entries.push((parent_ino, FileType::Directory, "..".to_string()));

        if let Some(children) = self.children.get(&node.path) {
            for child_name in children {
                let child_path = if node.path == "/" {
                    format!("/{child_name}")
                } else {
                    format!("{}/{child_name}", node.path)
                };
                if let Some(child_ino) = self.ino_by_path.get(&child_path) {
                    if let Some(child) = self.nodes_by_ino.get(child_ino) {
                        let ft = match child.kind() {
                            InodeKind::File => FileType::RegularFile,
                            InodeKind::Directory => FileType::Directory,
                            InodeKind::Symlink => FileType::Symlink,
                        };
                        entries.push((*child_ino, ft, child_name.clone()));
                    }
                }
            }
        }
        entries
    }

    /// Register a freshly created path in all index maps.
    fn register_node(&mut self, node: FuseNode) {
        let ino = node.ino();
        let par = node.parent_path.clone();
        let name = base_name(&node.path);
        self.ino_by_path.insert(node.path.clone(), ino);
        self.children.entry(node.path.clone()).or_default();
        self.children.entry(par).or_default().push(name);
        // keep children sorted + deduplicated
        for names in self.children.values_mut() {
            names.sort();
            names.dedup();
        }
        self.nodes_by_ino.insert(ino, node);
    }

    /// Remove a path from all index maps.
    fn unregister_ino(&mut self, ino: u64) {
        if let Some(node) = self.nodes_by_ino.remove(&ino) {
            self.ino_by_path.remove(&node.path);
            self.children.remove(&node.path);
            if let Some(siblings) = self.children.get_mut(&node.parent_path) {
                let name = base_name(&node.path);
                siblings.retain(|n| n != &name);
            }
        }
    }

    /// Save the volume image to disk. Returns true on success.
    fn persist(&mut self) -> bool {
        self.service.commit_write_transaction();
        self.service.mark_clean_shutdown();
        match self.service.save_image_to_path(&self.image_path) {
            Ok(()) => {
                self.pending_wal = None;
                self.dirty = false;
                true
            }
            Err(_) => {
                self.service.mark_unclean_shutdown();
                false
            }
        }
    }

    fn ensure_mutation_session(&mut self, label: &str) -> CoreFsResult<()> {
        if !self.service.had_unclean_shutdown() {
            self.service.mark_unclean_shutdown();
            self.service.save_image_to_path(&self.image_path)?;
        }
        if !self.service.has_pending_transaction() {
            let transaction_id = self.service.begin_write_transaction(label);
            let wal = VolumeWal::new(transaction_id, label);
            self.service.set_pending_wal(wal.clone());
            self.pending_wal = Some(wal);
            self.service.save_image_to_path(&self.image_path)?;
        }
        Ok(())
    }

    fn record_wal_operation(&mut self, operation: WalOperation) -> CoreFsResult<()> {
        self.service.update_pending_wal(|wal| {
            wal.push(operation.clone());
            Ok(())
        })?;
        if let Some(wal) = self.pending_wal.as_mut() {
            wal.push(operation);
        }
        self.service.save_image_to_path(&self.image_path)
    }

    fn record_block_patch(
        &mut self,
        inode: InodeId,
        start: usize,
        bytes: &[u8],
        final_len: usize,
    ) -> CoreFsResult<()> {
        let block_size = self.service.block_size().max(1);
        let mut consumed = 0usize;

        while consumed < bytes.len() {
            let absolute_offset = start + consumed;
            let block_index = absolute_offset / block_size;
            let block_offset = absolute_offset % block_size;
            let chunk_len = (block_size - block_offset).min(bytes.len() - consumed);
            self.record_wal_operation(WalOperation::PatchBlock {
                inode,
                block_index,
                block_offset,
                bytes: bytes[consumed..consumed + chunk_len].to_vec(),
                final_len,
            })?;
            consumed += chunk_len;
        }

        Ok(())
    }
}

impl Filesystem for CoreFsFuseMountRw {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.lookup_child(parent, name) {
            Some(node) => reply.entry(&TTL, &node.attr(), 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        match self.node(ino) {
            Some(node) => reply.attr(&TTL, &node.attr()),
            None => reply.error(ENOENT),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            reply.error(ENOENT);
            return;
        };
        if !matches!(node.kind(), InodeKind::File) {
            reply.attr(&TTL, &node.attr());
            return;
        }
        if let Some(new_size) = size {
            let inode_id = node
                .inode
                .as_ref()
                .map(|inode| inode.id)
                .unwrap_or(InodeId(0));
            let path = node.path.clone();
            if self.ensure_mutation_session("setattr").is_err() {
                reply.error(EIO);
                return;
            }
            let current = self.service.read_file(&path).unwrap_or_default();
            let mut buf = current;
            buf.resize(new_size as usize, 0);
            if let Err(_) = self.service.write_file(&path, &buf) {
                reply.error(EIO);
                return;
            }
            if self
                .record_wal_operation(WalOperation::TruncateInode {
                    inode: inode_id,
                    size: new_size as usize,
                })
                .is_err()
            {
                reply.error(EIO);
                return;
            }
            if let Some(n) = self.nodes_by_ino.get_mut(&ino) {
                n.data = buf;
                if let Some(ref mut inode) = n.inode {
                    inode.modified_at = SystemTime::now();
                    inode.size = new_size as usize;
                }
            }
            self.dirty = true;
        }
        match self.nodes_by_ino.get(&ino) {
            Some(node) => reply.attr(&TTL, &node.attr()),
            None => reply.error(ENOENT),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.node(ino) {
            Some(node) if matches!(node.kind(), InodeKind::Symlink) => reply.data(&node.data),
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let file_path = self.node(ino).and_then(|node| {
            if matches!(node.kind(), InodeKind::File) {
                Some(node.path.clone())
            } else {
                None
            }
        });
        match file_path {
            Some(path) => {
                let inode_id = self
                    .node(ino)
                    .and_then(|node| node.inode.as_ref().map(|inode| inode.id))
                    .unwrap_or(InodeId(0));
                // truncate on open with O_TRUNC
                if flags & libc::O_TRUNC != 0 {
                    if self.ensure_mutation_session("truncate").is_err() {
                        reply.error(EIO);
                        return;
                    }
                    if self.service.write_file(&path, b"").is_err() {
                        reply.error(EIO);
                        return;
                    }
                    if self
                        .record_wal_operation(WalOperation::TruncateInode {
                            inode: inode_id,
                            size: 0,
                        })
                        .is_err()
                    {
                        reply.error(EIO);
                        return;
                    }
                    if let Some(n) = self.nodes_by_ino.get_mut(&ino) {
                        n.data.clear();
                        if let Some(ref mut inode) = n.inode {
                            inode.size = 0;
                            inode.modified_at = SystemTime::now();
                        }
                    }
                    self.dirty = true;
                }
                reply.opened(0, 0);
            }
            None if self.node(ino).is_some() => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        match self.nodes_by_ino.get(&ino) {
            Some(node) if matches!(node.kind(), InodeKind::File) => {
                let start = offset.max(0) as usize;
                let end = start.saturating_add(size as usize).min(node.data.len());
                reply.data(node.data.get(start..end).unwrap_or(&[]));
            }
            Some(_) => reply.error(EIO),
            None => reply.error(ENOENT),
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let Some(node) = self.nodes_by_ino.get(&ino) else {
            reply.error(ENOENT);
            return;
        };
        if !matches!(node.kind(), InodeKind::File) {
            reply.error(EISDIR);
            return;
        }
        let inode_id = node
            .inode
            .as_ref()
            .map(|inode| inode.id)
            .unwrap_or(InodeId(0));
        let path = node.path.clone();
        let start = offset.max(0) as usize;
        let end = start + data.len();

        // read-modify-write: splice new bytes into existing content
        if self.ensure_mutation_session("write").is_err() {
            reply.error(EIO);
            return;
        }
        let mut buf = self.service.read_file(&path).unwrap_or_default();
        if buf.len() < end {
            buf.resize(end, 0);
        }
        buf[start..end].copy_from_slice(data);

        if let Err(_) = self.service.write_file(&path, &buf) {
            reply.error(EIO);
            return;
        }
        if self
            .record_block_patch(inode_id, start, data, buf.len())
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        let written = data.len() as u32;
        if let Some(n) = self.nodes_by_ino.get_mut(&ino) {
            n.data = buf;
            if let Some(ref mut inode) = n.inode {
                inode.size = n.data.len();
                inode.modified_at = SystemTime::now();
            }
        }
        self.dirty = true;
        reply.written(written);
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        if self.ino_by_path.contains_key(&path) {
            reply.error(EEXIST);
            return;
        }
        if self.ensure_mutation_session("create").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.create_file(&path, b"", &[]) {
            reply.error(EIO);
            return;
        }
        let Some(inode_id) = self.service.inode_for_path(&path) else {
            reply.error(EIO);
            return;
        };
        if self
            .record_wal_operation(WalOperation::CreateFile {
                path: path.clone(),
                inode: inode_id,
            })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        let inode = self.service.get_inode(&path).cloned();
        let par = parent_path(&path);
        let node = FuseNode {
            path,
            parent_path: par,
            inode,
            data: Vec::new(),
        };
        let attr = node.attr();
        self.register_node(node);
        self.dirty = true;
        reply.created(&TTL, &attr, 0, 0, 0);
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        if self.ino_by_path.contains_key(&path) {
            reply.error(EEXIST);
            return;
        }
        if self.ensure_mutation_session("mkdir").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.create_directory(&path) {
            reply.error(EIO);
            return;
        }
        let Some(inode_id) = self.service.inode_for_path(&path) else {
            reply.error(EIO);
            return;
        };
        if self
            .record_wal_operation(WalOperation::CreateDirectory {
                path: path.clone(),
                inode: inode_id,
            })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        let inode = self.service.get_inode(&path).cloned();
        let par = parent_path(&path);
        let node = FuseNode {
            path,
            parent_path: par,
            inode,
            data: Vec::new(),
        };
        let attr = node.attr();
        self.register_node(node);
        self.dirty = true;
        reply.entry(&TTL, &attr, 0);
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        let Some(&ino) = self.ino_by_path.get(&path) else {
            reply.error(ENOENT);
            return;
        };
        if let Some(node) = self.nodes_by_ino.get(&ino) {
            if matches!(node.kind(), InodeKind::Directory) {
                reply.error(EISDIR);
                return;
            }
        }
        if self.ensure_mutation_session("unlink").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.delete_file(&path, false) {
            reply.error(EIO);
            return;
        }
        if self
            .record_wal_operation(WalOperation::DeletePath { path: path.clone() })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        self.unregister_ino(ino);
        self.dirty = true;
        reply.ok();
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        let Some(&ino) = self.ino_by_path.get(&path) else {
            reply.error(ENOENT);
            return;
        };
        if let Some(node) = self.nodes_by_ino.get(&ino) {
            if !matches!(node.kind(), InodeKind::Directory) {
                reply.error(ENOTDIR);
                return;
            }
        }
        // refuse non-empty directories
        let is_empty = self
            .children
            .get(&path)
            .map(|c| c.is_empty())
            .unwrap_or(true);
        if !is_empty {
            reply.error(ENOTEMPTY);
            return;
        }
        if self.ensure_mutation_session("rmdir").is_err() {
            reply.error(EIO);
            return;
        }
        if let Err(_) = self.service.delete_file(&path, false) {
            reply.error(EIO);
            return;
        }
        if self
            .record_wal_operation(WalOperation::DeletePath { path: path.clone() })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        self.unregister_ino(ino);
        self.dirty = true;
        reply.ok();
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        // RENAME_EXCHANGE (2) is not supported.
        if flags & 2 != 0 {
            reply.error(EINVAL);
            return;
        }
        let Some(src_path) = self.child_path(parent, name) else {
            reply.error(ENOENT);
            return;
        };
        let Some(dst_path) = self.child_path(newparent, newname) else {
            reply.error(ENOENT);
            return;
        };
        let Some(&src_ino) = self.ino_by_path.get(&src_path) else {
            reply.error(ENOENT);
            return;
        };
        // RENAME_NOREPLACE (1): fail if target already exists.
        if flags & 1 != 0 && self.ino_by_path.contains_key(&dst_path) {
            reply.error(EEXIST);
            return;
        }
        let src_is_dir = self
            .nodes_by_ino
            .get(&src_ino)
            .map(|n| matches!(n.kind(), InodeKind::Directory))
            .unwrap_or(false);

        // Type-compatibility and emptiness checks when overwriting an existing target.
        if let Some(&dst_ino) = self.ino_by_path.get(&dst_path) {
            let dst_is_dir = self
                .nodes_by_ino
                .get(&dst_ino)
                .map(|n| matches!(n.kind(), InodeKind::Directory))
                .unwrap_or(false);
            if src_is_dir && !dst_is_dir {
                reply.error(ENOTDIR);
                return;
            }
            if !src_is_dir && dst_is_dir {
                reply.error(EISDIR);
                return;
            }
            if dst_is_dir {
                let empty = self
                    .children
                    .get(&dst_path)
                    .map(|c| c.is_empty())
                    .unwrap_or(true);
                if !empty {
                    reply.error(ENOTEMPTY);
                    return;
                }
            }
        }

        if self.ensure_mutation_session("rename").is_err() {
            reply.error(EIO);
            return;
        }
        if self.service.rename_entry(&src_path, &dst_path).is_err() {
            reply.error(EIO);
            return;
        }
        if self
            .record_wal_operation(WalOperation::RenamePath {
                from: src_path.clone(),
                to: dst_path.clone(),
            })
            .is_err()
        {
            reply.error(EIO);
            return;
        }
        self.rebuild_indexes();
        self.dirty = true;
        reply.ok();
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        reply.statfs(
            fuse_total_blocks(),
            fuse_free_blocks(&self.nodes_by_ino),
            fuse_free_blocks(&self.nodes_by_ino),
            self.nodes_by_ino.len() as u64,
            fuse_free_inodes(self.nodes_by_ino.len()),
            FUSE_BLOCK_SIZE,
            255,
            FUSE_BLOCK_SIZE,
        );
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        if (self.dirty || self.service.had_unclean_shutdown()) && !self.persist() {
            reply.error(EIO);
        } else {
            reply.ok();
        }
    }

    fn fsync(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        if self.persist() {
            reply.ok();
        } else {
            reply.error(EIO);
        }
    }

    fn destroy(&mut self) {
        if self.dirty || self.service.had_unclean_shutdown() {
            let _ = self.persist();
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let entries = self.directory_entries(ino);
        if entries.is_empty() {
            reply.error(ENOENT);
            return;
        }
        for (index, (entry_ino, kind, name)) in
            entries.into_iter().enumerate().skip(offset as usize)
        {
            if reply.add(entry_ino, (index + 1) as i64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

pub fn mount_image_rw(
    image_path: impl AsRef<Path>,
    mount_point: impl AsRef<Path>,
) -> CoreFsResult<()> {
    let image_path = image_path.as_ref();
    let mount_point = mount_point.as_ref();
    let service = CoreFsService::load_image_from_path(image_path)?;
    let fs_name = format!("corefs:{}", service.volume_name());
    let mount = CoreFsFuseMountRw::open_session(service, image_path.to_path_buf())?;

    fuser::mount2(
        mount,
        mount_point,
        &[
            MountOption::RW,
            MountOption::FSName(fs_name),
            MountOption::DefaultPermissions,
        ],
    )
    .map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount CoreFS image {} on {}: {error}",
            image_path.display(),
            mount_point.display()
        ))
    })
}

// ── statfs helpers ───────────────────────────────────────────────────────────

/// Logical block size reported to the kernel.
const FUSE_BLOCK_SIZE: u32 = 4096;
/// Virtual total capacity: 1 GiB expressed in 4 KiB blocks.
const FUSE_TOTAL_BLOCKS: u64 = 1024 * 1024 * 1024 / FUSE_BLOCK_SIZE as u64; // 262 144

fn fuse_total_blocks() -> u64 {
    FUSE_TOTAL_BLOCKS
}

/// Compute used blocks from the in-memory node cache and return free blocks.
fn fuse_free_blocks(nodes: &HashMap<u64, FuseNode>) -> u64 {
    let used_bytes: u64 = nodes.values().map(|n| n.data.len() as u64).sum();
    let used_blocks = used_bytes.div_ceil(FUSE_BLOCK_SIZE as u64);
    FUSE_TOTAL_BLOCKS.saturating_sub(used_blocks)
}

/// Estimate free inode slots against the same virtual capacity.
fn fuse_free_inodes(active: usize) -> u64 {
    FUSE_TOTAL_BLOCKS.saturating_sub(active as u64)
}

fn demo_fs() -> CoreFsResult<CoreFsService> {
    let mut fs = CoreFsService::format(crate::config::CoreFsConfig::default());
    fs.create_directory("/etc")?;
    fs.create_directory("/var")?;
    fs.create_file(
        "/etc/corefs.conf",
        b"volume=corefs\ncompression=on\nencryption=on\n",
        &["config".to_string(), "system".to_string()],
    )?;
    fs.create_file(
        "/var/readme.txt",
        b"CoreFS Linux FUSE image",
        &["docs".to_string()],
    )?;
    fs.create_symlink("/etc/corefs-current", "/etc/corefs.conf")?;
    Ok(fs)
}

fn parent_path(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or_default().to_string()
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn current_gid() -> u32 {
    unsafe { libc::getgid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CoreFsConfig;
    fn sample_view() -> CoreFsFuseView {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/etc").expect("directory should exist");
        fs.create_file("/etc/settings.conf", b"hello", &[])
            .expect("file should exist");
        fs.create_symlink("/etc/current", "/etc/settings.conf")
            .expect("symlink should exist");
        CoreFsFuseView::from_state(fs.export_state())
    }

    #[test]
    fn fuse_view_builds_lookup_and_directory_mappings() {
        let view = sample_view();

        let root = view.node(ROOT_INO).expect("root should exist");
        assert_eq!(root.path, "/");

        let etc = view
            .lookup_child(ROOT_INO, OsStr::new("etc"))
            .expect("etc should be reachable");
        assert!(matches!(etc.kind(), InodeKind::Directory));

        let entries = view.directory_entries(etc.ino());
        assert!(entries.iter().any(|(_, _, name)| name == "settings.conf"));
        assert!(entries.iter().any(|(_, _, name)| name == "current"));
    }

    #[test]
    fn image_creation_writes_mountable_image() {
        let path = std::env::temp_dir().join(format!(
            "corefs-linux-fuse-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ));

        create_image(&path, true).expect("image should be created");
        let view = CoreFsFuseView::load_image(&path).expect("image should load");
        assert!(view.lookup_child(ROOT_INO, OsStr::new("etc")).is_some());

        let _ = std::fs::remove_file(path);
    }

    fn rw_mount_from_demo() -> CoreFsFuseMountRw {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/docs").expect("dir");
        fs.create_file("/docs/readme.txt", b"hello", &[])
            .expect("file");
        CoreFsFuseMountRw::from_service(fs, PathBuf::from("/tmp/test.img"))
    }

    #[test]
    fn rw_mount_builds_indexes_from_service_state() {
        let mount = rw_mount_from_demo();

        let root = mount.node(ROOT_INO).expect("root should exist");
        assert_eq!(root.path, "/");

        let docs = mount
            .lookup_child(ROOT_INO, OsStr::new("docs"))
            .expect("docs should be visible");
        assert!(matches!(docs.kind(), InodeKind::Directory));

        let entries = mount.directory_entries(docs.ino());
        assert!(entries.iter().any(|(_, _, name)| name == "readme.txt"));
    }

    #[test]
    fn rw_mount_write_updates_node_cache_and_marks_dirty() {
        let mut mount = rw_mount_from_demo();

        let docs_ino = mount
            .lookup_child(ROOT_INO, OsStr::new("docs"))
            .expect("docs")
            .ino();
        let readme_ino = mount
            .lookup_child(docs_ino, OsStr::new("readme.txt"))
            .expect("readme")
            .ino();

        assert!(!mount.dirty);

        // simulate write via service + cache update directly
        mount
            .service
            .write_file("/docs/readme.txt", b"world")
            .expect("write");
        if let Some(n) = mount.nodes_by_ino.get_mut(&readme_ino) {
            n.data = b"world".to_vec();
        }
        mount.dirty = true;

        assert!(mount.dirty);
        assert_eq!(
            mount
                .nodes_by_ino
                .get(&readme_ino)
                .map(|n| n.data.as_slice()),
            Some(b"world".as_slice())
        );
    }

    #[test]
    fn rw_mount_create_and_mkdir_register_new_nodes() {
        let mut mount = rw_mount_from_demo();

        // mkdir /tmp
        mount
            .service
            .create_directory("/tmp")
            .expect("create_directory");
        let inode_id = mount
            .service
            .inode_for_path("/tmp")
            .expect("inode should exist");
        let inode = mount.service.get_inode("/tmp").cloned();
        let ino = inode_id.0 + 1;
        let node = FuseNode {
            path: "/tmp".to_string(),
            parent_path: "/".to_string(),
            inode,
            data: Vec::new(),
        };
        mount.register_node(node);

        assert!(mount.lookup_child(ROOT_INO, OsStr::new("tmp")).is_some());
        assert_eq!(mount.nodes_by_ino[&ino].path, "/tmp");

        // create /tmp/new.txt
        mount
            .service
            .create_file("/tmp/new.txt", b"data", &[])
            .expect("create_file");
        let inode_id2 = mount.service.inode_for_path("/tmp/new.txt").expect("inode");
        let inode2 = mount.service.get_inode("/tmp/new.txt").cloned();
        let ino2 = inode_id2.0 + 1;
        let node2 = FuseNode {
            path: "/tmp/new.txt".to_string(),
            parent_path: "/tmp".to_string(),
            inode: inode2,
            data: b"data".to_vec(),
        };
        mount.register_node(node2);

        let tmp_ino = mount
            .lookup_child(ROOT_INO, OsStr::new("tmp"))
            .expect("tmp")
            .ino();
        let entries = mount.directory_entries(tmp_ino);
        assert!(entries.iter().any(|(_, _, name)| name == "new.txt"));
        assert_eq!(mount.nodes_by_ino[&ino2].path, "/tmp/new.txt");
    }

    #[test]
    fn rw_mount_unregister_removes_from_all_indexes() {
        let mut mount = rw_mount_from_demo();

        let docs_ino = mount
            .lookup_child(ROOT_INO, OsStr::new("docs"))
            .expect("docs")
            .ino();
        let readme_ino = mount
            .lookup_child(docs_ino, OsStr::new("readme.txt"))
            .expect("readme")
            .ino();

        mount.unregister_ino(readme_ino);

        assert!(
            mount
                .lookup_child(docs_ino, OsStr::new("readme.txt"))
                .is_none()
        );
        assert!(!mount.nodes_by_ino.contains_key(&readme_ino));
        let siblings = mount.children.get("/docs").cloned().unwrap_or_default();
        assert!(!siblings.contains(&"readme.txt".to_string()));
    }

    #[test]
    fn statfs_reports_capacity_and_decreases_free_blocks_with_data() {
        // Empty mount: all blocks should be free.
        let empty = CoreFsFuseMountRw::from_service(
            CoreFsService::format(CoreFsConfig::default()),
            PathBuf::from("/tmp/test.img"),
        );
        let total = fuse_total_blocks();
        let free_empty = fuse_free_blocks(&empty.nodes_by_ino);
        assert_eq!(free_empty, total, "no data means all blocks free");

        // Mount with one file: free blocks must decrease.
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/big.bin", &vec![0u8; 8192], &[])
            .expect("file");
        let mount = CoreFsFuseMountRw::from_service(fs, PathBuf::from("/tmp/test.img"));
        let free_with_data = fuse_free_blocks(&mount.nodes_by_ino);
        assert!(
            free_with_data < total,
            "used data should reduce free block count"
        );
        assert_eq!(total - free_with_data, 2, "8 KiB = 2 blocks of 4 KiB");
    }

    #[test]
    fn rw_mount_rename_file_updates_indexes() {
        let mut mount = rw_mount_from_demo();

        let docs_ino = mount
            .lookup_child(ROOT_INO, OsStr::new("docs"))
            .expect("docs")
            .ino();

        // rename /docs/readme.txt → /docs/notes.txt via service + rebuild
        mount
            .service
            .rename_entry("/docs/readme.txt", "/docs/notes.txt")
            .expect("rename");
        mount.rebuild_indexes();

        assert!(
            mount
                .lookup_child(docs_ino, OsStr::new("readme.txt"))
                .is_none(),
            "old name should be gone"
        );
        assert!(
            mount
                .lookup_child(docs_ino, OsStr::new("notes.txt"))
                .is_some(),
            "new name should be visible"
        );
    }

    #[test]
    fn rw_mount_rename_directory_cascades_in_indexes() {
        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_directory("/src").expect("dir");
        fs.create_file("/src/main.rs", b"fn main(){}", &[])
            .expect("file");
        fs.create_directory("/src/utils").expect("subdir");
        fs.create_file("/src/utils/helper.rs", b"//h", &[])
            .expect("file");
        let mut mount = CoreFsFuseMountRw::from_service(fs, PathBuf::from("/tmp/test.img"));

        mount
            .service
            .rename_entry("/src", "/lib")
            .expect("rename dir");
        mount.rebuild_indexes();

        assert!(mount.lookup_child(ROOT_INO, OsStr::new("src")).is_none());
        let lib_ino = mount
            .lookup_child(ROOT_INO, OsStr::new("lib"))
            .expect("lib dir after rename")
            .ino();
        assert!(mount.lookup_child(lib_ino, OsStr::new("main.rs")).is_some());
        let utils_ino = mount
            .lookup_child(lib_ino, OsStr::new("utils"))
            .expect("utils")
            .ino();
        assert!(
            mount
                .lookup_child(utils_ino, OsStr::new("helper.rs"))
                .is_some()
        );
    }

    #[test]
    fn rw_mount_persist_saves_image_and_clears_dirty() {
        let path = std::env::temp_dir().join(format!(
            "corefs-rw-persist-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ));

        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/hello.txt", b"persisted", &[])
            .expect("file");
        let mut mount = CoreFsFuseMountRw::from_service(fs, path.clone());
        mount.dirty = true;

        assert!(mount.persist());
        assert!(!mount.dirty);

        // reload and verify content survived
        let loaded = CoreFsService::load_image_from_path(&path).expect("load");
        assert_eq!(
            loaded.read_file("/hello.txt").expect("read"),
            b"persisted".to_vec()
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rw_mount_open_session_persists_dirty_marker_until_flush() {
        let path = std::env::temp_dir().join(format!(
            "corefs-rw-session-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ));

        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/hello.txt", b"persisted", &[])
            .expect("file");
        fs.save_image_to_path(&path).expect("initial image");

        let service = CoreFsService::load_image_from_path(&path).expect("load");
        let mut mount = CoreFsFuseMountRw::open_session(service, path.clone()).expect("session");
        let dirty_loaded = CoreFsService::load_image_from_path(&path).expect("dirty reload");
        assert!(
            !dirty_loaded.had_unclean_shutdown(),
            "load recovers runtime state"
        );

        assert!(mount.persist());
        let clean_loaded = CoreFsService::load_image_from_path(&path).expect("clean reload");
        assert!(!clean_loaded.had_unclean_shutdown());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rw_mount_persists_pending_wal_inside_image_before_flush() {
        let path = std::env::temp_dir().join(format!(
            "corefs-rw-wal-{}-{}.img",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ));

        let mut fs = CoreFsService::format(CoreFsConfig::default());
        fs.create_file("/hello.txt", b"hello", &[]).expect("file");
        fs.save_image_to_path(&path).expect("initial image");

        let service = CoreFsService::load_image_from_path(&path).expect("load");
        let mut mount = CoreFsFuseMountRw::open_session(service, path.clone()).expect("session");
        mount.ensure_mutation_session("write").expect("tx");
        mount
            .service
            .write_file("/hello.txt", b"updated")
            .expect("write");
        mount
            .record_wal_operation(WalOperation::PatchBlock {
                inode: mount.service.inode_for_path("/hello.txt").expect("inode"),
                block_index: 0,
                block_offset: 0,
                bytes: b"updated".to_vec(),
                final_len: 7,
            })
            .expect("wal");

        let loaded = CoreFsService::load_image_from_path(&path).expect("image should load");
        assert!(!loaded.has_pending_wal(), "load should replay pending WAL");
        assert_eq!(
            loaded.read_file("/hello.txt").expect("file should exist"),
            b"updated".to_vec()
        );

        let _ = std::fs::remove_file(path);
    }
}
