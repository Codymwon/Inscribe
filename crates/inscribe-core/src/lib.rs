//! Core domain models, streaming decompression, and Drive Guard safety engine for Inscribe.

pub mod drive;
pub mod error;
pub mod image;
pub mod progress;
pub mod target;

// Re-exports for convenient top-level access
pub use drive::{BusType, DriveGuard, DriveInfo, DriveScanner, MockDriveScanner};
pub use error::InscribeError;
pub use image::{
    CountingReader, ImageFormat, ImageMetadata, detect_format, detect_from_path, inspect_image,
    open_decompressor, open_seekable_decompressor,
};
pub use progress::{FlashPhase, FlashProgress};
pub use target::{BlockTarget, MultiBlockTarget};
