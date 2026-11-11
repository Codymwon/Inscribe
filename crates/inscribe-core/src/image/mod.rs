//! Disk image metadata, compression format definitions, magic-byte detection, and streaming decompression.

pub mod decompress;
pub mod detector;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use decompress::{
    ChannelReader, CountingReader, open_decompressor, open_seekable_decompressor,
};
pub use detector::{detect_format, detect_from_path, inspect_image};

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
        Some(self.uncompressed_size.unwrap_or(self.file_size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_properties() {
        assert!(!ImageFormat::Raw.is_compressed());
        assert!(!ImageFormat::Unknown.is_compressed());
        assert!(ImageFormat::Gz.is_compressed());
        assert!(ImageFormat::Xz.is_compressed());
        assert!(ImageFormat::Zstd.is_compressed());
        assert!(ImageFormat::Bzip2.is_compressed());
        assert!(ImageFormat::Zip.is_compressed());

        assert_eq!(ImageFormat::Raw.extension_str(), "iso");
        assert_eq!(ImageFormat::Gz.extension_str(), "gz");
        assert_eq!(ImageFormat::Xz.extension_str(), "xz");
        assert_eq!(ImageFormat::Zstd.extension_str(), "zst");
        assert_eq!(ImageFormat::Bzip2.extension_str(), "bz2");
        assert_eq!(ImageFormat::Zip.extension_str(), "zip");
    }

    #[test]
    fn test_metadata_payload_size() {
        let meta = ImageMetadata::new(None, 1000, Some(2500), ImageFormat::Gz);
        assert!(meta.has_deterministic_size());
        assert_eq!(meta.payload_size(), Some(2500));

        let meta_indeterminate = ImageMetadata::new(None, 1000, None, ImageFormat::Bzip2);
        assert!(!meta_indeterminate.has_deterministic_size());
        assert_eq!(meta_indeterminate.payload_size(), Some(1000));
    }
}
