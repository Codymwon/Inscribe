use inscribe_gui::AppWindow;
use slint::ComponentHandle;

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt::init();
    let window = AppWindow::new()?;
    window.run()?;
    Ok(())
}
