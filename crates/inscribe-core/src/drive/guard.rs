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

        // Rule 7: Whole-disk target geometry assertion (reject individual partitions or volumes)
        if is_partition_path(&drive.path) {
            return Err(InscribeError::drive_guard_violation(
                &drive.path,
                "Target is a partition or volume, not a whole physical block device",
            ));
        }

        Ok(())
    }

    /// Attests a live device against an earlier scanned snapshot to eliminate TOCTOU device swap races.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if the live device violates Drive Guard predicates,
    /// or if its hardware attributes (bus type, capacity, sector size, removable flag, vendor, model)
    /// differ from the snapshot.
    pub fn attest_target(
        snapshot: &DriveInfo,
        live: &DriveInfo,
        allow_external_usb_ssd: bool,
    ) -> Result<(), InscribeError> {
        // 1. Live target must independently pass all Drive Guard safety rules
        Self::validate_target_with_options(live, allow_external_usb_ssd)?;

        // 2. Paths must match
        if snapshot.path != live.path {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target path mismatch: scan was {}, live is {}",
                    snapshot.path.display(),
                    live.path.display()
                ),
            ));
        }

        // 3. Bus interconnect must not have mutated
        if snapshot.bus_type != live.bus_type {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target bus type mismatch (device swap detected): scan was {:?}, live is {:?}",
                    snapshot.bus_type, live.bus_type
                ),
            ));
        }

        // 4. Capacity must match bit-for-bit
        if snapshot.size != live.size {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target capacity mismatch (device swap detected): scan was {} bytes, live is {} bytes",
                    snapshot.size, live.size
                ),
            ));
        }

        // 5. Removable media flag must match
        if snapshot.is_removable != live.is_removable {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target removable status changed: scan was {}, live is {}",
                    snapshot.is_removable, live.is_removable
                ),
            ));
        }

        // 6. Sector size must match
        if snapshot.sector_size != live.sector_size {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target sector size mismatch: scan was {}, live is {}",
                    snapshot.sector_size, live.sector_size
                ),
            ));
        }

        // 7. Vendor and model must match if populated
        if !snapshot.vendor.is_empty() && !live.vendor.is_empty() && snapshot.vendor != live.vendor
        {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target vendor mismatch: scan was '{}', live is '{}'",
                    snapshot.vendor, live.vendor
                ),
            ));
        }
        if !snapshot.model.is_empty() && !live.model.is_empty() && snapshot.model != live.model {
            return Err(InscribeError::drive_guard_violation(
                &live.path,
                format!(
                    "Live target model mismatch: scan was '{}', live is '{}'",
                    snapshot.model, live.model
                ),
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

    if mount_trimmed == "/" {
        return true;
    }

    // Standard Unix / macOS system directories
    const SYSTEM_DIRECTORIES: &[&str] = &[
        "/boot",
        "/home",
        "/etc",
        "/usr",
        "/var",
        "/root",
        "/System",
        "/Volumes/Macintosh HD",
        "/private",
    ];

    for &dir in SYSTEM_DIRECTORIES {
        if mount_trimmed == dir
            || mount_trimmed
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        {
            return true;
        }
    }

    // Windows system drive (case-insensitive check for C:, C:\..., C:/...)
    let upper = mount_trimmed.to_ascii_uppercase();
    if upper == "C:" || upper.starts_with("C:\\") || upper.starts_with("C:/") {
        return true;
    }

    // Active Linux / Unix swap space
    mount_trimmed == "[SWAP]" || mount_trimmed.eq_ignore_ascii_case("swap")
}

/// Predicate checking if a device path represents a partition or volume rather than a whole block device.
#[must_use]
pub fn is_partition_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let trimmed = path_str.trim();

    // Windows checks:
    // Volume GUIDs: \\?\Volume{...} or \\.\Volume{...}
    if trimmed.starts_with(r"\\?\Volume{") || trimmed.starts_with(r"\\.\Volume{") {
        return true;
    }
    // Drive letters: E:, \\.\E:, C:\, etc.
    let upper = trimmed.to_ascii_uppercase();
    let stripped = upper.strip_prefix(r"\\.\").unwrap_or(&upper);
    if stripped.len() >= 2
        && stripped.as_bytes()[0].is_ascii_alphabetic()
        && stripped.as_bytes()[1] == b':'
    {
        return true;
    }

    // macOS checks: /dev/disk2s1 or /dev/rdisk2s1
    let file_name = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let disk_part = file_name.strip_prefix('r').unwrap_or(file_name);
    if let Some(rest) = disk_part.strip_prefix("disk") {
        if let Some((_disk_idx, part_idx)) = rest.split_once('s') {
            if !part_idx.is_empty() && part_idx.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }

    // Linux checks:
    // /dev/sdb1, /dev/hda2, /dev/vda1, /dev/nvme0n1p1, /dev/mmcblk0p1, /dev/loop0p1
    if let Some(dev) = trimmed.strip_prefix("/dev/") {
        // SCSI/SATA/USB (sd), IDE (hd), VirtIO (vd), Xen (xvd)
        for prefix in &["sd", "hd", "vd", "xvd"] {
            if let Some(rest) = dev.strip_prefix(prefix) {
                let letters_end = rest.find(|c: char| c.is_ascii_digit());
                if let Some(pos) = letters_end {
                    if pos > 0 && rest[pos..].chars().all(|c| c.is_ascii_digit()) {
                        return true;
                    }
                }
            }
        }

        // NVMe / MMC / Loop with 'p' separator: e.g. nvme0n1p1, mmcblk0p1
        if let Some((prefix, part_num)) = dev.rsplit_once('p') {
            if !part_num.is_empty()
                && part_num.chars().all(|c| c.is_ascii_digit())
                && prefix.chars().last().is_some_and(|c| c.is_ascii_digit())
            {
                return true;
            }
        }
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
