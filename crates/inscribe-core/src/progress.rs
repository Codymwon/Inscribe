//! Telemetry, pipeline lifecycle phases, and live progress reporting.

use serde::{Deserialize, Serialize};

/// High-level lifecycle phase of the two-pass flashing and verification pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlashPhase {
    /// Acquiring exclusive device locks, allocating aligned buffers, preparing streaming readers.
    Initializing,
    /// Pass 1: Streaming decompression, direct aligned block writing, and in-flight BLAKE3 hashing.
    Flashing,
    /// Pass 2: Cache invalidation, raw block read-back, and BLAKE3 bit-for-bit checksum verification.
    Verifying,
    /// Successfully finished dual-pass write and verification with bit-for-bit match.
    Completed,
    /// Pipeline halted due to an unrecoverable I/O or verification failure.
    Failed,
    /// Pipeline was gracefully aborted by user request.
    Cancelled,
}

/// Instantaneous flash telemetry emitted at 60Hz over lock-free channels to GUI / CLI runners.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlashProgress {
    /// Active execution phase.
    pub phase: FlashPhase,
    /// Current pass index (1 for Pass 1 direct write, 2 for Pass 2 read-back verification).
    pub current_pass: u8,
    /// Total passes configured (normally 2 for write + verify).
    pub total_passes: u8,
    /// Total uncompressed/processed bytes completed in the current pass.
    pub bytes_processed: u64,
    /// Total expected bytes for this pass (None in indeterminate streaming mode).
    pub total_bytes: Option<u64>,
    /// Instantaneous processing throughput in bytes per second.
    pub bytes_per_second: f64,
    /// Estimated time to completion in seconds (None if indeterminate).
    pub eta_seconds: Option<u64>,
    /// Progress completion fraction from 0.0 to 1.0 (None if indeterminate).
    pub fraction: Option<f32>,
}

impl FlashProgress {
    /// Initial progress state before pipeline starts streaming.
    #[must_use]
    pub fn initial(total_bytes: Option<u64>) -> Self {
        Self {
            phase: FlashPhase::Initializing,
            current_pass: 1,
            total_passes: 2,
            bytes_processed: 0,
            total_bytes,
            bytes_per_second: 0.0,
            eta_seconds: None,
            fraction: total_bytes.map(|_| 0.0),
        }
    }

    /// Instantaneous transfer speed in Megabytes per second (MB/s).
    #[must_use]
    pub fn speed_mb_per_sec(&self) -> f64 {
        self.bytes_per_second / (1024.0 * 1024.0)
    }

    /// Progress expressed as a percentage value between 0.0 and 100.0.
    #[must_use]
    pub fn percentage(&self) -> Option<f32> {
        self.fraction.map(|f| f * 100.0)
    }

    /// Returns true if flashing successfully completed all passes.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.phase == FlashPhase::Completed
    }

    /// Returns true if an unrecoverable failure occurred.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.phase == FlashPhase::Failed
    }

    /// Returns true if flashing was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.phase == FlashPhase::Cancelled
    }
}
