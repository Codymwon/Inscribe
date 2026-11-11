//! 4096-byte aligned buffer and direct block device storage writer.

pub mod aligned;

pub use aligned::{AlignedBuffer, DEFAULT_ALIGNMENT, DEFAULT_BUFFER_SIZE};
