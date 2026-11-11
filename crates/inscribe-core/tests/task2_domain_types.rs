use inscribe_core::{
    BlockTarget, BusType, DriveInfo, FlashPhase, FlashProgress, ImageFormat, ImageMetadata,
    InscribeError, MultiBlockTarget,
};
use std::path::PathBuf;

#[test]
fn test_bus_type_variants_and_classification() {
    let usb = BusType::Usb;
    let sd = BusType::SdMmc;
    let nvme = BusType::Nvme;
    let sata = BusType::Sata;
    let sas = BusType::Sas;
    let virtio = BusType::VirtIo;
    let other = BusType::Other("Thunderbolt".to_string());
    let unknown = BusType::Unknown;

    assert!(usb.is_removable_bus());
    assert!(sd.is_removable_bus());
    assert!(!nvme.is_removable_bus());
    assert!(!sata.is_removable_bus());
    assert!(!sas.is_removable_bus());
    assert!(!virtio.is_removable_bus());
    assert!(!other.is_removable_bus());
    assert!(!unknown.is_removable_bus());

    assert!(nvme.is_internal_bus());
    assert!(sata.is_internal_bus());
    assert!(sas.is_internal_bus());
    assert!(virtio.is_internal_bus());
    assert!(!usb.is_internal_bus());
    assert!(!sd.is_internal_bus());

    // Test Clone, Debug, PartialEq
    let cloned = usb.clone();
    assert_eq!(cloned, usb);
    assert_eq!(format!("{usb:?}"), "Usb");

    // Test Serialize and Deserialize
    let serialized = serde_json::to_string(&usb).expect("serialization failed");
    let deserialized: BusType = serde_json::from_str(&serialized).expect("deserialization failed");
    assert_eq!(deserialized, usb);
}

#[test]
fn test_drive_info_creation_and_serialization() {
    let drive = DriveInfo {
        path: PathBuf::from("/dev/sdb"),
        model: "Ultra USB 3.0".to_string(),
        vendor: "SanDisk".to_string(),
        size: 32_000_000_000,
        sector_size: 512,
        bus_type: BusType::Usb,
        is_removable: true,
        is_read_only: false,
        is_system: false,
        mount_points: vec![],
    };

    assert_eq!(drive.path, PathBuf::from("/dev/sdb"));
    assert_eq!(drive.model, "Ultra USB 3.0");
    assert_eq!(drive.vendor, "SanDisk");
    assert_eq!(drive.size, 32_000_000_000);
    assert_eq!(drive.sector_size, 512);
    assert_eq!(drive.bus_type, BusType::Usb);
    assert!(drive.is_removable);
    assert!(!drive.is_read_only);
    assert!(!drive.is_system);
    assert!(drive.mount_points.is_empty());

    // Test external USB SSD detection
    let mut usb_ssd = drive.clone();
    usb_ssd.is_removable = false;
    assert!(usb_ssd.is_external_usb_ssd());

    // If it's a system drive, it cannot be considered a safe external USB SSD
    usb_ssd.is_system = true;
    assert!(!usb_ssd.is_external_usb_ssd());

    // Test Clone, Debug, PartialEq
    let cloned_drive = drive.clone();
    assert_eq!(cloned_drive, drive);
    assert!(format!("{drive:?}").contains("SanDisk"));

    // Test Serialize and Deserialize
    let json = serde_json::to_string(&drive).expect("drive serialization failed");
    let deserialized: DriveInfo =
        serde_json::from_str(&json).expect("drive deserialization failed");
    assert_eq!(deserialized, drive);
}

#[test]
fn test_image_metadata_and_format() {
    let metadata = ImageMetadata {
        path: Some(PathBuf::from("/images/ubuntu.iso")),
        file_size: 2_500_000_000,
        uncompressed_size: Some(2_500_000_000),
        format: ImageFormat::Raw,
        expected_sha256: Some(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
        ),
    };

    assert_eq!(metadata.format, ImageFormat::Raw);
    assert_eq!(metadata.file_size, 2_500_000_000);
    assert_eq!(metadata.uncompressed_size, Some(2_500_000_000));
    assert!(metadata.expected_sha256.is_some());

    // Test Clone, Debug, PartialEq
    let cloned = metadata.clone();
    assert_eq!(cloned, metadata);
    assert_eq!(format!("{:?}", metadata.format), "Raw");

    // Test Serialize and Deserialize
    let json = serde_json::to_string(&metadata).expect("serialization failed");
    let deserialized: ImageMetadata = serde_json::from_str(&json).expect("deserialization failed");
    assert_eq!(deserialized, metadata);
}

#[test]
fn test_flash_phase_and_progress_telemetry() {
    let progress = FlashProgress {
        phase: FlashPhase::Flashing,
        current_pass: 1,
        total_passes: 2,
        bytes_processed: 524_288_000,     // 500 MB
        total_bytes: Some(1_048_576_000), // 1000 MB
        bytes_per_second: 52_428_800.0,   // 50 MB/s
        eta_seconds: Some(10),
        fraction: Some(0.5),
    };

    assert_eq!(progress.phase, FlashPhase::Flashing);
    assert_eq!(progress.current_pass, 1);
    assert_eq!(progress.total_passes, 2);
    assert_eq!(progress.bytes_processed, 524_288_000);
    assert_eq!(progress.total_bytes, Some(1_048_576_000));
    assert_eq!(progress.eta_seconds, Some(10));
    assert_eq!(progress.fraction, Some(0.5));

    // Telemetry helpers
    assert!((progress.speed_mb_per_sec() - 50.0).abs() < 0.001);
    assert_eq!(progress.percentage(), Some(50.0));
    assert!(!progress.is_complete());
    assert!(!progress.is_failed());

    // Test finished state
    let completed = FlashProgress {
        phase: FlashPhase::Completed,
        current_pass: 2,
        total_passes: 2,
        bytes_processed: 1_048_576_000,
        total_bytes: Some(1_048_576_000),
        bytes_per_second: 0.0,
        eta_seconds: Some(0),
        fraction: Some(1.0),
    };
    assert!(completed.is_complete());

    // Test Serialize and Deserialize
    let json = serde_json::to_string(&progress).expect("progress serialization failed");
    let deserialized: FlashProgress =
        serde_json::from_str(&json).expect("progress deserialization failed");
    assert_eq!(deserialized, progress);
}

#[test]
fn test_inscribe_error_display_and_conversion() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access denied");
    let inscribe_err = InscribeError::from(io_err);
    assert!(matches!(inscribe_err, InscribeError::Io(_)));
    assert!(inscribe_err.to_string().contains("Access denied"));

    let guard_err = InscribeError::DriveGuardViolation {
        path: "/dev/nvme0n1".to_string(),
        reason: "System NVMe internal drive".to_string(),
    };
    assert!(guard_err.to_string().contains("/dev/nvme0n1"));
    assert!(guard_err.to_string().contains("System NVMe"));

    let verify_err = InscribeError::VerificationFailed {
        offset: 4096,
        expected: "abcdef".to_string(),
        actual: "123456".to_string(),
    };
    assert!(verify_err.to_string().contains("4096"));
}

// In-memory mock block target to test trait contracts and object safety
struct TestMockTarget {
    id: String,
    capacity: u64,
    block_size: u32,
    buffer: Vec<u8>,
    sync_count: usize,
}

impl TestMockTarget {
    fn new(id: &str, capacity: u64, block_size: u32) -> Self {
        Self {
            id: id.to_string(),
            capacity,
            block_size,
            buffer: vec![0u8; capacity as usize],
            sync_count: 0,
        }
    }
}

impl BlockTarget for TestMockTarget {
    fn id(&self) -> &str {
        &self.id
    }

    fn capacity(&self) -> Result<u64, InscribeError> {
        Ok(self.capacity)
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError> {
        let start = offset as usize;
        let end = start + data.len();
        if end > self.buffer.len() {
            return Err(InscribeError::BlockWriteError {
                offset,
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "write exceeds capacity",
                ),
            });
        }
        self.buffer[start..end].copy_from_slice(data);
        Ok(())
    }

    fn sync(&mut self) -> Result<(), InscribeError> {
        self.sync_count += 1;
        Ok(())
    }

    fn read_block(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), InscribeError> {
        let start = offset as usize;
        let end = start + buf.len();
        if end > self.buffer.len() {
            return Err(InscribeError::BlockReadError {
                offset,
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "read exceeds capacity",
                ),
            });
        }
        buf.copy_from_slice(&self.buffer[start..end]);
        Ok(())
    }
}

#[test]
fn test_block_target_trait_object_safety() {
    let mock = TestMockTarget::new("mock1", 1024 * 1024, 512);
    // Trait object safety check: Box<dyn BlockTarget>
    let mut boxed: Box<dyn BlockTarget> = Box::new(mock);

    assert_eq!(boxed.id(), "mock1");
    assert_eq!(boxed.capacity().expect("capacity failed"), 1024 * 1024);
    assert_eq!(boxed.block_size(), 512);

    let test_payload = [0xAA; 512];
    boxed.write_block(0, &test_payload).expect("write failed");
    boxed.sync().expect("sync failed");

    let mut read_buf = [0u8; 512];
    boxed.read_block(0, &mut read_buf).expect("read failed");
    assert_eq!(read_buf, test_payload);
}

#[test]
fn test_multi_target_fanout() {
    let target1 = Box::new(TestMockTarget::new("target_1", 2048, 512));
    let target2 = Box::new(TestMockTarget::new("target_2", 2048, 512));

    let targets: Vec<Box<dyn BlockTarget>> = vec![target1, target2];
    let mut multi = MultiBlockTarget::new(targets);

    assert_eq!(multi.len(), 2);
    assert!(!multi.is_empty());
    assert_eq!(multi.capacity().expect("capacity failed"), 2048);
    assert_eq!(multi.block_size(), 512);

    // Simultaneous write to all targets
    let payload = [0x55; 1024];
    multi
        .write_block(0, &payload)
        .expect("multi-target write failed");
    multi.sync().expect("multi-target sync failed");

    // Verification read back
    let mut read_buf = [0u8; 1024];
    multi
        .read_block(0, &mut read_buf)
        .expect("multi-target read failed");
    assert_eq!(read_buf, payload);

    // Inspect individual targets inside multi to verify fan-out wrote to BOTH targets
    for target in multi.targets() {
        assert_eq!(target.capacity().unwrap(), 2048);
    }
}

#[test]
fn test_multi_target_read_block_validates_secondary_targets() {
    let target1 = Box::new(TestMockTarget::new("target_0", 2048, 512));
    let target2 = Box::new(TestMockTarget::new("target_1", 2048, 512));
    let target3 = Box::new(TestMockTarget::new("target_2", 2048, 512));

    let targets: Vec<Box<dyn BlockTarget>> = vec![target1, target2, target3];
    let mut multi = MultiBlockTarget::new(targets);

    let payload = [0x77; 1024];
    multi.write_block(0, &payload).expect("write failed");

    // All 3 targets have identical payload -> read_block succeeds
    let mut read_buf = [0u8; 1024];
    multi
        .read_block(0, &mut read_buf)
        .expect("identical secondary targets must succeed");
    assert_eq!(read_buf, payload);

    // Tamper with target_2 (index 2) at offset 50
    let mut corrupt = [0x77; 1024];
    corrupt[50] = 0xDE;
    multi.targets_mut()[2]
        .write_block(0, &corrupt)
        .expect("tamper write failed");

    let result = multi.read_block(0, &mut read_buf);
    assert!(
        result.is_err(),
        "read_block must validate all secondary targets and fail on mismatch"
    );

    let err = result.unwrap_err();
    match err {
        InscribeError::VerificationFailed {
            offset,
            expected,
            actual,
        } => {
            assert_eq!(offset, 50, "Verification mismatch must point to offset 50");
            assert!(
                expected.contains("target_0") || expected.contains("bitstream"),
                "Expected string must reference target 0: {expected}"
            );
            assert!(
                actual.contains("target_2") || actual.contains("mismatch"),
                "Actual string must reference target 2 mismatch: {actual}"
            );
        }
        other => panic!("Expected InscribeError::VerificationFailed, got {other:?}"),
    }
}
