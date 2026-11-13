//! Slint GUI event bridge, hotplug drive poller, and native file dialog service.

use crate::state::{AppState, DriveStateChange};
use crate::{AppWindow, DriveItem};
use inscribe_core::drive::{DriveInfo, DriveScanner};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, info};

/// Whitelist of supported disk image and archive formats for file selection.
pub const SUPPORTED_IMAGE_EXTENSIONS: &[&str] = &["iso", "img", "xz", "gz", "zst", "bz2", "zip"];

/// Trait abstracting native or simulated file picker dialogs.
pub trait FilePicker: Send + Sync {
    /// Opens the file picker dialog and returns the selected filesystem path.
    fn pick_image_file(&self) -> Option<PathBuf>;
}

/// Production file picker delegating to the native OS file dialog via `rfd`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RfdFilePicker;

impl RfdFilePicker {
    /// Creates a new `RfdFilePicker`.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl FilePicker for RfdFilePicker {
    fn pick_image_file(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Select Disk Image")
            .add_filter(
                "Supported Disk Images (*.iso, *.img, *.xz, *.gz, *.zst, *.bz2, *.zip)",
                SUPPORTED_IMAGE_EXTENSIONS,
            )
            .pick_file()
    }
}

/// Mock file picker for deterministic testing without opening modal windows.
#[derive(Debug, Default)]
pub struct MockFilePicker {
    next_file: Mutex<Option<PathBuf>>,
}

impl MockFilePicker {
    /// Creates an empty `MockFilePicker`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the file path that will be returned on the next `pick_image_file` call.
    pub fn set_next_file(&self, path: Option<PathBuf>) {
        if let Ok(mut lock) = self.next_file.lock() {
            *lock = path;
        }
    }
}

impl FilePicker for MockFilePicker {
    fn pick_image_file(&self) -> Option<PathBuf> {
        self.next_file.lock().ok().and_then(|mut lock| lock.take())
    }
}

/// Dedicated background worker thread polling storage drives for USB hotplug events.
pub struct DrivePoller {
    running: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl DrivePoller {
    /// Spawns a background drive polling thread.
    ///
    /// Every `interval`, the scanner is queried. If the detected drive list changes,
    /// `on_update` is called with the fresh snapshot.
    pub fn start<F>(scanner: Arc<dyn DriveScanner>, interval: Duration, mut on_update: F) -> Self
    where
        F: FnMut(Vec<DriveInfo>) + Send + 'static,
    {
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();

        let thread_handle = std::thread::Builder::new()
            .name("inscribe-drive-poller".to_string())
            .spawn(move || {
                let mut last_drives: Option<Vec<DriveInfo>> = None;

                while thread_running.load(Ordering::Relaxed) {
                    match scanner.scan_drives() {
                        Ok(current_drives) => {
                            let has_changed = match last_drives.as_ref() {
                                Some(prev) => prev != &current_drives,
                                None => true,
                            };

                            if has_changed {
                                debug!(
                                    drive_count = current_drives.len(),
                                    "Drive hotplug state change detected"
                                );
                                on_update(current_drives.clone());
                                last_drives = Some(current_drives);
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Background drive poller scan failure");
                        }
                    }

                    // Sleep in 50ms increments to allow instant response to cancellation
                    let check_interval = Duration::from_millis(50);
                    let iterations =
                        (interval.as_millis() / check_interval.as_millis().max(1)).max(1) as usize;

                    for _ in 0..iterations {
                        if !thread_running.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(check_interval);
                    }
                }
            })
            .ok();

        Self {
            running,
            thread_handle,
        }
    }

    /// Signals the background poller thread to stop and joins it.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DrivePoller {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Formats a human-readable display string for a drive (e.g. `SanDisk Ultra (/dev/sdb)`).
#[must_use]
pub fn format_drive_display_name(drive: &DriveInfo) -> String {
    let base = if drive.vendor.is_empty()
        || drive
            .model
            .to_lowercase()
            .starts_with(&drive.vendor.to_lowercase())
    {
        drive.model.clone()
    } else {
        format!("{} {}", drive.vendor, drive.model)
    };
    format!("{} ({})", base.trim(), drive.path.display())
}

/// Central event bridge coordinating Slint UI, hardware scanners, and background workers.
pub struct GuiBridge {
    window: Weak<AppWindow>,
    state: Arc<Mutex<AppState>>,
    scanner: Arc<dyn DriveScanner>,
    file_picker: Arc<dyn FilePicker>,
    poller: Arc<Mutex<Option<DrivePoller>>>,
}

impl GuiBridge {
    /// Creates and initializes a new `GuiBridge`.
    #[must_use]
    pub fn new(
        window: Weak<AppWindow>,
        scanner: Arc<dyn DriveScanner>,
        file_picker: Arc<dyn FilePicker>,
    ) -> Arc<Self> {
        Arc::new(Self {
            window,
            state: Arc::new(Mutex::new(AppState::new())),
            scanner,
            file_picker,
            poller: Arc::new(Mutex::new(None)),
        })
    }

    /// Access the underlying application state.
    #[must_use]
    pub fn state(&self) -> Arc<Mutex<AppState>> {
        self.state.clone()
    }

    /// Starts background hotplug drive polling at the specified interval.
    pub fn start_polling(self: &Arc<Self>, interval: Duration) {
        let bridge_weak = Arc::downgrade(self);
        let scanner = self.scanner.clone();

        let poller = DrivePoller::start(scanner, interval, move |drives| {
            if let Some(bridge) = bridge_weak.upgrade() {
                bridge.apply_drive_update(drives);
            }
        });

        if let Ok(mut lock) = self.poller.lock() {
            *lock = Some(poller);
        }
    }

    /// Manually triggers a drive scan and updates the Slint UI model.
    pub fn scan_and_update_ui(&self) {
        if let Ok(drives) = self.scanner.scan_drives() {
            self.apply_drive_update(drives);
        }
    }

    /// Updates internal state with the latest drive list and synchronizes Slint properties.
    pub fn apply_drive_update(&self, drives: Vec<DriveInfo>) {
        let (change, allow_ssd, selected_drive, can_flash) = {
            let mut state = match self.state.lock() {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to acquire state lock for drive update");
                    return;
                }
            };
            let change = state.set_drives(drives.clone());
            (
                change,
                state.allow_external_usb_ssd(),
                state.selected_drive().cloned(),
                state.can_flash(),
            )
        };

        let items: Vec<DriveItem> = drives
            .iter()
            .map(|d| {
                let is_flashable = d.is_flashable(allow_ssd);
                DriveItem {
                    path: SharedString::from(d.path.to_string_lossy().as_ref()),
                    name: SharedString::from(format!("{} {}", d.vendor, d.model).trim()),
                    size_text: SharedString::from(d.formatted_size()),
                    is_system: d.is_system,
                    is_flashable,
                    is_external_usb_ssd: d.is_external_usb_ssd(),
                    vendor: SharedString::from(&d.vendor),
                    model: SharedString::from(&d.model),
                    bus: SharedString::from(format!("{:?}", d.bus_type)),
                }
            })
            .collect();

        let drive_count = drives.len() as i32;

        let apply_to_win = move |win: AppWindow| {
            win.set_drive_count(drive_count);
            win.set_drives(ModelRc::new(VecModel::from(items)));

            match change {
                DriveStateChange::SelectedDriveRemoved => {
                    win.set_drive_selected(false);
                    win.set_drive_name(SharedString::default());
                    win.set_drive_size_text(SharedString::default());
                    win.set_selected_drive_index(-1);
                    win.set_can_flash(false);
                }
                DriveStateChange::SelectedDriveUpdated => {
                    if let Some(drive) = selected_drive {
                        let name_formatted = format_drive_display_name(&drive);
                        win.set_drive_selected(true);
                        win.set_drive_name(SharedString::from(name_formatted));
                        win.set_drive_size_text(SharedString::from(drive.formatted_size()));
                        win.set_can_flash(can_flash);
                    }
                }
                DriveStateChange::NoSelectionChange => {
                    win.set_can_flash(can_flash);
                }
            }
        };

        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
    }

    /// Handles clicking the "Select Image" button by triggering the file dialog.
    pub fn handle_select_image(&self) {
        if let Some(path) = self.file_picker.pick_image_file() {
            self.apply_image_path(path);
        }
    }

    /// Handles a drag-and-dropped file URI or path.
    pub fn handle_file_dropped(&self, path_str: &str) {
        let clean_path = path_str
            .trim()
            .strip_prefix("file://")
            .unwrap_or(path_str.trim());
        let path = PathBuf::from(clean_path);
        self.apply_image_path(path);
    }

    /// Inspects and applies an image path, updating internal state and Slint properties.
    pub fn apply_image_path(&self, path: PathBuf) {
        let result = {
            let mut state = match self.state.lock() {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to acquire state lock for image selection");
                    return;
                }
            };
            state.set_image(path).map(|img| (img, state.can_flash()))
        };

        match result {
            Ok((image, can_flash)) => {
                info!(image_name = %image.name, size = image.size, "Disk image selected successfully");
                let apply_to_win = move |win: AppWindow| {
                    win.set_image_selected(true);
                    win.set_image_name(SharedString::from(image.name));
                    win.set_image_size_text(SharedString::from(image.formatted_size));
                    win.set_image_format_badge(SharedString::from(image.format_name));
                    win.set_can_flash(can_flash);
                };
                if let Some(win) = self.window.upgrade() {
                    apply_to_win(win);
                } else {
                    let _ = self.window.upgrade_in_event_loop(apply_to_win);
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to inspect selected image");
            }
        }
    }

    /// Selects a target drive by its native device path.
    pub fn handle_select_drive_by_path(&self, path: &Path) {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };

        if state.select_drive(path) {
            let drive = state.selected_drive().cloned().unwrap();
            let can_flash = state.can_flash();
            let name_formatted = format_drive_display_name(&drive);
            let size_text = drive.formatted_size();

            // Find matching index in drives list
            let index = state
                .drives()
                .iter()
                .position(|d| d.path == drive.path)
                .map(|i| i as i32)
                .unwrap_or(-1);

            let apply_to_win = move |win: AppWindow| {
                win.set_drive_selected(true);
                win.set_drive_name(SharedString::from(name_formatted.trim()));
                win.set_drive_size_text(SharedString::from(size_text));
                win.set_selected_drive_index(index);
                win.set_can_flash(can_flash);
            };
            if let Some(win) = self.window.upgrade() {
                apply_to_win(win);
            } else {
                let _ = self.window.upgrade_in_event_loop(apply_to_win);
            }
        }
    }

    /// Selects a target drive by its index in the current drives list.
    pub fn handle_select_drive_by_index(&self, index: usize) {
        let path = {
            let state = match self.state.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            state.drives().get(index).map(|d| d.path.clone())
        };

        if let Some(path) = path {
            self.handle_select_drive_by_path(&path);
        }
    }

    /// Toggles the external USB SSD setting and re-evaluates all drives and active selection.
    pub fn handle_toggle_allow_external_usb_ssd(&self, allow: bool) {
        let drives = {
            let mut state = match self.state.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            state.set_allow_external_usb_ssd(allow);
            state.drives().to_vec()
        };

        self.apply_drive_update(drives);
    }

    /// Connects all UI callbacks on the provided `AppWindow` to this bridge instance.
    pub fn wire_callbacks(self: &Arc<Self>, window: &AppWindow) {
        // Step 1: Select Image
        {
            let bridge = self.clone();
            window.on_select_image_clicked(move || {
                bridge.handle_select_image();
            });
        }

        // File drop
        {
            let bridge = self.clone();
            window.on_file_dropped(move |path| {
                bridge.handle_file_dropped(&path);
            });
        }

        // Step 2: Select Drive (opens selection modal)
        {
            let weak_win = window.as_weak();
            let bridge = self.clone();
            window.on_select_drive_clicked(move || {
                // Ensure fresh drive list before opening modal
                bridge.scan_and_update_ui();
                let _ = weak_win.upgrade_in_event_loop(|win| {
                    win.set_show_drive_modal(true);
                });
            });
        }

        // Drive selection from list
        {
            let bridge = self.clone();
            window.on_drive_selected_by_index(move |idx| {
                if idx >= 0 {
                    bridge.handle_select_drive_by_index(idx as usize);
                }
            });
        }

        // Manual refresh button in modal
        {
            let bridge = self.clone();
            window.on_refresh_drives_clicked(move || {
                bridge.scan_and_update_ui();
            });
        }

        // USB SSD safety setting modal toggle
        {
            let bridge = self.clone();
            let weak_win = window.as_weak();
            window.on_toggle_usb_ssd_modal_clicked(move || {
                let bridge_inner = bridge.clone();
                let _ = weak_win.upgrade_in_event_loop(move |win| {
                    let current = win.get_allow_external_usb_ssd();
                    bridge_inner.handle_toggle_allow_external_usb_ssd(current);
                });
            });
        }
    }
}
