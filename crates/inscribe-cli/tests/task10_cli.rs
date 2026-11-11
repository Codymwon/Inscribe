//! Integration tests for Task 10: inscribe-cli subcommands and terminal UI.

use clap::Parser;
use inscribe_cli::args::{Cli, Commands, FlashArgs, InspectArgs, ListArgs};
use inscribe_cli::commands::execute_flash_pipeline;
use inscribe_cli::term::{create_progress_bar, format_drive_list, prompt_unsafe_override};
use inscribe_core::drive::{BusType, DriveInfo};
use inscribe_core::error::InscribeError;
use inscribe_core::target::BlockTarget;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Parameters for constructing a mock `DriveInfo`.
struct MockDriveParams<'a> {
    path: &'a str,
    model: &'a str,
    vendor: &'a str,
    size: u64,
    bus: BusType,
    removable: bool,
    system: bool,
    mounts: Vec<&'a str>,
}

/// Helper to construct a mock `DriveInfo`.
fn make_drive(p: MockDriveParams<'_>) -> DriveInfo {
    DriveInfo {
        path: PathBuf::from(p.path),
        model: p.model.to_string(),
        vendor: p.vendor.to_string(),
        size: p.size,
        sector_size: 512,
        bus_type: p.bus,
        is_removable: p.removable,
        is_read_only: false,
        is_system: p.system,
        mount_points: p.mounts.into_iter().map(PathBuf::from).collect(),
    }
}

/// In-memory mock block target for verifying CLI flashing.
#[derive(Debug, Clone)]
struct VirtualBlockTarget {
    buffer: Arc<Mutex<Vec<u8>>>,
    capacity: u64,
    sync_count: Arc<Mutex<u32>>,
}

impl VirtualBlockTarget {
    fn new(capacity: u64) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(vec![0u8; capacity as usize])),
            capacity,
            sync_count: Arc::new(Mutex::new(0)),
        }
    }

    fn data(&self) -> Vec<u8> {
        self.buffer.lock().unwrap().clone()
    }
}

impl BlockTarget for VirtualBlockTarget {
    fn id(&self) -> &str {
        "virtual-target"
    }

    fn capacity(&self) -> Result<u64, InscribeError> {
        Ok(self.capacity)
    }

    fn block_size(&self) -> u32 {
        512
    }

    fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError> {
        let mut buf = self.buffer.lock().unwrap();
        let start = offset as usize;
        let end = start + data.len();
        if end > buf.len() {
            return Err(InscribeError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Write exceeds virtual block target capacity",
            )));
        }
        buf[start..end].copy_from_slice(data);
        Ok(())
    }

    fn sync(&mut self) -> Result<(), InscribeError> {
        let mut count = self.sync_count.lock().unwrap();
        *count += 1;
        Ok(())
    }

    fn read_block(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), InscribeError> {
        let buf = self.buffer.lock().unwrap();
        let start = offset as usize;
        let end = start + buffer.len();
        if end > buf.len() {
            return Err(InscribeError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Read exceeds virtual block target capacity",
            )));
        }
        buffer.copy_from_slice(&buf[start..end]);
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Test 1: CLI Argument Parsing (list, inspect, flash)
// -----------------------------------------------------------------------------
#[test]
fn test_cli_argument_parsing() {
    // 1. inscribe list
    let cli = Cli::try_parse_from(["inscribe", "list"]).expect("parse list");
    assert_eq!(
        cli.command,
        Commands::List(ListArgs {
            all: false,
            allow_usb_ssd: false
        })
    );

    // 1b. inscribe list --all --allow-usb-ssd
    let cli = Cli::try_parse_from(["inscribe", "list", "--all", "--allow-usb-ssd"])
        .expect("parse list --all");
    assert_eq!(
        cli.command,
        Commands::List(ListArgs {
            all: true,
            allow_usb_ssd: true
        })
    );

    // 2. inscribe inspect <image>
    let cli = Cli::try_parse_from(["inscribe", "inspect", "ubuntu.iso"]).expect("parse inspect");
    assert_eq!(
        cli.command,
        Commands::Inspect(InspectArgs {
            image: PathBuf::from("ubuntu.iso")
        })
    );

    // 3. inscribe flash <image> <device>
    let cli =
        Cli::try_parse_from(["inscribe", "flash", "arch.iso", "/dev/sdb"]).expect("parse flash");
    assert_eq!(
        cli.command,
        Commands::Flash(FlashArgs {
            image: PathBuf::from("arch.iso"),
            device: PathBuf::from("/dev/sdb"),
            verify_sha256: None,
            unsafe_allow_system_drives: false,
            allow_usb_ssd: false,
            no_verify: false,
        })
    );

    // 3b. inscribe flash with options: --verify-sha256, --unsafe-allow-system-drives, --allow-usb-ssd, --no-verify
    let cli = Cli::try_parse_from([
        "inscribe",
        "flash",
        "debian.iso",
        "/dev/sdc",
        "--verify-sha256",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "--unsafe-allow-system-drives",
        "--allow-usb-ssd",
        "--no-verify",
    ])
    .expect("parse flash with all flags");

    assert_eq!(
        cli.command,
        Commands::Flash(FlashArgs {
            image: PathBuf::from("debian.iso"),
            device: PathBuf::from("/dev/sdc"),
            verify_sha256: Some(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()
            ),
            unsafe_allow_system_drives: true,
            allow_usb_ssd: true,
            no_verify: true,
        })
    );
}

// -----------------------------------------------------------------------------
// Test 2: Acceptance Criterion: `inscribe list` outputs detected removable drives,
// buses, and Drive Guard status.
// -----------------------------------------------------------------------------
#[test]
fn test_drive_list_formatting_shows_removable_bus_and_guard_status() {
    let usb_stick = make_drive(MockDriveParams {
        path: "/dev/sdb",
        model: "Ultra Flair",
        vendor: "SanDisk",
        size: 16_000_000_000,
        bus: BusType::Usb,
        removable: true,
        system: false,
        mounts: vec!["/media/usb"],
    });

    let system_nvme = make_drive(MockDriveParams {
        path: "/dev/nvme0n1",
        model: "SSD 970 EVO",
        vendor: "Samsung",
        size: 500_000_000_000,
        bus: BusType::Nvme,
        removable: false,
        system: true,
        mounts: vec!["/"],
    });

    let usb_ssd = make_drive(MockDriveParams {
        path: "/dev/sdc",
        model: "T7 Shield",
        vendor: "Samsung",
        size: 1_000_000_000_000,
        bus: BusType::Usb,
        removable: false, // non-removable bit set
        system: false,
        mounts: vec![],
    });

    let drives = vec![usb_stick.clone(), system_nvme.clone(), usb_ssd.clone()];

    // Format list with default options (removable only, allow_usb_ssd = false)
    let output = format_drive_list(&drives, false, false);
    assert!(
        output.contains("/dev/sdb"),
        "Should contain USB flash drive"
    );
    assert!(output.contains("Ultra Flair"), "Should contain model name");
    assert!(output.contains("Usb"), "Should show USB bus type");
    assert!(
        output.contains("Safe") || output.contains("Flashable"),
        "Should show Drive Guard safe status"
    );
    // System disk should NOT appear when all=false
    assert!(
        !output.contains("/dev/nvme0n1"),
        "System disk should be filtered out by default"
    );

    // Format list with all=true
    let output_all = format_drive_list(&drives, true, false);
    assert!(output_all.contains("/dev/sdb"));
    assert!(
        output_all.contains("/dev/nvme0n1"),
        "All=true must include system disk"
    );
    assert!(output_all.contains("Nvme"), "Must show Nvme bus type");
    assert!(
        output_all.contains("Blocked") || output_all.contains("System"),
        "Must display Drive Guard blocked status for system drive"
    );

    // Format list with allow_usb_ssd = true
    let output_ssd = format_drive_list(&drives, true, true);
    assert!(output_ssd.contains("/dev/sdc"));
    assert!(
        output_ssd.contains("Safe") || output_ssd.contains("Flashable"),
        "USB SSD should be marked safe when allow_usb_ssd is true"
    );
}

// -----------------------------------------------------------------------------
// Test 3: Acceptance Criterion: `--unsafe-allow-system-drives` aborts unless user
// types the exact device path interactively.
// -----------------------------------------------------------------------------
#[test]
fn test_unsafe_allow_system_drives_aborts_on_mismatch_or_empty() {
    let target = Path::new("/dev/nvme0n1");

    // Case 1: User types "yes" instead of the exact device path
    let mut stdin = Cursor::new(b"yes\n");
    let mut stdout = Vec::new();
    let result = prompt_unsafe_override(&mut stdin, &mut stdout, target);
    assert!(
        result.is_err(),
        "Must abort when input does not match exact device path"
    );
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("Unsafe override aborted") || err_str.contains("does not match"),
        "Unexpected error: {err_str}"
    );

    // Case 2: User types a typo "/dev/nvme0"
    let mut stdin = Cursor::new(b"/dev/nvme0\n");
    let mut stdout = Vec::new();
    let result = prompt_unsafe_override(&mut stdin, &mut stdout, target);
    assert!(result.is_err(), "Must abort on path typo");

    // Case 3: Empty input or EOF
    let mut stdin = Cursor::new(b"");
    let mut stdout = Vec::new();
    let result = prompt_unsafe_override(&mut stdin, &mut stdout, target);
    assert!(result.is_err(), "Must abort on EOF");
}

#[test]
fn test_unsafe_allow_system_drives_accepts_exact_match() {
    let target = Path::new("/dev/nvme0n1");

    // User types the exact path
    let mut stdin = Cursor::new(b"/dev/nvme0n1\n");
    let mut stdout = Vec::new();
    let result = prompt_unsafe_override(&mut stdin, &mut stdout, target);
    assert!(result.is_ok(), "Must accept exact matching path");

    let prompt_text = String::from_utf8_lossy(&stdout);
    assert!(prompt_text.contains("WARNING"));
    assert!(prompt_text.contains("/dev/nvme0n1"));
}

// -----------------------------------------------------------------------------
// Test 4: Acceptance Criterion: `inscribe flash` displays smooth `indicatif` progress bar
// with write speed (MB/s) and ETA.
// -----------------------------------------------------------------------------
#[test]
fn test_indicatif_progress_bar_creation_and_style() {
    // Deterministic size progress bar
    let pb = create_progress_bar(Some(100 * 1024 * 1024));
    assert_eq!(pb.length(), Some(100 * 1024 * 1024));
    pb.set_position(50 * 1024 * 1024);
    pb.set_message("52.4 MB/s");
    pb.finish_with_message("Complete");

    // Indeterminate size spinner
    let spinner = create_progress_bar(None);
    spinner.set_position(10 * 1024 * 1024);
    spinner.set_message("30.1 MB/s");
    spinner.finish_with_message("Complete");
}

// -----------------------------------------------------------------------------
// Test 5: End-to-end flash execution to virtual target with progress bar updates
// and BLAKE3 verification.
// -----------------------------------------------------------------------------
#[test]
fn test_flash_execution_to_virtual_target_with_verification() {
    // 8MB test payload
    let test_data = vec![0x42u8; 8 * 1024 * 1024];
    let reader = Cursor::new(test_data.clone());
    let target = VirtualBlockTarget::new(8 * 1024 * 1024);

    let options = inscribe_core::pipeline::PipelineOptions {
        expected_size: Some(8 * 1024 * 1024),
        verify: true,
        expected_sha256: None,
        chunk_size: 4 * 1024 * 1024,
        telemetry_rate_hz: 60,
    };

    let summary = execute_flash_pipeline(reader, target.clone(), options)
        .expect("Flash pipeline must succeed");

    assert_eq!(summary.bytes_written, 8 * 1024 * 1024);
    assert!(!summary.blake3_hash.is_empty());
    assert_eq!(
        target.data(),
        test_data,
        "Target data must match source byte-for-byte"
    );
}

// -----------------------------------------------------------------------------
// Test 6: `run_list` handler using MockDriveScanner
// -----------------------------------------------------------------------------
#[test]
fn test_run_list_with_mock_scanner() {
    let usb_stick = make_drive(MockDriveParams {
        path: "/dev/sdb",
        model: "DataTraveler",
        vendor: "Kingston",
        size: 32_000_000_000,
        bus: BusType::Usb,
        removable: true,
        system: false,
        mounts: vec!["/media/usb"],
    });
    let system_nvme = make_drive(MockDriveParams {
        path: "/dev/nvme0n1",
        model: "980 PRO",
        vendor: "Samsung",
        size: 1_000_000_000_000,
        bus: BusType::Nvme,
        removable: false,
        system: true,
        mounts: vec!["/"],
    });

    let scanner = inscribe_core::drive::MockDriveScanner::new(vec![usb_stick, system_nvme]);

    // Removable only
    let out = inscribe_cli::commands::run_list(
        &ListArgs {
            all: false,
            allow_usb_ssd: false,
        },
        &scanner,
    )
    .expect("run_list must succeed");

    assert!(out.contains("/dev/sdb"));
    assert!(out.contains("Kingston"));
    assert!(!out.contains("/dev/nvme0n1"));

    // All drives
    let out_all = inscribe_cli::commands::run_list(
        &ListArgs {
            all: true,
            allow_usb_ssd: false,
        },
        &scanner,
    )
    .expect("run_list --all must succeed");

    assert!(out_all.contains("/dev/sdb"));
    assert!(out_all.contains("/dev/nvme0n1"));
    assert!(out_all.contains("Blocked"));
}

fn scratch_dir(name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/test-scratch")
        .join(format!("{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// -----------------------------------------------------------------------------
// Test 7: `run_inspect` handler on raw image
// -----------------------------------------------------------------------------
#[test]
fn test_run_inspect_on_valid_image() {
    let temp_dir = scratch_dir("inscribe_test_inspect");
    let image_path = temp_dir.join("test_os.iso");
    // Write 1MB dummy ISO
    std::fs::write(&image_path, vec![0xABu8; 1024 * 1024]).unwrap();

    let out = inscribe_cli::commands::run_inspect(&InspectArgs {
        image: image_path.clone(),
    })
    .expect("run_inspect must succeed");

    assert!(out.contains("Disk Image Inspection"));
    assert!(out.contains("Raw"));
    assert!(out.contains("1.05 MB") || out.contains("1.00 MB") || out.contains("1048576 bytes"));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// -----------------------------------------------------------------------------
// Test 8: `run_flash` blocks system drive when unsafe flag is false
// -----------------------------------------------------------------------------
#[test]
fn test_run_flash_drive_guard_blocks_system_drive() {
    let temp_dir = scratch_dir("inscribe_test_guard");
    let image_path = temp_dir.join("image.iso");
    std::fs::write(&image_path, vec![0u8; 512 * 1024]).unwrap();

    let system_disk = make_drive(MockDriveParams {
        path: "/dev/nvme0n1",
        model: "Internal SSD",
        vendor: "Crucial",
        size: 500_000_000_000,
        bus: BusType::Nvme,
        removable: false,
        system: true,
        mounts: vec!["/"],
    });
    let scanner = inscribe_core::drive::MockDriveScanner::new(vec![system_disk]);

    let flash_args = FlashArgs {
        image: image_path,
        device: PathBuf::from("/dev/nvme0n1"),
        verify_sha256: None,
        unsafe_allow_system_drives: false,
        allow_usb_ssd: false,
        no_verify: false,
    };

    let mut stdin = Cursor::new(b"");
    let mut stdout = Vec::new();

    let result = inscribe_cli::commands::run_flash(&flash_args, &mut stdin, &mut stdout, &scanner);
    assert!(result.is_err(), "Must reject writing to system drive");

    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("Drive Guard rejected destination") || err_str.contains("system"),
        "Error must describe Drive Guard rejection: {err_str}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// -----------------------------------------------------------------------------
// Test 9: `run_flash` unsafe override aborts on prompt failure
// -----------------------------------------------------------------------------
#[test]
fn test_run_flash_unsafe_override_aborts_on_prompt_failure() {
    let temp_dir = scratch_dir("inscribe_test_unsafe");
    let image_path = temp_dir.join("image.iso");
    std::fs::write(&image_path, vec![0u8; 512 * 1024]).unwrap();

    let system_disk = make_drive(MockDriveParams {
        path: "/dev/sda",
        model: "System Drive",
        vendor: "WD",
        size: 500_000_000_000,
        bus: BusType::Sata,
        removable: false,
        system: true,
        mounts: vec!["/"],
    });
    let scanner = inscribe_core::drive::MockDriveScanner::new(vec![system_disk]);

    let flash_args = FlashArgs {
        image: image_path,
        device: PathBuf::from("/dev/sda"),
        verify_sha256: None,
        unsafe_allow_system_drives: true,
        allow_usb_ssd: false,
        no_verify: false,
    };

    // User types "no" or wrong path
    let mut stdin = Cursor::new(b"wrong_device\n");
    let mut stdout = Vec::new();

    let result = inscribe_cli::commands::run_flash(&flash_args, &mut stdin, &mut stdout, &scanner);
    assert!(result.is_err(), "Must abort when prompt confirmation fails");

    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("Unsafe override aborted"),
        "Unexpected error: {err_str}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// -----------------------------------------------------------------------------
// Test 10: Task 10.5: `run_flash` fails closed on unenumerated target paths
// -----------------------------------------------------------------------------
#[test]
fn test_run_flash_blocks_unenumerated_target_path_fail_closed() {
    let temp_dir = scratch_dir("inscribe_test_fail_closed");
    let image_path = temp_dir.join("image.iso");
    std::fs::write(&image_path, vec![0u8; 512 * 1024]).unwrap();

    // Scanner enumerates only /dev/sdb
    let safe_usb = make_drive(MockDriveParams {
        path: "/dev/sdb",
        model: "USB Flash",
        vendor: "SanDisk",
        size: 32_000_000_000,
        bus: BusType::Usb,
        removable: true,
        system: false,
        mounts: vec![],
    });
    let scanner = inscribe_core::drive::MockDriveScanner::new(vec![safe_usb]);

    // User specifies an unenumerated device /dev/sdz without --unsafe-allow-system-drives
    let flash_args = FlashArgs {
        image: image_path,
        device: PathBuf::from("/dev/sdz"),
        verify_sha256: None,
        unsafe_allow_system_drives: false,
        allow_usb_ssd: false,
        no_verify: false,
    };

    let mut stdin = Cursor::new(b"");
    let mut stdout = Vec::new();

    let result = inscribe_cli::commands::run_flash(&flash_args, &mut stdin, &mut stdout, &scanner);
    assert!(
        result.is_err(),
        "Must fail closed for unenumerated target device path"
    );

    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("Drive Guard rejected destination")
            || err_str.contains("not found among detected safe removable drives"),
        "Error must describe fail-closed Drive Guard rejection: {err_str}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// -----------------------------------------------------------------------------
// Test 11: Task 10.5: `run_flash` displays elevation guidance when run unelevated
// -----------------------------------------------------------------------------
#[test]
fn test_run_flash_displays_elevation_guidance_when_unelevated() {
    let temp_dir = scratch_dir("inscribe_test_elevation_guidance");
    let image_path = temp_dir.join("image.iso");
    std::fs::write(&image_path, vec![0u8; 512 * 1024]).unwrap();

    // Force unelevated state in PrivilegeManager test override
    inscribe_core::privilege::PrivilegeManager::set_test_override(Some(false));

    let safe_usb = make_drive(MockDriveParams {
        path: "/dev/sdb",
        model: "USB Flash",
        vendor: "SanDisk",
        size: 32_000_000_000,
        bus: BusType::Usb,
        removable: true,
        system: false,
        mounts: vec![],
    });
    let scanner = inscribe_core::drive::MockDriveScanner::new(vec![safe_usb]);

    let flash_args = FlashArgs {
        image: image_path,
        device: PathBuf::from("/dev/sdb"),
        verify_sha256: None,
        unsafe_allow_system_drives: false,
        allow_usb_ssd: false,
        no_verify: false,
    };

    let mut stdin = Cursor::new(b"");
    let mut stdout = Vec::new();

    // Note: run_flash will attempt to open /dev/sdb which does not exist as block device in unit test,
    // but before opening the target it must output elevation guidance if unelevated!
    let _ = inscribe_cli::commands::run_flash(&flash_args, &mut stdin, &mut stdout, &scanner);

    let out_str = String::from_utf8_lossy(&stdout);
    assert!(
        out_str.contains("Elevated permissions may be required")
            || out_str.contains("Recommended command"),
        "Stdout must display elevation guidance when running unelevated. Got: '{out_str}'"
    );

    // Reset test override
    inscribe_core::privilege::PrivilegeManager::set_test_override(None);
    let _ = std::fs::remove_dir_all(&temp_dir);
}
