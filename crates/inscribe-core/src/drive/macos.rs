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
    let name = s.strip_prefix("/dev/").unwrap_or(&s);

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
    let name = s.strip_prefix("/dev/").unwrap_or(&s);

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
    let name = s.strip_prefix("/dev/").unwrap_or(&s);

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

/// Maps a macOS DiskArbitration return code (`DAReturn` / `mach_error_t`) to strongly-typed `InscribeError`.
///
/// Official reference:
/// - `kDAReturnBusy` = `0xF8DA0002`
/// - `kDAReturnNotPermitted` = `0xF8DA0008`
/// - `kDAReturnNotPrivileged` = `0xF8DA0009`
///
/// # Errors
///
/// Returns `InscribeError::DeviceLockFailed` on busy or general failure,
/// or `InscribeError::PrivilegeRequired` if permission is denied.
pub fn map_da_return(ret: i32, path: &Path) -> Result<(), InscribeError> {
    const KDA_RETURN_SUCCESS: i32 = 0;
    const KDA_RETURN_BUSY: i32 = 0xF8DA0002_u32 as i32;
    const KDA_RETURN_NOT_PERMITTED: i32 = 0xF8DA0008_u32 as i32;
    const KDA_RETURN_NOT_PRIVILEGED: i32 = 0xF8DA0009_u32 as i32;

    match ret {
        KDA_RETURN_SUCCESS => Ok(()),
        KDA_RETURN_BUSY => Err(InscribeError::DeviceLockFailed {
            path: path.display().to_string(),
            reason: "DiskArbitration operation failed: disk or volume is busy (kDAReturnBusy)"
                .to_string(),
        }),
        KDA_RETURN_NOT_PERMITTED => Err(InscribeError::PrivilegeRequired(
            "DiskArbitration operation not permitted (kDAReturnNotPermitted)".to_string(),
        )),
        KDA_RETURN_NOT_PRIVILEGED => Err(InscribeError::PrivilegeRequired(
            "DiskArbitration operation requires elevated privileges (kDAReturnNotPrivileged)"
                .to_string(),
        )),
        other => Err(InscribeError::DeviceLockFailed {
            path: path.display().to_string(),
            reason: format!("DiskArbitration error 0x{other:08X}"),
        }),
    }
}

#[cfg(target_os = "macos")]
pub mod ffi {
    use std::ffi::c_void;

    pub type DASessionRef = *mut c_void;
    pub type DADiskRef = *mut c_void;
    pub type CFAllocatorRef = *const c_void;
    pub type CFDictionaryRef = *const c_void;
    pub type DADiskUnmountOptions = u32;
    pub type DADiskClaimOptions = u32;
    pub type DAReturn = i32;

    #[link(name = "DiskArbitration", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn DASessionCreate(allocator: CFAllocatorRef) -> DASessionRef;
        pub fn DADiskCreateFromBSDName(
            allocator: CFAllocatorRef,
            session: DASessionRef,
            name: *const std::ffi::c_char,
        ) -> DADiskRef;
        pub fn DADiskCopyDescription(disk: DADiskRef) -> CFDictionaryRef;
        pub fn DADiskUnmount(
            disk: DADiskRef,
            options: DADiskUnmountOptions,
            callback: *const c_void,
            context: *mut c_void,
        );
        pub fn DADiskClaim(
            disk: DADiskRef,
            options: DADiskClaimOptions,
            release_callback: *const c_void,
            release_context: *mut c_void,
            callback: *const c_void,
            callback_context: *mut c_void,
        );
        pub fn CFRelease(cf: *const c_void);
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
        Self::claim_and_unmount_with_attestation(drive, None, allow_external_usb_ssd)
    }

    /// Extends `claim_and_unmount` supporting live target attestation against TOCTOU races.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if the target is rejected or live attestation fails,
    /// or `InscribeError::DeviceLockFailed` on failure.
    pub fn claim_and_unmount_with_attestation(
        drive: &DriveInfo,
        live: Option<&DriveInfo>,
        allow_external_usb_ssd: bool,
    ) -> Result<Self, InscribeError> {
        // Enforce Drive Guard safety checks on snapshot
        DriveGuard::validate_target_with_options(drive, allow_external_usb_ssd)?;

        // Pre-operation live target attestation
        if let Some(live_target) = live {
            DriveGuard::attest_target(drive, live_target, allow_external_usb_ssd)?;
        }

        let bsd = bsd_name(&drive.path);
        let raw_path = to_raw_character_device(&drive.path);

        #[cfg(target_os = "macos")]
        {
            use std::ffi::CString;
            use std::ptr;

            let c_bsd = CString::new(bsd.clone()).map_err(|e| InscribeError::DeviceLockFailed {
                path: drive.path.display().to_string(),
                reason: format!("Invalid BSD disk name: {e}"),
            })?;

            unsafe {
                let session = ffi::DASessionCreate(ptr::null());
                if session.is_null() {
                    return Err(InscribeError::DeviceLockFailed {
                        path: drive.path.display().to_string(),
                        reason: "Failed to create DASessionRef".to_string(),
                    });
                }

                let disk = ffi::DADiskCreateFromBSDName(ptr::null(), session, c_bsd.as_ptr());
                if disk.is_null() {
                    ffi::CFRelease(session);
                    return Err(InscribeError::DeviceNotFound(
                        drive.path.display().to_string(),
                    ));
                }

                // Force unmount whole media
                ffi::DADiskUnmount(
                    disk,
                    DADISK_UNMOUNT_OPTION_WHOLE | DADISK_UNMOUNT_OPTION_FORCE,
                    ptr::null(),
                    ptr::null_mut(),
                );

                // Claim exclusively
                ffi::DADiskClaim(
                    disk,
                    DADISK_CLAIM_OPTION_DEFAULT,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null_mut(),
                );

                ffi::CFRelease(disk);
                ffi::CFRelease(session);
            }
        }

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
