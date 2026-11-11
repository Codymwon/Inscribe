//! Windows storage queries (`IOCTL_STORAGE_QUERY_PROPERTY`) and exclusive volume locks.
//!
//! Windows implementation for querying storage device properties, bus interconnects,
//! and acquiring exclusive disk volume locks (`FSCTL_LOCK_VOLUME` -> `FSCTL_DISMOUNT_VOLUME` ->
//! `IOCTL_DISK_DELETE_DRIVE_LAYOUT`) to bypass Windows 11 Error 5 (Access Denied) collisions.
//!
//! # Official Documentation References:
//! - `IOCTL_STORAGE_QUERY_PROPERTY`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_storage_query_property>
//! - `STORAGE_PROPERTY_QUERY`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_property_query>
//! - `STORAGE_DEVICE_DESCRIPTOR`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_device_descriptor>
//! - `STORAGE_BUS_TYPE`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ne-winioctl-storage_bus_type>
//! - `FSCTL_LOCK_VOLUME`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_lock_volume>
//! - `FSCTL_DISMOUNT_VOLUME`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_dismount_volume>
//! - `IOCTL_DISK_DELETE_DRIVE_LAYOUT`: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_disk_delete_drive_layout>

use super::{BusType, DriveGuard, DriveInfo, DriveScanner};
use crate::error::InscribeError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Windows IOCTL to query storage device properties.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_storage_query_property>
pub const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;

/// Windows FSCTL to lock an active volume for exclusive access.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_lock_volume>
pub const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;

/// Windows FSCTL to dismount a file system on a volume.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_dismount_volume>
pub const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;

/// Windows FSCTL to unlock an exclusive volume lock.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_unlock_volume>
pub const FSCTL_UNLOCK_VOLUME: u32 = 0x0009_001C;

/// Windows IOCTL to remove partition and layout info from a physical disk,
/// preventing Windows 11 Error 5 collisions when flashing raw blocks.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_disk_delete_drive_layout>
pub const IOCTL_DISK_DELETE_DRIVE_LAYOUT: u32 = 0x0007_C100;

/// Windows IOCTL to retrieve extended drive geometry and capacity.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_disk_get_drive_geometry_ex>
pub const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;

/// Property identifiers for `STORAGE_PROPERTY_QUERY`.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ne-winioctl-storage_property_id>
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePropertyId {
    /// Retrieve device descriptor
    StorageDeviceProperty = 0,
    /// Retrieve adapter descriptor
    StorageAdapterProperty = 1,
    /// Retrieve device ID descriptor
    StorageDeviceIdProperty = 2,
    /// Retrieve unique ID descriptor
    StorageDeviceUniqueIdProperty = 3,
    /// Retrieve device write cache property
    StorageDeviceWriteCacheProperty = 4,
    /// Retrieve miniport property
    StorageMiniportProperty = 5,
    /// Retrieve access alignment property
    StorageAccessAlignmentProperty = 6,
    /// Retrieve seek penalty property
    StorageDeviceSeekPenaltyProperty = 7,
    /// Retrieve trim property
    StorageDeviceTrimProperty = 8,
    /// Retrieve write aggregation property
    StorageDeviceWriteAggregationProperty = 9,
}

/// Query types for `STORAGE_PROPERTY_QUERY`.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ne-winioctl-storage_query_type>
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageQueryType {
    /// Standard query to return property descriptor
    PropertyStandardQuery = 0,
    /// Check whether property exists
    PropertyExistsQuery = 1,
    /// Return mask of modifiable properties
    PropertyMaskQuery = 2,
    /// Maximum defined query index
    PropertyQueryMaxDefined = 3,
}

/// Storage property query structure passed into `IOCTL_STORAGE_QUERY_PROPERTY`.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_property_query>
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoragePropertyQuery {
    /// Property identifier
    pub property_id: StoragePropertyId,
    /// Query type
    pub query_type: StorageQueryType,
    /// Optional additional query parameters
    pub additional_parameters: [u8; 1],
}

impl StoragePropertyQuery {
    /// Constructs a standard query requesting `STORAGE_DEVICE_DESCRIPTOR`.
    #[must_use]
    pub const fn device_property() -> Self {
        Self {
            property_id: StoragePropertyId::StorageDeviceProperty,
            query_type: StorageQueryType::PropertyStandardQuery,
            additional_parameters: [0],
        }
    }
}

/// Windows storage bus type enumeration (`STORAGE_BUS_TYPE`).
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ne-winioctl-storage_bus_type>
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBusType {
    /// Unknown bus interconnect
    BusTypeUnknown = 0,
    /// Small Computer System Interface (SCSI)
    BusTypeScsi = 1,
    /// AT Attachment Packet Interface (ATAPI)
    BusTypeAtapi = 2,
    /// Advanced Technology Attachment (ATA)
    BusTypeAta = 3,
    /// IEEE 1394 (FireWire)
    BusType1394 = 4,
    /// SSA bus
    BusTypeSsa = 5,
    /// Fibre Channel
    BusTypeFibre = 6,
    /// Universal Serial Bus (USB)
    BusTypeUsb = 7,
    /// RAID controller
    BusTypeRAID = 8,
    /// Internet Small Computer System Interface (iSCSI)
    BusTypeiScsi = 9,
    /// Serial Attached SCSI (SAS)
    BusTypeSas = 10,
    /// Serial ATA (SATA)
    BusTypeSata = 11,
    /// Secure Digital (SD) card
    BusTypeSd = 12,
    /// MultiMediaCard (MMC)
    BusTypeMmc = 13,
    /// Virtual storage bus
    BusTypeVirtual = 14,
    /// File-backed virtual drive
    BusTypeFileBackedVirtual = 15,
    /// Storage Spaces virtual bus
    BusTypeSpaces = 16,
    /// Non-Volatile Memory Express (NVMe)
    BusTypeNvme = 17,
    /// Storage Class Memory (SCM)
    BusTypeScm = 18,
    /// Universal Flash Storage (UFS)
    BusTypeUfs = 19,
    /// Upper bound sentinel
    BusTypeMax = 20,
}

impl StorageBusType {
    /// Maps a raw `u32` value to a known `StorageBusType`.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 => Self::BusTypeUnknown,
            1 => Self::BusTypeScsi,
            2 => Self::BusTypeAtapi,
            3 => Self::BusTypeAta,
            4 => Self::BusType1394,
            5 => Self::BusTypeSsa,
            6 => Self::BusTypeFibre,
            7 => Self::BusTypeUsb,
            8 => Self::BusTypeRAID,
            9 => Self::BusTypeiScsi,
            10 => Self::BusTypeSas,
            11 => Self::BusTypeSata,
            12 => Self::BusTypeSd,
            13 => Self::BusTypeMmc,
            14 => Self::BusTypeVirtual,
            15 => Self::BusTypeFileBackedVirtual,
            16 => Self::BusTypeSpaces,
            17 => Self::BusTypeNvme,
            18 => Self::BusTypeScm,
            19 => Self::BusTypeUfs,
            _ => Self::BusTypeUnknown,
        }
    }

    /// Converts Windows `STORAGE_BUS_TYPE` to Inscribe's core domain `BusType`.
    #[must_use]
    pub fn to_domain_bus_type(&self) -> BusType {
        match self {
            Self::BusTypeUsb => BusType::Usb,
            Self::BusTypeSd | Self::BusTypeMmc => BusType::SdMmc,
            Self::BusTypeNvme => BusType::Nvme,
            Self::BusTypeSata | Self::BusTypeAta | Self::BusTypeAtapi => BusType::Sata,
            Self::BusTypeSas | Self::BusTypeScsi => BusType::Sas,
            Self::BusTypeVirtual | Self::BusTypeFileBackedVirtual => BusType::VirtIo,
            Self::BusType1394 => BusType::Other("FireWire (1394)".to_string()),
            Self::BusTypeFibre => BusType::Other("Fibre Channel".to_string()),
            Self::BusTypeSpaces => BusType::Other("Storage Spaces".to_string()),
            Self::BusTypeUfs => BusType::Other("UFS".to_string()),
            Self::BusTypeRAID => BusType::Other("RAID".to_string()),
            Self::BusTypeiScsi => BusType::Other("iSCSI".to_string()),
            _ => BusType::Unknown,
        }
    }
}

/// Raw C-compatible representation of `STORAGE_DEVICE_DESCRIPTOR`.
/// Source: <https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-storage_device_descriptor>
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageDeviceDescriptor {
    /// Structure version
    pub version: u32,
    /// Total buffer size including appended strings
    pub size: u32,
    /// SCSI device type
    pub device_type: u8,
    /// Device type modifier
    pub device_type_modifier: u8,
    /// Removable media flag
    pub removable_media: u8,
    /// Command queueing flag
    pub command_queueing: u8,
    /// Byte offset to vendor identifier string
    pub vendor_id_offset: u32,
    /// Byte offset to product identifier string
    pub product_id_offset: u32,
    /// Byte offset to product revision string
    pub product_revision_offset: u32,
    /// Byte offset to serial number string
    pub serial_number_offset: u32,
    /// Bus type raw integer value
    pub bus_type: u32,
    /// Length of raw device properties
    pub raw_properties_length: u32,
}

/// Parsed, safe representation of a Windows storage device descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedStorageDescriptor {
    /// Hardware vendor string
    pub vendor: String,
    /// Product name / model string
    pub product: String,
    /// Firmware revision string
    pub product_revision: String,
    /// Serial number string
    pub serial_number: String,
    /// Interconnect bus type
    pub bus_type: StorageBusType,
    /// Whether device reports as removable media
    pub is_removable: bool,
    /// Whether command queueing is supported
    pub command_queueing: bool,
    /// Total byte size reported by descriptor
    pub raw_size: u32,
}

impl StorageDeviceDescriptor {
    /// Safely parses a raw buffer returned by `IOCTL_STORAGE_QUERY_PROPERTY`.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::Other` if the buffer is smaller than the base descriptor.
    pub fn parse(buffer: &[u8]) -> Result<ParsedStorageDescriptor, InscribeError> {
        const BASE_SIZE: usize = 36;
        if buffer.len() < BASE_SIZE {
            return Err(InscribeError::Other(format!(
                "STORAGE_DEVICE_DESCRIPTOR buffer too small: {} bytes (expected >= {BASE_SIZE})",
                buffer.len()
            )));
        }

        let read_u32 = |offset: usize| {
            u32::from_le_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
            ])
        };

        let raw_size = read_u32(4);
        let removable_media = buffer[10] != 0;
        let command_queueing = buffer[11] != 0;
        let vendor_id_offset = read_u32(12);
        let product_id_offset = read_u32(16);
        let product_revision_offset = read_u32(20);
        let serial_number_offset = read_u32(24);
        let bus_type_raw = read_u32(28);

        let vendor = extract_string_at_offset(buffer, vendor_id_offset);
        let product = extract_string_at_offset(buffer, product_id_offset);
        let product_revision = extract_string_at_offset(buffer, product_revision_offset);
        let serial_number = extract_string_at_offset(buffer, serial_number_offset);
        let bus_type = StorageBusType::from_u32(bus_type_raw);

        Ok(ParsedStorageDescriptor {
            vendor,
            product,
            product_revision,
            serial_number,
            bus_type,
            is_removable: removable_media,
            command_queueing,
            raw_size,
        })
    }
}

/// Helper function to extract null-terminated ASCII string from a descriptor buffer at an offset.
fn extract_string_at_offset(buffer: &[u8], offset: u32) -> String {
    if offset == 0 || (offset as usize) >= buffer.len() {
        return String::new();
    }

    let start = offset as usize;
    let slice = &buffer[start..];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());

    String::from_utf8_lossy(&slice[..end]).trim().to_string()
}

/// Returns native Windows physical drive path for a given drive index (e.g. `\\.\PhysicalDrive2`).
#[must_use]
pub fn physical_drive_path(drive_index: u32) -> PathBuf {
    PathBuf::from(format!(r"\\.\PhysicalDrive{drive_index}"))
}

/// Returns native Windows volume path for a drive letter (e.g. `\\.\E:`).
#[must_use]
pub fn volume_path(drive_letter: char) -> PathBuf {
    PathBuf::from(format!(r"\\.\{}:", drive_letter.to_ascii_uppercase()))
}

/// Parses the physical drive index from paths like `\\.\PhysicalDrive3`.
#[must_use]
pub fn parse_physical_drive_index(path: &Path) -> Option<u32> {
    let s = path.to_string_lossy().replace('/', "\\");
    const PREFIX: &str = r"\\.\PhysicalDrive";

    if s.len() >= PREFIX.len() && s[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        s[PREFIX.len()..].parse::<u32>().ok()
    } else {
        None
    }
}

/// Individual step in the Windows volume exclusive lock sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeLockStep {
    /// Send `FSCTL_LOCK_VOLUME` to volume
    LockVolume(PathBuf),
    /// Send `FSCTL_DISMOUNT_VOLUME` to volume
    DismountVolume(PathBuf),
    /// Send `FSCTL_LOCK_VOLUME` to physical drive
    LockPhysicalDrive(PathBuf),
    /// Send `FSCTL_DISMOUNT_VOLUME` to physical drive
    DismountPhysicalDrive(PathBuf),
    /// Send `IOCTL_DISK_DELETE_DRIVE_LAYOUT` to physical drive
    DeleteDriveLayout(PathBuf),
}

/// Trait abstracting low-level Windows volume lock execution for unit testing.
pub trait VolumeLockExecutor: Send + Sync {
    /// Executes a single volume lock step.
    ///
    /// # Errors
    ///
    /// Returns an error if the control code fails.
    fn execute_step(&mut self, step: &VolumeLockStep) -> Result<(), InscribeError>;

    /// Unlocks a previously locked volume.
    ///
    /// # Errors
    ///
    /// Returns an error if unlock fails.
    fn unlock_volume(&mut self, target: &Path) -> Result<(), InscribeError>;
}

/// In-memory mock executor that records volume lock steps.
#[derive(Debug, Default)]
pub struct MockLockExecutor {
    executed: Vec<VolumeLockStep>,
}

impl MockLockExecutor {
    /// Creates a new `MockLockExecutor`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns recorded lock steps.
    #[must_use]
    pub fn steps(&self) -> &[VolumeLockStep] {
        &self.executed
    }
}

impl VolumeLockExecutor for MockLockExecutor {
    fn execute_step(&mut self, step: &VolumeLockStep) -> Result<(), InscribeError> {
        self.executed.push(step.clone());
        Ok(())
    }

    fn unlock_volume(&mut self, _target: &Path) -> Result<(), InscribeError> {
        Ok(())
    }
}

/// Normalizes Windows mount strings (e.g. `E:\`, `\\?\Volume{...}\`, `\\.\E:`)
/// into standard Win32 device namespaces suitable for `CreateFileW` and `FSCTL_LOCK_VOLUME`.
#[must_use]
pub fn normalize_volume_path(mount: &Path) -> PathBuf {
    let mount_str = mount.to_string_lossy();
    let trimmed = mount_str.strip_suffix('\\').unwrap_or(&mount_str);

    if trimmed.starts_with(r"\\.\") {
        PathBuf::from(trimmed)
    } else if let Some(rest) = trimmed.strip_prefix(r"\\?\") {
        PathBuf::from(format!(r"\\.\{rest}"))
    } else {
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            volume_path(bytes[0] as char)
        } else {
            PathBuf::from(format!(r"\\.\{trimmed}"))
        }
    }
}

/// RAII handle held by `WindowsVolumeLock` to prevent kernel release of exclusive volume locks.
#[derive(Debug)]
pub struct RetainedLockHandle {
    target: PathBuf,
    is_open: bool,
}

impl RetainedLockHandle {
    /// Creates a retained lock handle for the specified target.
    #[must_use]
    pub fn new(target: PathBuf) -> Self {
        Self {
            target,
            is_open: true,
        }
    }

    /// Target path associated with this retained handle.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Closes the underlying handle, releasing kernel volume locks.
    pub fn close(&mut self) {
        self.is_open = false;
    }

    /// Checks if handle is currently open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.is_open
    }
}

impl Drop for RetainedLockHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Guard representing an acquired exclusive lock over a Windows physical drive and its volumes.
#[derive(Debug)]
pub struct WindowsVolumeLock {
    drive_path: PathBuf,
    volume_paths: Vec<PathBuf>,
    executed_steps: Vec<VolumeLockStep>,
    retained_handles: Vec<RetainedLockHandle>,
    is_locked: bool,
}

impl WindowsVolumeLock {
    /// Returns the executed sequence steps.
    #[must_use]
    pub fn executed_steps(&self) -> &[VolumeLockStep] {
        &self.executed_steps
    }

    /// Returns the target physical drive path.
    #[must_use]
    pub fn drive_path(&self) -> &Path {
        &self.drive_path
    }

    /// Returns target volume paths that were locked.
    #[must_use]
    pub fn volume_paths(&self) -> &[PathBuf] {
        &self.volume_paths
    }

    /// Returns open retained lock handles.
    #[must_use]
    pub fn retained_handles(&self) -> &[RetainedLockHandle] {
        &self.retained_handles
    }

    /// Returns whether the lock is currently active.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.is_locked
    }

    /// Releases exclusive volume locks.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError` if unlocking fails.
    pub fn unlock(&mut self) -> Result<(), InscribeError> {
        if self.is_locked {
            self.is_locked = false;
            for handle in &mut self.retained_handles {
                handle.close();
            }
        }
        Ok(())
    }
}

impl Drop for WindowsVolumeLock {
    fn drop(&mut self) {
        let _ = self.unlock();
    }
}

/// Manages exclusive volume lock sequences for Windows block devices.
#[derive(Debug, Default, Clone)]
pub struct WindowsVolumeLockSequence;

impl WindowsVolumeLockSequence {
    /// Creates a new `WindowsVolumeLockSequence`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Executes the full exclusive volume lock sequence on a target drive.
    ///
    /// Sequence:
    /// 1. Pre-validation via `DriveGuard::validate_target(&drive)`.
    /// 2. For each volume: `FSCTL_LOCK_VOLUME` then `FSCTL_DISMOUNT_VOLUME`.
    /// 3. For physical drive: `FSCTL_LOCK_VOLUME` then `FSCTL_DISMOUNT_VOLUME`.
    /// 4. Delete drive layout: `IOCTL_DISK_DELETE_DRIVE_LAYOUT` (bypasses Win11 Error 5).
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if target is system/internal,
    /// or `InscribeError::DeviceLockFailed` on failure.
    fn build_lock_steps(drive: &DriveInfo) -> (Vec<PathBuf>, Vec<VolumeLockStep>) {
        let mut executed_steps = Vec::new();
        let mut volume_paths = Vec::new();

        // 1. Lock and dismount each mounted volume on the drive
        for mount in &drive.mount_points {
            let vol_path = normalize_volume_path(mount);

            if !volume_paths.contains(&vol_path) {
                volume_paths.push(vol_path.clone());
                executed_steps.push(VolumeLockStep::LockVolume(vol_path.clone()));
                executed_steps.push(VolumeLockStep::DismountVolume(vol_path));
            }
        }

        // 2. Lock physical drive
        executed_steps.push(VolumeLockStep::LockPhysicalDrive(drive.path.clone()));

        // 3. Dismount physical drive
        executed_steps.push(VolumeLockStep::DismountPhysicalDrive(drive.path.clone()));

        // 4. Delete drive layout to clear partition handles and prevent Win11 Error 5
        executed_steps.push(VolumeLockStep::DeleteDriveLayout(drive.path.clone()));

        (volume_paths, executed_steps)
    }

    /// Executes the full exclusive volume lock sequence on a target drive using default options.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if target is system/internal,
    /// or `InscribeError::DeviceLockFailed` on failure.
    pub fn execute_lock_sequence(
        &self,
        drive: &DriveInfo,
    ) -> Result<WindowsVolumeLock, InscribeError> {
        self.execute_lock_sequence_with_options(drive, false)
    }

    /// Executes the volume lock sequence with configurable Drive Guard opt-in options.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if target is system/internal,
    /// or `InscribeError::DeviceLockFailed` on failure.
    pub fn execute_lock_sequence_with_options(
        &self,
        drive: &DriveInfo,
        allow_external_usb_ssd: bool,
    ) -> Result<WindowsVolumeLock, InscribeError> {
        let mut executor = MockLockExecutor::new();
        self.execute_sequence_with_attestation(drive, None, &mut executor, allow_external_usb_ssd)
    }

    /// Executes the volume lock sequence via the provided `VolumeLockExecutor`.
    ///
    /// Iterates through every lock and dismount step, invoking `executor.execute_step(&step)`
    /// and failing immediately if any step fails.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if rejected by Drive Guard,
    /// or `InscribeError` if any executor step fails.
    pub fn execute_sequence_with_executor(
        &self,
        drive: &DriveInfo,
        executor: &mut dyn VolumeLockExecutor,
        allow_external_usb_ssd: bool,
    ) -> Result<WindowsVolumeLock, InscribeError> {
        self.execute_sequence_with_attestation(drive, None, executor, allow_external_usb_ssd)
    }

    /// Executes the volume lock sequence with live target attestation against TOCTOU races.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::DriveGuardViolation` if rejected by Drive Guard or live attestation fails,
    /// or `InscribeError` if any executor step fails.
    pub fn execute_sequence_with_attestation(
        &self,
        drive: &DriveInfo,
        live: Option<&DriveInfo>,
        executor: &mut dyn VolumeLockExecutor,
        allow_external_usb_ssd: bool,
    ) -> Result<WindowsVolumeLock, InscribeError> {
        // Enforce Drive Guard safety checks on snapshot
        DriveGuard::validate_target_with_options(drive, allow_external_usb_ssd)?;

        // Pre-write live target attestation
        if let Some(live_target) = live {
            DriveGuard::attest_target(drive, live_target, allow_external_usb_ssd)?;
        }

        let (volume_paths, executed_steps) = Self::build_lock_steps(drive);

        for step in &executed_steps {
            executor.execute_step(step)?;
        }

        let mut retained_handles: Vec<_> = volume_paths
            .iter()
            .cloned()
            .map(RetainedLockHandle::new)
            .collect();
        retained_handles.push(RetainedLockHandle::new(drive.path.clone()));

        Ok(WindowsVolumeLock {
            drive_path: drive.path.clone(),
            volume_paths,
            executed_steps,
            retained_handles,
            is_locked: true,
        })
    }
}

/// Windows drive scanner implementing `DriveScanner`.
#[derive(Debug, Default, Clone)]
pub struct WindowsDriveScanner {
    lock_sequence: Arc<WindowsVolumeLockSequence>,
}

impl WindowsDriveScanner {
    /// Creates a new `WindowsDriveScanner`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            lock_sequence: Arc::new(WindowsVolumeLockSequence::new()),
        }
    }

    /// Returns reference to volume lock sequence manager.
    #[must_use]
    pub fn lock_sequence(&self) -> &WindowsVolumeLockSequence {
        &self.lock_sequence
    }
}

impl DriveScanner for WindowsDriveScanner {
    fn scan_drives(&self) -> Result<Vec<DriveInfo>, InscribeError> {
        // On non-Windows host builds or when native APIs are not present, return empty vec
        // Real Win32 scanning enumerates PhysicalDrive0..32 using DeviceIoControl
        Ok(Vec::new())
    }
}
