//! UDisks2 D-Bus client for partition unmounting and raw block device access.
//!
//! Grounded in the official UDisks2 specification:
//! - Block interface: <https://storaged.org/doc/udisks2-api/latest/gdbus-org.freedesktop.UDisks2.Block.html>
//! - Filesystem interface: <https://storaged.org/doc/udisks2-api/latest/gdbus-org.freedesktop.UDisks2.Filesystem.html>

use crate::drive::guard::DriveGuard;
use crate::drive::linux::{LinuxDriveScanner, is_partition_of};
use crate::drive::{DriveInfo, DriveScanner};
use crate::error::InscribeError;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::Path;
use zbus::zvariant::{OwnedObjectPath, Value};

/// Generates the UDisks2 D-Bus object path for a given block device path.
///
/// Canonicalizes symlinks (e.g. `/dev/disk/by-id/...` -> `/dev/sda`) to match UDisks2
/// object path conventions (`/org/freedesktop/UDisks2/block_devices/<node>`).
///
/// # Errors
///
/// Returns `InscribeError::DeviceNotFound` if the path has no valid file name,
/// or `InscribeError::UDisks` if the resulting D-Bus path is invalid.
pub fn dev_path_to_object_path(path: &Path) -> Result<OwnedObjectPath, InscribeError> {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let file_name = resolved
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| InscribeError::DeviceNotFound(path.display().to_string()))?;

    // In UDisks2, object path for block device is /org/freedesktop/UDisks2/block_devices/<name>
    // Characters outside [A-Za-z0-9_] are converted to '_' for valid D-Bus object path semantics.
    let sanitized: String = file_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    let path_str = format!("/org/freedesktop/UDisks2/block_devices/{sanitized}");
    OwnedObjectPath::try_from(path_str)
        .map_err(|e| InscribeError::UDisks(format!("Invalid object path: {e}")))
}

#[zbus::proxy(
    default_service = "org.freedesktop.UDisks2",
    interface = "org.freedesktop.UDisks2.Block"
)]
pub trait Block {
    /// Opens the block device with specified mode and options, returning a file descriptor.
    ///
    /// Source: <https://storaged.org/doc/udisks2-api/latest/gdbus-org.freedesktop.UDisks2.Block.html#gdbus-method-org-freedesktop-UDisks2-Block.OpenDevice>
    fn open_device(
        &self,
        mode: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;

    /// Read-only device property.
    #[zbus(property)]
    fn read_only(&self) -> zbus::Result<bool>;

    /// Capacity in bytes.
    #[zbus(property)]
    fn size(&self) -> zbus::Result<u64>;

    /// Kernel device file path as byte string (e.g. `/dev/sdb\0`).
    #[zbus(property)]
    fn device(&self) -> zbus::Result<Vec<u8>>;
}

#[zbus::proxy(
    default_service = "org.freedesktop.UDisks2",
    interface = "org.freedesktop.UDisks2.Filesystem"
)]
pub trait Filesystem {
    /// Unmounts the active filesystem.
    ///
    /// Source: <https://storaged.org/doc/udisks2-api/latest/gdbus-org.freedesktop.UDisks2.Filesystem.html#gdbus-method-org-freedesktop-UDisks2-Filesystem.Unmount>
    fn unmount(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    /// Array of active filesystem mount paths.
    #[zbus(property)]
    fn mount_points(&self) -> zbus::Result<Vec<Vec<u8>>>;
}

/// Client for communicating with the system `org.freedesktop.UDisks2` D-Bus daemon.
#[derive(Debug, Clone)]
pub struct UDisksClient {
    connection: zbus::Connection,
}

impl UDisksClient {
    /// Connects to the system D-Bus bus.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::UDisks` if unable to establish a system bus connection.
    pub async fn new() -> Result<Self, InscribeError> {
        let connection = zbus::Connection::system().await.map_err(|e| {
            InscribeError::UDisks(format!("Failed to connect to system D-Bus: {e}"))
        })?;
        Ok(Self { connection })
    }

    /// Wraps an existing D-Bus connection.
    #[must_use]
    pub fn from_connection(connection: zbus::Connection) -> Self {
        Self { connection }
    }

    /// Returns a reference to the underlying D-Bus connection.
    #[must_use]
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }

    /// Obtains a `BlockProxy` for the given device path.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::UDisks` or `InscribeError::DeviceNotFound`.
    pub async fn block_proxy<'a>(
        &'a self,
        dev_path: &Path,
    ) -> Result<BlockProxy<'a>, InscribeError> {
        let object_path = dev_path_to_object_path(dev_path)?;
        BlockProxy::builder(&self.connection)
            .path(object_path)
            .map_err(|e| InscribeError::UDisks(e.to_string()))?
            .build()
            .await
            .map_err(|e| InscribeError::UDisks(e.to_string()))
    }

    /// Obtains a `FilesystemProxy` for the given device path.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::UDisks` or `InscribeError::DeviceNotFound`.
    pub async fn filesystem_proxy<'a>(
        &'a self,
        dev_path: &Path,
    ) -> Result<FilesystemProxy<'a>, InscribeError> {
        let object_path = dev_path_to_object_path(dev_path)?;
        FilesystemProxy::builder(&self.connection)
            .path(object_path)
            .map_err(|e| InscribeError::UDisks(e.to_string()))?
            .build()
            .await
            .map_err(|e| InscribeError::UDisks(e.to_string()))
    }

    /// Unmounts a mounted filesystem partition.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::UDisks` if the unmount operation fails.
    pub async fn unmount_partition(&self, partition_path: &Path) -> Result<(), InscribeError> {
        let proxy = self.filesystem_proxy(partition_path).await?;
        let options = HashMap::new();
        match proxy.unmount(options).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("NotAuthorized") {
                    Err(InscribeError::PrivilegeRequired(format!(
                        "Privilege required to unmount {}: {err_str}",
                        partition_path.display()
                    )))
                } else if err_str.contains("NotMounted") || err_str.contains("not mounted") {
                    // Treat "not mounted" as success since target is already unmounted
                    Ok(())
                } else {
                    Err(InscribeError::UDisks(format!(
                        "Failed to unmount {}: {err_str}",
                        partition_path.display()
                    )))
                }
            }
        }
    }

    /// Requests a raw block file descriptor via UDisks2 `OpenDevice`.
    ///
    /// When `direct` is true, passes POSIX `O_DIRECT | O_SYNC` flags in options dictionary.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::PrivilegeRequired` if Polkit authorization is denied,
    /// or `InscribeError::UDisks` / `InscribeError::DeviceNotFound`.
    pub async fn open_block_device(
        &self,
        dev_path: &Path,
        read_write: bool,
        direct: bool,
    ) -> Result<OwnedFd, InscribeError> {
        let proxy = self.block_proxy(dev_path).await?;
        let mode = if read_write { "rw" } else { "r" };

        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        if direct {
            let flags = libc::O_DIRECT | libc::O_SYNC;
            options.insert("flags", Value::from(flags));
        }

        let zfd = proxy.open_device(mode, options).await.map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("NotAuthorized") {
                InscribeError::PrivilegeRequired(format!(
                    "Privilege required to open block device {}: {err_str}",
                    dev_path.display()
                ))
            } else {
                InscribeError::UDisks(format!(
                    "OpenDevice failed for {}: {err_str}",
                    dev_path.display()
                ))
            }
        })?;

        Ok(OwnedFd::from(zfd))
    }

    /// Safely validates with Drive Guard, performs live target attestation, unmounts active partitions,
    /// and acquires a raw block file descriptor.
    ///
    /// Fails fast if any partition unmount operation fails, preventing raw block access on active mounts.
    /// Fails closed if `/proc/mounts` cannot be read.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if target is rejected by Drive Guard or live attestation fails,
    /// `InscribeError::PrivilegeRequired` if Polkit authorization fails,
    /// or `InscribeError::UDisks` if D-Bus communication or unmounting fails.
    pub async fn unmount_and_open(
        &self,
        drive: &DriveInfo,
        direct: bool,
        allow_external_usb_ssd: bool,
    ) -> Result<OwnedFd, InscribeError> {
        self.unmount_and_open_with_scanner(drive, direct, allow_external_usb_ssd, None)
            .await
    }

    /// Extends `unmount_and_open` allowing custom live scanner injection for deterministic attestation tests.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if target is rejected by Drive Guard or live attestation fails,
    /// `InscribeError::PrivilegeRequired` if Polkit authorization fails,
    /// or `InscribeError::UDisks` if D-Bus communication or unmounting fails.
    pub async fn unmount_and_open_with_scanner(
        &self,
        drive: &DriveInfo,
        direct: bool,
        allow_external_usb_ssd: bool,
        scanner: Option<&dyn crate::drive::DriveScanner>,
    ) -> Result<OwnedFd, InscribeError> {
        // Enforce Drive Guard safety gate before touching any partition or block device
        DriveGuard::validate_target_with_options(drive, allow_external_usb_ssd)?;

        // Pre-operation live target attestation against TOCTOU device swap races
        if let Some(sc) = scanner {
            let live_drives = sc.scan_drives()?;
            let live_drive = live_drives
                .into_iter()
                .find(|d| d.path == drive.path)
                .ok_or_else(|| InscribeError::DeviceNotFound(drive.path.display().to_string()))?;
            DriveGuard::attest_target(drive, &live_drive, allow_external_usb_ssd)?;
        } else if Path::new("/sys/class/block").exists() {
            let scanner = LinuxDriveScanner::new();
            if let Ok(live_drives) = scanner.scan_drives() {
                if let Some(live_drive) = live_drives.into_iter().find(|d| d.path == drive.path) {
                    DriveGuard::attest_target(drive, &live_drive, allow_external_usb_ssd)?;
                }
            }
        }

        // Query active mounts from /proc/mounts to find partitions associated with this drive.
        // Fails closed if /proc/mounts cannot be read.
        let mounts = LinuxDriveScanner::parse_mounts(Path::new("/proc/mounts"))?;
        for (mounted_dev, _) in mounts {
            if mounted_dev == drive.path || is_partition_of(&drive.path, &mounted_dev) {
                self.unmount_partition(&mounted_dev).await?;
            }
        }

        // Open raw block device with specified flags
        self.open_block_device(&drive.path, true, direct).await
    }
}
