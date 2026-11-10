use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // manifest_dir is crates/inscribe-core, parent is crates, parent.parent is workspace root
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to locate workspace root")
        .to_path_buf()
}

#[test]
fn test_root_workspace_manifest_contains_all_members_and_dependencies() {
    let root = workspace_root();
    let root_cargo = root.join("Cargo.toml");
    let content = fs::read_to_string(&root_cargo).expect("Root Cargo.toml must exist");

    // Acceptance criterion 1: Root Cargo.toml configured with workspace members
    assert!(
        content.contains("\"crates/inscribe-core\""),
        "Root Cargo.toml must include crates/inscribe-core"
    );
    assert!(
        content.contains("\"crates/inscribe-cli\""),
        "Root Cargo.toml must include crates/inscribe-cli"
    );
    assert!(
        content.contains("\"crates/inscribe-gui\""),
        "Root Cargo.toml must include crates/inscribe-gui"
    );

    // Acceptance criterion 1: dependency specifications
    let required_deps = [
        "tokio",
        "crossbeam-channel",
        "blake3",
        "sha2",
        "thiserror",
        "lzma-rust2",
        "flate2",
        "zstd",
        "bzip2",
        "zip",
        "zbus",
        "slint",
    ];

    for dep in required_deps {
        assert!(
            content.contains(dep),
            "Root Cargo.toml workspace.dependencies must specify {dep}"
        );
    }
}

#[test]
fn test_deny_toml_configured_to_mitigate_vulnerabilities() {
    let root = workspace_root();
    let deny_path = root.join("deny.toml");
    let content = fs::read_to_string(&deny_path)
        .expect("deny.toml must exist at root to prevent supply chain CVEs");

    // Acceptance criterion 2: deny.toml configured to disallow unmaintained crates and advisories
    assert!(
        content.contains("[advisories]"),
        "deny.toml must have [advisories] section"
    );
    assert!(
        content.contains("vulnerability = \"deny\""),
        "deny.toml must deny known vulnerabilities"
    );
    assert!(
        content.contains("unmaintained = \"deny\""),
        "deny.toml must disallow unmaintained crates to mitigate CVE-2026-77649"
    );
    assert!(
        content.contains("[sources]"),
        "deny.toml must restrict dependency sources"
    );
    assert!(
        content.contains("unknown-registry = \"deny\""),
        "deny.toml must deny unknown registries"
    );
    assert!(
        content.contains("unknown-git = \"deny\""),
        "deny.toml must deny unknown git sources"
    );
}

#[test]
fn test_slint_configured_in_gui_without_skia() {
    let root = workspace_root();
    let gui_cargo = root.join("crates/inscribe-gui/Cargo.toml");
    let content =
        fs::read_to_string(&gui_cargo).expect("crates/inscribe-gui/Cargo.toml must exist");

    // Acceptance criterion 3: Slint 1.18 configured in inscribe-gui without Skia to maintain sub-10MB binary footprint
    assert!(
        content.contains("slint"),
        "inscribe-gui must depend on Slint"
    );

    let root_cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let combined = format!("{content}\n{root_cargo}");

    assert!(
        combined.contains("renderer-femtovg"),
        "Slint configuration must include renderer-femtovg"
    );
    assert!(
        combined.contains("renderer-software"),
        "Slint configuration must include renderer-software"
    );
    assert!(
        !combined.contains("renderer-skia"),
        "Slint configuration must strictly avoid Skia renderer to stay under 10MB"
    );
}

#[test]
fn test_core_is_headless_without_gui_dependencies() {
    let root = workspace_root();
    let core_cargo = root.join("crates/inscribe-core/Cargo.toml");
    let content =
        fs::read_to_string(&core_cargo).expect("crates/inscribe-core/Cargo.toml must exist");

    assert!(
        !content.contains("slint"),
        "inscribe-core must remain strictly headless (no Slint dependency)"
    );
    assert!(
        !content.contains("winit"),
        "inscribe-core must remain strictly headless (no winit dependency)"
    );
}

#[test]
fn test_all_crates_target_rust_2024_edition() {
    let root = workspace_root();
    let root_cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        root_cargo.contains("edition = \"2024\""),
        "Root workspace must target Rust 2024 edition"
    );

    let crates = [
        root.join("crates/inscribe-core/Cargo.toml"),
        root.join("crates/inscribe-cli/Cargo.toml"),
        root.join("crates/inscribe-gui/Cargo.toml"),
    ];

    for crate_cargo in crates {
        let content = fs::read_to_string(&crate_cargo)
            .unwrap_or_else(|_| panic!("Manifest {} must exist", crate_cargo.display()));
        let has_edition =
            content.contains("edition.workspace = true") || content.contains("edition = \"2024\"");
        assert!(
            has_edition,
            "Crate manifest {} must inherit or set edition 2024",
            crate_cargo.display()
        );
    }
}
