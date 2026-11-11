//! Integration tests for Task 9: CLI Privilege Validator & Platform Elevation.

use inscribe_core::error::InscribeError;
use inscribe_core::privilege::{
    ElevationCommand, ElevationStrategy, MockEnvironmentDetector, MockPrivilegeDetector,
    PrivilegeDetector, PrivilegeManager,
};
use std::sync::Arc;

#[test]
fn test_is_elevated_on_current_host() {
    // Current host execution: should return a valid boolean without panic.
    let is_elevated = PrivilegeManager::is_elevated();
    let manager = PrivilegeManager::new();
    assert_eq!(manager.check_elevated(), is_elevated);

    // Also verify the PrivilegeDetector trait implementation on PrivilegeManager.
    assert_eq!(manager.is_elevated(), is_elevated);
}

#[test]
fn test_mock_elevation_states() {
    // 1. Mock elevated detector
    let elevated_detector = Arc::new(MockPrivilegeDetector::new(true));
    let elevated_env = Arc::new(MockEnvironmentDetector::default());
    let manager_elevated = PrivilegeManager::with_detectors(elevated_detector, elevated_env);
    assert!(manager_elevated.check_elevated());
    assert_eq!(
        manager_elevated.recommended_strategy(),
        ElevationStrategy::AlreadyElevated
    );

    // 2. Mock unelevated detector
    let unelevated_detector = Arc::new(MockPrivilegeDetector::new(false));
    let unelevated_env = Arc::new(MockEnvironmentDetector::default());
    let manager_unelevated = PrivilegeManager::with_detectors(unelevated_detector, unelevated_env);
    assert!(!manager_unelevated.check_elevated());

    // 3. Static test override mechanism
    PrivilegeManager::set_test_override(Some(true));
    assert!(PrivilegeManager::is_elevated());

    PrivilegeManager::set_test_override(Some(false));
    assert!(!PrivilegeManager::is_elevated());

    // Restore to normal OS detection
    PrivilegeManager::set_test_override(None);
}

#[test]
fn test_elevation_strategy_already_elevated() {
    let detector = Arc::new(MockPrivilegeDetector::new(true));
    let env = Arc::new(MockEnvironmentDetector::linux_desktop(true));
    let manager = PrivilegeManager::with_detectors(detector, env);

    assert_eq!(
        manager.recommended_strategy(),
        ElevationStrategy::AlreadyElevated
    );
    assert!(
        manager
            .generate_elevation_command("inscribe", &["list"])
            .is_none()
    );
    assert!(manager.require_elevation("flash").is_ok());
}

#[test]
fn test_elevation_strategy_linux_headless_ssh() {
    let detector = Arc::new(MockPrivilegeDetector::new(false));
    let env = Arc::new(MockEnvironmentDetector {
        is_ssh: true,
        has_display: false,
        udisks2_available: true,
        os_type: "linux".to_string(),
    });
    let manager = PrivilegeManager::with_detectors(detector, env);

    // In headless SSH sessions, sudo must be recommended even if UDisks2 is present
    assert_eq!(manager.recommended_strategy(), ElevationStrategy::Sudo);

    let cmd = manager
        .generate_elevation_command("inscribe", &["flash", "arch.iso", "/dev/sdb"])
        .expect("Should generate elevation command for unelevated user");

    assert_eq!(cmd.program, "sudo");
    assert_eq!(cmd.args, vec!["inscribe", "flash", "arch.iso", "/dev/sdb"]);
    assert_eq!(
        cmd.to_shell_string(),
        "sudo inscribe flash arch.iso /dev/sdb"
    );
    assert_eq!(
        cmd,
        ElevationCommand::sudo("inscribe", &["flash", "arch.iso", "/dev/sdb"])
    );
}

#[test]
fn test_elevation_strategy_linux_desktop_with_udisks2() {
    let detector = Arc::new(MockPrivilegeDetector::new(false));
    let env = Arc::new(MockEnvironmentDetector {
        is_ssh: false,
        has_display: true,
        udisks2_available: true,
        os_type: "linux".to_string(),
    });
    let manager = PrivilegeManager::with_detectors(detector, env);

    // On Linux desktop with UDisks2, Polkit D-Bus handles unprivileged raw device access
    assert_eq!(manager.recommended_strategy(), ElevationStrategy::UDisks2);
    assert!(
        manager
            .recommended_strategy()
            .supports_unprivileged_udisks()
    );
}

#[test]
fn test_elevation_strategy_linux_desktop_without_udisks2() {
    let detector = Arc::new(MockPrivilegeDetector::new(false));
    let env = Arc::new(MockEnvironmentDetector {
        is_ssh: false,
        has_display: true,
        udisks2_available: false,
        os_type: "linux".to_string(),
    });
    let manager = PrivilegeManager::with_detectors(detector, env);

    // On Linux desktop without UDisks2, pkexec is used for graphical elevation prompt
    assert_eq!(
        manager.recommended_strategy(),
        ElevationStrategy::PolkitPkexec
    );

    let cmd = manager
        .generate_elevation_command("inscribe", &["flash", "ubuntu.iso", "/dev/sdc"])
        .expect("Should generate pkexec elevation command");

    assert_eq!(cmd.program, "pkexec");
    assert_eq!(
        cmd.args,
        vec!["inscribe", "flash", "ubuntu.iso", "/dev/sdc"]
    );
    assert_eq!(
        cmd.to_shell_string(),
        "pkexec inscribe flash ubuntu.iso /dev/sdc"
    );
}

#[test]
fn test_elevation_strategy_windows_uac() {
    let detector = Arc::new(MockPrivilegeDetector::new(false));
    let env = Arc::new(MockEnvironmentDetector {
        is_ssh: false,
        has_display: true,
        udisks2_available: false,
        os_type: "windows".to_string(),
    });
    let manager = PrivilegeManager::with_detectors(detector, env);

    // On Windows, UAC prompt via PowerShell Start-Process -Verb RunAs is used
    assert_eq!(
        manager.recommended_strategy(),
        ElevationStrategy::WindowsUac
    );

    let cmd = manager
        .generate_elevation_command(
            "inscribe.exe",
            &["flash", "win.iso", "\\\\.\\PhysicalDrive2"],
        )
        .expect("Should generate Windows UAC command");

    assert_eq!(cmd.program, "powershell.exe");
    let shell_str = cmd.to_shell_string();
    assert!(shell_str.contains("Start-Process"));
    assert!(shell_str.contains("-Verb RunAs"));
    assert!(shell_str.contains("inscribe.exe"));
}

#[test]
fn test_elevation_strategy_macos_terminal_and_gui() {
    // 1. macOS in SSH session -> Sudo
    let detector = Arc::new(MockPrivilegeDetector::new(false));
    let env_ssh = Arc::new(MockEnvironmentDetector {
        is_ssh: true,
        has_display: false,
        udisks2_available: false,
        os_type: "macos".to_string(),
    });
    let manager_ssh = PrivilegeManager::with_detectors(detector.clone(), env_ssh);
    assert_eq!(manager_ssh.recommended_strategy(), ElevationStrategy::Sudo);

    // 2. macOS in Desktop GUI -> AppleScript osascript
    let env_gui = Arc::new(MockEnvironmentDetector {
        is_ssh: false,
        has_display: true,
        udisks2_available: false,
        os_type: "macos".to_string(),
    });
    let manager_gui = PrivilegeManager::with_detectors(detector, env_gui);
    assert_eq!(
        manager_gui.recommended_strategy(),
        ElevationStrategy::MacOsOsascript
    );

    let cmd = manager_gui
        .generate_elevation_command("inscribe", &["flash", "macos.iso", "/dev/rdisk3"])
        .expect("Should generate osascript command");

    assert_eq!(cmd.program, "osascript");
    let shell_str = cmd.to_shell_string();
    assert!(shell_str.contains("do shell script"));
    assert!(shell_str.contains("with administrator privileges"));
}

#[test]
fn test_require_elevation_error_reporting() {
    let detector = Arc::new(MockPrivilegeDetector::new(false));
    let env = Arc::new(MockEnvironmentDetector {
        is_ssh: true,
        has_display: false,
        udisks2_available: false,
        os_type: "linux".to_string(),
    });
    let manager = PrivilegeManager::with_detectors(detector, env);

    let err = manager
        .require_elevation("flash target block device")
        .expect_err("Should error when elevation required");

    match err {
        InscribeError::PrivilegeRequired(msg) => {
            assert!(msg.contains("flash target block device"));
            assert!(msg.contains("sudo"));
        }
        other => panic!("Expected PrivilegeRequired error, got: {other:?}"),
    }
}
