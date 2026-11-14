//! Core drive detection, storage hardware domain models, and Drive Guard safety engine.

pub mod guard;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod macos;
pub mod mock;
#[cfg(target_os = "linux")]
pub mod udisks;
pub mod windows;

pub use guard::DriveGuard;
#[cfg(target_os = "linux")]
pub use linux::LinuxDriveScanner;
pub use macos::MacOsDriveScanner;
pub use mock::{DriveScanner, MockDriveScanner};
#[cfg(target_os = "linux")]
pub use udisks::UDisksClient;
pub use windows::WindowsDriveScanner;

/// Returns the default platform block drive scanner.
#[must_use]
pub fn default_scanner() -> Box<dyn DriveScanner> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxDriveScanner::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsDriveScanner::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacOsDriveScanner::new())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Box::new(MockDriveScanner::empty())
    }
}

/// Invokes platform volume unmounting and exclusive lock acquisition prior to raw block writing.
///
/// Strictly enforces Drive Guard validation before any volume lock or unmount is executed.
///
/// # Errors
/// Returns `InscribeError::DriveGuardViolation` if target is system/internal,
/// or `InscribeError` if volume unmounting or lock acquisition fails.
pub fn prepare_drive_for_writing(
    drive: &DriveInfo,
    allow_external_usb_ssd: bool,
) -> Result<(), crate::error::InscribeError> {
    DriveGuard::validate_target_with_options(drive, allow_external_usb_ssd)?;

    #[cfg(target_os = "windows")]
    {
        let _ = windows::WindowsVolumeLockSequence::new()
            .execute_lock_sequence_with_options(drive, allow_external_usb_ssd)?;
    }

    #[cfg(target_os = "macos")]
    {
        let _ = macos::MacDiskClaim::claim_and_unmount(drive, allow_external_usb_ssd)?;
    }

    #[cfg(target_os = "linux")]
    {
        unmount_linux_drive_partitions(drive);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn unmount_linux_drive_partitions(drive: &DriveInfo) {
    if let Ok(mounts) = linux::LinuxDriveScanner::parse_mounts(std::path::Path::new("/proc/mounts"))
    {
        let partitions: Vec<PathBuf> = mounts
            .into_keys()
            .filter_map(|dev| {
                if dev == drive.path || linux::is_partition_of(&drive.path, &dev) {
                    Some(dev)
                } else {
                    None
                }
            })
            .collect();

        if !partitions.is_empty() {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async {
                    if let Ok(client) = UDisksClient::new().await {
                        for part in &partitions {
                            let _ = client.unmount_partition(part).await;
                        }
                    }
                });
            }
        }
    }
}

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Storage bus interconnect protocol detected from host controller and sysfs/IOKit/Win32 APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusType {
    /// Universal Serial Bus flash drives, external hard drives, or enclosures.
    Usb,
    /// Secure Digital card or MultiMediaCard (integrated or external reader).
    SdMmc,
    /// Non-Volatile Memory Express internal SSD.
    Nvme,
    /// Serial ATA internal drive.
    Sata,
    /// Serial Attached SCSI internal enterprise storage.
    Sas,
    /// Virtualized I/O block device (e.g. QEMU/KVM, VirtIO).
    VirtIo,
    /// Alternate bus descriptor (e.g. Thunderbolt, FireWire).
    Other(String),
    /// Unidentified or indeterminate bus interconnect.
    Unknown,
}

impl BusType {
    /// Check whether this bus is typically removable and safe for external media flashing.
    #[must_use]
    pub fn is_removable_bus(&self) -> bool {
        matches!(self, Self::Usb | Self::SdMmc)
    }

    /// Check whether this bus corresponds to internal host storage.
    #[must_use]
    pub fn is_internal_bus(&self) -> bool {
        matches!(self, Self::Nvme | Self::Sata | Self::Sas | Self::VirtIo)
    }
}

/// Normalized description of a physical block storage drive detected on the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveInfo {
    /// Native system device path (e.g. `/dev/sdb`, `\\\\.\\PhysicalDrive2`, `/dev/disk2`).
    pub path: PathBuf,
    /// Human-readable device model or product name.
    pub model: String,
    /// Hardware manufacturer / vendor name.
    pub vendor: String,
    /// Total storage capacity in bytes.
    pub size: u64,
    /// Physical block/sector size in bytes (e.g., 512, 4096).
    pub sector_size: u32,
    /// Detected hardware bus interconnect.
    pub bus_type: BusType,
    /// Flag indicating whether hardware reports as removable media.
    pub is_removable: bool,
    /// Flag indicating whether the drive is hardware or firmware write-protected.
    pub is_read_only: bool,
    /// Flag indicating whether Drive Guard identified this disk as backing host OS root/mounts.
    pub is_system: bool,
    /// Currently active filesystem mount points associated with this drive or its partitions.
    pub mount_points: Vec<PathBuf>,
}

impl DriveInfo {
    /// Determines if this drive qualifies as an "External USB SSD"
    /// (connected via USB, non-removable bit set by bridge controller, not a host system drive).
    #[must_use]
    pub fn is_external_usb_ssd(&self) -> bool {
        self.bus_type == BusType::Usb && !self.is_removable && !self.is_system
    }

    /// Determines whether this target is safe and eligible for flashing.
    #[must_use]
    pub fn is_flashable(&self, allow_usb_ssd: bool) -> bool {
        DriveGuard::validate_target_with_options(self, allow_usb_ssd).is_ok()
    }

    /// Returns a human-friendly formatted representation of the drive capacity (e.g. "32.0 GB").
    #[must_use]
    pub fn formatted_size(&self) -> String {
        const KB: f64 = 1_000.0;
        const MB: f64 = KB * 1_000.0;
        const GB: f64 = MB * 1_000.0;
        const TB: f64 = GB * 1_000.0;

        let size = self.size as f64;
        if size >= TB {
            format!("{:.1} TB", size / TB)
        } else if size >= GB {
            format!("{:.1} GB", size / GB)
        } else if size >= MB {
            format!("{:.1} MB", size / MB)
        } else if size >= KB {
            format!("{:.1} KB", size / KB)
        } else {
            format!("{} B", self.size)
        }
    }
}
