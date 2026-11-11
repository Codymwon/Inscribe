//! Command-line argument specifications for Inscribe CLI.
//!
//! # Official Documentation References:
//! - Clap 4 Derive Tutorial: <https://docs.rs/clap/latest/clap/_derive/_tutorial/index.html>
//! - Clap Subcommands: <https://docs.rs/clap/latest/clap/enum.Subcommand.html>

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Inscribe: Blazing-fast native USB image flasher in Rust.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    name = "inscribe",
    about = "Blazing-fast native USB image flasher",
    version,
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Available Inscribe subcommands.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Commands {
    /// List detected block drives and their Drive Guard safety status
    List(ListArgs),
    /// Inspect disk image compression format and payload size
    Inspect(InspectArgs),
    /// Flash a disk image directly to a target block device
    Flash(FlashArgs),
}

/// Arguments for `inscribe list`.
#[derive(Args, Debug, Clone, PartialEq, Eq, Default)]
pub struct ListArgs {
    /// Show all drives including internal and system storage
    #[arg(short, long)]
    pub all: bool,

    /// Opt-in to include external USB SSD enclosures as flashable
    #[arg(long)]
    pub allow_usb_ssd: bool,
}

/// Arguments for `inscribe inspect <image>`.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct InspectArgs {
    /// Path to the disk image file (.iso, .img, .xz, .gz, .zst, .bz2, .zip)
    #[arg(value_name = "IMAGE")]
    pub image: PathBuf,
}

/// Arguments for `inscribe flash <image> <device>`.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct FlashArgs {
    /// Source disk image file path (.iso, .img, .xz, .gz, .zst, .bz2, .zip)
    #[arg(value_name = "IMAGE")]
    pub image: PathBuf,

    /// Destination block device path (e.g. /dev/sdb, \\\\.\\PhysicalDrive1, /dev/disk2)
    #[arg(value_name = "DEVICE")]
    pub device: PathBuf,

    /// Verify against an expected SHA-256 hash string in Pass 1
    #[arg(long, value_name = "HASH")]
    pub verify_sha256: Option<String>,

    /// Expert-only override allowing flashing to system or internal drives (guarded by confirmation prompt)
    #[arg(long)]
    pub unsafe_allow_system_drives: bool,

    /// Opt-in to allow flashing external USB SSD enclosures (non-removable USB drives)
    #[arg(long)]
    pub allow_usb_ssd: bool,

    /// Skip Pass 2 read-back verification (default: verify is enabled)
    #[arg(long)]
    pub no_verify: bool,
}
