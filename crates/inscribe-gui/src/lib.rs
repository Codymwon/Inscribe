//! `inscribe-gui`: Native Slint graphical user interface for Inscribe.
//!
//! Provides the 3-step flashing workflow (Select Image -> Select Target -> Flash!)
//! with Fluent dark styling, keyboard shortcuts, drag-and-drop support, and
//! Drive Guard safety integration.

slint::slint! {
    export { AppWindow } from "../ui/appwindow.slint";
}

/// Create a new instance of the Inscribe application window.
pub fn create_app_window() -> Result<AppWindow, slint::PlatformError> {
    AppWindow::new()
}
