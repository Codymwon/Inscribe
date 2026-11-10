//! Strongly-typed domain errors for Inscribe using `thiserror`.

use std::path::PathBuf;
use thiserror::Error;

/// Core error enumeration encompassing all operational failures across Inscribe.
#[derive(Error, Debug)]
pub enum InscribeError {
    /// Standard I/O operational failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Drive Guard safety engine blocked an unsafe disk destination.
    #[error("Drive Guard rejected destination {path}: {reason}")]
    DriveGuardViolation {
        /// Target device path or identifier
        path: String,
        /// Reason for rejection (e.g., active root mount, internal NVMe/SATA bus)
        reason: String,
    },

    /// Specified device could not be located in system enumeration.
    #[error("Target block device not found: {0}")]
    DeviceNotFound(String),

    /// Failed to acquire exclusive lock or unmount target volumes.
    #[error("Failed to acquire exclusive lock on device {path}: {reason}")]
    DeviceLockFailed {
        /// Target device path
        path: String,
        /// Operating system error or collision detail
        reason: String,
    },

    /// Decompression stream encountered corrupt headers or payload.
    #[error("Image decompression failed: {0}")]
    DecompressionFailed(String),

    /// Image file has an invalid or unrecognized format.
    #[error("Invalid image format or header: {0}")]
    InvalidImage(String),

    /// Bit-for-bit checksum verification failed between source image and target block device.
    #[error(
        "Verification failed at byte offset {offset}: expected checksum {expected}, got {actual}"
    )]
    VerificationFailed {
        /// Byte offset where discrepancy was detected
        offset: u64,
        /// Expected BLAKE3 / SHA checksum
        expected: String,
        /// Actual calculated checksum read back from disk
        actual: String,
    },

    /// Flash operation was cancelled by the user.
    #[error("Flash operation was cancelled by user")]
    Cancelled,

    /// Elevation is required to perform raw block writes.
    #[error("Elevated administrative/root privileges required: {0}")]
    PrivilegeRequired(String),

    /// Direct block write failure at a specific sector boundary.
    #[error("Block write error at offset {offset}: {source}")]
    BlockWriteError {
        /// Byte offset
        offset: u64,
        /// Underlying I/O error
        #[source]
        source: std::io::Error,
    },

    /// Verification read failure at a specific sector boundary.
    #[error("Block read error at offset {offset}: {source}")]
    BlockReadError {
        /// Byte offset
        offset: u64,
        /// Underlying I/O error
        #[source]
        source: std::io::Error,
    },

    /// Simultaneous multi-target flashing fan-out failure.
    #[error("Multi-target failure: {0}")]
    MultiTargetError(String),

    /// Target device capacity is smaller than image payload.
    #[error(
        "Target capacity ({target_bytes} bytes) is insufficient for image ({image_bytes} bytes)"
    )]
    InsufficientCapacity {
        /// Target device capacity in bytes
        target_bytes: u64,
        /// Required image size in bytes
        image_bytes: u64,
    },

    /// Linux UDisks2 / D-Bus communication error.
    #[error("UDisks2 D-Bus error: {0}")]
    UDisks(String),

    /// General internal failure.
    #[error("Internal error: {0}")]
    Other(String),
}

impl InscribeError {
    /// Helper to construct a DriveGuardViolation from a path reference.
    pub fn drive_guard_violation(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self::DriveGuardViolation {
            path: path.into().to_string_lossy().into_owned(),
            reason: reason.into(),
        }
    }
}
