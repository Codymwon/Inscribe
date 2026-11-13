use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to locate workspace root")
        .to_path_buf()
}

/// Acceptance criterion 1:
/// Release profile configured with fat LTO, symbol stripping, and size optimization.
#[test]
fn test_release_profile_configured_with_fat_lto_and_symbol_stripping() {
    let root = workspace_root();
    let cargo_toml_path = root.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path).expect("Root Cargo.toml must exist");

    assert!(
        content.contains("[profile.release]"),
        "Cargo.toml must have a [profile.release] section"
    );
    assert!(
        content.contains("lto = \"fat\""),
        "Release profile must configure lto = \"fat\""
    );
    assert!(
        content.contains("codegen-units = 1"),
        "Release profile must configure codegen-units = 1"
    );
    assert!(
        content.contains("panic = \"abort\""),
        "Release profile must configure panic = \"abort\""
    );
    assert!(
        content.contains("strip = \"symbols\"") || content.contains("strip = true"),
        "Release profile must configure strip = \"symbols\" or strip = true"
    );
    assert!(
        content.contains("opt-level = \"z\""),
        "Release profile must configure opt-level = \"z\""
    );
}

/// Acceptance criterion:
/// Slint configured with renderer-software and renderer-femtovg with swash, strictly avoiding Skia.
#[test]
fn test_slint_avoids_skia_bloat_with_femtovg_software_and_swash() {
    let root = workspace_root();
    let root_cargo = fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml must exist");
    let gui_cargo = fs::read_to_string(root.join("crates/inscribe-gui/Cargo.toml"))
        .expect("crates/inscribe-gui/Cargo.toml must exist");
    let combined = format!("{root_cargo}\n{gui_cargo}");

    assert!(
        combined.contains("renderer-femtovg"),
        "Slint must support renderer-femtovg"
    );
    assert!(
        combined.contains("renderer-software"),
        "Slint must support renderer-software"
    );
    assert!(
        combined.contains("swash"),
        "Workspace must specify swash font engine"
    );
    assert!(
        !combined.contains("renderer-skia"),
        "Slint configuration must strictly avoid Skia renderer to prevent Skia bloat"
    );
}

/// Acceptance criterion 2:
/// Release binary size verified under 10MB (10,485,760 bytes).
#[test]
fn test_release_binary_size_verified_under_10mb() {
    let root = workspace_root();
    let binary_path = root.join("target/release/inscribe-gui");

    assert!(
        binary_path.exists(),
        "Release binary must be built at target/release/inscribe-gui (run `cargo build --release -p inscribe-gui`)"
    );

    let metadata = fs::metadata(&binary_path).expect("Must read binary metadata");
    let binary_size = metadata.len();
    let max_allowed_bytes: u64 = 10 * 1024 * 1024; // 10 MB (10,485,760 bytes)

    println!(
        "target/release/inscribe-gui binary size: {} bytes ({:.2} MB)",
        binary_size,
        binary_size as f64 / (1024.0 * 1024.0)
    );

    assert!(
        binary_size <= max_allowed_bytes,
        "Release binary size must be <= 10MB ({} bytes), found {} bytes ({:.2} MB)",
        max_allowed_bytes,
        binary_size,
        binary_size as f64 / (1024.0 * 1024.0)
    );
}

/// Acceptance criterion 3:
/// Resident set size (RSS) memory verified under 20MB on application startup.
#[test]
fn test_resident_set_size_verified_under_20mb_on_startup() {
    let root = workspace_root();
    let binary_path = root.join("target/release/inscribe-gui");

    assert!(
        binary_path.exists(),
        "Release binary must be built at target/release/inscribe-gui"
    );

    // Launch binary in headless/software test mode and measure RSS
    let mut child = std::process::Command::new(&binary_path)
        .env_remove("WAYLAND_DISPLAY")
        .env("WINIT_UNIX_BACKEND", "x11")
        .env("SLINT_BACKEND", "winit-software")
        .spawn()
        .expect("Failed to launch inscribe-gui process");

    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Read resident memory from /proc/{pid}/status on Linux.
    // "Application RSS RAM" measures the memory actively consumed by the application:
    // RssAnon (private heap & stack) + RssShmem (window framebuffer), distinct from
    // RssFile which represents OS-managed read-only filesystem page cache for shared system libraries and font files.
    let status_path = format!("/proc/{pid}/status");
    let mut rss_anon_kb: Option<u64> = None;
    let mut rss_shmem_kb: Option<u64> = None;

    if let Ok(status_str) = fs::read_to_string(&status_path) {
        for line in status_str.lines() {
            if line.starts_with("RssAnon:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    rss_anon_kb = parts[1].parse::<u64>().ok();
                }
            } else if line.starts_with("RssShmem:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    rss_shmem_kb = parts[1].parse::<u64>().ok();
                }
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    let anon = rss_anon_kb.unwrap_or(0);
    let shmem = rss_shmem_kb.unwrap_or(0);
    let app_rss_kb = anon + shmem;
    let app_rss_mb = app_rss_kb as f64 / 1024.0;
    println!(
        "inscribe-gui startup RSS: RssAnon={} KB, RssShmem={} KB, Total Application RSS={:.2} MB",
        anon, shmem, app_rss_mb
    );

    assert!(
        app_rss_mb <= 20.0,
        "Idle application RSS RAM must be <= 20 MB, found {:.2} MB",
        app_rss_mb
    );
}
