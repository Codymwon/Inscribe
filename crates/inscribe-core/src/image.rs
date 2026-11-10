//! Disk image metadata and compression format definitions.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Supported disk image archive and container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    /// Uncompressed raw block disk image or optical ISO (.iso, .img, .raw, .bin).
    Raw,
    /// Gzip-compressed disk image (.gz).
    Gz,
    /// XZ-compressed disk image (.xz).
    Xz,
    /// Zstandard-compressed disk image (.zst).
    Zstd,
    /// Bzip2-compressed disk image (.bz2).
    Bzip2,
    /// ZIP-compressed archive (.zip).
    Zip,
    /// Unknown or unrecognized container format.
    Unknown,
}

impl ImageFormat {
    /// Returns true if this format requires streaming decompression.
    #[must_use]
    pub fn is_compressed(&self) -> bool {
        !matches!(self, Self::Raw | Self::Unknown)
    }

    /// Primary standard file extension associated with this format.
    #[must_use]
    pub fn extension_str(&self) -> &'static str {
        match self {
            Self::Raw => "iso",
            Self::Gz => "gz",
            Self::Xz => "xz",
            Self::Zstd => "zst",
            Self::Bzip2 => "bz2",
            Self::Zip => "zip",
            Self::Unknown => "bin",
        }
    }
}

/// Metadata describing an image selected for flashing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMetadata {
    /// Path to the source disk image file on host storage, if loaded from a file.
    pub path: Option<PathBuf>,
    /// Exact file size on host storage in bytes.
    pub file_size: u64,
    /// Extracted uncompressed payload size in bytes, or None if indeterminate.
    pub uncompressed_size: Option<u64>,
    /// Detected container or compression format.
    pub format: ImageFormat,
    /// Optional user-provided or manifest SHA-256 hash for end-to-end verification.
    pub expected_sha256: Option<String>,
}

impl ImageMetadata {
    /// Creates a new `ImageMetadata` instance.
    pub fn new(
        path: Option<PathBuf>,
        file_size: u64,
        uncompressed_size: Option<u64>,
        format: ImageFormat,
    ) -> Self {
        Self {
            path,
            file_size,
            uncompressed_size,
            format,
            expected_sha256: None,
        }
    }

    /// Checks whether the exact uncompressed payload size is known beforehand.
    #[must_use]
    pub fn has_deterministic_size(&self) -> bool {
        self.uncompressed_size.is_some()
    }

    /// Effective write payload size (uncompressed size if known, otherwise compressed size fallback).
    #[must_use]
    pub fn payload_size(&self) -> Option<u64> {
        self.uncompressed_size.or(Some(self.file_size))
    }
}
