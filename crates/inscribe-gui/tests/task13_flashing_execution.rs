use inscribe_core::drive::{BusType, DriveInfo, MockDriveScanner};
use inscribe_core::error::InscribeError;
use inscribe_core::pipeline::{FlashHandle, FlashSummary};
use inscribe_core::progress::{FlashPhase, FlashProgress};
use inscribe_gui::AppWindow;
use inscribe_gui::bridge::{FlashExecutor, GuiBridge, MockFilePicker};
use slint::ComponentHandle;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("Failed to create test temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn init_test_env() {
    // SAFETY: Preparing environment for test runner before Winit event loop initializes
    unsafe {
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::set_var("WINIT_UNIX_BACKEND", "x11");
    }
}

fn sample_removable_drive(path: &str, model: &str, size_bytes: u64) -> DriveInfo {
    let vendor = model
        .split_whitespace()
        .next()
        .unwrap_or("Vendor")
        .to_string();
    DriveInfo {
        path: PathBuf::from(path),
        model: model.to_string(),
        vendor,
        size: size_bytes,
        sector_size: 512,
        bus_type: BusType::Usb,
        is_removable: true,
        is_read_only: false,
        is_system: false,
        mount_points: vec![],
    }
}

fn create_temp_dummy_image(name: &str, size_bytes: usize) -> TestTempDir {
    let dir = TestTempDir::new("inscribe_task13_img");
    let file_path = dir.path().join(name);
    let mut file = File::create(&file_path).expect("Failed to create test image");
    let buf = vec![0x5au8; size_bytes];
    file.write_all(&buf).expect("Failed to write test bytes");
    dir
}

/// Mock flash executor to simulate flashing telemetry, cancellation, and outcomes deterministically.
pub struct MockFlashExecutor {
    pub simulated_progress: Mutex<Vec<FlashProgress>>,
    pub simulated_result: Mutex<Option<Result<FlashSummary, InscribeError>>>,
    pub was_called: AtomicBool,
    pub step_delay: Duration,
}

impl MockFlashExecutor {
    pub fn new() -> Self {
        Self {
            simulated_progress: Mutex::new(Vec::new()),
            simulated_result: Mutex::new(None),
            was_called: AtomicBool::new(false),
            step_delay: Duration::from_millis(10),
        }
    }

    pub fn with_success(summary: FlashSummary, progress_events: Vec<FlashProgress>) -> Self {
        let executor = Self::new();
        *executor.simulated_result.lock().unwrap() = Some(Ok(summary));
        *executor.simulated_progress.lock().unwrap() = progress_events;
        executor
    }
}

impl Default for MockFlashExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl FlashExecutor for MockFlashExecutor {
    fn start_flash(
        &self,
        _image_path: &Path,
        _drive: &DriveInfo,
        _allow_external_usb_ssd: bool,
    ) -> Result<FlashHandle, InscribeError> {
        self.was_called.store(true, Ordering::SeqCst);
        let (progress_tx, progress_rx) = crossbeam_channel::unbounded();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_for_worker = Arc::clone(&cancel_token);

        let progress_events = self.simulated_progress.lock().unwrap().clone();
        let result = self
            .simulated_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(FlashSummary {
                    bytes_written: 1024 * 1024,
                    blake3_hash: "mock-blake3-hash-1234567890".to_string(),
                    sha256_hash: None,
                    duration: Duration::from_secs(2),
                    average_mb_per_sec: 120.5,
                })
            });

        let delay = self.step_delay;

        let thread_handle = std::thread::spawn(move || {
            for event in progress_events {
                if cancel_for_worker.load(Ordering::SeqCst) {
                    return Err(InscribeError::Cancelled);
                }
                let _ = progress_tx.send(event);
                if delay > Duration::ZERO {
                    std::thread::sleep(delay);
                }
            }

            if cancel_for_worker.load(Ordering::SeqCst) {
                return Err(InscribeError::Cancelled);
            }

            result
        });

        Ok(FlashHandle {
            cancel_token,
            progress_rx,
            thread_handle,
        })
    }
}

#[test]
fn test_task13_flashing_execution_full_acceptance_criteria() {
    init_test_env();
    let window = AppWindow::new().expect("AppWindow should compile and initialize");
    let mock_scanner = Arc::new(MockDriveScanner::empty());
    let mock_picker = Arc::new(MockFilePicker::new());

    let drive = sample_removable_drive("/dev/sdb", "SanDisk Ultra", 16_000_000_000);
    mock_scanner.add_drive(drive);

    let temp_img = create_temp_dummy_image("debian-live.iso", 1024 * 1024);
    let img_path = temp_img.path().join("debian-live.iso");

    let progress_events = vec![
        FlashProgress {
            phase: FlashPhase::Flashing,
            current_pass: 1,
            total_passes: 2,
            bytes_processed: 512 * 1024,
            total_bytes: Some(1024 * 1024),
            bytes_per_second: 50.0 * 1024.0 * 1024.0,
            eta_seconds: Some(1),
            fraction: Some(0.5),
        },
        FlashProgress {
            phase: FlashPhase::Verifying,
            current_pass: 2,
            total_passes: 2,
            bytes_processed: 1024 * 1024,
            total_bytes: Some(1024 * 1024),
            bytes_per_second: 90.0 * 1024.0 * 1024.0,
            eta_seconds: Some(0),
            fraction: Some(1.0),
        },
    ];

    let summary = FlashSummary {
        bytes_written: 1024 * 1024,
        blake3_hash: "a1b2c3d4e5f6".to_string(),
        sha256_hash: None,
        duration: Duration::from_secs(1),
        average_mb_per_sec: 75.0,
    };

    let mut executor = MockFlashExecutor::with_success(summary, progress_events);
    executor.step_delay = Duration::from_millis(15);
    let mock_executor = Arc::new(executor);

    let bridge = GuiBridge::with_executor(
        window.as_weak(),
        mock_scanner,
        mock_picker,
        mock_executor.clone(),
    );
    bridge.wire_callbacks(&window);

    bridge.scan_and_update_ui();
    bridge.apply_image_path(img_path);
    bridge.handle_select_drive_by_path(Path::new("/dev/sdb"));

    // Verify initial ready-to-flash state
    assert!(window.get_can_flash());
    assert!(!window.get_is_flashing());

    // =========================================================================
    // Acceptance Criterion 1: Clicking "Flash!" launches flash worker without
    // blocking the UI thread.
    // =========================================================================
    window.invoke_flash_clicked();

    assert!(mock_executor.was_called.load(Ordering::SeqCst));
    assert!(window.get_is_flashing());

    // =========================================================================
    // Acceptance Criterion 2: Telemetry updates progress bar and live speed indicator.
    // =========================================================================
    let pass1_progress = FlashProgress {
        phase: FlashPhase::Flashing,
        current_pass: 1,
        total_passes: 2,
        bytes_processed: 512 * 1024,
        total_bytes: Some(1024 * 1024),
        bytes_per_second: 100.0 * 1024.0 * 1024.0,
        eta_seconds: Some(5),
        fraction: Some(0.5),
    };
    bridge.apply_flash_progress(&pass1_progress);

    assert!((window.get_flash_progress() - 0.5).abs() < 1e-4);
    assert!(window.get_flash_phase_text().contains("Pass 1/2"));
    assert!(window.get_flash_speed_text().contains("100.0 MB/s"));
    assert!(window.get_flash_eta_text().contains("5s"));

    let pass2_progress = FlashProgress {
        phase: FlashPhase::Verifying,
        current_pass: 2,
        total_passes: 2,
        bytes_processed: 1024 * 1024,
        total_bytes: Some(1024 * 1024),
        bytes_per_second: 150.0 * 1024.0 * 1024.0,
        eta_seconds: Some(0),
        fraction: Some(1.0),
    };
    bridge.apply_flash_progress(&pass2_progress);

    assert!((window.get_flash_progress() - 1.0).abs() < 1e-4);
    assert!(window.get_flash_phase_text().contains("Pass 2/2"));
    assert!(window.get_flash_speed_text().contains("150.0 MB/s"));

    // =========================================================================
    // Acceptance Criterion 3: Cancellation button prompts confirmation and
    // cleanly halts pipeline.
    // =========================================================================
    // 1. User clicks "Cancel Flashing" -> modal appears
    assert!(!window.get_show_cancel_modal());
    window.invoke_cancel_flash_clicked();
    assert!(window.get_show_cancel_modal());
    assert!(window.get_is_flashing());

    // 2. User dismisses cancel confirmation -> flashing continues
    window.invoke_dismiss_cancel_modal_clicked();
    assert!(!window.get_show_cancel_modal());
    assert!(window.get_is_flashing());

    // 3. User clicks cancel again and confirms abort
    window.invoke_cancel_flash_clicked();
    assert!(window.get_show_cancel_modal());
    window.invoke_confirm_cancel_clicked();
    assert!(!window.get_show_cancel_modal());

    // Worker thread halts cleanly
    std::thread::sleep(Duration::from_millis(60));
    assert!(bridge.poll_active_flash());

    assert!(!window.get_is_flashing());
    assert!(window.get_show_error_screen());
    assert!(window.get_error_title_text().contains("Cancel"));

    // Dismiss error screen
    window.invoke_dismiss_result_clicked();
    assert!(!window.get_show_error_screen());

    // =========================================================================
    // Acceptance Criterion 4: Completion transitions to celebration screen;
    // errors show actionable details.
    // =========================================================================
    // 1. Completion / Celebration transition
    let full_summary = FlashSummary {
        bytes_written: 32_000_000_000,
        blake3_hash: "fedcba9876543210".to_string(),
        sha256_hash: None,
        duration: Duration::from_secs(250),
        average_mb_per_sec: 128.0,
    };
    bridge.apply_flash_success(&full_summary);

    assert!(!window.get_is_flashing());
    assert!(window.get_show_success_screen());
    assert!(!window.get_show_error_screen());
    assert!(window.get_success_summary_text().contains("32.0 GB"));
    assert!(window.get_success_summary_text().contains("128.0 MB/s"));
    assert!(window.get_success_hash_text().contains("fedcba9876543210"));

    // Dismiss celebration screen
    window.invoke_dismiss_result_clicked();
    assert!(!window.get_show_success_screen());

    // 2. Actionable error: Verification failure with offset and expected/actual details
    let verification_err = InscribeError::VerificationFailed {
        offset: 1024,
        expected: "aaaa1111".to_string(),
        actual: "bbbb2222".to_string(),
    };
    bridge.apply_flash_error(&verification_err);

    assert!(window.get_show_error_screen());
    assert!(
        window
            .get_error_message_text()
            .contains("verification failed")
            || window
                .get_error_message_text()
                .contains("Verification failed")
    );
    assert!(window.get_error_message_text().contains("1024"));
    assert!(window.get_error_message_text().contains("aaaa1111"));
    assert!(window.get_error_message_text().contains("bbbb2222"));

    window.invoke_dismiss_result_clicked();
    assert!(!window.get_show_error_screen());

    // 3. Actionable error: Insufficient capacity with byte sizes
    let cap_err = InscribeError::InsufficientCapacity {
        target_bytes: 8_000_000_000,
        image_bytes: 16_000_000_000,
    };
    bridge.apply_flash_error(&cap_err);

    assert!(window.get_show_error_screen());
    assert!(window.get_error_message_text().contains("exceeds"));
    assert!(window.get_error_message_text().contains("16.0 GB"));
    assert!(window.get_error_message_text().contains("8.0 GB"));

    window.invoke_dismiss_result_clicked();
    assert!(!window.get_show_error_screen());
}
