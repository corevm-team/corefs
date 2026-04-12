use crate::app::DirectoryEntry;
use crate::config::CoreFsConfig;
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::runtime::{MountAdapter, PlatformAdapterDescriptor};
use crate::storage::volume_session::VolumeSession;
use fuser::{
    BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags,
    Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request,
    SessionACL, TimeOrNow, WriteFlags,
};
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(1);
const ROOT_INODE: u64 = 1;

#[derive(Debug, Clone)]
pub struct LinuxMountOptions {
    pub create_if_missing: bool,
    pub read_only: bool,
    pub auto_unmount: bool,
    pub threads: usize,
}

impl Default for LinuxMountOptions {
    fn default() -> Self {
        Self {
            create_if_missing: false,
            read_only: false,
            auto_unmount: false,
            threads: 4,
        }
    }
}

#[derive(Debug)]
pub struct CoreFsFuseFilesystem {
    session: Mutex<VolumeSession>,
}

impl CoreFsFuseFilesystem {
    pub fn open(image_path: impl AsRef<Path>, create_if_missing: bool) -> CoreFsResult<Self> {
        let session = if create_if_missing {
            VolumeSession::open_or_format(image_path, CoreFsConfig::default())?
        } else {
            VolumeSession::open(image_path)?
        };
        Ok(Self {
            session: Mutex::new(session),
        })
    }

    fn inode_to_fuse(inode: InodeId) -> INodeNo {
        INodeNo(inode.0 + 1)
    }

    fn fuse_to_inode(ino: INodeNo) -> Option<InodeId> {
        match u64::from(ino) {
            ROOT_INODE => None,
            value => Some(InodeId(value - 1)),
        }
    }

    fn root_attr() -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino: INodeNo(ROOT_INODE),
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: current_uid(),
            gid: current_gid(),
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn file_attr(inode: &Inode) -> FileAttr {
        FileAttr {
            ino: Self::inode_to_fuse(inode.id),
            size: inode.size as u64,
            blocks: inode.size.div_ceil(512) as u64,
            atime: inode.modified_at,
            mtime: inode.modified_at,
            ctime: inode.modified_at,
            crtime: inode.created_at,
            kind: file_type_for_inode(inode.kind),
            perm: permissions_for_inode(inode.kind),
            nlink: if inode.kind == InodeKind::Directory {
                2
            } else {
                1
            },
            uid: current_uid(),
            gid: current_gid(),
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn with_session<T>(&self, operation: impl FnOnce(&VolumeSession) -> T) -> T {
        let guard = self.session.lock().expect("volume session mutex poisoned");
        operation(&guard)
    }

    fn with_session_mut<T>(
        &self,
        operation: impl FnOnce(&mut VolumeSession) -> CoreFsResult<T>,
    ) -> Result<T, Errno> {
        let mut guard = self.session.lock().expect("volume session mutex poisoned");
        operation(&mut guard).map_err(errno_for_error)
    }

    fn path_for_parent_name(&self, parent: INodeNo, name: &OsStr) -> Result<String, Errno> {
        let name = name.to_str().ok_or(Errno::EINVAL)?;
        let parent_path = self.path_for_ino(parent)?;
        Ok(join_child_path(&parent_path, name))
    }

    fn path_for_ino(&self, ino: INodeNo) -> Result<String, Errno> {
        if u64::from(ino) == ROOT_INODE {
            return Ok("/".to_string());
        }

        self.with_session(|session| {
            Self::fuse_to_inode(ino)
                .and_then(|inode| session.service().path_for_inode(inode))
                .ok_or(Errno::ENOENT)
        })
    }

    fn attr_for_ino(&self, ino: INodeNo) -> Result<FileAttr, Errno> {
        if u64::from(ino) == ROOT_INODE {
            return Ok(Self::root_attr());
        }

        self.with_session(|session| {
            Self::fuse_to_inode(ino)
                .and_then(|inode| session.service().get_inode_by_id(inode))
                .map(|inode| Self::file_attr(&inode))
                .ok_or(Errno::ENOENT)
        })
    }

    fn entry_reply(entry: Inode) -> (Duration, FileAttr, Generation) {
        (TTL, Self::file_attr(&entry), Generation(0))
    }
}

#[derive(Debug, Default)]
pub struct LinuxFuseMountAdapter;

impl MountAdapter for LinuxFuseMountAdapter {
    fn descriptor(&self) -> PlatformAdapterDescriptor {
        PlatformAdapterDescriptor {
            name: "linux-fuse".to_string(),
            runtime: "userspace".to_string(),
            persistent_volume: true,
        }
    }
}

impl Filesystem for CoreFsFuseFilesystem {
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr_for_ino(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(errno) => reply.error(errno),
        }
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let path = match self.path_for_parent_name(parent, name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let result = self.with_session(|session| session.service().get_inode(&path));
        match result {
            Some(inode) => {
                let (ttl, attr, generation) = Self::entry_reply(inode);
                reply.entry(&ttl, &attr, generation);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let path = match self.path_for_ino(ino) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let target = self.with_session(|session| session.service().read_symlink(&path));
        match target {
            Ok(target) => reply.data(target.as_bytes()),
            Err(error) => reply.error(errno_for_error(error)),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let path = match self.path_for_parent_name(parent, name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        match self.with_session_mut(|session| {
            session.mutate(|fs| {
                fs.create_directory(&path)?;
                fs.get_inode(&path).ok_or_else(|| {
                    CoreFsError::State(format!("directory not found after create: {path}"))
                })
            })
        }) {
            Ok(inode) => {
                let (ttl, attr, generation) = Self::entry_reply(inode);
                reply.entry(&ttl, &attr, generation);
            }
            Err(errno) => reply.error(errno),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let path = match self.path_for_parent_name(parent, name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        match self.with_session_mut(|session| session.mutate(|fs| fs.delete_file(&path, false))) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let path = match self.path_for_parent_name(parent, name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        match self.with_session_mut(|session| session.mutate(|fs| fs.remove_directory(&path))) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn symlink(
        &self,
        _req: &Request,
        parent: INodeNo,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let path = match self.path_for_parent_name(parent, link_name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };
        let target = match target.to_str() {
            Some(target) => target.to_string(),
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
        };

        match self.with_session_mut(|session| {
            session.mutate(|fs| {
                fs.create_symlink(&path, &target)?;
                fs.get_inode(&path).ok_or_else(|| {
                    CoreFsError::State(format!("symlink not found after create: {path}"))
                })
            })
        }) {
            Ok(inode) => {
                let (ttl, attr, generation) = Self::entry_reply(inode);
                reply.entry(&ttl, &attr, generation);
            }
            Err(errno) => reply.error(errno),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let old_path = match self.path_for_parent_name(parent, name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };
        let new_path = match self.path_for_parent_name(newparent, newname) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        match self
            .with_session_mut(|session| session.mutate(|fs| fs.rename_path(&old_path, &new_path)))
        {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let path = match self.path_for_ino(ino) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let result = self.with_session(|session| {
            session
                .service()
                .read_file_range(&path, offset as usize, size as usize)
        });
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(errno_for_error(error)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let path = match self.path_for_ino(ino) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        match self.with_session_mut(|session| {
            session.mutate(|fs| fs.write_file_range(&path, offset as usize, data))
        }) {
            Ok(written) => reply.written(written as u32),
            Err(errno) => reply.error(errno),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let path = match self.path_for_ino(ino) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        if let Some(size) = size {
            match self.with_session_mut(|session| {
                session.mutate(|fs| {
                    fs.truncate_file(&path, size as usize)?;
                    fs.get_inode(&path).ok_or_else(|| {
                        CoreFsError::State(format!("path not found after truncate: {path}"))
                    })
                })
            }) {
                Ok(inode) => {
                    let attr = Self::file_attr(&inode);
                    reply.attr(&TTL, &attr);
                }
                Err(errno) => reply.error(errno),
            }
            return;
        }

        match self.attr_for_ino(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(errno) => reply.error(errno),
        }
    }

    fn opendir(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let path = match self.path_for_ino(ino) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        let entries = self.with_session(|session| session.service().list_directory(&path));
        let entries = match entries {
            Ok(entries) => entries,
            Err(error) => {
                reply.error(errno_for_error(error));
                return;
            }
        };

        let parent_path = parent_path(&path);
        let parent_ino = if path == "/" {
            INodeNo(ROOT_INODE)
        } else {
            self.with_session(|session| {
                session
                    .service()
                    .inode_for_path(parent_path)
                    .map(Self::inode_to_fuse)
                    .unwrap_or(INodeNo(ROOT_INODE))
            })
        };

        let mut fused_entries = vec![
            (INodeNo(ROOT_INODE), FileType::Directory, ".".to_string()),
            (parent_ino, FileType::Directory, "..".to_string()),
        ];
        fused_entries.extend(entries.into_iter().map(map_directory_entry));

        for (index, (entry_ino, kind, name)) in
            fused_entries.into_iter().enumerate().skip(offset as usize)
        {
            if reply.add(entry_ino, (index + 1) as u64, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.with_session_mut(|session| session.flush()) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn fsyncdir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.with_session_mut(|session| session.flush()) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        self.with_session(|session| {
            let stats = session.service().stats();
            let block_size = 4096;
            let total_blocks = 1_048_576;
            let used_blocks = stats.files.max(1) as u64;
            let free_blocks = total_blocks - used_blocks.min(total_blocks);

            reply.statfs(
                total_blocks,
                free_blocks,
                free_blocks,
                stats.files as u64 + 1,
                1_000_000,
                block_size,
                255,
                block_size,
            );
        });
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let path = match self.path_for_parent_name(parent, name) {
            Ok(path) => path,
            Err(errno) => {
                reply.error(errno);
                return;
            }
        };

        match self.with_session_mut(|session| {
            session.mutate(|fs| {
                fs.create_file(&path, &[], &[])?;
                fs.get_inode(&path).ok_or_else(|| {
                    CoreFsError::State(format!("file not found after create: {path}"))
                })
            })
        }) {
            Ok(inode) => {
                let attr = Self::file_attr(&inode);
                reply.created(
                    &TTL,
                    &attr,
                    Generation(0),
                    FileHandle(0),
                    FopenFlags::empty(),
                );
            }
            Err(errno) => reply.error(errno),
        }
    }
}

pub fn mount_volume(
    image_path: impl AsRef<Path>,
    mountpoint: impl AsRef<Path>,
    options: LinuxMountOptions,
) -> CoreFsResult<()> {
    let filesystem = CoreFsFuseFilesystem::open(image_path, options.create_if_missing)?;
    let mut config = Config::default();
    config
        .mount_options
        .push(MountOption::FSName("corefs".to_string()));
    config
        .mount_options
        .push(MountOption::Subtype("corefs".to_string()));
    config.mount_options.push(if options.read_only {
        MountOption::RO
    } else {
        MountOption::RW
    });
    config.mount_options.push(MountOption::DefaultPermissions);
    if options.auto_unmount {
        config.mount_options.push(MountOption::AutoUnmount);
        config.acl = SessionACL::RootAndOwner;
    }
    config.n_threads = Some(options.threads.max(1));
    config.clone_fd = true;

    fuser::mount2(filesystem, mountpoint, &config).map_err(|error| {
        CoreFsError::State(format!(
            "failed to mount CoreFS volume through FUSE: {error}"
        ))
    })
}

fn file_type_for_inode(kind: InodeKind) -> FileType {
    match kind {
        InodeKind::File => FileType::RegularFile,
        InodeKind::Directory => FileType::Directory,
        InodeKind::Symlink => FileType::Symlink,
    }
}

fn permissions_for_inode(kind: InodeKind) -> u16 {
    match kind {
        InodeKind::File => 0o644,
        InodeKind::Directory => 0o755,
        InodeKind::Symlink => 0o777,
    }
}

fn join_child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn parent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

fn map_directory_entry(entry: DirectoryEntry) -> (INodeNo, FileType, String) {
    (
        CoreFsFuseFilesystem::inode_to_fuse(entry.inode),
        file_type_for_inode(entry.kind),
        entry.name,
    )
}

fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn current_gid() -> u32 {
    unsafe { libc::getegid() }
}

fn errno_for_error(error: CoreFsError) -> Errno {
    match error {
        CoreFsError::AlreadyExists(_) => Errno::EEXIST,
        CoreFsError::InvalidCommand(_) | CoreFsError::InvalidInput(_) => Errno::EINVAL,
        CoreFsError::NotFound(_) => Errno::ENOENT,
        CoreFsError::PolicyViolation(message) => {
            if message.contains("not empty") {
                Errno::ENOTEMPTY
            } else {
                Errno::ENOTDIR
            }
        }
        CoreFsError::State(_) => Errno::EIO,
    }
}
