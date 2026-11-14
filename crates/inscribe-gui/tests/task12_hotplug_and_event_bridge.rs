use inscribe_core::drive::{BusType, DriveInfo, DriveScanner, MockDriveScanner};
use inscribe_gui::AppWindow;
use inscribe_gui::bridge::{
    DrivePoller, FilePicker, GuiBridge, MockFilePicker, SUPPORTED_IMAGE_EXTENSIONS,
};
use inscribe_gui::state::{AppState, DriveStateChange};
use slint::{ComponentHandle, Model};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some() {
            unsafe {
                std::env::set_var("WINIT_UNIX_BACKEND", "x11");
            }
        }
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

fn sample_system_nvme_drive(path: &str) -> DriveInfo {
    DriveInfo {
        path: PathBuf::from(path),
        model: "Samsung NVMe 980 Pro".to_string(),
        vendor: "Samsung".to_string(),
        size: 1_000_000_000_000,
        sector_size: 512,
        bus_type: BusType::Nvme,
        is_removable: false,
        is_read_only: false,
        is_system: true,
        mount_points: vec![PathBuf::from("/")],
    }
}

fn sample_external_usb_ssd(path: &str) -> DriveInfo {
    DriveInfo {
        path: PathBuf::from(path),
        model: "Crucial X9 Pro SSD".to_string(),
        vendor: "Crucial".to_string(),
        size: 500_000_000_000,
        sector_size: 512,
        bus_type: BusType::Usb,
        is_removable: false,
        is_read_only: false,
        is_system: false,
        mount_points: vec![],
    }
}

fn create_temp_dummy_image(name: &str, size_bytes: usize) -> TestTempDir {
    let dir = TestTempDir::new("inscribe_img_test");
    let file_path = dir.path().join(name);
    let mut file = File::create(&file_path).expect("Failed to create test image");
    let buf = vec![0x42u8; size_bytes];
    file.write_all(&buf).expect("Failed to write test bytes");
    dir
}

#[test]
fn test_app_state_image_selection_and_format_inspection() {
    let temp_dir = create_temp_dummy_image("test_os_install.iso", 1024 * 1024);
    let image_path = temp_dir.path().join("test_os_install.iso");

    let mut state = AppState::new();
    assert!(!state.can_flash());
    assert!(state.selected_image().is_none());

    let selected = state
        .set_image(image_path.clone())
        .expect("Image selection should succeed for existing file");
    assert_eq!(selected.name, "test_os_install.iso");
    assert_eq!(selected.size, 1024 * 1024);
    assert_eq!(selected.path, image_path);

    assert!(state.selected_image().is_some());
    assert!(!state.can_flash(), "Cannot flash without a target drive");

    state.clear_image();
    assert!(state.selected_image().is_none());
}

#[test]
fn test_app_state_drive_management_and_unplug_reactivity() {
    let mut state = AppState::new();

    let usb_drive = sample_removable_drive("/dev/sdb", "SanDisk Ultra", 32_000_000_000);
    let nvme_drive = sample_system_nvme_drive("/dev/nvme0n1");
    let ssd_drive = sample_external_usb_ssd("/dev/sdc");

    // Add drives
    let change = state.set_drives(vec![
        usb_drive.clone(),
        nvme_drive.clone(),
        ssd_drive.clone(),
    ]);
    assert_eq!(change, DriveStateChange::NoSelectionChange);
    assert_eq!(state.drives().len(), 3);

    // NVMe is never flashable
    assert!(!state.is_drive_flashable(&nvme_drive));
    // USB flash drive is flashable
    assert!(state.is_drive_flashable(&usb_drive));
    // External USB SSD is not flashable by default
    assert!(!state.is_drive_flashable(&ssd_drive));

    // Enable external USB SSDs
    state.set_allow_external_usb_ssd(true);
    assert!(state.is_drive_flashable(&ssd_drive));
    assert!(!state.is_drive_flashable(&nvme_drive));

    // Select USB drive
    assert!(state.select_drive(Path::new("/dev/sdb")));
    assert_eq!(
        state.selected_drive().unwrap().path,
        PathBuf::from("/dev/sdb")
    );

    // Cannot select unflashable internal NVMe drive
    assert!(!state.select_drive(Path::new("/dev/nvme0n1")));

    // Simulate unplug of selected drive
    let change = state.set_drives(vec![nvme_drive.clone(), ssd_drive.clone()]);
    assert_eq!(change, DriveStateChange::SelectedDriveRemoved);
    assert!(
        state.selected_drive().is_none(),
        "Selected drive must reset on removal"
    );
}

#[test]
fn test_supported_image_extensions() {
    let expected = ["iso", "img", "xz", "gz", "zst", "bz2", "zip"];
    for ext in &expected {
        assert!(
            SUPPORTED_IMAGE_EXTENSIONS.contains(ext),
            "SUPPORTED_IMAGE_EXTENSIONS must include {ext}"
        );
    }
}

#[test]
fn test_drive_poller_detects_hotplug_events() {
    let mock_scanner = Arc::new(MockDriveScanner::empty());
    let usb1 = sample_removable_drive("/dev/sdb", "USB 1", 16_000_000_000);
    let usb2 = sample_removable_drive("/dev/sdc", "USB 2", 32_000_000_000);

    let event_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let last_drives = Arc::new(std::sync::Mutex::new(Vec::new()));

    let ec = event_count.clone();
    let ld = last_drives.clone();

    // Start poller with 20ms interval for fast test execution
    let mut poller = DrivePoller::start(
        mock_scanner.clone() as Arc<dyn DriveScanner>,
        Duration::from_millis(20),
        move |drives| {
            ec.fetch_add(1, Ordering::SeqCst);
            let mut lock = ld.lock().unwrap();
            *lock = drives;
        },
    );

    // Initial state: 0 drives
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(last_drives.lock().unwrap().len(), 0);

    // Event 1: Plug in USB 1
    mock_scanner.add_drive(usb1.clone());
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(last_drives.lock().unwrap().len(), 1);
    assert_eq!(
        last_drives.lock().unwrap()[0].path,
        PathBuf::from("/dev/sdb")
    );

    // Event 2: Plug in USB 2
    mock_scanner.add_drive(usb2.clone());
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(last_drives.lock().unwrap().len(), 2);

    // Event 3: Unplug USB 1
    mock_scanner.remove_drive(Path::new("/dev/sdb"));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(last_drives.lock().unwrap().len(), 1);
    assert_eq!(
        last_drives.lock().unwrap()[0].path,
        PathBuf::from("/dev/sdc")
    );

    // Gracefully stop poller
    poller.stop();
}

#[test]
fn test_gui_bridge_full_lifecycle_and_slint_reactive_model() {
    init_test_env();

    let window = match AppWindow::new() {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "Skipping UI integration test: display server unavailable in headless test environment: {e}"
            );
            return;
        }
    };
    let mock_scanner = Arc::new(MockDriveScanner::empty());
    let mock_picker = Arc::new(MockFilePicker::new());

    let usb_drive = sample_removable_drive("/dev/sdb", "SanDisk Ultra USB 3.0", 32_000_000_000);
    let nvme_drive = sample_system_nvme_drive("/dev/nvme0n1");

    let bridge = GuiBridge::new(
        window.as_weak(),
        mock_scanner.clone() as Arc<dyn DriveScanner>,
        mock_picker.clone() as Arc<dyn FilePicker>,
    );

    // 1. Initial UI state
    assert!(!window.get_image_selected());
    assert!(!window.get_drive_selected());
    assert_eq!(window.get_drive_count(), 0);
    assert!(!window.get_can_flash());

    // 2. Hotplug detected: plug in USB and NVMe
    mock_scanner.add_drive(usb_drive.clone());
    mock_scanner.add_drive(nvme_drive.clone());

    // Trigger scan update through bridge
    bridge.scan_and_update_ui();

    // Verify Slint UI updated reactively
    assert_eq!(window.get_drive_count(), 2);
    let slint_drives = window.get_drives();
    assert_eq!(slint_drives.row_count(), 2);

    let item0 = slint_drives.row_data(0).unwrap();
    assert_eq!(item0.path, "/dev/sdb");
    assert!(item0.is_flashable);
    assert!(!item0.is_system);

    let item1 = slint_drives.row_data(1).unwrap();
    assert_eq!(item1.path, "/dev/nvme0n1");
    assert!(
        !item1.is_flashable,
        "System NVMe drive must not be flashable"
    );
    assert!(item1.is_system);

    // 3. Select Image via file picker
    let temp_dir = create_temp_dummy_image("archlinux-2026.iso", 2_000_000);
    let img_path = temp_dir.path().join("archlinux-2026.iso");
    mock_picker.set_next_file(Some(img_path.clone()));

    // Invoke image picker callback
    bridge.handle_select_image();
    assert!(window.get_image_selected());
    assert_eq!(window.get_image_name(), "archlinux-2026.iso");
    assert_eq!(window.get_image_size_text(), "2.0 MB");
    assert!(!window.get_can_flash(), "Need target drive to enable flash");

    // 4. Select Target Drive
    bridge.handle_select_drive_by_path(Path::new("/dev/sdb"));
    assert!(window.get_drive_selected());
    assert_eq!(window.get_drive_name(), "SanDisk Ultra USB 3.0 (/dev/sdb)");
    assert_eq!(window.get_drive_size_text(), "32.0 GB");

    // Both image and target selected -> Can Flash!
    assert!(window.get_can_flash());

    // 5. Unplug the selected drive -> UI must reactively reset target and can_flash
    mock_scanner.remove_drive(Path::new("/dev/sdb"));
    bridge.scan_and_update_ui();

    assert_eq!(window.get_drive_count(), 1);
    assert!(
        !window.get_drive_selected(),
        "Target drive selection must clear on unplug"
    );
    assert_eq!(window.get_drive_name(), "");
    assert!(
        !window.get_can_flash(),
        "can_flash must revert to false when target is removed"
    );
    assert!(
        window.get_image_selected(),
        "Selected image remains selected"
    );

    // 6. Test File Drop Handling with URL Percent-Decoding
    let drop_dir = create_temp_dummy_image("fedora workstation 2026.img.xz", 512 * 1024);
    let file_uri = format!(
        "file://{}/fedora%20workstation%202026.img.xz",
        drop_dir.path().to_str().unwrap()
    );
    bridge.handle_file_dropped(&file_uri);
    assert!(window.get_image_selected());
    assert_eq!(window.get_image_name(), "fedora workstation 2026.img.xz");

    // 7. Test Select Drive by Index
    let usb2 = sample_removable_drive("/dev/sdd", "Kingston DataTraveler", 64_000_000_000);
    mock_scanner.add_drive(usb2);
    bridge.scan_and_update_ui();
    assert_eq!(window.get_drive_count(), 2);

    // Index 1 should be the new Kingston USB
    bridge.handle_select_drive_by_index(1);
    assert!(window.get_drive_selected());
    assert!(window.get_drive_name().contains("Kingston DataTraveler"));
    assert!(window.get_can_flash());

    // 8. Test External USB SSD modal toggle reactivity
    let usb_ssd = sample_external_usb_ssd("/dev/sde");
    mock_scanner.add_drive(usb_ssd);
    bridge.scan_and_update_ui();
    assert_eq!(window.get_drive_count(), 3);

    // USB SSD index 2 is not flashable by default
    let slint_drives = window.get_drives();
    let ssd_item = slint_drives.row_data(2).unwrap();
    assert_eq!(ssd_item.path, "/dev/sde");
    assert!(!ssd_item.is_flashable);

    // Toggle USB SSD to allowed
    bridge.handle_toggle_allow_external_usb_ssd(true);
    let slint_drives = window.get_drives();
    let ssd_item = slint_drives.row_data(2).unwrap();
    assert!(ssd_item.is_flashable);

    // 9. Wire callbacks and verify UI callback updates backend AppState
    bridge.wire_callbacks(&window);

    // Toggle back to false via UI callback
    window.invoke_allow_external_usb_ssd_changed(false);
    assert!(!bridge.state().lock().unwrap().allow_external_usb_ssd());
    assert!(!window.get_allow_external_usb_ssd());
    let slint_drives = window.get_drives();
    let ssd_item = slint_drives.row_data(2).unwrap();
    assert!(!ssd_item.is_flashable);

    // Toggle to true via UI callback
    window.invoke_allow_external_usb_ssd_changed(true);
    assert!(bridge.state().lock().unwrap().allow_external_usb_ssd());
    assert!(window.get_allow_external_usb_ssd());
    let slint_drives = window.get_drives();
    let ssd_item = slint_drives.row_data(2).unwrap();
    assert!(ssd_item.is_flashable);
}
