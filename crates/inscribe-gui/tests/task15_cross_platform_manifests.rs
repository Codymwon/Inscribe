use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR for inscribe-gui is crates/inscribe-gui. Workspace root is 2 levels up.
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

#[test]
fn test_windows_uac_manifest_configured_in_build_script() {
    let root = workspace_root();
    let build_rs_path = root.join("crates/inscribe-gui/build.rs");
    let build_rs_content = fs::read_to_string(&build_rs_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", build_rs_path.display()));

    // Verify Windows target conditional and winresource usage
    assert!(
        build_rs_content.contains("winresource") || build_rs_content.contains("WindowsResource"),
        "crates/inscribe-gui/build.rs must utilize winresource::WindowsResource for Windows targets"
    );

    // Verify UAC requireAdministrator execution level
    assert!(
        build_rs_content.contains("requireAdministrator"),
        "crates/inscribe-gui/build.rs must set requestedExecutionLevel level=\"requireAdministrator\""
    );

    // Verify XML manifest namespaces
    assert!(
        build_rs_content.contains("urn:schemas-microsoft-com:asm.v1")
            && build_rs_content.contains("urn:schemas-microsoft-com:asm.v3"),
        "Manifest must declare official Microsoft XML assembly namespaces (asm.v1 and asm.v3)"
    );

    // Verify inscribe-gui/Cargo.toml configures winresource dependency
    let cargo_toml_path = root.join("crates/inscribe-gui/Cargo.toml");
    let cargo_toml_content = fs::read_to_string(&cargo_toml_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", cargo_toml_path.display()));

    assert!(
        cargo_toml_content.contains("winresource"),
        "crates/inscribe-gui/Cargo.toml must declare winresource under Windows build-dependencies"
    );
}

#[test]
fn test_linux_desktop_entry_specification() {
    let root = workspace_root();
    let desktop_path = root.join("dist/inscribe.desktop");
    assert!(
        desktop_path.exists(),
        "Desktop entry dist/inscribe.desktop must exist"
    );

    let content = fs::read_to_string(&desktop_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", desktop_path.display()));

    // FreeDesktop Desktop Entry Specification validations
    assert!(
        content.contains("[Desktop Entry]"),
        "Desktop file must start with [Desktop Entry] group header"
    );
    assert!(
        content.contains("Type=Application"),
        "Desktop file must declare Type=Application"
    );
    assert!(
        content.contains("Name=Inscribe"),
        "Desktop file must declare Name=Inscribe"
    );
    assert!(
        content.contains("Exec=inscribe-gui"),
        "Desktop file must set Exec=inscribe-gui"
    );
    assert!(
        content.contains("Icon=inscribe"),
        "Desktop file must set Icon=inscribe"
    );
    assert!(
        content.contains("Terminal=false"),
        "Desktop file must set Terminal=false for GUI"
    );
    assert!(
        content.contains("Categories=")
            && (content.contains("Utility") || content.contains("System")),
        "Desktop file must declare appropriate Categories (Utility;System;)"
    );
}

#[test]
fn test_linux_policykit_action_specification() {
    let root = workspace_root();
    let policy_path = root.join("dist/org.inscribe.policy");
    assert!(
        policy_path.exists(),
        "Polkit policy file dist/org.inscribe.policy must exist"
    );

    let content = fs::read_to_string(&policy_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", policy_path.display()));

    // Verify XML declaration and Polkit DTD
    assert!(
        content.contains("<?xml version=\"1.0\"")
            && content.contains("<!DOCTYPE policyconfig PUBLIC"),
        "Policy file must include standard XML declaration and Polkit DTD"
    );
    assert!(
        content.contains("http://www.freedesktop.org/software/polkit/policyconfig-1.dtd"),
        "Policy file must link official freedesktop policyconfig-1.dtd"
    );

    // Verify Action ID
    assert!(
        content.contains("<action id=\"org.inscribe.pkexec.run\">")
            || content.contains("<action id=\"org.inscribe.flash\">"),
        "Policy file must define action id (e.g. org.inscribe.pkexec.run)"
    );

    // Verify Active User Authentication requirement (auth_admin or auth_admin_keep)
    assert!(
        content.contains("<allow_active>auth_admin_keep</allow_active>")
            || content.contains("<allow_active>auth_admin</allow_active>"),
        "allow_active must require administrator authentication (auth_admin or auth_admin_keep)"
    );

    // Verify Freedesktop Polkit Exec path and GUI annotations
    assert!(
        content.contains("org.freedesktop.policykit.exec.path"),
        "Policy file must specify org.freedesktop.policykit.exec.path"
    );
    assert!(
        content.contains("org.freedesktop.policykit.exec.allow_gui"),
        "Policy file must specify org.freedesktop.policykit.exec.allow_gui"
    );
}

#[test]
fn test_linux_flatpak_manifest_specification() {
    let root = workspace_root();
    let flatpak_path = root.join("dist/flatpak/org.inscribe.Inscribe.yaml");
    assert!(
        flatpak_path.exists(),
        "Flatpak manifest dist/flatpak/org.inscribe.Inscribe.yaml must exist"
    );

    let content = fs::read_to_string(&flatpak_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", flatpak_path.display()));

    // Verify Flatpak app-id, runtime, and command
    assert!(
        content.contains("app-id: org.inscribe.Inscribe"),
        "Flatpak manifest must specify app-id: org.inscribe.Inscribe"
    );
    assert!(
        content.contains("command: inscribe-gui"),
        "Flatpak manifest must specify command: inscribe-gui"
    );

    // Verify UDisks2 System D-Bus access permission (crucial for unprivileged drive flashing)
    assert!(
        content.contains("--system-talk-name=org.freedesktop.UDisks2"),
        "Flatpak finish-args must grant system D-Bus access to org.freedesktop.UDisks2 via --system-talk-name"
    );

    // Verify Display, IPC, and Device permissions
    assert!(
        content.contains("--socket=wayland"),
        "Flatpak finish-args must include --socket=wayland"
    );
    assert!(
        content.contains("--socket=fallback-x11"),
        "Flatpak finish-args must include --socket=fallback-x11"
    );
    assert!(
        content.contains("--device=all"),
        "Flatpak finish-args must include --device=all for direct block access"
    );
}

#[test]
fn test_linux_appstream_metainfo_specification() {
    let root = workspace_root();
    let metainfo_path = root.join("dist/org.inscribe.Inscribe.metainfo.xml");
    assert!(
        metainfo_path.exists(),
        "AppStream metainfo dist/org.inscribe.Inscribe.metainfo.xml must exist for AppImage & Flatpak"
    );

    let content = fs::read_to_string(&metainfo_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", metainfo_path.display()));

    assert!(
        content.contains("<component type=\"desktop-application\">"),
        "Metainfo must declare component type desktop-application"
    );
    assert!(
        content.contains("<id>org.inscribe.Inscribe</id>"),
        "Metainfo must declare ID org.inscribe.Inscribe"
    );
    assert!(
        content.contains("<name>Inscribe</name>"),
        "Metainfo must declare name Inscribe"
    );
    assert!(
        content.contains("<metadata_license>FSFAP</metadata_license>")
            || content.contains("<metadata_license>CC0-1.0</metadata_license>")
            || content.contains("<metadata_license>MIT</metadata_license>"),
        "Metainfo must specify valid metadata license"
    );
}

#[test]
fn test_macos_bundle_plist_and_dmg_configuration() {
    let root = workspace_root();
    let plist_path = root.join("dist/macos/Info.plist");
    assert!(
        plist_path.exists(),
        "macOS Info.plist dist/macos/Info.plist must exist"
    );

    let content = fs::read_to_string(&plist_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", plist_path.display()));

    // Verify Apple XML Plist doctype and structure
    assert!(
        content.contains("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\""),
        "Info.plist must declare Apple PLIST 1.0 DTD"
    );
    assert!(
        content.contains("<key>CFBundleIdentifier</key>")
            && content.contains("<string>org.inscribe.Inscribe</string>"),
        "Info.plist must specify CFBundleIdentifier as org.inscribe.Inscribe"
    );
    assert!(
        content.contains("<key>CFBundleExecutable</key>")
            && content.contains("<string>inscribe-gui</string>"),
        "Info.plist must specify CFBundleExecutable as inscribe-gui"
    );
    assert!(
        content.contains("<key>CFBundlePackageType</key>")
            && content.contains("<string>APPL</string>"),
        "Info.plist must specify CFBundlePackageType as APPL"
    );
    assert!(
        content.contains("<key>NSHighResolutionCapable</key>") && content.contains("<true/>"),
        "Info.plist must set NSHighResolutionCapable to true"
    );
    assert!(
        content.contains("<key>LSMinimumSystemVersion</key>"),
        "Info.plist must specify LSMinimumSystemVersion"
    );

    // Verify macOS DMG packaging script/configuration
    let dmg_script_path = root.join("dist/macos/create-dmg.sh");
    assert!(
        dmg_script_path.exists(),
        "macOS DMG packaging configuration dist/macos/create-dmg.sh must exist"
    );

    let script_content = fs::read_to_string(&dmg_script_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", dmg_script_path.display()));

    assert!(
        script_content.contains("hdiutil create") || script_content.contains("create-dmg"),
        "create-dmg.sh must use hdiutil create or create-dmg to generate Inscribe.dmg"
    );
}

#[test]
fn test_cross_platform_toolchain_and_workspace_dependency_hygiene() {
    let root = workspace_root();

    // 1. Verify .cargo/config.toml scopes target settings to Linux x86_64, not cfg(all())
    let cargo_config_path = root.join(".cargo/config.toml");
    assert!(cargo_config_path.exists(), ".cargo/config.toml must exist");
    let config_content =
        fs::read_to_string(&cargo_config_path).expect("Failed to read .cargo/config.toml");

    assert!(
        !config_content.contains("cfg(all())"),
        ".cargo/config.toml must never use cfg(all()) for platform-specific linker settings"
    );
    assert!(
        config_content.contains("[target.x86_64-unknown-linux-gnu]"),
        ".cargo/config.toml must scope target configurations strictly to [target.x86_64-unknown-linux-gnu]"
    );

    // 2. Verify root Cargo.toml does not contain unused slint-build dependency
    let root_cargo_path = root.join("Cargo.toml");
    let root_cargo_content =
        fs::read_to_string(&root_cargo_path).expect("Failed to read Cargo.toml");

    assert!(
        !root_cargo_content.contains("slint-build"),
        "Root Cargo.toml workspace.dependencies must not contain unused slint-build"
    );

    // 3. Verify .cargo/ld-wrapper.sh handles missing objcopy and supports $CC / gcc
    let ld_wrapper_path = root.join(".cargo/ld-wrapper.sh");
    assert!(ld_wrapper_path.exists(), ".cargo/ld-wrapper.sh must exist");
    let wrapper_content =
        fs::read_to_string(&ld_wrapper_path).expect("Failed to read .cargo/ld-wrapper.sh");
    assert!(
        wrapper_content.contains("${CC:-")
            || wrapper_content.contains("command -v objcopy")
            || wrapper_content.contains("which objcopy")
            || wrapper_content.contains("type objcopy"),
        ".cargo/ld-wrapper.sh must safely check objcopy availability and fallback to CC/gcc"
    );
}
