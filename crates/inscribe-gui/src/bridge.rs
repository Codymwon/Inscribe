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

struct ActiveFlash {
    cancel_token: Arc<AtomicBool>,
    progress_rx: crossbeam_channel::Receiver<FlashProgress>,
    thread_handle: Option<std::thread::JoinHandle<Result<FlashSummary, InscribeError>>>,
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

            state.set_is_flashing(true);
            (image_path, drive, state.allow_external_usb_ssd())
        };

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
            });
        }

        let bridge = self.clone();
        std::thread::Builder::new()
            .name("inscribe-telemetry-bridge".to_string())
            .spawn(move || {
                while let Ok(progress) = progress_rx_for_bridge.recv() {
                    bridge.apply_flash_progress(&progress);
                }

                // Channel disconnected -> worker finished. Finalize on UI thread.
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
        let handle_opt = {
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

                    if active
                        .thread_handle
                        .as_ref()
                        .is_some_and(|h| h.is_finished())
                    {
                        let h = active.thread_handle.take();
                        *lock = None;
                        h
                    } else {
                        None
                    }
                }
                None => None,
            }
        };

        if let Some(handle) = handle_opt {
            match handle.join() {
                Ok(Ok(summary)) => self.apply_flash_success(&summary),
                Ok(Err(err)) => self.apply_flash_error(&err),
                Err(_) => self.apply_flash_error(&InscribeError::Other(
                    "Flash worker thread panicked".to_string(),
                )),
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

    /// Transitions the UI to the error summary screen with actionable failure details.
    pub fn apply_flash_error(&self, err: &InscribeError) {
        if let Ok(mut state) = self.state.lock() {
            state.set_is_flashing(false);
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
