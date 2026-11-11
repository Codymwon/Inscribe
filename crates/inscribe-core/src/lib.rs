//! Core domain models, streaming decompression, and Drive Guard safety engine for Inscribe.

pub mod drive;
pub mod error;
pub mod image;
pub mod pipeline;
pub mod privilege;
pub mod progress;
pub mod target;
pub mod writer;

// Re-exports for convenient top-level access
pub use drive::{BusType, DriveGuard, DriveInfo, DriveScanner, MockDriveScanner, default_scanner};
pub use error::InscribeError;
pub use image::{
    CountingReader, ImageFormat, ImageMetadata, detect_format, detect_from_path, inspect_image,
    open_decompressor, open_seekable_decompressor,
};
pub use pipeline::{FlashHandle, FlashPipeline, FlashSummary, PipelineOptions};
pub use privilege::{
    ElevationCommand, ElevationStrategy, MockEnvironmentDetector, MockPrivilegeDetector,
    PrivilegeDetector, PrivilegeManager,
};
pub use progress::{FlashPhase, FlashProgress};
pub use target::{BlockTarget, MultiBlockTarget};
pub use writer::{AlignedBuffer, BlockDeviceWriter, WriterOptions};
