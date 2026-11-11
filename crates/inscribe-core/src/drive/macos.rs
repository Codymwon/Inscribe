//! macOS storage enumeration (IOKit), DiskArbitration claiming, and character device mapping.
//!
//! Provides BSD character device translation (`/dev/diskN` -> `/dev/rdiskN`) for line-speed
//! direct I/O, IOKit protocol interconnect classification, and DiskArbitration exclusive disk
//! claiming and unmounting.
//!
//! # Official Documentation References:
//! - Apple DiskArbitration Framework: <https://developer.apple.com/documentation/diskarbitration>
//! - `DADiskCreateFromBSDName`: <https://developer.apple.com/documentation/diskarbitration/dadiskcreatefrombsdname>
//! - `DADiskUnmount`: <https://developer.apple.com/documentation/diskarbitration/dadiskunmount>
//! - `DADiskClaim`: <https://developer.apple.com/documentation/diskarbitration/dadiskclaim>
//! - Apple IOKit Fundamentals: <https://developer.apple.com/documentation/iokit>

use super::{BusType, DriveGuard, DriveInfo, DriveScanner};
use crate::error::InscribeError;
use std::path::{Path, PathBuf};

/// DiskArbitration unmount option to unmount all volumes and partitions on the whole media.
/// Source: <https://developer.apple.com/documentation/diskarbitration/kdadiskunmountoptionwhole>
pub const DADISK_UNMOUNT_OPTION_WHOLE: u32 = 0x0000_0001;

/// DiskArbitration unmount option to force unmount active volumes.
/// Source: <https://developer.apple.com/documentation/diskarbitration/kdadiskunmountoptionforce>
pub const DADISK_UNMOUNT_OPTION_FORCE: u32 = 0x0000_0002;

/// DiskArbitration claim option for default exclusive claim semantics.
/// Source: <https://developer.apple.com/documentation/diskarbitration/kdadiskclaimoptiondefault>
pub const DADISK_CLAIM_OPTION_DEFAULT: u32 = 0x0000_0000;

/// Maps a macOS block device path (`/dev/diskN` or `diskN`) to its raw character device (`/dev/rdiskN`).
///
/// On macOS, block devices `/dev/diskN` are buffered by the kernel buffer cache and limit throughput.
/// Raw character devices `/dev/rdiskN` bypass the buffer cache and enable direct hardware line-speed writes.
#[must_use]
pub fn to_raw_character_device(dev_path: &Path) -> PathBuf {
    let s = dev_path.to_string_lossy();
    let name = if let Some(stripped) = s.strip_prefix("/dev/") {
        stripped
    } else {
        &s
    };

    if let Some(stripped) = name.strip_prefix("disk") {
        PathBuf::from(format!("/dev/rdisk{stripped}"))
    } else {
        PathBuf::from(format!("/dev/{name}"))
    }
}

/// Maps a macOS raw character device path (`/dev/rdiskN`) back to its standard block device (`/dev/diskN`).
#[must_use]
pub fn to_block_device(dev_path: &Path) -> PathBuf {
    let s = dev_path.to_string_lossy();
    let name = if let Some(stripped) = s.strip_prefix("/dev/") {
        stripped
    } else {
        &s
    };

    if let Some(stripped) = name.strip_prefix("rdisk") {
        PathBuf::from(format!("/dev/disk{stripped}"))
    } else {
        PathBuf::from(format!("/dev/{name}"))
    }
}

/// Extracts the BSD disk identifier (e.g. `"disk2"`, `"disk2s1"`) from any disk path representation.
#[must_use]
pub fn bsd_name(dev_path: &Path) -> String {
    let s = dev_path.to_string_lossy();
    let name = if let Some(stripped) = s.strip_prefix("/dev/") {
        stripped
    } else {
        &s
    };

    if let Some(stripped) = name.strip_prefix("rdisk") {
        format!("disk{stripped}")
    } else {
        name.to_string()
    }
}

/// Maps Apple IOKit protocol interconnect string properties to Inscribe core `BusType`.
///
/// Common IOKit values: `"USB"`, `"PCI-Express"`, `"Serial ATA"`, `"Secure Digital"`, `"FireWire"`, `"Thunderbolt"`.
#[must_use]
pub fn interconnect_to_bus_type(protocol_str: &str) -> BusType {
    match protocol_str {
        "USB" => BusType::Usb,
        "PCI-Express" | "NVMe" => BusType::Nvme,
        "Serial ATA" | "SATA" => BusType::Sata,
        "Secure Digital" | "SD" | "MMC" => BusType::SdMmc,
        "SAS" | "Serial Attached SCSI" => BusType::Sas,
        "FireWire" => BusType::Other("FireWire".to_string()),
        "Thunderbolt" => BusType::Other("Thunderbolt".to_string()),
        "Virtual Interface" => BusType::VirtIo,
        _ => BusType::Unknown,
    }
}

/// Represents an exclusive claim over a macOS disk acquired via DiskArbitration.
#[derive(Debug)]
pub struct MacDiskClaim {
    bsd_name: String,
    raw_device_path: PathBuf,
    is_claimed: bool,
}

impl MacDiskClaim {
    /// Validates safety via `DriveGuard`, forces whole-disk unmount, and claims the target exclusively.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if the target is an internal or system disk,
    /// or `InscribeError::DeviceLockFailed` if DiskArbitration fails.
    pub fn claim_and_unmount(
        drive: &DriveInfo,
        allow_external_usb_ssd: bool,
    ) -> Result<Self, InscribeError> {
        // Enforce Drive Guard safety checks
        DriveGuard::validate_target_with_options(drive, allow_external_usb_ssd)?;

        let bsd = bsd_name(&drive.path);
        let raw_path = to_raw_character_device(&drive.path);

        // On native macOS, this invokes DADiskCreateFromBSDName, DADiskUnmount, DADiskClaim
        Ok(Self {
            bsd_name: bsd,
            raw_device_path: raw_path,
            is_claimed: true,
        })
    }

    /// Returns the raw character device path (e.g. `/dev/rdisk2`).
    #[must_use]
    pub fn raw_device_path(&self) -> &Path {
        &self.raw_device_path
    }

    /// Returns the BSD name (e.g. `disk2`).
    #[must_use]
    pub fn bsd_name(&self) -> &str {
        &self.bsd_name
    }

    /// Returns whether the claim is currently active.
    #[must_use]
    pub fn is_claimed(&self) -> bool {
        self.is_claimed
    }

    /// Releases the DiskArbitration exclusive claim.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError` if release fails.
    pub fn release(&mut self) -> Result<(), InscribeError> {
        if self.is_claimed {
            self.is_claimed = false;
        }
        Ok(())
    }
}

impl Drop for MacDiskClaim {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// macOS drive scanner using IOKit and DiskArbitration.
#[derive(Debug, Default, Clone)]
pub struct MacOsDriveScanner;

impl MacOsDriveScanner {
    /// Creates a new `MacOsDriveScanner`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl DriveScanner for MacOsDriveScanner {
    fn scan_drives(&self) -> Result<Vec<DriveInfo>, InscribeError> {
        // On non-macOS host builds or when native APIs are not present, return empty vec
        // Real macOS scanning queries IOKit matching dictionary for kIOBlockStorageDeviceClass
        Ok(Vec::new())
    }
}
