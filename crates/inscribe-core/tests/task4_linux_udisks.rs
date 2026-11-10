//! Task 4 Integration tests: Linux Scanner & UDisks2 D-Bus Polkit Integration

#[cfg(target_os = "linux")]
use inscribe_core::drive::linux::LinuxDriveScanner;
#[cfg(target_os = "linux")]
use inscribe_core::drive::udisks::{UDisksClient, dev_path_to_object_path};
#[cfg(target_os = "linux")]
use inscribe_core::drive::{BusType, DriveInfo, DriveScanner};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[test]
#[cfg(target_os = "linux")]
fn test_udisks_object_path_generation() {
    let sda_path = dev_path_to_object_path(Path::new("/dev/sda")).expect("Must convert sda");
    assert_eq!(
        sda_path.as_str(),
        "/org/freedesktop/UDisks2/block_devices/sda"
    );

    let nvme_path =
        dev_path_to_object_path(Path::new("/dev/nvme0n1p2")).expect("Must convert nvme partition");
    assert_eq!(
        nvme_path.as_str(),
        "/org/freedesktop/UDisks2/block_devices/nvme0n1p2"
    );

    let special_path = dev_path_to_object_path(Path::new("/dev/disk/by-id/usb-SanDisk_Ultra-0:0"))
        .expect("Must sanitize special chars");
    assert_eq!(
        special_path.as_str(),
        "/org/freedesktop/UDisks2/block_devices/usb_SanDisk_Ultra_0_0"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn test_drive_guard_blocks_unmount_and_open_on_system_drive() {
    let client = match UDisksClient::new().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping live D-Bus test: system bus unavailable: {e}");
            return;
        }
    };

    let system_drive = DriveInfo {
        path: PathBuf::from("/dev/nvme0n1"),
        model: "Host SSD".to_string(),
        vendor: "Samsung".to_string(),
        size: 500_000_000_000,
        sector_size: 512,
        bus_type: BusType::Nvme,
        is_removable: false,
        is_read_only: false,
        is_system: true,
        mount_points: vec![PathBuf::from("/")],
    };

    let res = client.unmount_and_open(&system_drive, true).await;
    assert!(
        res.is_err(),
        "Drive Guard must reject flashing system drive before opening via UDisks2"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn test_drive_guard_blocks_unmount_and_open_on_internal_nvme() {
    let client = match UDisksClient::new().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping live D-Bus test: system bus unavailable: {e}");
            return;
        }
    };

    let internal_nvme = DriveInfo {
        path: PathBuf::from("/dev/nvme1n1"),
        model: "Internal Secondary NVMe".to_string(),
        vendor: "Micron".to_string(),
        size: 1_000_000_000_000,
        sector_size: 512,
        bus_type: BusType::Nvme,
        is_removable: false,
        is_read_only: false,
        is_system: false,
        mount_points: vec![],
    };

    let res = client.unmount_and_open(&internal_nvme, true).await;
    assert!(
        res.is_err(),
        "Drive Guard must reject non-removable internal NVMe storage"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn test_live_udisks_block_device_proxy_inspection() {
    let client = match UDisksClient::new().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping live D-Bus test: system bus unavailable: {e}");
            return;
        }
    };

    // Scan actual devices on host via LinuxDriveScanner
    let scanner = LinuxDriveScanner::new();
    let drives = scanner.scan_drives().expect("Host scan must succeed");
    if let Some(first_drive) = drives.first() {
        if let Ok(proxy) = client.block_proxy(&first_drive.path).await {
            if let Ok(size) = proxy.size().await {
                assert!(size > 0, "UDisks2 reported capacity must be > 0 bytes");
            }
        }
    }
}
