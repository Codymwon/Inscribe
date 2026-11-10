//! Mock drive scanner implementation for deterministic unit and integration testing.

use super::DriveInfo;
use crate::error::InscribeError;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Trait abstracting physical and virtual block storage drive enumeration.
pub trait DriveScanner: Send + Sync {
    /// Enumerates and returns detected physical storage drives on the host.
    ///
    /// # Errors
    ///
    /// Returns an `InscribeError` if low-level OS scanning or D-Bus communication fails.
    fn scan_drives(&self) -> Result<Vec<DriveInfo>, InscribeError>;
}

/// Deterministic in-memory drive scanner for unit testing and UI simulation.
#[derive(Debug, Default, Clone)]
pub struct MockDriveScanner {
    drives: Arc<Mutex<Vec<DriveInfo>>>,
}

impl MockDriveScanner {
    /// Creates a new `MockDriveScanner` initialized with the provided drives.
    #[must_use]
    pub fn new(drives: Vec<DriveInfo>) -> Self {
        Self {
            drives: Arc::new(Mutex::new(drives)),
        }
    }

    /// Creates an empty `MockDriveScanner`.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Adds a drive to the mock scanner's inventory.
    pub fn add_drive(&self, drive: DriveInfo) {
        if let Ok(mut lock) = self.drives.lock() {
            lock.push(drive);
        }
    }

    /// Removes a drive by its path. Returns `true` if a drive was found and removed.
    pub fn remove_drive(&self, path: &Path) -> bool {
        if let Ok(mut lock) = self.drives.lock() {
            let initial_len = lock.len();
            lock.retain(|d| d.path != path);
            lock.len() < initial_len
        } else {
            false
        }
    }

    /// Clears all drives from the mock scanner.
    pub fn clear(&self) {
        if let Ok(mut lock) = self.drives.lock() {
            lock.clear();
        }
    }

    /// Overwrites the full set of drives in the mock scanner.
    pub fn set_drives(&self, drives: Vec<DriveInfo>) {
        if let Ok(mut lock) = self.drives.lock() {
            *lock = drives;
        }
    }

    /// Returns a snapshot copy of the currently configured drives.
    #[must_use]
    pub fn drives(&self) -> Vec<DriveInfo> {
        self.drives.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl DriveScanner for MockDriveScanner {
    fn scan_drives(&self) -> Result<Vec<DriveInfo>, InscribeError> {
        let drives = self
            .drives
            .lock()
            .map_err(|e| InscribeError::Other(format!("MockDriveScanner lock poisoned: {e}")))?;
        Ok(drives.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::BusType;
    use std::path::PathBuf;

    fn sample_drive(path: &str) -> DriveInfo {
        DriveInfo {
            path: PathBuf::from(path),
            model: "Mock Drive".to_string(),
            vendor: "Mock Vendor".to_string(),
            size: 16_000_000_000,
            sector_size: 512,
            bus_type: BusType::Usb,
            is_removable: true,
            is_read_only: false,
            is_system: false,
            mount_points: vec![],
        }
    }

    #[test]
    fn test_mock_scanner_lifecycle() {
        let scanner = MockDriveScanner::empty();
        assert!(scanner.scan_drives().unwrap().is_empty());

        let drive1 = sample_drive("/dev/sdb");
        scanner.add_drive(drive1);
        assert_eq!(scanner.scan_drives().unwrap().len(), 1);

        let drive2 = sample_drive("/dev/sdc");
        scanner.add_drive(drive2);
        assert_eq!(scanner.scan_drives().unwrap().len(), 2);

        assert!(scanner.remove_drive(Path::new("/dev/sdb")));
        assert_eq!(scanner.scan_drives().unwrap().len(), 1);
        assert_eq!(
            scanner.scan_drives().unwrap()[0].path,
            PathBuf::from("/dev/sdc")
        );

        assert!(!scanner.remove_drive(Path::new("/dev/sdb")));

        scanner.clear();
        assert!(scanner.scan_drives().unwrap().is_empty());
    }
}
