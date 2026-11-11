//! 4096-byte aligned buffer and direct block device storage writer.

pub mod aligned;
pub mod direct;

pub use aligned::{AlignedBuffer, DEFAULT_ALIGNMENT, DEFAULT_BUFFER_SIZE};
pub use direct::{BlockDeviceWriter, WriterOptions};
