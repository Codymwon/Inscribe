//! GUI application state management, image inspection, and drive selection tracking.

use inscribe_core::drive::DriveInfo;
use inscribe_core::error::InscribeError;
use inscribe_core::image::{ImageFormat, detect_format, inspect_image};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Information regarding the currently selected disk image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedImage {
    /// Filesystem path to the image file.
    pub path: PathBuf,
    /// File name for display (e.g., `ubuntu-24.04.iso`).
    pub name: String,
    /// Total image file size or uncompressed payload size in bytes.
    pub size: u64,
    /// Human-readable formatted size string (e.g. `2.5 GB`).
    pub formatted_size: String,
    /// Detected image format descriptor (e.g. `ISO 9660`, `Gzip`, `Raw`).
    pub format_name: String,
}

/// Result of updating detected drives, indicating whether the active selection changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveStateChange {
    /// The active drive selection was unchanged.
    NoSelectionChange,
    /// The active drive selection was updated with fresh hardware attributes.
    SelectedDriveUpdated,
    /// The previously selected drive was unplugged/removed from the host.
    SelectedDriveRemoved,
}

/// Central state of the Inscribe graphical application.
#[derive(Debug, Clone)]
pub struct AppState {
    selected_image: Option<SelectedImage>,
    selected_drive: Option<DriveInfo>,
    drives: Vec<DriveInfo>,
    allow_external_usb_ssd: bool,
    is_flashing: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Creates a new default `AppState` with empty selections.
    #[must_use]
    pub fn new() -> Self {
        Self {
            selected_image: None,
            selected_drive: None,
            drives: Vec::new(),
            allow_external_usb_ssd: false,
            is_flashing: false,
        }
    }

    /// Currently selected image file, if any.
    #[must_use]
    pub fn selected_image(&self) -> Option<&SelectedImage> {
        self.selected_image.as_ref()
    }

    /// Currently selected target drive, if any.
    #[must_use]
    pub fn selected_drive(&self) -> Option<&DriveInfo> {
        self.selected_drive.as_ref()
    }

    /// List of all currently detected physical drives on the host.
    #[must_use]
    pub fn drives(&self) -> &[DriveInfo] {
        &self.drives
    }

    /// Whether "External USB SSDs" are permitted for flashing.
    #[must_use]
    pub fn allow_external_usb_ssd(&self) -> bool {
        self.allow_external_usb_ssd
    }

    /// Whether flashing is currently in progress.
    #[must_use]
    pub fn is_flashing(&self) -> bool {
        self.is_flashing
    }

    /// Sets whether flashing is in progress.
    pub fn set_is_flashing(&mut self, flashing: bool) {
        self.is_flashing = flashing;
    }

    /// Toggles or sets the "Allow External USB SSD" safety preference.
    ///
    /// If an external USB SSD was selected and the setting is disabled, the selection is cleared.
    pub fn set_allow_external_usb_ssd(&mut self, allow: bool) {
        self.allow_external_usb_ssd = allow;

        // If current selection is no longer flashable, clear it
        if let Some(ref current) = self.selected_drive {
            if !self.is_drive_flashable(current) {
                self.selected_drive = None;
            }
        }
    }

    /// Inspects and sets the active image file from a filesystem path.
    ///
    /// # Errors
    ///
    /// Returns an `InscribeError` if the file cannot be opened or metadata cannot be read.
    pub fn set_image(&mut self, path: PathBuf) -> Result<SelectedImage, InscribeError> {
        let mut file = File::open(&path).map_err(InscribeError::Io)?;
        let metadata = file.metadata().map_err(InscribeError::Io)?;
        let file_size = metadata.len();

        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown Image")
            .to_string();

        let mut header = [0u8; 4096];
        let bytes_read = file.read(&mut header).unwrap_or(0);
        let format = detect_format(&header[..bytes_read], Some(&path));
        let inspection = inspect_image(&mut file, format, Some(&path)).ok();
        let (display_size, format_name) = match inspection {
            Some(meta) => {
                let size = meta.uncompressed_size.unwrap_or(file_size);
                let name = match meta.format {
                    ImageFormat::Raw => "RAW / ISO".to_string(),
                    ImageFormat::Gz => "Gzip (.gz)".to_string(),
                    ImageFormat::Xz => "XZ (.xz)".to_string(),
                    ImageFormat::Zstd => "Zstandard (.zst)".to_string(),
                    ImageFormat::Bzip2 => "Bzip2 (.bz2)".to_string(),
                    ImageFormat::Zip => "ZIP (.zip)".to_string(),
                    ImageFormat::Unknown => "Disk Image".to_string(),
                };
                (size, name)
            }
            None => (file_size, "Disk Image".to_string()),
        };

        let selected = SelectedImage {
            path,
            name: filename,
            size: display_size,
            formatted_size: format_bytes(display_size),
            format_name,
        };

        self.selected_image = Some(selected.clone());
        Ok(selected)
    }

    /// Clears the currently selected image file.
    pub fn clear_image(&mut self) {
        self.selected_image = None;
    }

    /// Updates the detected drive inventory, checking if the active drive selection remains valid.
    pub fn set_drives(&mut self, new_drives: Vec<DriveInfo>) -> DriveStateChange {
        self.drives = new_drives;

        if let Some(ref current) = self.selected_drive {
            let current_path = current.path.clone();
            if let Some(updated) = self.drives.iter().find(|d| d.path == current_path) {
                if self.is_drive_flashable(updated) {
                    self.selected_drive = Some(updated.clone());
                    DriveStateChange::SelectedDriveUpdated
                } else {
                    self.selected_drive = None;
                    DriveStateChange::SelectedDriveRemoved
                }
            } else {
                self.selected_drive = None;
                DriveStateChange::SelectedDriveRemoved
            }
        } else {
            DriveStateChange::NoSelectionChange
        }
    }

    /// Selects a target drive by its native device path.
    ///
    /// Returns `true` if the drive was found and is safe/flashable under current safety rules.
    pub fn select_drive(&mut self, path: &Path) -> bool {
        if let Some(drive) = self.drives.iter().find(|d| d.path == path) {
            if self.is_drive_flashable(drive) {
                self.selected_drive = Some(drive.clone());
                return true;
            }
        }
        false
    }

    /// Clears the currently selected target drive.
    pub fn clear_drive(&mut self) {
        self.selected_drive = None;
    }

    /// Validates whether a target drive is safe and eligible for flashing.
    #[must_use]
    pub fn is_drive_flashable(&self, drive: &DriveInfo) -> bool {
        drive.is_flashable(self.allow_external_usb_ssd)
    }

    /// Evaluates whether all conditions are satisfied to begin flashing.
    #[must_use]
    pub fn can_flash(&self) -> bool {
        self.selected_image.is_some() && self.selected_drive.is_some() && !self.is_flashing
    }
}

/// Formats a byte quantity into a human-readable decimal unit (e.g. `32.0 GB`).
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = KB * 1_000.0;
    const GB: f64 = MB * 1_000.0;
    const TB: f64 = GB * 1_000.0;

    let size = bytes as f64;
    if size >= TB {
        format!("{:.1} TB", size / TB)
    } else if size >= GB {
        format!("{:.1} GB", size / GB)
    } else if size >= MB {
        format!("{:.1} MB", size / MB)
    } else if size >= KB {
        format!("{:.1} KB", size / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inscribe_core::drive::BusType;

    fn make_drive(path: &str, removable: bool, system: bool, bus: BusType) -> DriveInfo {
        DriveInfo {
            path: PathBuf::from(path),
            model: "Test Drive".to_string(),
            vendor: "Test Vendor".to_string(),
            size: 32_000_000_000,
            sector_size: 512,
            bus_type: bus,
            is_removable: removable,
            is_read_only: false,
            is_system: system,
            mount_points: vec![],
        }
    }

    #[test]
    fn test_format_bytes_decimal_units() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2_000), "2.0 KB");
        assert_eq!(format_bytes(2_500_000), "2.5 MB");
        assert_eq!(format_bytes(32_000_000_000), "32.0 GB");
        assert_eq!(format_bytes(2_000_000_000_000), "2.0 TB");
    }

    #[test]
    fn test_app_state_selection_and_safety_toggle() {
        let mut state = AppState::new();

        let usb = make_drive("/dev/sdb", true, false, BusType::Usb);
        let usb_ssd = make_drive("/dev/sdc", false, false, BusType::Usb);

        state.set_drives(vec![usb.clone(), usb_ssd.clone()]);
        assert!(state.select_drive(Path::new("/dev/sdb")));
        assert!(state.selected_drive().is_some());

        // USB SSD cannot be selected yet
        assert!(!state.select_drive(Path::new("/dev/sdc")));

        // Enable USB SSD
        state.set_allow_external_usb_ssd(true);
        assert!(state.select_drive(Path::new("/dev/sdc")));
        assert_eq!(
            state.selected_drive().unwrap().path,
            PathBuf::from("/dev/sdc")
        );

        // Disabling USB SSD clears the selection
        state.set_allow_external_usb_ssd(false);
        assert!(state.selected_drive().is_none());
    }
}
