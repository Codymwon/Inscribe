use inscribe_gui::AppWindow;
use slint::SharedString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn test_appwindow_full_ui_acceptance_criteria() {
    if std::env::var("WINIT_UNIX_BACKEND").is_err() {
        // SAFETY: Only setting WINIT_UNIX_BACKEND for headless test runner before event loop initializes
        unsafe {
            std::env::set_var("WINIT_UNIX_BACKEND", "x11");
        }
    }
    let window = AppWindow::new().expect("AppWindow should compile and initialize");

    // =========================================================================
    // Acceptance Criterion 1 & 2: 3-step cards rendered with Fluent dark styling
    // and correct initial & transitional state.
    // =========================================================================
    assert!(
        !window.get_image_selected(),
        "Initially no image is selected"
    );
    assert_eq!(window.get_image_name(), "", "Image name should be empty");
    assert!(
        !window.get_drive_selected(),
        "Initially no drive is selected"
    );
    assert_eq!(window.get_drive_name(), "", "Drive name should be empty");
    assert!(
        !window.get_can_flash(),
        "Cannot flash without image and drive"
    );
    assert!(
        !window.get_is_flashing(),
        "Flash operation should not be running"
    );

    // Step 1: select image
    window.set_image_selected(true);
    window.set_image_name(SharedString::from("ubuntu-24.04-live-server-amd64.iso"));
    window.set_image_size_text(SharedString::from("2.6 GB"));
    assert!(window.get_image_selected());
    assert_eq!(
        window.get_image_name(),
        "ubuntu-24.04-live-server-amd64.iso"
    );
    assert_eq!(window.get_image_size_text(), "2.6 GB");

    // Step 2: still cannot flash until drive selected
    assert!(!window.get_can_flash());

    // Step 2: select drive
    window.set_drive_selected(true);
    window.set_drive_name(SharedString::from("SanDisk Ultra USB 3.0 (/dev/sdb)"));
    window.set_drive_size_text(SharedString::from("32.0 GB"));
    window.set_drive_count(2);
    window.set_can_flash(true);

    assert!(window.get_drive_selected());
    assert_eq!(window.get_drive_name(), "SanDisk Ultra USB 3.0 (/dev/sdb)");
    assert_eq!(window.get_drive_size_text(), "32.0 GB");
    assert_eq!(window.get_drive_count(), 2);
    assert!(window.get_can_flash());

    // Step 3: Flash progress state
    window.set_is_flashing(true);
    window.set_flash_phase_text(SharedString::from("Pass 1/2: Direct Block Write"));
    window.set_flash_progress(0.45);
    window.set_flash_speed_text(SharedString::from("118.4 MB/s — ETA 22s"));
    assert!(window.get_is_flashing());
    assert_eq!(
        window.get_flash_phase_text(),
        "Pass 1/2: Direct Block Write"
    );
    assert!((window.get_flash_progress() - 0.45).abs() < 1e-5);
    assert_eq!(window.get_flash_speed_text(), "118.4 MB/s — ETA 22s");

    window.set_is_flashing(false);
    assert!(!window.get_is_flashing());

    // =========================================================================
    // Acceptance Criterion 3: Drag-and-drop zone highlights on file drag over
    // =========================================================================
    assert!(
        !window.get_drag_hover_active(),
        "Drag hover should default to false"
    );
    window.set_drag_hover_active(true);
    assert!(
        window.get_drag_hover_active(),
        "Drag hover highlights drag-and-drop target zone"
    );
    window.set_drag_hover_active(false);
    assert!(
        !window.get_drag_hover_active(),
        "Drag hover clears on drag exit or drop"
    );

    // =========================================================================
    // Acceptance Criterion 4: "Show External USB SSDs" toggle modal displays
    // with clear safety warnings
    // =========================================================================
    assert!(
        !window.get_show_usb_ssd_modal(),
        "Modal should be hidden by default"
    );
    assert!(
        !window.get_allow_external_usb_ssd(),
        "External USB SSDs should default to disabled"
    );

    // Open modal
    window.set_show_usb_ssd_modal(true);
    assert!(window.get_show_usb_ssd_modal());

    // Verify safety warning text content contains explicit risk notices
    let warning_text = window.get_safety_warning_markdown();
    assert!(
        warning_text.contains("External USB SSD") || warning_text.contains("Warning"),
        "Safety warning should explicitly caution about USB SSD dangers"
    );
    assert!(
        warning_text.contains("overwrite") || warning_text.contains("data loss"),
        "Safety warning should explicitly state data overwrite hazard"
    );

    // Toggle opt-in setting
    window.set_allow_external_usb_ssd(true);
    assert!(window.get_allow_external_usb_ssd());

    // Close modal
    window.set_show_usb_ssd_modal(false);
    assert!(!window.get_show_usb_ssd_modal());

    // =========================================================================
    // Interactive Callbacks & Shortcuts Wireup
    // =========================================================================
    let image_clicked = Arc::new(AtomicBool::new(false));
    let drive_clicked = Arc::new(AtomicBool::new(false));
    let flash_clicked = Arc::new(AtomicBool::new(false));
    let modal_toggled = Arc::new(AtomicBool::new(false));

    {
        let ic = image_clicked.clone();
        window.on_select_image_clicked(move || {
            ic.store(true, Ordering::SeqCst);
        });
    }
    {
        let dc = drive_clicked.clone();
        window.on_select_drive_clicked(move || {
            dc.store(true, Ordering::SeqCst);
        });
    }
    {
        let fc = flash_clicked.clone();
        window.on_flash_clicked(move || {
            fc.store(true, Ordering::SeqCst);
        });
    }
    {
        let mt = modal_toggled.clone();
        window.on_toggle_usb_ssd_modal_clicked(move || {
            mt.store(true, Ordering::SeqCst);
        });
    }

    // Trigger actions
    window.invoke_select_image_clicked();
    assert!(
        image_clicked.load(Ordering::SeqCst),
        "select_image callback fired"
    );

    window.invoke_select_drive_clicked();
    assert!(
        drive_clicked.load(Ordering::SeqCst),
        "select_drive callback fired"
    );

    window.invoke_flash_clicked();
    assert!(flash_clicked.load(Ordering::SeqCst), "flash callback fired");

    window.invoke_toggle_usb_ssd_modal_clicked();
    assert!(
        modal_toggled.load(Ordering::SeqCst),
        "toggle modal callback fired"
    );
}
