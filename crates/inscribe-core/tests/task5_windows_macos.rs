//! Task 5 Integration tests: Windows & macOS Scanners and Exclusive Volume Locks

use inscribe_core::drive::macos::{
    DADISK_CLAIM_OPTION_DEFAULT, DADISK_UNMOUNT_OPTION_FORCE, DADISK_UNMOUNT_OPTION_WHOLE,
    MacDiskClaim, bsd_name, interconnect_to_bus_type, to_block_device, to_raw_character_device,
};
use inscribe_core::drive::windows::{
    FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME, IOCTL_DISK_DELETE_DRIVE_LAYOUT,
    IOCTL_STORAGE_QUERY_PROPERTY, MockLockExecutor, StorageBusType, StorageDeviceDescriptor,
    StoragePropertyId, StoragePropertyQuery, StorageQueryType, VolumeLockExecutor, VolumeLockStep,
    WindowsVolumeLockSequence, parse_physical_drive_index, physical_drive_path, volume_path,
};
use inscribe_core::drive::{BusType, DriveInfo};
use std::path::{Path, PathBuf};

#[test]
fn test_windows_control_codes_and_paths() {
    assert_eq!(IOCTL_STORAGE_QUERY_PROPERTY, 0x002D_1400);
    assert_eq!(FSCTL_LOCK_VOLUME, 0x0009_0018);
    assert_eq!(FSCTL_DISMOUNT_VOLUME, 0x0009_0020);
    assert_eq!(FSCTL_UNLOCK_VOLUME, 0x0009_001C);
    assert_eq!(IOCTL_DISK_DELETE_DRIVE_LAYOUT, 0x0007_C100);

    assert_eq!(physical_drive_path(2), PathBuf::from(r"\\.\PhysicalDrive2"));
    assert_eq!(volume_path('E'), PathBuf::from(r"\\.\E:"));
    assert_eq!(
        parse_physical_drive_index(Path::new(r"\\.\PhysicalDrive3")),
        Some(3)
    );
    assert_eq!(
        parse_physical_drive_index(Path::new(r"\\.\PhysicalDrive12")),
        Some(12)
    );
    assert_eq!(parse_physical_drive_index(Path::new(r"/dev/sdb")), None);
}

#[test]
fn test_windows_storage_query_structures_and_descriptor_parsing() {
    let query = StoragePropertyQuery::device_property();
    assert_eq!(query.property_id, StoragePropertyId::StorageDeviceProperty);
    assert_eq!(query.query_type, StorageQueryType::PropertyStandardQuery);

    // Build a mock raw STORAGE_DEVICE_DESCRIPTOR buffer
    // Layout:
    // DWORD Version (0)
    // DWORD Size (4)
    // BYTE DeviceType (8)
    // BYTE DeviceTypeModifier (9)
    // BOOLEAN RemovableMedia (10)
    // BOOLEAN CommandQueueing (11)
    // DWORD VendorIdOffset (12)
    // DWORD ProductIdOffset (16)
    // DWORD ProductRevisionOffset (20)
    // DWORD SerialNumberOffset (24)
    // STORAGE_BUS_TYPE BusType (28)
    // DWORD RawPropertiesLength (32)
    // BYTE RawDeviceProperties[1] (36)
    let mut buffer = vec![0u8; 128];
    let total_size = buffer.len() as u32;

    // Header values
    buffer[0..4].copy_from_slice(&1u32.to_le_bytes()); // Version
    buffer[4..8].copy_from_slice(&total_size.to_le_bytes()); // Size
    buffer[8] = 0; // Direct access block device
    buffer[9] = 0;
    buffer[10] = 1; // RemovableMedia = TRUE
    buffer[11] = 0; // CommandQueueing = FALSE

    let vendor_offset = 40u32;
    let product_offset = 56u32;
    let serial_offset = 80u32;

    buffer[12..16].copy_from_slice(&vendor_offset.to_le_bytes());
    buffer[16..20].copy_from_slice(&product_offset.to_le_bytes());
    buffer[20..24].copy_from_slice(&0u32.to_le_bytes()); // Revision offset
    buffer[24..28].copy_from_slice(&serial_offset.to_le_bytes());
    buffer[28..32].copy_from_slice(&(StorageBusType::BusTypeUsb as u32).to_le_bytes());

    // Strings
    let vendor_bytes = b"SanDisk\0";
    buffer[vendor_offset as usize..vendor_offset as usize + vendor_bytes.len()]
        .copy_from_slice(vendor_bytes);

    let product_bytes = b"Extreme Pro\0";
    buffer[product_offset as usize..product_offset as usize + product_bytes.len()]
        .copy_from_slice(product_bytes);

    let serial_bytes = b"AA0102030405\0";
    buffer[serial_offset as usize..serial_offset as usize + serial_bytes.len()]
        .copy_from_slice(serial_bytes);

    let parsed = StorageDeviceDescriptor::parse(&buffer).expect("Must parse valid descriptor");
    assert_eq!(parsed.vendor, "SanDisk");
    assert_eq!(parsed.product, "Extreme Pro");
    assert_eq!(parsed.serial_number, "AA0102030405");
    assert_eq!(parsed.bus_type, StorageBusType::BusTypeUsb);
    assert_eq!(parsed.bus_type.to_domain_bus_type(), BusType::Usb);
    assert!(parsed.is_removable);
}

#[test]
fn test_windows_bus_type_conversions() {
    assert_eq!(
        StorageBusType::BusTypeUsb.to_domain_bus_type(),
        BusType::Usb
    );
    assert_eq!(
        StorageBusType::BusTypeNvme.to_domain_bus_type(),
        BusType::Nvme
    );
    assert_eq!(
        StorageBusType::BusTypeSata.to_domain_bus_type(),
        BusType::Sata
    );
    assert_eq!(
        StorageBusType::BusTypeSas.to_domain_bus_type(),
        BusType::Sas
    );
    assert_eq!(
        StorageBusType::BusTypeSd.to_domain_bus_type(),
        BusType::SdMmc
    );
    assert_eq!(
        StorageBusType::BusTypeMmc.to_domain_bus_type(),
        BusType::SdMmc
    );
    assert_eq!(
        StorageBusType::BusTypeVirtual.to_domain_bus_type(),
        BusType::VirtIo
    );
    assert_eq!(
        StorageBusType::BusTypeUnknown.to_domain_bus_type(),
        BusType::Unknown
    );
}

#[test]
fn test_windows_exclusive_volume_lock_sequence_order_and_guard_safety() {
    // 1. Guard safety: rejection of system drive
    let sys_drive = DriveInfo {
        path: PathBuf::from(r"\\.\PhysicalDrive0"),
        model: "NVMe Samsung 980".to_string(),
        vendor: "Samsung".to_string(),
        size: 512_000_000_000,
        sector_size: 512,
        bus_type: BusType::Nvme,
        is_removable: false,
        is_read_only: false,
        is_system: true,
        mount_points: vec![PathBuf::from(r"C:\")],
    };

    let lock_seq = WindowsVolumeLockSequence::new();
    let res = lock_seq.execute_lock_sequence(&sys_drive);
    assert!(res.is_err(), "Must reject locking system disk hosting C:\\");

    // 2. Guard safety: rejection of internal SATA drive
    let internal_sata = DriveInfo {
        path: PathBuf::from(r"\\.\PhysicalDrive1"),
        model: "Crucial MX500".to_string(),
        vendor: "Crucial".to_string(),
        size: 1_000_000_000_000,
        sector_size: 512,
        bus_type: BusType::Sata,
        is_removable: false,
        is_read_only: false,
        is_system: false,
        mount_points: vec![PathBuf::from(r"D:\")],
    };
    let res = lock_seq.execute_lock_sequence(&internal_sata);
    assert!(
        res.is_err(),
        "Must reject locking internal non-removable SATA drive"
    );

    // 3. Valid removable USB target: verify exact sequence steps
    let usb_drive = DriveInfo {
        path: PathBuf::from(r"\\.\PhysicalDrive2"),
        model: "Ultra USB".to_string(),
        vendor: "SanDisk".to_string(),
        size: 32_000_000_000,
        sector_size: 512,
        bus_type: BusType::Usb,
        is_removable: true,
        is_read_only: false,
        is_system: false,
        mount_points: vec![PathBuf::from(r"E:\"), PathBuf::from(r"F:\")],
    };

    let lock = lock_seq
        .execute_lock_sequence(&usb_drive)
        .expect("Lock sequence on valid USB drive must succeed");

    let steps = lock.executed_steps();
    // Verify sequence:
    // Step 1 & 2: Lock & Dismount Volume E:
    // Step 3 & 4: Lock & Dismount Volume F:
    // Step 5: Lock PhysicalDrive2
    // Step 6: Dismount PhysicalDrive2
    // Step 7: Delete drive layout (IOCTL_DISK_DELETE_DRIVE_LAYOUT to prevent Win11 Error 5)
    assert_eq!(
        steps,
        vec![
            VolumeLockStep::LockVolume(PathBuf::from(r"\\.\E:")),
            VolumeLockStep::DismountVolume(PathBuf::from(r"\\.\E:")),
            VolumeLockStep::LockVolume(PathBuf::from(r"\\.\F:")),
            VolumeLockStep::DismountVolume(PathBuf::from(r"\\.\F:")),
            VolumeLockStep::LockPhysicalDrive(PathBuf::from(r"\\.\PhysicalDrive2")),
            VolumeLockStep::DismountPhysicalDrive(PathBuf::from(r"\\.\PhysicalDrive2")),
            VolumeLockStep::DeleteDriveLayout(PathBuf::from(r"\\.\PhysicalDrive2")),
        ]
    );
}

#[test]
fn test_macos_character_device_mapping() {
    assert_eq!(
        to_raw_character_device(Path::new("/dev/disk2")),
        PathBuf::from("/dev/rdisk2")
    );
    assert_eq!(
        to_raw_character_device(Path::new("/dev/disk2s1")),
        PathBuf::from("/dev/rdisk2s1")
    );
    assert_eq!(
        to_raw_character_device(Path::new("disk3")),
        PathBuf::from("/dev/rdisk3")
    );
    assert_eq!(
        to_raw_character_device(Path::new("/dev/rdisk4")),
        PathBuf::from("/dev/rdisk4")
    );

    assert_eq!(
        to_block_device(Path::new("/dev/rdisk2")),
        PathBuf::from("/dev/disk2")
    );
    assert_eq!(
        to_block_device(Path::new("/dev/rdisk2s1")),
        PathBuf::from("/dev/disk2s1")
    );

    assert_eq!(bsd_name(Path::new("/dev/disk2")), "disk2");
    assert_eq!(bsd_name(Path::new("/dev/rdisk2s1")), "disk2s1");
    assert_eq!(bsd_name(Path::new("disk5")), "disk5");
}

#[test]
fn test_macos_diskarbitration_options_and_interconnect_mapping() {
    assert_eq!(DADISK_UNMOUNT_OPTION_WHOLE, 0x0000_0001);
    assert_eq!(DADISK_UNMOUNT_OPTION_FORCE, 0x0000_0002);
    assert_eq!(DADISK_CLAIM_OPTION_DEFAULT, 0x0000_0000);

    assert_eq!(interconnect_to_bus_type("USB"), BusType::Usb);
    assert_eq!(interconnect_to_bus_type("PCI-Express"), BusType::Nvme);
    assert_eq!(interconnect_to_bus_type("NVMe"), BusType::Nvme);
    assert_eq!(interconnect_to_bus_type("Serial ATA"), BusType::Sata);
    assert_eq!(interconnect_to_bus_type("Secure Digital"), BusType::SdMmc);
    assert_eq!(interconnect_to_bus_type("SAS"), BusType::Sas);
    assert_eq!(
        interconnect_to_bus_type("FireWire"),
        BusType::Other("FireWire".to_string())
    );
    assert_eq!(
        interconnect_to_bus_type("Thunderbolt"),
        BusType::Other("Thunderbolt".to_string())
    );
    assert_eq!(interconnect_to_bus_type("UnknownProt"), BusType::Unknown);
}

#[test]
fn test_macos_claim_sequence_and_drive_guard_rejection() {
    let sys_drive = DriveInfo {
        path: PathBuf::from("/dev/disk1"),
        model: "Apple SSD".to_string(),
        vendor: "Apple".to_string(),
        size: 500_000_000_000,
        sector_size: 4096,
        bus_type: BusType::Nvme,
        is_removable: false,
        is_read_only: false,
        is_system: true,
        mount_points: vec![PathBuf::from("/System/Volumes/Data")],
    };

    let res = MacDiskClaim::claim_and_unmount(&sys_drive);
    assert!(
        res.is_err(),
        "Drive Guard must reject claiming macOS system disk"
    );

    let usb_drive = DriveInfo {
        path: PathBuf::from("/dev/disk3"),
        model: "Flash Drive".to_string(),
        vendor: "Kingston".to_string(),
        size: 64_000_000_000,
        sector_size: 512,
        bus_type: BusType::Usb,
        is_removable: true,
        is_read_only: false,
        is_system: false,
        mount_points: vec![PathBuf::from("/Volumes/USB_STICK")],
    };

    let claim = MacDiskClaim::claim_and_unmount(&usb_drive)
        .expect("Valid USB drive must claim successfully");
    assert_eq!(claim.raw_device_path(), Path::new("/dev/rdisk3"));
    assert_eq!(claim.bsd_name(), "disk3");
}

#[test]
fn test_windows_mock_lock_executor() {
    let mut executor = MockLockExecutor::new();
    let step = VolumeLockStep::LockPhysicalDrive(PathBuf::from(r"\\.\PhysicalDrive5"));
    executor.execute_step(&step).expect("Must execute step");
    assert_eq!(executor.steps().len(), 1);
    assert_eq!(executor.steps()[0], step);
    executor
        .unlock_volume(Path::new(r"\\.\PhysicalDrive5"))
        .expect("Must unlock");
}
