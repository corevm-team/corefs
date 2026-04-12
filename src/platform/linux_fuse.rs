use crate::app::{CoreFsService, PersistedState};
use crate::domain::inode::{Inode, InodeId, InodeKind};
use crate::error::{CoreFsError, CoreFsResult};
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request,
};
use libc::{EIO, ENOENT, EROFS};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(1);
const ROOT_INO: u64 = 1;

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
}
