use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

const SECTOR: u32 = 4096;
const FOUR_SECTORS: u64 = 4 * 4096;

fn unique_path(label: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "corefs-blkdev-{label}-{}-{ts}.img",
        std::process::id()
    ))
}

// -----------------------------------------------------------------------
// Alignment & bounds helpers
// -----------------------------------------------------------------------

#[test]
fn check_alignment_accepts_aligned_values() {
    assert!(check_alignment(0, 0, 512).is_ok());
    assert!(check_alignment(512, 1024, 512).is_ok());
    assert!(check_alignment(4096, 4096, 4096).is_ok());
}

#[test]
fn check_alignment_rejects_misaligned_offset() {
    let err = check_alignment(1, 512, 512).unwrap_err();
    assert!(err.to_string().contains("offset 1"));
}

#[test]
fn check_alignment_rejects_misaligned_length() {
    let err = check_alignment(512, 100, 512).unwrap_err();
    assert!(err.to_string().contains("length 100"));
}

#[test]
fn check_bounds_accepts_exact_capacity() {
    assert!(check_bounds(0, 4096, 4096).is_ok());
}

#[test]
fn check_bounds_rejects_past_end() {
    let err = check_bounds(4096, 1, 4096).unwrap_err();
    assert!(err.to_string().contains("past end"));
}

#[test]
fn check_bounds_rejects_overflow() {
    let err = check_bounds(u64::MAX, 1, u64::MAX).unwrap_err();
    assert!(err.to_string().contains("overflows"));
}

// -----------------------------------------------------------------------
// DeviceGeometry
// -----------------------------------------------------------------------

#[test]
fn device_geometry_stores_correct_values() {
    let g = DeviceGeometry {
        sector_size: 512,
        sector_count: 2048,
        capacity_bytes: 512 * 2048,
        read_only: false,
    };
    assert_eq!(g.capacity_bytes, 1_048_576);
    assert_eq!(g.sector_count, 2048);
    assert!(!g.read_only);
}

// -----------------------------------------------------------------------
// MemoryDevice — core operations
// -----------------------------------------------------------------------

#[test]
fn memory_device_creation_and_geometry() {
    let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    assert_eq!(dev.capacity(), FOUR_SECTORS);
    assert_eq!(dev.sector_size(), SECTOR);
    assert_eq!(dev.geometry().sector_count, 4);
    assert!(!dev.is_read_only());
}

#[test]
fn memory_device_rejects_zero_capacity() {
    let err = MemoryDevice::new(0, SECTOR).unwrap_err();
    assert!(err.to_string().contains("greater than zero"));
}

#[test]
fn memory_device_rejects_non_power_of_two_sector() {
    let err = MemoryDevice::new(4096, 3000).unwrap_err();
    assert!(err.to_string().contains("power of two"));
}

#[test]
fn memory_device_rejects_unaligned_capacity() {
    let err = MemoryDevice::new(5000, SECTOR).unwrap_err();
    assert!(err.to_string().contains("multiple of sector size"));
}

#[test]
fn memory_device_write_then_read() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let mut payload = vec![0u8; SECTOR as usize];
    payload[0..4].copy_from_slice(b"TEST");

    dev.write_at(SECTOR as u64, &payload).unwrap();

    let readback = dev.read_at(SECTOR as u64, SECTOR as u64).unwrap();
    assert_eq!(&readback[0..4], b"TEST");
}

#[test]
fn memory_device_write_multiple_sectors() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let payload = vec![0xABu8; 2 * SECTOR as usize];

    dev.write_at(0, &payload).unwrap();

    let readback = dev.read_at(0, 2 * SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0xAB));

    // Sectors 2–3 should still be zero
    let untouched = dev.read_at(2 * SECTOR as u64, 2 * SECTOR as u64).unwrap();
    assert!(untouched.iter().all(|&b| b == 0));
}

#[test]
fn memory_device_read_zero_length() {
    let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let result = dev.read_at(0, 0).unwrap();
    assert!(result.is_empty());
}

#[test]
fn memory_device_write_zero_length() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    dev.write_at(0, &[]).unwrap();
}

#[test]
fn memory_device_rejects_misaligned_read() {
    let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let err = dev.read_at(100, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

#[test]
fn memory_device_rejects_misaligned_write() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let payload = vec![0u8; 100];
    let err = dev.write_at(0, &payload).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

#[test]
fn memory_device_rejects_out_of_bounds_read() {
    let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let err = dev.read_at(FOUR_SECTORS, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

#[test]
fn memory_device_rejects_out_of_bounds_write() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let payload = vec![0u8; SECTOR as usize];
    let err = dev.write_at(FOUR_SECTORS, &payload).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

// -----------------------------------------------------------------------
// MemoryDevice — read-only enforcement
// -----------------------------------------------------------------------

#[test]
fn memory_device_read_only_blocks_writes() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    dev.set_read_only(true);

    let payload = vec![0u8; SECTOR as usize];
    let err = dev.write_at(0, &payload).unwrap_err();
    assert!(matches!(err, CoreFsError::PolicyViolation(_)));
    assert!(err.to_string().contains("read-only"));
}

#[test]
fn memory_device_read_only_allows_reads() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    dev.set_read_only(true);

    let result = dev.read_at(0, SECTOR as u64);
    assert!(result.is_ok());
}

// -----------------------------------------------------------------------
// MemoryDevice — TRIM
// -----------------------------------------------------------------------

#[test]
fn memory_device_trim_zeros_range() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let payload = vec![0xFFu8; SECTOR as usize];
    dev.write_at(0, &payload).unwrap();

    dev.trim(0, SECTOR as u64).unwrap();

    let readback = dev.read_at(0, SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0));
}

#[test]
fn memory_device_trim_records_ranges() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    dev.trim(0, SECTOR as u64).unwrap();
    dev.trim(2 * SECTOR as u64, SECTOR as u64).unwrap();

    assert_eq!(dev.trimmed_ranges().len(), 2);
    assert_eq!(dev.trimmed_ranges()[0], (0, SECTOR as u64));
    assert_eq!(dev.trimmed_ranges()[1], (2 * SECTOR as u64, SECTOR as u64));
}

#[test]
fn memory_device_trim_disabled_is_noop() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let payload = vec![0xFFu8; SECTOR as usize];
    dev.write_at(0, &payload).unwrap();
    dev.set_trim_supported(false);

    assert!(!dev.supports_trim());
    dev.trim(0, SECTOR as u64).unwrap();

    // Data should NOT be zeroed
    let readback = dev.read_at(0, SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0xFF));
    assert!(dev.trimmed_ranges().is_empty());
}

#[test]
fn memory_device_trim_rejects_misaligned() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let err = dev.trim(100, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

#[test]
fn memory_device_trim_rejects_out_of_bounds() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let err = dev.trim(FOUR_SECTORS, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

// -----------------------------------------------------------------------
// MemoryDevice — from_bytes
// -----------------------------------------------------------------------

#[test]
fn memory_device_from_bytes_preserves_content() {
    let mut original = vec![0u8; FOUR_SECTORS as usize];
    original[0..5].copy_from_slice(b"HELLO");

    let dev = MemoryDevice::from_bytes(original, SECTOR).unwrap();
    let readback = dev.read_at(0, SECTOR as u64).unwrap();
    assert_eq!(&readback[0..5], b"HELLO");
}

#[test]
fn memory_device_from_bytes_rejects_unaligned() {
    let data = vec![0u8; 100];
    let err = MemoryDevice::from_bytes(data, SECTOR).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));
}

// -----------------------------------------------------------------------
// MemoryDevice — sync
// -----------------------------------------------------------------------

#[test]
fn memory_device_sync_succeeds() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    assert!(dev.sync().is_ok());
}

// -----------------------------------------------------------------------
// MemoryDevice — data() accessor
// -----------------------------------------------------------------------

#[test]
fn memory_device_data_reflects_writes() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let mut payload = vec![0u8; SECTOR as usize];
    payload[0] = 42;
    dev.write_at(SECTOR as u64, &payload).unwrap();

    assert_eq!(dev.data()[SECTOR as usize], 42);
}

// -----------------------------------------------------------------------
// MemoryDevice — different sector sizes
// -----------------------------------------------------------------------

#[test]
fn memory_device_512_byte_sectors() {
    let mut dev = MemoryDevice::new(8 * 512, 512).unwrap();
    assert_eq!(dev.sector_size(), 512);
    assert_eq!(dev.geometry().sector_count, 8);

    let payload = vec![0xCDu8; 512];
    dev.write_at(512, &payload).unwrap();
    let readback = dev.read_at(512, 512).unwrap();
    assert!(readback.iter().all(|&b| b == 0xCD));
}

// -----------------------------------------------------------------------
// MemoryDevice — clear_trimmed_ranges
// -----------------------------------------------------------------------

#[test]
fn memory_device_clear_trimmed_ranges() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    dev.trim(0, SECTOR as u64).unwrap();
    assert_eq!(dev.trimmed_ranges().len(), 1);

    dev.clear_trimmed_ranges();
    assert!(dev.trimmed_ranges().is_empty());
}

// -----------------------------------------------------------------------
// FileImageDevice — core operations
// -----------------------------------------------------------------------

#[test]
fn file_image_create_and_geometry() {
    let path = unique_path("create-geom");
    let dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    assert_eq!(dev.capacity(), FOUR_SECTORS);
    assert_eq!(dev.sector_size(), SECTOR);
    assert_eq!(dev.geometry().sector_count, 4);
    assert!(!dev.is_read_only());

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_create_rejects_zero_capacity() {
    let path = unique_path("create-zero");
    let err = FileImageDevice::create(&path, 0, SECTOR).unwrap_err();
    assert!(err.to_string().contains("greater than zero"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_create_rejects_bad_sector_size() {
    let path = unique_path("create-bad-ss");
    let err = FileImageDevice::create(&path, 4096, 3000).unwrap_err();
    assert!(err.to_string().contains("power of two"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_create_rejects_unaligned_capacity() {
    let path = unique_path("create-unaligned");
    let err = FileImageDevice::create(&path, 5000, SECTOR).unwrap_err();
    assert!(err.to_string().contains("multiple of sector size"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_write_then_read() {
    let path = unique_path("write-read");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    let mut payload = vec![0u8; SECTOR as usize];
    payload[0..6].copy_from_slice(b"COREFS");
    dev.write_at(0, &payload).unwrap();

    let readback = dev.read_at(0, SECTOR as u64).unwrap();
    assert_eq!(&readback[0..6], b"COREFS");

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_write_multiple_sectors_and_read_back() {
    let path = unique_path("multi-sector");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    let payload = vec![0xABu8; 2 * SECTOR as usize];
    dev.write_at(SECTOR as u64, &payload).unwrap();

    let readback = dev.read_at(SECTOR as u64, 2 * SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0xAB));

    // First sector should still be zero
    let first = dev.read_at(0, SECTOR as u64).unwrap();
    assert!(first.iter().all(|&b| b == 0));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_persistence_across_reopen() {
    let path = unique_path("persist");
    {
        let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
        let mut payload = vec![0u8; SECTOR as usize];
        payload[0..7].copy_from_slice(b"persist");
        dev.write_at(0, &payload).unwrap();
        dev.sync().unwrap();
    }

    let dev2 = FileImageDevice::open(&path, true).unwrap();
    let readback = dev2.read_at(0, SECTOR as u64).unwrap();
    assert_eq!(&readback[0..7], b"persist");
    assert!(dev2.is_read_only());

    drop(dev2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_read_only_blocks_writes() {
    let path = unique_path("ro-write");
    {
        let _dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    }

    let mut dev = FileImageDevice::open(&path, true).unwrap();
    let payload = vec![0u8; SECTOR as usize];
    let err = dev.write_at(0, &payload).unwrap_err();
    assert!(matches!(err, CoreFsError::PolicyViolation(_)));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_rejects_misaligned_operations() {
    let path = unique_path("misaligned");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    let err = dev.read_at(100, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));

    let payload = vec![0u8; 100];
    let err = dev.write_at(0, &payload).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_rejects_out_of_bounds() {
    let path = unique_path("oob");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    let err = dev.read_at(FOUR_SECTORS, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));

    let payload = vec![0u8; SECTOR as usize];
    let err = dev.write_at(FOUR_SECTORS, &payload).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_trim_is_noop() {
    let path = unique_path("trim-noop");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    assert!(!dev.supports_trim());
    assert!(dev.trim(0, SECTOR as u64).is_ok());

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_sync_succeeds() {
    let path = unique_path("sync");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    assert!(dev.sync().is_ok());

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_read_zero_length() {
    let path = unique_path("read-zero");
    let dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    let result = dev.read_at(0, 0).unwrap();
    assert!(result.is_empty());

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_write_zero_length() {
    let path = unique_path("write-zero");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    dev.write_at(0, &[]).unwrap();

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

// -----------------------------------------------------------------------
// FileImageDevice — resize
// -----------------------------------------------------------------------

#[test]
fn file_image_resize_grow() {
    let path = unique_path("resize-grow");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    let new_cap = 8 * SECTOR as u64;
    dev.resize(new_cap).unwrap();
    assert_eq!(dev.capacity(), new_cap);
    assert_eq!(dev.geometry().sector_count, 8);

    // Can now write to the extended region
    let payload = vec![0xBBu8; SECTOR as usize];
    dev.write_at(FOUR_SECTORS, &payload).unwrap();
    let readback = dev.read_at(FOUR_SECTORS, SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0xBB));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_resize_shrink() {
    let path = unique_path("resize-shrink");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();

    let new_cap = 2 * SECTOR as u64;
    dev.resize(new_cap).unwrap();
    assert_eq!(dev.capacity(), new_cap);
    assert_eq!(dev.geometry().sector_count, 2);

    // Out of bounds now
    let err = dev.read_at(new_cap, SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_resize_rejects_unaligned() {
    let path = unique_path("resize-unaligned");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    let err = dev.resize(5000).unwrap_err();
    assert!(matches!(err, CoreFsError::InvalidInput(_)));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_image_resize_rejects_read_only() {
    let path = unique_path("resize-ro");
    {
        let _dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    }
    let mut dev = FileImageDevice::open(&path, true).unwrap();
    let err = dev.resize(8 * SECTOR as u64).unwrap_err();
    assert!(matches!(err, CoreFsError::PolicyViolation(_)));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

// -----------------------------------------------------------------------
// FileImageDevice — open non-existent
// -----------------------------------------------------------------------

#[test]
fn file_image_open_nonexistent_fails() {
    let err = FileImageDevice::open("/tmp/nonexistent-corefs-image.img", false).unwrap_err();
    assert!(matches!(err, CoreFsError::State(_)));
}

// -----------------------------------------------------------------------
// FileImageDevice — create refuses overwrite
// -----------------------------------------------------------------------

#[test]
fn file_image_create_refuses_existing_file() {
    let path = unique_path("create-exists");
    {
        let _dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    }
    let err = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap_err();
    assert!(matches!(err, CoreFsError::State(_)));

    let _ = std::fs::remove_file(&path);
}

// -----------------------------------------------------------------------
// FileImageDevice — 512-byte sector size
// -----------------------------------------------------------------------

#[test]
fn file_image_512_byte_sectors() {
    let path = unique_path("512-sectors");
    let mut dev = FileImageDevice::create(&path, 8 * 512, 512).unwrap();
    assert_eq!(dev.sector_size(), 512);
    assert_eq!(dev.geometry().sector_count, 8);

    let payload = vec![0xEFu8; 512];
    dev.write_at(512, &payload).unwrap();
    let readback = dev.read_at(512, 512).unwrap();
    assert!(readback.iter().all(|&b| b == 0xEF));

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

// -----------------------------------------------------------------------
// Trait object usage (dyn BlockDevice)
// -----------------------------------------------------------------------

#[test]
fn trait_object_dispatch_works() {
    let mut dev: Box<dyn BlockDevice> =
        Box::new(MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap());

    let mut payload = vec![0u8; SECTOR as usize];
    payload[0] = 0xFF;
    dev.write_at(0, &payload).unwrap();

    let readback = dev.read_at(0, SECTOR as u64).unwrap();
    assert_eq!(readback[0], 0xFF);

    assert_eq!(dev.capacity(), FOUR_SECTORS);
    assert_eq!(dev.sector_size(), SECTOR);
    assert!(!dev.is_read_only());
}

// -----------------------------------------------------------------------
// Full-device read/write round trip
// -----------------------------------------------------------------------

#[test]
fn full_device_round_trip_memory() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let data = (0..FOUR_SECTORS as usize)
        .map(|i| (i % 256) as u8)
        .collect::<Vec<_>>();

    dev.write_at(0, &data).unwrap();
    dev.sync().unwrap();

    let readback = dev.read_at(0, FOUR_SECTORS).unwrap();
    assert_eq!(readback, data);
}

#[test]
fn full_device_round_trip_file() {
    let path = unique_path("full-rt");
    let mut dev = FileImageDevice::create(&path, FOUR_SECTORS, SECTOR).unwrap();
    let data = (0..FOUR_SECTORS as usize)
        .map(|i| (i % 256) as u8)
        .collect::<Vec<_>>();

    dev.write_at(0, &data).unwrap();
    dev.sync().unwrap();

    let readback = dev.read_at(0, FOUR_SECTORS).unwrap();
    assert_eq!(readback, data);

    drop(dev);
    let _ = std::fs::remove_file(&path);
}

// -----------------------------------------------------------------------
// Sector-by-sector access pattern
// -----------------------------------------------------------------------

#[test]
fn sector_by_sector_write_and_verify() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let ss = SECTOR as u64;

    for i in 0..4u64 {
        let payload = vec![(i + 1) as u8; SECTOR as usize];
        dev.write_at(i * ss, &payload).unwrap();
    }

    for i in 0..4u64 {
        let readback = dev.read_at(i * ss, ss).unwrap();
        assert!(readback.iter().all(|&b| b == (i + 1) as u8));
    }
}

// -----------------------------------------------------------------------
// Overwrite semantics
// -----------------------------------------------------------------------

#[test]
fn overwrite_replaces_previous_data() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();

    let first = vec![0xAAu8; SECTOR as usize];
    dev.write_at(0, &first).unwrap();

    let second = vec![0xBBu8; SECTOR as usize];
    dev.write_at(0, &second).unwrap();

    let readback = dev.read_at(0, SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0xBB));
}

// -----------------------------------------------------------------------
// Edge: exact capacity boundary
// -----------------------------------------------------------------------

#[test]
fn write_at_exact_end_boundary() {
    let mut dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let payload = vec![0xCCu8; SECTOR as usize];
    // Write the very last sector
    dev.write_at(3 * SECTOR as u64, &payload).unwrap();
    let readback = dev.read_at(3 * SECTOR as u64, SECTOR as u64).unwrap();
    assert!(readback.iter().all(|&b| b == 0xCC));
}

#[test]
fn read_entire_device() {
    let dev = MemoryDevice::new(FOUR_SECTORS, SECTOR).unwrap();
    let result = dev.read_at(0, FOUR_SECTORS).unwrap();
    assert_eq!(result.len(), FOUR_SECTORS as usize);
}

// -----------------------------------------------------------------------
// Fake-stick detection helpers
// -----------------------------------------------------------------------

/// A MemoryDevice that rejects writes past a certain offset — simulates a
/// fake USB stick.
#[derive(Debug)]
struct FakeStickDevice {
    inner: MemoryDevice,
    writable_until: u64,
}

impl BlockDevice for FakeStickDevice {
    fn geometry(&self) -> &DeviceGeometry {
        self.inner.geometry()
    }
    fn read_at(&self, offset: u64, length: u64) -> CoreFsResult<Vec<u8>> {
        self.inner.read_at(offset, length)
    }
    fn write_at(&mut self, offset: u64, data: &[u8]) -> CoreFsResult<()> {
        if offset + data.len() as u64 > self.writable_until {
            return Err(CoreFsError::State("write protected".to_string()));
        }
        self.inner.write_at(offset, data)
    }
    fn sync(&mut self) -> CoreFsResult<()> {
        self.inner.sync()
    }
    fn trim(&mut self, offset: u64, length: u64) -> CoreFsResult<()> {
        self.inner.trim(offset, length)
    }
    fn supports_trim(&self) -> bool {
        false
    }
}

const ONE_MIB: u64 = 1024 * 1024;

#[test]
fn sanity_check_on_honest_device_passes() {
    let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
    let report = sanity_check_writable(&mut dev, 0).unwrap();
    assert!(report.is_honest());
    assert!(report.failed_offsets.is_empty());
    assert_eq!(report.estimated_usable_bytes, report.advertised_bytes);
    assert_eq!(report.fake_ratio_percent(), 0);
}

#[test]
fn sanity_check_detects_fake_stick() {
    // Advertises 16 MiB, actually only writable up to 2 MiB.
    let fake = FakeStickDevice {
        inner: MemoryDevice::new(16 * ONE_MIB, 4096).unwrap(),
        writable_until: 2 * ONE_MIB,
    };
    let mut dev = fake;
    let report = sanity_check_writable(&mut dev, 0).unwrap();
    assert!(!report.is_honest());
    assert!(!report.failed_offsets.is_empty());
    assert!(report.estimated_usable_bytes < report.advertised_bytes);
    assert!(report.fake_ratio_percent() > 50);
}

#[test]
fn sanity_check_zero_fills_probe_regions() {
    let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
    // Write known markers everywhere so we can detect that probe regions
    // were zeroed afterwards.
    let marker = vec![0xAB; (16 * ONE_MIB) as usize];
    dev.write_at(0, &marker).unwrap();

    let report = sanity_check_writable(&mut dev, 0).unwrap();
    assert!(report.is_honest());

    // Every probe region must be zero-filled after the check.
    for &offset in &report.probed_offsets {
        let read = dev.read_at(offset, 4096).unwrap();
        assert!(
            read.iter().all(|&b| b == 0),
            "probe at offset {offset} was not zero-filled"
        );
    }
}

#[test]
fn sanity_check_respects_skip_below() {
    let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
    // Write marker within the skip region.
    let marker = vec![0xCD; 4096];
    dev.write_at(0, &marker).unwrap();

    let report = sanity_check_writable(&mut dev, 1 * ONE_MIB).unwrap();
    assert!(report.is_honest());

    // Marker at offset 0 should be preserved (below skip_below=1 MiB).
    let read = dev.read_at(0, 4096).unwrap();
    assert_eq!(read, marker);
}

#[test]
fn verify_device_capacity_on_honest_device() {
    let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
    let report = verify_device_capacity(&mut dev, 64 * 1024, 10).unwrap();
    assert!(report.is_honest());
    assert_eq!(report.probed_offsets.len(), 10);
    assert_eq!(report.fake_ratio_percent(), 0);
}

#[test]
fn verify_device_capacity_detects_fake() {
    let fake = FakeStickDevice {
        inner: MemoryDevice::new(16 * ONE_MIB, 4096).unwrap(),
        writable_until: ONE_MIB,
    };
    let mut dev = fake;
    let report = verify_device_capacity(&mut dev, 64 * 1024, 20).unwrap();
    assert!(!report.is_honest());
    // Most probes should fail since writable region is tiny.
    assert!(report.failed_offsets.len() > 10);
    assert!(report.fake_ratio_percent() > 80);
}

#[test]
fn verify_device_capacity_rejects_zero_chunks() {
    let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
    let err = verify_device_capacity(&mut dev, 64 * 1024, 0).unwrap_err();
    assert!(err.to_string().contains("chunk_count"));
}

#[test]
fn verify_device_rejects_read_only() {
    let mut dev = MemoryDevice::new(16 * ONE_MIB, 4096).unwrap();
    dev.set_read_only(true);
    let err = verify_device_capacity(&mut dev, 64 * 1024, 10).unwrap_err();
    assert!(matches!(err, CoreFsError::PolicyViolation(_)));
}

#[test]
fn generate_test_pattern_is_offset_dependent() {
    let p1 = generate_test_pattern(0, 4096);
    let p2 = generate_test_pattern(4096, 4096);
    let p3 = generate_test_pattern(8192, 4096);
    assert_ne!(p1, p2);
    assert_ne!(p2, p3);
    assert_ne!(p1, p3);
    assert_eq!(p1.len(), 4096);
}

#[test]
fn generate_test_pattern_is_deterministic() {
    let p1 = generate_test_pattern(12345, 4096);
    let p2 = generate_test_pattern(12345, 4096);
    assert_eq!(p1, p2);
}

#[test]
fn device_verification_report_fake_ratio_calculation() {
    let report = DeviceVerificationReport {
        advertised_bytes: 1000,
        probed_offsets: vec![0, 500, 1000],
        failed_offsets: vec![500, 1000],
        highest_verified_offset: 100,
        estimated_usable_bytes: 100,
    };
    assert!(!report.is_honest());
    assert_eq!(report.fake_ratio_percent(), 90);
}
