use super::*;
use crate::storage::volume_wal::{VolumeWal, WalOperation};

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "corefs-session-{name}-{}-{}.img",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ))
}

#[test]
fn format_mutate_and_reopen_round_trip() {
    let path = temp_path("roundtrip");
    let mut session =
        VolumeSession::format_new(&path, CoreFsConfig::default()).expect("format succeeds");
    session
        .mutate(|fs| {
            fs.create_directory("/data")?;
            fs.create_file("/data/file.txt", b"hello", &[])?;
            Ok(())
        })
        .expect("mutation succeeds");

    let reopened = VolumeSession::open(&path).expect("reopen succeeds");
    assert_eq!(
        reopened
            .service()
            .read_file("/data/file.txt")
            .expect("file exists"),
        b"hello".to_vec()
    );

    let _ = fs::remove_file(path);
}

#[test]
fn reopen_recovers_pending_wal_before_loading_service() {
    let path = temp_path("wal-recover");
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.set_pending_wal(VolumeWal {
        transaction_id: 1,
        label: "rw-writeback".to_string(),
        created_at: SystemTime::now(),
        operations: vec![
            WalOperation::CreateDirectory {
                path: "/data".to_string(),
                inode: crate::domain::inode::InodeId(1),
            },
            WalOperation::CreateFile {
                path: "/data/file.txt".to_string(),
                inode: crate::domain::inode::InodeId(2),
            },
            WalOperation::PatchExtent {
                inode: crate::domain::inode::InodeId(2),
                device_block: 0,
                block_offset: 0,
                inode_offset: 0,
                bytes: b"hello".to_vec(),
                final_len: 5,
            },
        ],
    });
    fs.mark_unclean_shutdown();
    fs.save_image_to_path(&path).expect("image should save");

    let reopened = VolumeSession::open(&path).expect("reopen succeeds");
    assert_eq!(
        reopened
            .service()
            .read_file("/data/file.txt")
            .expect("file exists"),
        b"hello".to_vec()
    );
    assert!(!reopened.service().has_pending_wal());

    let _ = fs::remove_file(path);
}

// -----------------------------------------------------------------------
// DeviceVolumeSession tests
// -----------------------------------------------------------------------

use crate::storage::block_device::MemoryDevice;

fn memory_device() -> Box<dyn crate::storage::block_device::BlockDevice> {
    // 2 MiB device with 4 KiB sectors — plenty for test volumes.
    Box::new(MemoryDevice::new(2 * 1024 * 1024, 4096).unwrap())
}

#[test]
fn device_session_format_and_read_back() {
    let dev = memory_device();
    let mut session =
        DeviceVolumeSession::format_new(dev, CoreFsConfig::default()).expect("format");
    session
        .mutate(|svc| {
            svc.create_directory("/data")?;
            svc.create_file("/data/hello.txt", b"world", &[])?;
            Ok(())
        })
        .expect("mutate");

    // Re-open from the same device
    let dev2 = {
        let mem = session.device.as_ref() as *const dyn crate::storage::block_device::BlockDevice;
        // Clone the underlying MemoryDevice data for re-open.
        let mem_ref = unsafe { &*mem };
        let data = mem_ref.read_at(0, mem_ref.capacity()).unwrap();
        Box::new(MemoryDevice::from_bytes(data, 4096).unwrap())
            as Box<dyn crate::storage::block_device::BlockDevice>
    };

    let reopened = DeviceVolumeSession::open(dev2).expect("reopen");
    assert_eq!(
        reopened.service().read_file("/data/hello.txt").unwrap(),
        b"world".to_vec()
    );
}

#[test]
fn device_session_format_creates_valid_volume() {
    let dev = memory_device();
    let session =
        DeviceVolumeSession::format_new(dev, CoreFsConfig::default()).expect("format");
    assert_eq!(session.service().volume_name(), "corefs");
    assert!(session.service().list_paths().is_empty()); // empty volume, no files yet
}
