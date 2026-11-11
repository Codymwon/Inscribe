//! Terminal UI formatting, indicatif progress bars, and interactive confirmation prompts.
//!
//! # Official Documentation References:
//! - Indicatif ProgressStyle: <https://docs.rs/indicatif/latest/indicatif/struct.ProgressStyle.html>
//! - Indicatif ProgressBar: <https://docs.rs/indicatif/latest/indicatif/struct.ProgressBar.html>

use indicatif::{ProgressBar, ProgressStyle};
use inscribe_core::drive::{DriveGuard, DriveInfo};
use inscribe_core::error::InscribeError;
use inscribe_core::image::ImageMetadata;
use std::io::{BufRead, Write};
use std::path::Path;

/// Formats a list of detected storage drives into a readable terminal table.
#[must_use]
pub fn format_drive_list(drives: &[DriveInfo], show_all: bool, allow_usb_ssd: bool) -> String {
    let filtered: Vec<&DriveInfo> = drives
        .iter()
        .filter(|d| show_all || d.is_removable || (allow_usb_ssd && d.is_external_usb_ssd()))
        .collect();

    if filtered.is_empty() {
        if !show_all {
            return "No removable storage drives detected. (Use --all to show internal/system disks)\n"
                .to_string();
        }
        return "No storage drives detected.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{:<16} {:<30} {:<12} {:<8} {:<10} {}\n",
        "Target", "Model / Vendor", "Size", "Bus", "Removable", "Status"
    ));
    out.push_str(&format!("{}\n", "-".repeat(95)));

    for drive in filtered {
        let path = drive.path.to_string_lossy();
        let model_vendor = if drive.vendor.is_empty() {
            drive.model.clone()
        } else if drive.model.is_empty() {
            drive.vendor.clone()
        } else {
            format!("{} {}", drive.vendor, drive.model)
        };

        let size_str = drive.formatted_size();
        let bus_str = format!("{:?}", drive.bus_type);
        let removable_str = if drive.is_removable { "Yes" } else { "No" };

        let status_str = match DriveGuard::validate_target_with_options(drive, allow_usb_ssd) {
            Ok(()) => "Safe".to_string(),
            Err(InscribeError::DriveGuardViolation { reason, .. }) => format!("Blocked: {reason}"),
            Err(e) => format!("Blocked: {e}"),
        };

        out.push_str(&format!(
            "{:<16} {:<30} {:<12} {:<8} {:<10} {}\n",
            path,
            truncate_string(&model_vendor, 28),
            size_str,
            bus_str,
            removable_str,
            status_str
        ));
    }

    out
}

/// Helper to truncate strings with ellipsis if exceeding maximum character width.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s[..max_len].to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Prompts the user interactively to confirm an unsafe system drive override.
///
/// Requires the user to type the exact target device path (e.g. `/dev/sda` or `\\\\.\\PhysicalDrive0`).
///
/// # Errors
///
/// Returns `InscribeError::DriveGuardViolation` if the user input does not match the target device path,
/// or `InscribeError::Io` if reading from stdin fails.
pub fn prompt_unsafe_override<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    device_path: &Path,
) -> Result<(), InscribeError> {
    writeln!(
        writer,
        "\n================================================================================"
    )?;
    writeln!(
        writer,
        "WARNING: YOU ARE ABOUT TO OVERRIDE DRIVE GUARD PROTECTION FOR SYSTEM DRIVES!"
    )?;
    writeln!(writer, "Target device: {}", device_path.display())?;
    writeln!(
        writer,
        "This operation can DESTROY your host operating system and cause data loss."
    )?;
    writeln!(
        writer,
        "================================================================================"
    )?;
    write!(
        writer,
        "To proceed, type the exact device path ({}) to confirm: ",
        device_path.display()
    )?;
    writer.flush()?;

    let mut input = String::new();
    reader.read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed == device_path.to_string_lossy() {
        writeln!(writer, "Override confirmed. Proceeding with caution...")?;
        writer.flush()?;
        Ok(())
    } else {
        Err(InscribeError::drive_guard_violation(
            device_path,
            format!(
                "Unsafe override aborted: confirmation input '{trimmed}' does not match expected '{}'",
                device_path.display()
            ),
        ))
    }
}

/// Creates a styled `indicatif` progress bar for streaming write or verification passes.
///
/// If `total_bytes` is known, creates a bounded progress bar with bytes, percentage, speed, and ETA.
/// If `total_bytes` is `None`, creates an indeterminate spinner with bytes processed and current speed.
#[must_use]
pub fn create_progress_bar(total_bytes: Option<u64>) -> ProgressBar {
    match total_bytes {
        Some(total) => {
            let pb = ProgressBar::new(total);
            let style = ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta}) {msg}",
                )
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("#>-");
            pb.set_style(style);
            pb
        }
        None => {
            let pb = ProgressBar::new_spinner();
            let style = ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {bytes} ({bytes_per_sec}) {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner());
            pb.set_style(style);
            pb
        }
    }
}

/// Formats image inspection details into a human-readable summary.
#[must_use]
pub fn format_inspection(metadata: &ImageMetadata) -> String {
    let mut out = String::new();
    out.push_str("Disk Image Inspection\n");
    out.push_str(&format!("{}\n", "-".repeat(40)));

    if let Some(path) = &metadata.path {
        out.push_str(&format!("{:<20}: {}\n", "Path", path.display()));
    }
    out.push_str(&format!(
        "{:<20}: {:?} (.{}) [Compressed: {}]\n",
        "Format",
        metadata.format,
        metadata.format.extension_str(),
        metadata.format.is_compressed()
    ));

    out.push_str(&format!(
        "{:<20}: {} bytes ({})\n",
        "File Size",
        metadata.file_size,
        format_bytes(metadata.file_size)
    ));

    match metadata.uncompressed_size {
        Some(uncompressed) => {
            out.push_str(&format!(
                "{:<20}: {} bytes ({})\n",
                "Payload Size",
                uncompressed,
                format_bytes(uncompressed)
            ));
            out.push_str(&format!("{:<20}: Yes\n", "Deterministic"));
        }
        None => {
            out.push_str(&format!(
                "{:<20}: Indeterminate (streaming byte-counter mode)\n",
                "Payload Size"
            ));
            out.push_str(&format!("{:<20}: No\n", "Deterministic"));
        }
    }

    if let Some(hash) = &metadata.expected_sha256 {
        out.push_str(&format!("{:<20}: {}\n", "Expected SHA-256", hash));
    }

    out
}

/// Helper function to format byte counts into human-readable strings.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = KB * 1_000.0;
    const GB: f64 = MB * 1_000.0;
    const TB: f64 = GB * 1_000.0;

    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
