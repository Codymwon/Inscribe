//! Command execution handlers for Inscribe CLI subcommands.

use crate::args::{FlashArgs, InspectArgs, ListArgs};
use crate::term::{
    create_progress_bar, format_drive_list, format_inspection, prompt_unsafe_override,
};
use inscribe_core::drive::{DriveGuard, DriveScanner};
use inscribe_core::error::InscribeError;
use inscribe_core::image::{detect_format, inspect_image, open_decompressor};
use inscribe_core::pipeline::{FlashPipeline, FlashSummary, PipelineOptions};
use inscribe_core::privilege::PrivilegeManager;
use inscribe_core::progress::FlashPhase;
use inscribe_core::target::BlockTarget;
use inscribe_core::writer::{BlockDeviceWriter, WriterOptions};
use std::fs::File;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};

/// Handles the `inscribe list` subcommand.
///
/// # Errors
///
/// Returns `InscribeError` if drive enumeration fails.
pub fn run_list(args: &ListArgs, scanner: &dyn DriveScanner) -> Result<String, InscribeError> {
    let drives = scanner.scan_drives()?;
    Ok(format_drive_list(&drives, args.all, args.allow_usb_ssd))
}

/// Handles the `inscribe inspect <image>` subcommand.
///
/// # Errors
///
/// Returns `InscribeError::Io` if image file cannot be opened,
/// or `InscribeError` if inspection fails.
pub fn run_inspect(args: &InspectArgs) -> Result<String, InscribeError> {
    let mut file = File::open(&args.image)?;
    let mut header = [0u8; 4096];
    let n = file.read(&mut header)?;
    let format = detect_format(&header[..n], Some(&args.image));
    file.seek(SeekFrom::Start(0))?;

    let metadata = inspect_image(&mut file, format, Some(&args.image))?;
    Ok(format_inspection(&metadata))
}

/// Executes the flash pipeline with an explicit reader and target.
///
/// Connects `FlashPipeline` telemetry to `indicatif::ProgressBar`, displaying
/// write speed (MB/s) and ETA.
///
/// # Errors
///
/// Returns `InscribeError` if flashing or verification fails.
pub fn execute_flash_pipeline<R: Read + Send + 'static, T: BlockTarget + 'static>(
    reader: R,
    target: T,
    options: PipelineOptions,
) -> Result<FlashSummary, InscribeError> {
    let pb = create_progress_bar(options.expected_size);
    let verify_enabled = options.verify;
    let handle = FlashPipeline::spawn(reader, target, options);

    while let Ok(progress) = handle.progress_rx.recv() {
        pb.set_position(progress.bytes_processed);
        let phase_str = match progress.phase {
            FlashPhase::Initializing => "Initializing",
            FlashPhase::Flashing => "Flashing",
            FlashPhase::Verifying => "Verifying",
            FlashPhase::Completed => "Completed",
            FlashPhase::Failed => "Failed",
            FlashPhase::Cancelled => "Cancelled",
        };

        if let Some(eta) = progress.eta_seconds {
            pb.set_message(format!(
                "[{phase_str}] {:.1} MB/s (ETA: {eta}s)",
                progress.speed_mb_per_sec()
            ));
        } else {
            pb.set_message(format!(
                "[{phase_str}] {:.1} MB/s",
                progress.speed_mb_per_sec()
            ));
        }
    }

    match handle.join() {
        Ok(summary) => {
            if verify_enabled {
                pb.finish_with_message("Flashing and verification complete!");
            } else {
                pb.finish_with_message("Flashing complete (verification skipped)!");
            }
            Ok(summary)
        }
        Err(e) => {
            pb.abandon_with_message(format!("Flashing failed: {e}"));
            Err(e)
        }
    }
}

/// Handles the `inscribe flash <image> <device>` subcommand.
///
/// # Errors
///
/// Returns `InscribeError` if unsafe override fails, Drive Guard rejects the target,
/// or flashing fails.
pub fn run_flash<R: BufRead, W: Write>(
    args: &FlashArgs,
    stdin: &mut R,
    stdout: &mut W,
    scanner: &dyn DriveScanner,
) -> Result<FlashSummary, InscribeError> {
    // 1. Inspect image
    let mut file = File::open(&args.image)?;
    let mut header = [0u8; 4096];
    let n = file.read(&mut header)?;
    let format = detect_format(&header[..n], Some(&args.image));
    file.seek(SeekFrom::Start(0))?;

    let metadata = inspect_image(&mut file, format, Some(&args.image))?;
    file.seek(SeekFrom::Start(0))?;

    // 2. Drive Guard and Safety validation
    if args.unsafe_allow_system_drives {
        prompt_unsafe_override(stdin, stdout, &args.device)?;
    } else {
        // Enforce Drive Guard protection against system/internal disks
        let drives = scanner.scan_drives().unwrap_or_default();
        if let Some(target_drive) = drives.iter().find(|d| d.path == args.device) {
            DriveGuard::validate_target_with_options(target_drive, args.allow_usb_ssd)?;
        } else {
            // Target path not found in enumerated devices - fail closed unless unsafe override enabled
            return Err(InscribeError::drive_guard_violation(
                &args.device,
                format!(
                    "Target device '{}' was not found among detected safe removable drives. To flash an unverified target, pass --unsafe-allow-system-drives and confirm.",
                    args.device.display()
                ),
            ));
        }
    }

    // 3. Privilege warning/guidance
    let priv_mgr = PrivilegeManager::new();
    if !priv_mgr.check_elevated() {
        let args_vec: Vec<String> = std::env::args().collect();
        let str_refs: Vec<&str> = args_vec.iter().map(String::as_str).collect();
        let program = args_vec.first().map(String::as_str).unwrap_or("inscribe");
        let rest_args = if args_vec.len() > 1 {
            &str_refs[1..]
        } else {
            &[]
        };

        let cmd_str = priv_mgr
            .generate_elevation_command(program, rest_args)
            .map(|c| c.to_shell_string())
            .unwrap_or_else(|| {
                inscribe_core::privilege::ElevationCommand::sudo(program, rest_args)
                    .to_shell_string()
            });

        writeln!(
            stdout,
            "Note: Elevated permissions may be required to write directly to block devices."
        )?;
        writeln!(stdout, "Recommended command: {cmd_str}")?;
        stdout.flush()?;
    }

    // 4. Open streaming decompressor (zero temporary host storage writes)
    let reader = open_decompressor(file, metadata.format)?;

    // 5. Open block device writer with O_DIRECT / FILE_FLAG_NO_BUFFERING
    let writer_options = WriterOptions::direct();
    let target = BlockDeviceWriter::open_path(&args.device, &writer_options)?;

    // 6. Configure pipeline options
    let options = PipelineOptions {
        expected_size: metadata.uncompressed_size,
        verify: !args.no_verify,
        expected_sha256: args.verify_sha256.clone(),
        chunk_size: 4 * 1024 * 1024,
        telemetry_rate_hz: 60,
    };

    // 7. Execute flash pipeline with live progress
    let summary = execute_flash_pipeline(reader, target, options)?;

    // 8. Output summary metrics
    writeln!(stdout, "\n{}", "-".repeat(50))?;
    writeln!(stdout, "Flashing Summary:")?;
    writeln!(
        stdout,
        "  Bytes Written   : {} bytes",
        summary.bytes_written
    )?;
    writeln!(stdout, "  Duration        : {:.2?}", summary.duration)?;
    writeln!(
        stdout,
        "  Average Speed   : {:.2} MB/s",
        summary.average_mb_per_sec
    )?;
    writeln!(stdout, "  BLAKE3 Checksum : {}", summary.blake3_hash)?;
    if let Some(sha) = &summary.sha256_hash {
        writeln!(stdout, "  SHA-256 Checksum: {sha}")?;
    }
    writeln!(stdout, "{}", "-".repeat(50))?;
    stdout.flush()?;

    Ok(summary)
}
