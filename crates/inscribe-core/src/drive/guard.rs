//! Drive Guard safety engine: strict validation to prevent overwriting host system drives.

use super::{BusType, DriveInfo};
use crate::error::InscribeError;
use std::path::Path;

/// Safety validator enforcing non-targetability of host OS disks, internal storage, and read-only media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DriveGuard {
    /// When true, allows external USB SSD enclosures (USB bus with non-removable bit set).
    pub allow_external_usb_ssd: bool,
}

impl DriveGuard {
    /// Creates a new `DriveGuard` instance with the specified opt-in settings.
    #[must_use]
    pub const fn new(allow_external_usb_ssd: bool) -> Self {
        Self {
            allow_external_usb_ssd,
        }
    }

    /// Validates whether a drive is safe to flash using default safety rules.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if the drive is a system disk,
    /// contains system mount points, uses an internal bus, is read-only, or is non-removable.
    pub fn validate_target(drive: &DriveInfo) -> Result<(), InscribeError> {
        Self::validate_target_with_options(drive, false)
    }

    /// Validates whether a drive is safe to flash with configurable opt-in options.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if safety predicates fail.
    pub fn validate_target_with_options(
        drive: &DriveInfo,
        allow_external_usb_ssd: bool,
    ) -> Result<(), InscribeError> {
        // Rule 1: Capacity must be strictly positive
        if drive.size == 0 {
            return Err(InscribeError::drive_guard_violation(
                &drive.path,
                "Target capacity is zero bytes",
            ));
        }

        // Rule 2: Hardware write-protected or read-only media cannot be written to
        if drive.is_read_only {
            return Err(InscribeError::drive_guard_violation(
                &drive.path,
                "Target is write-protected or read-only",
            ));
        }

        // Rule 3: Explicitly marked system drive
        if drive.is_system {
            return Err(InscribeError::drive_guard_violation(
                &drive.path,
                "Target is marked as a host operating system drive",
            ));
        }

        // Rule 4: System partition mount points (/, /boot, /home, C:\, /System/Volumes/Data, etc.)
        for mount in &drive.mount_points {
            if is_system_mount(mount) {
                return Err(InscribeError::drive_guard_violation(
                    &drive.path,
                    format!(
                        "Target contains critical system mount point: {}",
                        mount.display()
                    ),
                ));
            }
        }

        // Rule 5: Internal storage bus interconnect (NVMe, SATA, SAS, VirtIO)
        if drive.bus_type.is_internal_bus() {
            return Err(InscribeError::drive_guard_violation(
                &drive.path,
                format!(
                    "Target bus interconnect ({:?}) is internal host storage",
                    drive.bus_type
                ),
            ));
        }

        // Rule 6: Removable media requirement and external USB SSD opt-in
        if drive.is_removable {
            if !drive.bus_type.is_removable_bus() {
                return Err(InscribeError::drive_guard_violation(
                    &drive.path,
                    format!(
                        "Target reports removable media on unsupported bus: {:?}",
                        drive.bus_type
                    ),
                ));
            }
        } else if allow_external_usb_ssd {
            if drive.bus_type != BusType::Usb {
                return Err(InscribeError::drive_guard_violation(
                    &drive.path,
                    format!(
                        "Target is non-removable media on non-USB bus: {:?}",
                        drive.bus_type
                    ),
                ));
            }
        } else {
            return Err(InscribeError::drive_guard_violation(
                &drive.path,
                format!("Target is non-removable media on bus {:?}", drive.bus_type),
            ));
        }

        Ok(())
    }

    /// Validates a drive using this guard's configured settings.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if safety predicates fail.
    pub fn validate(&self, drive: &DriveInfo) -> Result<(), InscribeError> {
        Self::validate_target_with_options(drive, self.allow_external_usb_ssd)
    }

    /// Filters a list of drives, returning only those safe to flash under the given settings.
    #[must_use]
    pub fn filter_safe_drives(
        drives: &[DriveInfo],
        allow_external_usb_ssd: bool,
    ) -> Vec<DriveInfo> {
        drives
            .iter()
            .filter(|d| Self::validate_target_with_options(d, allow_external_usb_ssd).is_ok())
            .cloned()
            .collect()
    }

    /// Filters a list of drives, returning only those safe to flash using this guard instance.
    #[must_use]
    pub fn filter(&self, drives: &[DriveInfo]) -> Vec<DriveInfo> {
        Self::filter_safe_drives(drives, self.allow_external_usb_ssd)
    }
}

/// Predicate checking if a given mount path corresponds to an operating system host mount.
#[must_use]
pub fn is_system_mount(mount: &Path) -> bool {
    let mount_str = mount.to_string_lossy();
    let mount_trimmed = mount_str.trim();

    // Linux / Unix root and core system directories
    if mount_trimmed == "/" {
        return true;
    }

    if mount_trimmed == "/boot" || mount_trimmed.starts_with("/boot/") {
        return true;
    }

    if mount_trimmed == "/home" || mount_trimmed.starts_with("/home/") {
        return true;
    }

    if mount_trimmed == "/etc" || mount_trimmed.starts_with("/etc/") {
        return true;
    }

    if mount_trimmed == "/usr" || mount_trimmed.starts_with("/usr/") {
        return true;
    }

    if mount_trimmed == "/var" || mount_trimmed.starts_with("/var/") {
        return true;
    }

    if mount_trimmed == "/root" || mount_trimmed.starts_with("/root/") {
        return true;
    }

    // Windows system drive (case-insensitive check for C:, C:\, C:/)
    let upper = mount_trimmed.to_ascii_uppercase();
    if upper == "C:" || upper == "C:\\" || upper == "C:/" {
        return true;
    }
    if upper.starts_with("C:\\") || upper.starts_with("C:/") {
        return true;
    }

    // macOS system volumes
    if mount_trimmed == "/System" || mount_trimmed.starts_with("/System/") {
        return true;
    }
    if mount_trimmed == "/Volumes/Macintosh HD"
        || mount_trimmed.starts_with("/Volumes/Macintosh HD/")
    {
        return true;
    }
    if mount_trimmed == "/private" || mount_trimmed.starts_with("/private/") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_usb_drive() -> DriveInfo {
        DriveInfo {
            path: PathBuf::from("/dev/sdb"),
            model: "Ultra Flair".to_string(),
            vendor: "SanDisk".to_string(),
            size: 32_000_000_000,
            sector_size: 512,
            bus_type: BusType::Usb,
            is_removable: true,
            is_read_only: false,
            is_system: false,
            mount_points: vec![],
        }
    }

    #[test]
    fn test_valid_usb_drive_passes() {
        let drive = sample_usb_drive();
        assert!(DriveGuard::validate_target(&drive).is_ok());
    }

    #[test]
    fn test_system_mount_detection() {
        assert!(is_system_mount(Path::new("/")));
        assert!(is_system_mount(Path::new("/boot")));
        assert!(is_system_mount(Path::new("/boot/efi")));
        assert!(is_system_mount(Path::new("/home")));
        assert!(is_system_mount(Path::new("/home/user")));
        assert!(is_system_mount(Path::new("C:\\")));
        assert!(is_system_mount(Path::new("c:/Windows")));
        assert!(is_system_mount(Path::new("/System/Volumes/Data")));

        assert!(!is_system_mount(Path::new("/media/user/USB")));
        assert!(!is_system_mount(Path::new("/run/media/user/USB")));
        assert!(!is_system_mount(Path::new("D:\\")));
        assert!(!is_system_mount(Path::new("/Volumes/EXTERNAL")));
    }

    #[test]
    fn test_internal_nvme_and_sata_fail() {
        let mut nvme = sample_usb_drive();
        nvme.bus_type = BusType::Nvme;
        nvme.is_removable = false;
        assert!(DriveGuard::validate_target(&nvme).is_err());
        assert!(DriveGuard::validate_target_with_options(&nvme, true).is_err());

        let mut sata = sample_usb_drive();
        sata.bus_type = BusType::Sata;
        sata.is_removable = false;
        assert!(DriveGuard::validate_target(&sata).is_err());
    }

    #[test]
    fn test_zero_capacity_fails() {
        let mut drive = sample_usb_drive();
        drive.size = 0;
        assert!(DriveGuard::validate_target(&drive).is_err());
    }

    #[test]
    fn test_read_only_fails() {
        let mut drive = sample_usb_drive();
        drive.is_read_only = true;
        assert!(DriveGuard::validate_target(&drive).is_err());
    }
}
