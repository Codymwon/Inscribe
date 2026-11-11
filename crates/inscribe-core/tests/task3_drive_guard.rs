use inscribe_core::drive::{DriveGuard, DriveScanner, MockDriveScanner};
use inscribe_core::{BusType, DriveInfo, InscribeError};
use std::path::PathBuf;

fn create_base_drive(path: &str, bus_type: BusType, is_removable: bool) -> DriveInfo {
    DriveInfo {
        path: PathBuf::from(path),
        model: "Test Drive".to_string(),
        vendor: "Test Vendor".to_string(),
        size: 32_000_000_000,
        sector_size: 512,
        bus_type,
        is_removable,
        is_read_only: false,
        is_system: false,
        mount_points: vec![],
    }
}

#[test]
fn test_valid_removable_usb_drive_is_accepted() {
    let drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    assert!(DriveGuard::validate_target(&drive).is_ok());
    assert!(DriveGuard::validate_target_with_options(&drive, false).is_ok());
    assert!(DriveGuard::validate_target_with_options(&drive, true).is_ok());

    let guard = DriveGuard::default();
    assert!(guard.validate(&drive).is_ok());
}

#[test]
fn test_valid_removable_sdmmc_drive_is_accepted() {
    let drive = create_base_drive("/dev/mmcblk0", BusType::SdMmc, true);
    assert!(DriveGuard::validate_target(&drive).is_ok());
    assert!(DriveGuard::validate_target_with_options(&drive, false).is_ok());
}

#[test]
fn test_internal_nvme_drive_is_strictly_rejected() {
    let drive = create_base_drive("/dev/nvme0n1", BusType::Nvme, false);
    let result = DriveGuard::validate_target(&drive);
    assert!(result.is_err());
    match result.unwrap_err() {
        InscribeError::DriveGuardViolation { path, reason } => {
            assert_eq!(path, "/dev/nvme0n1");
            assert!(reason.contains("internal") || reason.contains("Nvme"));
        }
        other => panic!("Expected DriveGuardViolation, got {other:?}"),
    }

    // Even if allow_external_usb_ssd is enabled, NVMe must remain blocked!
    assert!(DriveGuard::validate_target_with_options(&drive, true).is_err());
}

#[test]
fn test_internal_sata_drive_is_strictly_rejected() {
    let drive = create_base_drive("/dev/sda", BusType::Sata, false);
    assert!(DriveGuard::validate_target(&drive).is_err());
    assert!(DriveGuard::validate_target_with_options(&drive, true).is_err());
}

#[test]
fn test_internal_sas_and_virtio_buses_are_strictly_rejected() {
    let sas = create_base_drive("/dev/sda", BusType::Sas, false);
    assert!(DriveGuard::validate_target(&sas).is_err());
    assert!(DriveGuard::validate_target_with_options(&sas, true).is_err());

    let virtio = create_base_drive("/dev/vda", BusType::VirtIo, false);
    assert!(DriveGuard::validate_target(&virtio).is_err());
    assert!(DriveGuard::validate_target_with_options(&virtio, true).is_err());
}

#[test]
fn test_marked_system_drive_is_strictly_rejected() {
    let mut drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    drive.is_system = true;

    let result = DriveGuard::validate_target(&drive);
    assert!(result.is_err());
    match result.unwrap_err() {
        InscribeError::DriveGuardViolation { path, reason } => {
            assert_eq!(path, "/dev/sdb");
            assert!(reason.contains("system") || reason.contains("System"));
        }
        other => panic!("Expected DriveGuardViolation, got {other:?}"),
    }

    assert!(DriveGuard::validate_target_with_options(&drive, true).is_err());
}

#[test]
fn test_simulated_root_disk_with_usb_id_blocked_by_mount_check() {
    // Crucial manual check criterion: simulated root disk with USB controller ID
    // must be strictly blocked by mount check!
    let mut drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    drive.mount_points = vec![PathBuf::from("/")];

    let result = DriveGuard::validate_target(&drive);
    assert!(result.is_err());
    match result.unwrap_err() {
        InscribeError::DriveGuardViolation { path, reason } => {
            assert_eq!(path, "/dev/sdb");
            assert!(reason.contains("system mount") || reason.contains('/'));
        }
        other => panic!("Expected DriveGuardViolation, got {other:?}"),
    }

    // Must still fail even with allow_external_usb_ssd = true
    assert!(DriveGuard::validate_target_with_options(&drive, true).is_err());
}

#[test]
fn test_system_mount_points_blocked() {
    let dangerous_mounts = [
        "/",
        "/boot",
        "/boot/efi",
        "/home",
        "/home/user",
        "/etc",
        "/usr",
        "/var",
        "C:\\",
        "C:\\Windows",
        "c:\\",
        "C:",
        "/System/Volumes/Data",
        "/System",
        "/Volumes/Macintosh HD",
    ];

    for mount in dangerous_mounts {
        let mut drive = create_base_drive("/dev/sdb", BusType::Usb, true);
        drive.mount_points = vec![PathBuf::from(mount)];

        let result = DriveGuard::validate_target(&drive);
        assert!(
            result.is_err(),
            "Expected mount {mount} to be blocked by DriveGuard, but it was accepted"
        );
    }
}

#[test]
fn test_non_system_user_mount_points_are_not_blocked_by_guard() {
    // Normal USB flash drive mounts like /media/... or /run/media/... are unmounted before flash,
    // not blocked as system drives by DriveGuard.
    let user_mounts = [
        "/media/user/USB_STICK",
        "/run/media/user/PENDRIVE",
        "/mnt/external",
        "D:\\",
        "E:\\Flash",
        "/Volumes/USB_DRIVE",
    ];

    for mount in user_mounts {
        let mut drive = create_base_drive("/dev/sdb", BusType::Usb, true);
        drive.mount_points = vec![PathBuf::from(mount)];

        assert!(
            DriveGuard::validate_target(&drive).is_ok(),
            "Non-system user mount {mount} should be accepted by DriveGuard"
        );
    }
}

#[test]
fn test_read_only_drive_is_rejected() {
    let mut drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    drive.is_read_only = true;

    let result = DriveGuard::validate_target(&drive);
    assert!(result.is_err());
    match result.unwrap_err() {
        InscribeError::DriveGuardViolation { reason, .. } => {
            assert!(reason.contains("read-only") || reason.contains("write-protected"));
        }
        other => panic!("Expected DriveGuardViolation, got {other:?}"),
    }
}

#[test]
fn test_zero_size_drive_is_rejected() {
    let mut drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    drive.size = 0;

    let result = DriveGuard::validate_target(&drive);
    assert!(result.is_err());
    match result.unwrap_err() {
        InscribeError::DriveGuardViolation { reason, .. } => {
            assert!(reason.contains("capacity") || reason.contains("zero"));
        }
        other => panic!("Expected DriveGuardViolation, got {other:?}"),
    }
}

#[test]
fn test_external_usb_ssd_opt_in_filter() {
    // USB external drive with non-removable bit set (common for external SSDs in enclosures)
    let drive = create_base_drive("/dev/sdc", BusType::Usb, false);
    assert!(!drive.is_removable);
    assert!(drive.is_external_usb_ssd());

    // By default, non-removable media is blocked
    let default_result = DriveGuard::validate_target(&drive);
    assert!(default_result.is_err());

    let default_opt_result = DriveGuard::validate_target_with_options(&drive, false);
    assert!(default_opt_result.is_err());

    // With explicit opt-in, external USB SSD is accepted
    let allowed_result = DriveGuard::validate_target_with_options(&drive, true);
    assert!(allowed_result.is_ok());

    let guard_with_opt_in = DriveGuard::new(true);
    assert!(guard_with_opt_in.validate(&drive).is_ok());

    // But if that USB SSD has a system mount point or is marked system, opt-in still rejects it!
    let mut system_usb_ssd = drive.clone();
    system_usb_ssd.is_system = true;
    assert!(DriveGuard::validate_target_with_options(&system_usb_ssd, true).is_err());

    let mut root_mounted_usb_ssd = drive.clone();
    root_mounted_usb_ssd.mount_points = vec![PathBuf::from("/")];
    assert!(DriveGuard::validate_target_with_options(&root_mounted_usb_ssd, true).is_err());
}

#[test]
fn test_filter_safe_drives_filters_mixed_list() {
    let usb_removable = create_base_drive("/dev/sdb", BusType::Usb, true);
    let usb_ssd = create_base_drive("/dev/sdc", BusType::Usb, false);
    let nvme_system = create_base_drive("/dev/nvme0n1", BusType::Nvme, false);
    let mut usb_with_root = create_base_drive("/dev/sdd", BusType::Usb, true);
    usb_with_root.mount_points = vec![PathBuf::from("/")];

    let drives = vec![
        usb_removable.clone(),
        usb_ssd.clone(),
        nvme_system.clone(),
        usb_with_root.clone(),
    ];

    // Default filter: only usb_removable should pass
    let safe_default = DriveGuard::filter_safe_drives(&drives, false);
    assert_eq!(safe_default.len(), 1);
    assert_eq!(safe_default[0].path, PathBuf::from("/dev/sdb"));

    // Opt-in filter: usb_removable AND usb_ssd should pass
    let safe_with_ssd = DriveGuard::filter_safe_drives(&drives, true);
    assert_eq!(safe_with_ssd.len(), 2);
    assert_eq!(safe_with_ssd[0].path, PathBuf::from("/dev/sdb"));
    assert_eq!(safe_with_ssd[1].path, PathBuf::from("/dev/sdc"));
}

#[test]
fn test_mock_drive_scanner_operations_and_guard_filtering() {
    let usb_drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    let nvme_drive = create_base_drive("/dev/nvme0n1", BusType::Nvme, false);

    let scanner = MockDriveScanner::new(vec![usb_drive.clone(), nvme_drive.clone()]);
    let scanned = scanner.scan_drives().expect("scan failed");
    assert_eq!(scanned.len(), 2);

    // Filter using DriveGuard
    let safe = DriveGuard::filter_safe_drives(&scanned, false);
    assert_eq!(safe.len(), 1);
    assert_eq!(safe[0].path, PathBuf::from("/dev/sdb"));

    // Dynamic drive addition and removal (simulating hotplug)
    let new_usb = create_base_drive("/dev/sdc", BusType::Usb, true);
    scanner.add_drive(new_usb);
    assert_eq!(scanner.scan_drives().unwrap().len(), 3);

    let removed = scanner.remove_drive(&PathBuf::from("/dev/sdb"));
    assert!(removed);
    assert_eq!(scanner.scan_drives().unwrap().len(), 2);

    let removed_again = scanner.remove_drive(&PathBuf::from("/dev/sdb"));
    assert!(!removed_again);

    scanner.clear();
    assert_eq!(scanner.scan_drives().unwrap().len(), 0);
}

#[test]
fn test_drive_info_is_flashable_unifies_with_drive_guard() {
    let valid_usb = create_base_drive("/dev/sdb", BusType::Usb, true);
    assert!(valid_usb.is_flashable(false));
    assert!(valid_usb.is_flashable(true));

    // Zero-size drive must NOT be flashable
    let mut zero_size = valid_usb.clone();
    zero_size.size = 0;
    assert!(
        !zero_size.is_flashable(false),
        "Zero-size drive must not be flashable"
    );

    // Drive with critical system mount must NOT be flashable
    let mut root_mounted = valid_usb.clone();
    root_mounted.mount_points = vec![PathBuf::from("/")];
    assert!(
        !root_mounted.is_flashable(false),
        "Root mounted drive must not be flashable"
    );

    let mut home_mounted = valid_usb.clone();
    home_mounted.mount_points = vec![PathBuf::from("/home")];
    assert!(
        !home_mounted.is_flashable(false),
        "Home mounted drive must not be flashable"
    );

    // External USB SSD opt-in behavior
    let usb_ssd = create_base_drive("/dev/sdc", BusType::Usb, false);
    assert!(!usb_ssd.is_flashable(false));
    assert!(usb_ssd.is_flashable(true));

    let mut usb_ssd_with_root = usb_ssd.clone();
    usb_ssd_with_root.mount_points = vec![PathBuf::from("/")];
    assert!(!usb_ssd_with_root.is_flashable(true));

    // Internal NVMe drive must never be flashable
    let nvme = create_base_drive("/dev/nvme0n1", BusType::Nvme, false);
    assert!(!nvme.is_flashable(false));
    assert!(!nvme.is_flashable(true));
}

#[test]
fn test_partition_targets_are_strictly_rejected() {
    // Linux partition targeting
    let part_sdb1 = create_base_drive("/dev/sdb1", BusType::Usb, true);
    assert!(DriveGuard::validate_target(&part_sdb1).is_err());
    let part_nvme = create_base_drive("/dev/nvme0n1p1", BusType::Usb, true);
    assert!(DriveGuard::validate_target(&part_nvme).is_err());
    let part_mmc = create_base_drive("/dev/mmcblk0p2", BusType::SdMmc, true);
    assert!(DriveGuard::validate_target(&part_mmc).is_err());

    // macOS partition targeting
    let part_macos = create_base_drive("/dev/disk2s1", BusType::Usb, true);
    assert!(DriveGuard::validate_target(&part_macos).is_err());
    let part_macos_r = create_base_drive("/dev/rdisk3s2", BusType::Usb, true);
    assert!(DriveGuard::validate_target(&part_macos_r).is_err());

    // Windows volume / partition targeting
    let vol_win = create_base_drive(r"\\.\E:", BusType::Usb, true);
    assert!(DriveGuard::validate_target(&vol_win).is_err());
    let vol_guid = create_base_drive(
        r"\\?\Volume{12345678-1234-1234-1234-123456789abc}",
        BusType::Usb,
        true,
    );
    assert!(DriveGuard::validate_target(&vol_guid).is_err());
}

#[test]
fn test_live_target_attestation_detects_swapped_device() {
    let scan_drive = create_base_drive("/dev/sdb", BusType::Usb, true);
    let mut live_drive = scan_drive.clone();

    // Identical live drive passes attestation
    assert!(DriveGuard::attest_target(&scan_drive, &live_drive, false).is_ok());

    // Capacity swap (e.g. 32GB thumb drive replaced by 1TB SSD)
    live_drive.size = 1_000_000_000_000;
    assert!(DriveGuard::attest_target(&scan_drive, &live_drive, false).is_err());

    // Reset and test bus interconnect swap (e.g. replaced by NVMe controller)
    live_drive = scan_drive.clone();
    live_drive.bus_type = BusType::Nvme;
    assert!(DriveGuard::attest_target(&scan_drive, &live_drive, false).is_err());

    // Reset and test newly mounted system volume on live target
    live_drive = scan_drive.clone();
    live_drive.mount_points = vec![PathBuf::from("/")];
    assert!(DriveGuard::attest_target(&scan_drive, &live_drive, false).is_err());

    // Removable status changed
    live_drive = scan_drive.clone();
    live_drive.is_removable = false;
    assert!(DriveGuard::attest_target(&scan_drive, &live_drive, false).is_err());
}
