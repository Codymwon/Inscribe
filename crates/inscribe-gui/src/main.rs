use inscribe_core::drive::default_scanner;
use inscribe_gui::AppWindow;
use inscribe_gui::bridge::{GuiBridge, RfdFilePicker};
use slint::ComponentHandle;
use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt::init();
    let window = AppWindow::new()?;

    let scanner = Arc::from(default_scanner());
    let picker = Arc::new(RfdFilePicker::new());
    let bridge = GuiBridge::new(window.as_weak(), scanner, picker);
    bridge.wire_callbacks(&window);

    // Initial drive scan and start background hotplug poller (every 1 second)
    bridge.scan_and_update_ui();
    bridge.start_polling(Duration::from_secs(1));

    window.run()?;
    Ok(())
}
