//! Slint GUI event bridge, hotplug drive poller, and native file dialog service.

use crate::state::{AppState, DriveStateChange, format_bytes, format_duration};
use crate::{AppWindow, DriveItem};
use inscribe_core::drive::{DriveInfo, DriveScanner};
use inscribe_core::error::InscribeError;
use inscribe_core::image::{detect_format, inspect_image, open_seekable_decompressor};
use inscribe_core::pipeline::{FlashHandle, FlashPipeline, FlashSummary, PipelineOptions};
use inscribe_core::progress::{FlashPhase, FlashProgress};
use inscribe_core::writer::aligned::DEFAULT_BUFFER_SIZE;
use inscribe_core::writer::{BlockDeviceWriter, WriterOptions};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Whitelist of supported disk image and archive formats for file selection.
pub const SUPPORTED_IMAGE_EXTENSIONS: &[&str] = &["iso", "img", "xz", "gz", "zst", "bz2", "zip"];

/// Validates that a dropped path is an accessible regular file with a supported image extension.
///
/// Rejects directories, symlink loops, named pipes (FIFOs), sockets, and device files.
///
/// # Errors
/// Returns `InscribeError` if path cannot be accessed, is not a regular file, or has an unsupported extension.
pub fn validate_dropped_file(path: &Path) -> Result<(), InscribeError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| InscribeError::Other(format!("Cannot access '{}': {e}", path.display())))?;

    if !metadata.is_file() {
        return Err(InscribeError::Other(format!(
            "Target '{}' is not a regular file (directories, pipes, and special devices are not supported).",
            path.display()
        )));
    }

    let has_supported_ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            SUPPORTED_IMAGE_EXTENSIONS
                .iter()
                .any(|&supported| supported.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false);

    if !has_supported_ext {
        return Err(InscribeError::Other(format!(
            "File '{}' does not have a supported disk image extension ({})",
            path.display(),
            SUPPORTED_IMAGE_EXTENSIONS.join(", ")
        )));
    }

    Ok(())
}

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

/// Trait abstracting execution of the two-pass flash and verification pipeline.
pub trait FlashExecutor: Send + Sync {
    /// Launches the flash pipeline worker thread for the specified image and target drive.
    ///
    /// # Errors
    /// Returns `InscribeError` if target cannot be opened, Drive Guard rejects the target,
    /// or inspection fails.
    fn start_flash(
        &self,
        image_path: &Path,
        drive: &DriveInfo,
        allow_external_usb_ssd: bool,
    ) -> Result<FlashHandle, InscribeError>;
}

/// Production flash executor delegating to `inscribe_core::pipeline::FlashPipeline`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFlashExecutor;

impl RealFlashExecutor {
    /// Creates a new `RealFlashExecutor`.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl FlashExecutor for RealFlashExecutor {
    fn start_flash(
        &self,
        image_path: &Path,
        drive: &DriveInfo,
        allow_external_usb_ssd: bool,
    ) -> Result<FlashHandle, InscribeError> {
        // Platform volume unmounting and exclusive lock acquisition prior to opening raw block writers
        inscribe_core::drive::prepare_drive_for_writing(drive, allow_external_usb_ssd)?;

        let mut file = File::open(image_path).map_err(InscribeError::Io)?;
        let mut header = [0u8; 4096];
        let bytes_read = file.read(&mut header).unwrap_or(0);
        let format = detect_format(&header[..bytes_read], Some(image_path));
        file.seek(SeekFrom::Start(0)).map_err(InscribeError::Io)?;

        let metadata = inspect_image(&mut file, format, Some(image_path))?;
        file.seek(SeekFrom::Start(0)).map_err(InscribeError::Io)?;

        let reader = open_seekable_decompressor(file, format)?;

        let direct_opts = WriterOptions::direct();
        let buffered_opts = WriterOptions::buffered();
        let target = BlockDeviceWriter::open_drive(drive, &direct_opts, allow_external_usb_ssd)
            .or_else(|_| {
                BlockDeviceWriter::open_drive(drive, &buffered_opts, allow_external_usb_ssd)
            })?;

        let options = PipelineOptions {
            expected_size: metadata.uncompressed_size,
            verify: true,
            expected_sha256: None,
            chunk_size: DEFAULT_BUFFER_SIZE,
            telemetry_rate_hz: 60,
        };

        Ok(FlashPipeline::spawn(reader, target, options))
    }
}

/// Dedicated background worker thread polling storage drives for USB hotplug events.
pub struct DrivePoller {
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl DrivePoller {
    /// Pauses background drive polling notifications.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    /// Resumes background drive polling notifications.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    /// Whether polling notifications are currently paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Spawns a background drive polling thread.
    ///
    /// Every `interval`, the scanner is queried. If the detected drive list changes,
    /// `on_update` is called with the fresh snapshot.
    pub fn start<F>(scanner: Arc<dyn DriveScanner>, interval: Duration, mut on_update: F) -> Self
    where
        F: FnMut(Vec<DriveInfo>) + Send + 'static,
    {
        let running = Arc::new(AtomicBool::new(true));
        let paused = Arc::new(AtomicBool::new(false));
        let thread_running = running.clone();
        let thread_paused = paused.clone();

        let thread_handle = std::thread::Builder::new()
            .name("inscribe-drive-poller".to_string())
            .spawn(move || {
                let mut last_drives: Option<Vec<DriveInfo>> = None;

                while thread_running.load(Ordering::Relaxed) {
                    if !thread_paused.load(Ordering::Relaxed) {
                        match scanner.scan_drives() {
                            Ok(current_drives) => {
                                let has_changed = match last_drives.as_ref() {
                                    Some(prev) => prev != &current_drives,
                                    None => true,
                                };

                                if has_changed && !thread_paused.load(Ordering::Relaxed) {
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
            paused,
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

/// Decodes URL percent-encoding (e.g. `%20` -> `' '`) and normalizes `file://` URIs.
#[must_use]
pub fn parse_file_drop_uri(raw: &str) -> PathBuf {
    let trimmed = raw.trim();

    // 1. Strip file scheme prefix
    let without_scheme = if let Some(stripped) = trimmed.strip_prefix("file://") {
        stripped
    } else if let Some(stripped) = trimmed.strip_prefix("FILE://") {
        stripped
    } else {
        trimmed
    };

    // 2. Handle localhost authority: file://localhost/path
    let without_host = if let Some(stripped) = without_scheme.strip_prefix("localhost") {
        stripped
    } else {
        without_scheme
    };

    // 3. Percent-decode
    let decoded = decode_percent_encoded(without_host);

    // 4. Normalize Windows drive letters: e.g. "/C:/Users/..." -> "C:/Users/..."
    let normalized = if decoded.starts_with('/')
        && decoded.len() >= 3
        && decoded.as_bytes()[1].is_ascii_alphabetic()
        && decoded.as_bytes()[2] == b':'
    {
        &decoded[1..]
    } else {
        &decoded
    };

    PathBuf::from(normalized)
}

fn decode_percent_encoded(input: &str) -> String {
    let mut bytes = Vec::with_capacity(input.len());
    let mut iter = input.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let h1 = iter.next();
            let h2 = iter.next();
            if let (Some(h1), Some(h2)) = (h1, h2) {
                let hex_pair = [h1, h2];
                if let Ok(hex_str) = std::str::from_utf8(&hex_pair) {
                    if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                        bytes.push(val);
                        continue;
                    }
                }
                bytes.push(b'%');
                bytes.push(h1);
                bytes.push(h2);
            } else {
                bytes.push(b'%');
                if let Some(h1) = h1 {
                    bytes.push(h1);
                }
            }
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

struct ActiveFlash {
    cancel_token: Arc<AtomicBool>,
    progress_rx: crossbeam_channel::Receiver<FlashProgress>,
    thread_handle: Option<std::thread::JoinHandle<Result<FlashSummary, InscribeError>>>,
    completed_result: Option<Result<FlashSummary, InscribeError>>,
}

impl Drop for ActiveFlash {
    fn drop(&mut self) {
        self.cancel_token.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Central event bridge coordinating Slint UI, hardware scanners, and background workers.
pub struct GuiBridge {
    window: Weak<AppWindow>,
    state: Arc<Mutex<AppState>>,
    scanner: Arc<dyn DriveScanner>,
    file_picker: Arc<dyn FilePicker>,
    flash_executor: Arc<dyn FlashExecutor>,
    active_flash: Arc<Mutex<Option<ActiveFlash>>>,
    poller: Arc<Mutex<Option<DrivePoller>>>,
}

impl GuiBridge {
    /// Creates and initializes a new `GuiBridge` with default production services.
    #[must_use]
    pub fn new(
        window: Weak<AppWindow>,
        scanner: Arc<dyn DriveScanner>,
        file_picker: Arc<dyn FilePicker>,
    ) -> Arc<Self> {
        Self::with_executor(
            window,
            scanner,
            file_picker,
            Arc::new(RealFlashExecutor::new()),
        )
    }

    /// Creates a new `GuiBridge` with custom dependencies (useful for testing).
    #[must_use]
    pub fn with_executor(
        window: Weak<AppWindow>,
        scanner: Arc<dyn DriveScanner>,
        file_picker: Arc<dyn FilePicker>,
        flash_executor: Arc<dyn FlashExecutor>,
    ) -> Arc<Self> {
        Arc::new(Self {
            window,
            state: Arc::new(Mutex::new(AppState::new())),
            scanner,
            file_picker,
            flash_executor,
            active_flash: Arc::new(Mutex::new(None)),
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
                let is_flashing = bridge
                    .state
                    .lock()
                    .map(|s| s.is_flashing())
                    .unwrap_or(false);
                if !is_flashing {
                    let bridge_inner = bridge.clone();
                    let _ = bridge.window.upgrade_in_event_loop(move |_| {
                        bridge_inner.apply_drive_update(drives);
                    });
                }
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
            if state.is_flashing() {
                debug!("DrivePoller: Pausing drive list mutations while flashing is active");
                return;
            }
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
            win.set_allow_external_usb_ssd(allow_ssd);

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
        let path = parse_file_drop_uri(path_str);
        match validate_dropped_file(&path) {
            Ok(()) => {
                self.apply_image_path(path);
            }
            Err(e) => {
                error!(path = %path.display(), error = %e, "Invalid file dropped");
                let title = if e.to_string().contains("supported disk image extension") {
                    "Unsupported File Format"
                } else {
                    "Invalid Image File"
                };
                self.apply_ui_error(title, e.to_string());
            }
        }
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

    /// Launches the background flash worker thread and begins listening to telemetry.
    pub fn handle_flash(self: &Arc<Self>) {
        let (image_path, drive, allow_ssd) = {
            let mut state = match self.state.lock() {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to lock state for flashing");
                    return;
                }
            };

            if !state.can_flash() {
                warn!("handle_flash invoked but prerequisites not met");
                return;
            }

            let image_path = match state.selected_image() {
                Some(img) => img.path.clone(),
                None => return,
            };

            let drive = match state.selected_drive() {
                Some(d) => d.clone(),
                None => return,
            };

            let allow_ssd = state.allow_external_usb_ssd();

            // Atomically flag is_flashing under initial lock to drop concurrent invocations
            state.set_is_flashing(true);

            (image_path, drive, allow_ssd)
        };

        // Pause drive hotplug poller during flashing
        if let Ok(lock) = self.poller.lock() {
            if let Some(ref p) = *lock {
                p.pause();
            }
        }

        // Live target attestation (prevent TOCTOU device swap races)
        let live_drives = match self.scanner.scan_drives() {
            Ok(d) => d,
            Err(e) => {
                error!(error = %e, "Failed to scan drives for pre-flash attestation");
                if let Ok(mut state) = self.state.lock() {
                    state.set_is_flashing(false);
                }
                if let Ok(lock) = self.poller.lock() {
                    if let Some(ref p) = *lock {
                        p.resume();
                    }
                }
                self.apply_flash_error(&e);
                return;
            }
        };

        let live_drive = match live_drives.iter().find(|d| d.path == drive.path) {
            Some(d) => d,
            None => {
                let err = InscribeError::drive_guard_violation(
                    &drive.path,
                    format!(
                        "Target device '{}' disappeared between initial scan and write attempt",
                        drive.path.display()
                    ),
                );
                error!(error = %err, "Target device missing during pre-flash attestation");
                if let Ok(mut state) = self.state.lock() {
                    state.set_is_flashing(false);
                }
                if let Ok(lock) = self.poller.lock() {
                    if let Some(ref p) = *lock {
                        p.resume();
                    }
                }
                self.apply_flash_error(&err);
                return;
            }
        };

        if let Err(err) =
            inscribe_core::drive::guard::DriveGuard::attest_target(&drive, live_drive, allow_ssd)
        {
            error!(error = %err, "Pre-flash Drive Guard attestation failed");
            if let Ok(mut state) = self.state.lock() {
                state.set_is_flashing(false);
            }
            if let Ok(lock) = self.poller.lock() {
                if let Some(ref p) = *lock {
                    p.resume();
                }
            }
            self.apply_flash_error(&err);
            return;
        }

        let apply_to_win = |win: AppWindow| {
            win.set_is_flashing(true);
            win.set_flash_progress(0.0);
            win.set_flash_phase_text(SharedString::from("Initializing..."));
            win.set_flash_speed_text(SharedString::from("0.0 MB/s"));
            win.set_flash_eta_text(SharedString::default());
            win.set_show_cancel_modal(false);
            win.set_show_success_screen(false);
            win.set_show_error_screen(false);
        };

        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }

        let handle = match self
            .flash_executor
            .start_flash(&image_path, &drive, allow_ssd)
        {
            Ok(h) => h,
            Err(e) => {
                error!(error = %e, "Failed to launch flash worker");
                if let Ok(mut state) = self.state.lock() {
                    state.set_is_flashing(false);
                }
                if let Ok(lock) = self.poller.lock() {
                    if let Some(ref p) = *lock {
                        p.resume();
                    }
                }
                self.apply_flash_error(&e);
                return;
            }
        };

        let progress_rx_for_bridge = handle.progress_rx.clone();
        let cancel_token_clone = handle.cancel_token.clone();

        if let Ok(mut state) = self.state.lock() {
            state.set_cancel_token(Some(handle.cancel_token.clone()));
        }

        if let Ok(mut lock) = self.active_flash.lock() {
            *lock = Some(ActiveFlash {
                cancel_token: cancel_token_clone,
                progress_rx: handle.progress_rx,
                thread_handle: Some(handle.thread_handle),
                completed_result: None,
            });
        }

        let bridge_weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("inscribe-telemetry-bridge".to_string())
            .spawn(move || {
                while let Ok(progress) = progress_rx_for_bridge.recv() {
                    if let Some(bridge) = bridge_weak.upgrade() {
                        let bridge_prog = bridge.clone();
                        let _ = bridge.window.upgrade_in_event_loop(move |_| {
                            bridge_prog.apply_flash_progress(&progress);
                        });
                    } else {
                        break;
                    }
                }

                let bridge = match bridge_weak.upgrade() {
                    Some(b) => b,
                    None => return,
                };

                // Channel disconnected -> worker finished.
                // Join worker thread handle on this background worker thread to eliminate
                // any OS thread cleanup race condition before updating UI state.
                let handle_opt = {
                    if let Ok(mut lock) = bridge.active_flash.lock() {
                        if let Some(ref mut active) = *lock {
                            active.thread_handle.take()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                let result = if let Some(handle) = handle_opt {
                    match handle.join() {
                        Ok(res) => res,
                        Err(_) => Err(InscribeError::Other(
                            "Flash worker thread panicked".to_string(),
                        )),
                    }
                } else {
                    return;
                };

                if let Ok(mut lock) = bridge.active_flash.lock() {
                    if let Some(ref mut active) = *lock {
                        active.completed_result = Some(result);
                    }
                }

                let bridge_inner = bridge.clone();
                let _ = bridge.window.upgrade_in_event_loop(move |_| {
                    bridge_inner.poll_active_flash();
                });
            })
            .ok();
    }

    /// Polls progress and completion status from the active flash worker.
    ///
    /// Updates Slint properties synchronously on the calling thread.
    /// Returns `true` if the operation has completed or was cancelled.
    pub fn poll_active_flash(&self) -> bool {
        let result_opt = {
            let mut lock = match self.active_flash.lock() {
                Ok(l) => l,
                Err(_) => return false,
            };
            match *lock {
                Some(ref mut active) => {
                    let mut latest_progress = None;
                    while let Ok(p) = active.progress_rx.try_recv() {
                        latest_progress = Some(p);
                    }
                    if let Some(p) = latest_progress {
                        self.apply_flash_progress(&p);
                    }

                    if let Some(res) = active.completed_result.take() {
                        *lock = None;
                        Some(res)
                    } else if active
                        .thread_handle
                        .as_ref()
                        .is_some_and(|h| h.is_finished())
                    {
                        let h = active.thread_handle.take().unwrap();
                        *lock = None;
                        Some(match h.join() {
                            Ok(r) => r,
                            Err(_) => Err(InscribeError::Other(
                                "Flash worker thread panicked".to_string(),
                            )),
                        })
                    } else {
                        None
                    }
                }
                None => None,
            }
        };

        if let Some(result) = result_opt {
            match result {
                Ok(summary) => self.apply_flash_success(&summary),
                Err(err) => self.apply_flash_error(&err),
            }
            return true;
        }

        false
    }

    /// Dispatches telemetry updates to the Slint UI.
    pub fn apply_flash_progress(&self, progress: &FlashProgress) {
        let phase_text = match progress.phase {
            FlashPhase::Initializing => "Initializing pipeline...".to_string(),
            FlashPhase::Flashing => {
                format!(
                    "Pass {}/{}: Direct Block Write",
                    progress.current_pass, progress.total_passes
                )
            }
            FlashPhase::Verifying => {
                format!(
                    "Pass {}/{}: Verifying bitstream (BLAKE3)",
                    progress.current_pass, progress.total_passes
                )
            }
            FlashPhase::Completed => "Completed".to_string(),
            FlashPhase::Failed => "Failed".to_string(),
            FlashPhase::Cancelled => "Cancelled".to_string(),
        };

        let speed_mb = progress.speed_mb_per_sec();
        let speed_text = if speed_mb > 0.0 {
            format!("{speed_mb:.1} MB/s")
        } else {
            "0.0 MB/s".to_string()
        };

        let eta_text = progress
            .eta_seconds
            .map(|eta| format!("ETA: {eta}s"))
            .unwrap_or_default();

        let progress_val = progress.fraction.unwrap_or(0.0);

        let apply_to_win = move |win: AppWindow| {
            win.set_flash_progress(progress_val);
            win.set_flash_phase_text(SharedString::from(phase_text));
            win.set_flash_speed_text(SharedString::from(speed_text));
            win.set_flash_eta_text(SharedString::from(eta_text));
        };

        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
    }

    /// Transitions the UI to the celebration screen upon successful flash and verification.
    pub fn apply_flash_success(&self, summary: &FlashSummary) {
        if let Ok(mut state) = self.state.lock() {
            state.set_is_flashing(false);
        }
        if let Ok(lock) = self.poller.lock() {
            if let Some(ref p) = *lock {
                p.resume();
            }
        }

        let summary_text = format!(
            "{} written at {:.1} MB/s ({})",
            format_bytes(summary.bytes_written),
            summary.average_mb_per_sec,
            format_duration(summary.duration),
        );
        let hash_text = format!("BLAKE3 Checksum: {}", summary.blake3_hash);

        let apply_to_win = move |win: AppWindow| {
            win.set_is_flashing(false);
            win.set_flash_progress(1.0);
            win.set_show_cancel_modal(false);
            win.set_show_error_screen(false);
            win.set_show_success_screen(true);
            win.set_success_summary_text(SharedString::from(summary_text));
            win.set_success_hash_text(SharedString::from(hash_text));
        };

        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
    }

    /// Transitions the UI to display an error screen with specified title and message.
    pub fn apply_ui_error(&self, title: impl Into<String>, message: impl Into<String>) {
        let title = title.into();
        let message = message.into();
        let apply_to_win = move |win: AppWindow| {
            win.set_is_flashing(false);
            win.set_show_cancel_modal(false);
            win.set_show_success_screen(false);
            win.set_show_error_screen(true);
            win.set_error_title_text(SharedString::from(title));
            win.set_error_message_text(SharedString::from(message));
        };

        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
    }

    /// Transitions the UI to the error summary screen with actionable failure details.
    pub fn apply_flash_error(&self, err: &InscribeError) {
        if let Ok(mut state) = self.state.lock() {
            state.set_is_flashing(false);
        }
        if let Ok(lock) = self.poller.lock() {
            if let Some(ref p) = *lock {
                p.resume();
            }
        }

        let (title, message) = match err {
            InscribeError::Cancelled => (
                "Flashing Cancelled".to_string(),
                "The flash operation was cancelled by the user. The target drive was not fully written and may be unbootable.".to_string(),
            ),
            InscribeError::VerificationFailed {
                offset,
                expected,
                actual,
            } => (
                "Verification Failed".to_string(),
                format!(
                    "Cryptographic BLAKE3 verification failed at byte offset {offset}.\nExpected: {expected}\nActual:   {actual}\nThe written bytes do not match the source image. Please check physical media health."
                ),
            ),
            InscribeError::InsufficientCapacity {
                target_bytes,
                image_bytes,
            } => (
                "Insufficient Capacity".to_string(),
                format!(
                    "The selected disk image ({}) exceeds target capacity ({}). Choose a larger target drive or a smaller image.",
                    format_bytes(*image_bytes),
                    format_bytes(*target_bytes),
                ),
            ),
            InscribeError::DriveGuardViolation { path, reason } => (
                "Drive Guard Safety Violation".to_string(),
                format!("Drive Guard rejected target '{path}':\n{reason}"),
            ),
            other => (
                "Flashing Failed".to_string(),
                format!("{other}\nCheck permissions or physical device connection."),
            ),
        };

        self.apply_ui_error(title, message);
    }

    /// Shows the cancellation confirmation modal dialog.
    pub fn handle_cancel_flash_requested(&self) {
        let apply_to_win = |win: AppWindow| {
            win.set_show_cancel_modal(true);
        };
        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
    }

    /// Dismisses the cancellation confirmation modal without halting flashing.
    pub fn handle_dismiss_cancel_modal(&self) {
        let apply_to_win = |win: AppWindow| {
            win.set_show_cancel_modal(false);
        };
        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
    }

    /// Confirms cancellation and signals the active worker to halt immediately.
    pub fn handle_confirm_cancel(&self) {
        let apply_to_win = |win: AppWindow| {
            win.set_show_cancel_modal(false);
        };
        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }

        if let Ok(state) = self.state.lock() {
            state.cancel_active_flash();
        }

        if let Ok(lock) = self.active_flash.lock() {
            if let Some(ref active) = *lock {
                active.cancel_token.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Dismisses either the celebration screen or error screen, returning to main UI.
    pub fn handle_dismiss_result(&self) {
        let apply_to_win = |win: AppWindow| {
            win.set_show_success_screen(false);
            win.set_show_error_screen(false);
        };
        if let Some(win) = self.window.upgrade() {
            apply_to_win(win);
        } else {
            let _ = self.window.upgrade_in_event_loop(apply_to_win);
        }
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

        // Data transfer unpacker for file drops
        {
            window.on_data_to_uri(|data| {
                if let Ok(mut paths) = data.file_paths() {
                    if let Some(path) = paths.next() {
                        return SharedString::from(path.to_string_lossy().as_ref());
                    }
                }
                if let Ok(text) = data.plain_text() {
                    return text;
                }
                SharedString::default()
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

        // Allow External USB SSD checkbox changed
        {
            let bridge = self.clone();
            window.on_allow_external_usb_ssd_changed(move |allow| {
                bridge.handle_toggle_allow_external_usb_ssd(allow);
            });
        }

        // Step 3: Flash! Button clicked
        {
            let bridge = self.clone();
            window.on_flash_clicked(move || {
                bridge.handle_flash();
            });
        }

        // Cancel flashing button clicked (prompts confirmation modal)
        {
            let bridge = self.clone();
            window.on_cancel_flash_clicked(move || {
                bridge.handle_cancel_flash_requested();
            });
        }

        // Confirm cancellation in modal
        {
            let bridge = self.clone();
            window.on_confirm_cancel_clicked(move || {
                bridge.handle_confirm_cancel();
            });
        }

        // Dismiss cancellation modal
        {
            let bridge = self.clone();
            window.on_dismiss_cancel_modal_clicked(move || {
                bridge.handle_dismiss_cancel_modal();
            });
        }

        // Dismiss celebration or error screen
        {
            let bridge = self.clone();
            window.on_dismiss_result_clicked(move || {
                bridge.handle_dismiss_result();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_file_drop_uri_percent_decoding_and_normalization() {
        // Standard unix file URI with percent-encoded spaces
        let unix_uri = "file:///home/user/Ubuntu%2024.04%20Live.iso";
        assert_eq!(
            parse_file_drop_uri(unix_uri),
            PathBuf::from("/home/user/Ubuntu 24.04 Live.iso")
        );

        // Windows file URI with leading slash before drive letter
        let win_uri = "file:///C:/Users/Tester/Downloads/disk%20image.img";
        assert_eq!(
            parse_file_drop_uri(win_uri),
            PathBuf::from("C:/Users/Tester/Downloads/disk image.img")
        );

        // Localhost URI
        let localhost_uri = "file://localhost/tmp/tails-os.iso";
        assert_eq!(
            parse_file_drop_uri(localhost_uri),
            PathBuf::from("/tmp/tails-os.iso")
        );

        // Raw local path without scheme
        let raw_path = "/var/isos/debian-12.iso";
        assert_eq!(
            parse_file_drop_uri(raw_path),
            PathBuf::from("/var/isos/debian-12.iso")
        );

        // UTF-8 multibyte percent encoding
        let utf8_uri = "file:///tmp/%E2%9C%93_image.iso";
        assert_eq!(
            parse_file_drop_uri(utf8_uri),
            PathBuf::from("/tmp/✓_image.iso")
        );
    }

    #[test]
    fn test_active_flash_drop_cancels_worker_and_joins() {
        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel_token.clone();
        let (_progress_tx, progress_rx) = crossbeam_channel::unbounded();

        let thread_handle = std::thread::spawn(move || {
            for _ in 0..100 {
                if cancel_clone.load(Ordering::SeqCst) {
                    return Err(InscribeError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(InscribeError::Cancelled)
        });

        let active = ActiveFlash {
            cancel_token: cancel_token.clone(),
            progress_rx,
            thread_handle: Some(thread_handle),
            completed_result: None,
        };

        assert!(!cancel_token.load(Ordering::SeqCst));
        drop(active);
        // ActiveFlash drop must have set cancel_token to true and joined the thread
        assert!(
            cancel_token.load(Ordering::SeqCst),
            "ActiveFlash drop must set cancel_token to true"
        );
    }

    #[test]
    fn test_validate_dropped_file_rejects_directories_fifos_and_unsupported_extensions() {
        let temp_dir = std::env::temp_dir().join(format!("test_drop_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // 1. Directory must be rejected
        let res = validate_dropped_file(&temp_dir);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("not a regular file"));

        // 2. Unsupported extension must be rejected
        let txt_file = temp_dir.join("test.txt");
        std::fs::write(&txt_file, b"not an image").unwrap();
        let res = validate_dropped_file(&txt_file);
        assert!(res.is_err());
        assert!(
            res.unwrap_err()
                .to_string()
                .contains("supported disk image extension")
        );

        // 3. FIFO / named pipe on unix must be rejected
        #[cfg(unix)]
        {
            use std::ffi::CString;
            let fifo_file = temp_dir.join("test.iso");
            let c_path = CString::new(fifo_file.to_str().unwrap()).unwrap();
            unsafe {
                libc::mkfifo(c_path.as_ptr(), 0o644);
            }
            let res = validate_dropped_file(&fifo_file);
            assert!(res.is_err());
            assert!(res.unwrap_err().to_string().contains("not a regular file"));
            let _ = std::fs::remove_file(&fifo_file);
        }

        // 4. Valid regular file with supported extension must succeed
        let iso_file = temp_dir.join("debian.iso");
        std::fs::write(&iso_file, b"fake iso data").unwrap();
        assert!(validate_dropped_file(&iso_file).is_ok());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[derive(Default)]
    struct CountingExecutor {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FlashExecutor for CountingExecutor {
        fn start_flash(
            &self,
            _image_path: &Path,
            _drive: &DriveInfo,
            _allow_external_usb_ssd: bool,
        ) -> Result<FlashHandle, InscribeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (_tx, progress_rx) = crossbeam_channel::unbounded();
            let cancel_token = Arc::new(AtomicBool::new(false));
            let thread_handle = std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                Err(InscribeError::Cancelled)
            });
            Ok(FlashHandle {
                cancel_token,
                progress_rx,
                thread_handle,
            })
        }
    }

    #[test]
    fn test_handle_flash_double_invocation_drops_second_call() {
        let scanner = Arc::new(inscribe_core::drive::MockDriveScanner::empty());
        let drive = DriveInfo {
            path: PathBuf::from("/dev/sdb"),
            model: "USB Drive".to_string(),
            vendor: "Vendor".to_string(),
            size: 8_000_000_000,
            sector_size: 512,
            bus_type: inscribe_core::drive::BusType::Usb,
            is_removable: true,
            is_read_only: false,
            is_system: false,
            mount_points: vec![],
        };
        scanner.add_drive(drive);

        let temp_dir =
            std::env::temp_dir().join(format!("test_flash_double_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let img_path = temp_dir.join("image.iso");
        std::fs::write(&img_path, vec![0u8; 1024]).unwrap();

        let executor = Arc::new(CountingExecutor::default());
        let bridge = GuiBridge::with_executor(
            Weak::default(),
            scanner,
            Arc::new(MockFilePicker::new()),
            executor.clone(),
        );

        bridge.scan_and_update_ui();
        bridge.apply_image_path(img_path);
        bridge.handle_select_drive_by_path(Path::new("/dev/sdb"));

        // Rapid double invocation of handle_flash
        bridge.handle_flash();
        bridge.handle_flash();

        // Must only launch executor ONCE
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_drive_poller_pauses_mutations_while_flashing() {
        let scanner = Arc::new(inscribe_core::drive::MockDriveScanner::empty());
        let drive = DriveInfo {
            path: PathBuf::from("/dev/sdb"),
            model: "USB Drive".to_string(),
            vendor: "Vendor".to_string(),
            size: 8_000_000_000,
            sector_size: 512,
            bus_type: inscribe_core::drive::BusType::Usb,
            is_removable: true,
            is_read_only: false,
            is_system: false,
            mount_points: vec![],
        };
        scanner.add_drive(drive);

        let bridge = GuiBridge::with_executor(
            Weak::default(),
            scanner,
            Arc::new(MockFilePicker::new()),
            Arc::new(CountingExecutor::default()),
        );

        bridge.scan_and_update_ui();
        bridge.handle_select_drive_by_path(Path::new("/dev/sdb"));
        assert!(bridge.state().lock().unwrap().selected_drive().is_some());

        // Set flashing to true
        bridge.state().lock().unwrap().set_is_flashing(true);

        // Apply an update where drives list is empty (e.g. drive removal event)
        bridge.apply_drive_update(vec![]);

        // Drive list mutations must be paused; selected_drive must NOT be cleared
        assert!(
            bridge.state().lock().unwrap().selected_drive().is_some(),
            "Selected drive must not be removed while is_flashing is true"
        );
        assert_eq!(
            bridge.state().lock().unwrap().drives().len(),
            1,
            "Drives list must not be wiped while is_flashing is true"
        );
    }

    #[test]
    fn test_real_flash_executor_rejects_system_drive() {
        let sys_drive = DriveInfo {
            path: PathBuf::from("/dev/nvme0n1"),
            model: "System NVMe".to_string(),
            vendor: "Samsung".to_string(),
            size: 500_000_000_000,
            sector_size: 512,
            bus_type: inscribe_core::drive::BusType::Nvme,
            is_removable: false,
            is_read_only: false,
            is_system: true,
            mount_points: vec![PathBuf::from("/")],
        };

        let temp_dir = std::env::temp_dir().join(format!("test_real_exec_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let img_path = temp_dir.join("image.iso");
        std::fs::write(&img_path, vec![0u8; 1024]).unwrap();

        let executor = RealFlashExecutor::new();
        let res = executor.start_flash(&img_path, &sys_drive, false);
        match res {
            Err(InscribeError::DriveGuardViolation { .. }) => (),
            _ => panic!("Expected DriveGuardViolation"),
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
