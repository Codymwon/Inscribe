//! Two-pass direct flash and cryptographic BLAKE3 verification pipeline.

pub mod progress_tracker;
pub mod verifier;

use crate::error::InscribeError;
use crate::progress::{FlashPhase, FlashProgress};
use crate::target::BlockTarget;
use crate::writer::aligned::{AlignedBuffer, DEFAULT_BUFFER_SIZE};
use progress_tracker::ProgressTracker;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use verifier::{ChecksumVerifier, ReadVerifier};

/// Execution options configuring `FlashPipeline`.
#[derive(Debug, Clone)]
pub struct PipelineOptions {
    /// Expected uncompressed payload size in bytes, if known beforehand.
    pub expected_size: Option<u64>,
    /// Whether to execute Pass 2 read-back verification (default: true).
    pub verify: bool,
    /// Optional SHA-256 checksum string to verify in Pass 1.
    pub expected_sha256: Option<String>,
    /// Buffer chunk size in bytes (default: 4MB).
    pub chunk_size: usize,
    /// Telemetry emission rate in Hertz (default: 60Hz).
    pub telemetry_rate_hz: u32,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            expected_size: None,
            verify: true,
            expected_sha256: None,
            chunk_size: DEFAULT_BUFFER_SIZE,
            telemetry_rate_hz: 60,
        }
    }
}

/// Final summary metrics produced upon successful flash and verification completion.
#[derive(Debug, Clone, PartialEq)]
pub struct FlashSummary {
    /// Total uncompressed bytes streamed and written to storage.
    pub bytes_written: u64,
    /// Final bit-for-bit BLAKE3 checksum of the image payload.
    pub blake3_hash: String,
    /// Optional SHA-256 checksum if requested or verified.
    pub sha256_hash: Option<String>,
    /// Total wall-clock execution duration across all passes.
    pub duration: Duration,
    /// Overall average throughput across both passes in MB/s.
    pub average_mb_per_sec: f64,
}

/// Control handle returned by `FlashPipeline::spawn` for background execution.
pub struct FlashHandle {
    /// Atomic token to request immediate cancellation.
    pub cancel_token: Arc<AtomicBool>,
    /// Telemetry receiver emitting progress snapshots at 60Hz.
    pub progress_rx: crossbeam_channel::Receiver<FlashProgress>,
    /// Worker thread join handle.
    pub thread_handle: std::thread::JoinHandle<Result<FlashSummary, InscribeError>>,
}

impl FlashHandle {
    /// Signal the worker thread to cancel immediately.
    pub fn cancel(&self) {
        self.cancel_token.store(true, Ordering::SeqCst);
    }

    /// Block awaiting completion of the flashing worker thread.
    ///
    /// # Errors
    /// Returns `InscribeError` if flashing failed, was cancelled, or the thread panicked.
    pub fn join(self) -> Result<FlashSummary, InscribeError> {
        self.thread_handle
            .join()
            .map_err(|_| InscribeError::Other("Flashing worker thread panicked".to_string()))?
    }
}

/// Two-pass direct flash and cryptographic BLAKE3 verification engine.
pub struct FlashPipeline {
    options: PipelineOptions,
}

impl FlashPipeline {
    /// Creates a new `FlashPipeline` with the given configuration options.
    #[must_use]
    pub fn new(options: PipelineOptions) -> Self {
        Self { options }
    }

    /// Spawns the pipeline execution on a dedicated background worker thread.
    pub fn spawn<R: Read + Send + 'static, T: BlockTarget + 'static>(
        mut reader: R,
        mut target: T,
        options: PipelineOptions,
    ) -> FlashHandle {
        let (progress_tx, progress_rx) = crossbeam_channel::unbounded();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = Arc::clone(&cancel_token);

        let thread_handle = std::thread::spawn(move || {
            let pipeline = FlashPipeline::new(options);
            pipeline.run(&mut reader, &mut target, &progress_tx, &cancel_for_thread)
        });

        FlashHandle {
            cancel_token,
            progress_rx,
            thread_handle,
        }
    }

    /// Synchronously executes the two-pass flash and verification pipeline.
    ///
    /// # Errors
    /// Returns `InscribeError` if target verification fails, storage capacity is insufficient,
    /// an I/O error occurs, or cancellation is triggered.
    pub fn run<R: Read + Send, T: BlockTarget>(
        &self,
        reader: &mut R,
        target: &mut T,
        progress_tx: &crossbeam_channel::Sender<FlashProgress>,
        cancel_token: &AtomicBool,
    ) -> Result<FlashSummary, InscribeError> {
        let total_passes = if self.options.verify { 2 } else { 1 };
        let mut tracker = ProgressTracker::new(
            self.options.expected_size,
            total_passes,
            self.options.telemetry_rate_hz,
        );

        // Query target capacity up front
        let target_capacity = target.capacity().unwrap_or(0);

        // Enforce capacity check if image size is known
        if let Some(image_bytes) = self.options.expected_size {
            if target_capacity > 0 && target_capacity < image_bytes {
                tracker.set_phase(FlashPhase::Failed, 1, Some(image_bytes));
                tracker.emit(progress_tx, true);
                return Err(InscribeError::InsufficientCapacity {
                    target_bytes: target_capacity,
                    image_bytes,
                });
            }
        }

        if cancel_token.load(Ordering::Relaxed) {
            tracker.set_phase(FlashPhase::Cancelled, 1, self.options.expected_size);
            tracker.emit(progress_tx, true);
            return Err(InscribeError::Cancelled);
        }

        tracker.emit(progress_tx, true);

        let chunk_size = self.options.chunk_size.max(4096);
        let mut buffer = AlignedBuffer::with_capacity_and_alignment(chunk_size, 4096)?;
        buffer.set_len(chunk_size)?;

        let overall_start = Instant::now();

        // -------------------------------------------------------------
        // PASS 1: Streaming Decompress -> Hash -> Direct Block Write
        // -------------------------------------------------------------
        tracker.set_phase(FlashPhase::Flashing, 1, self.options.expected_size);
        tracker.emit(progress_tx, true);

        let compute_sha256 = self.options.expected_sha256.is_some();
        let mut verifier = ChecksumVerifier::new(compute_sha256);
        let mut offset = 0u64;

        loop {
            let bytes_in_buf =
                match read_chunk_or_eof(reader, &mut buffer[..chunk_size], cancel_token) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(InscribeError::Cancelled) => {
                        tracker.set_phase(FlashPhase::Cancelled, 1, self.options.expected_size);
                        tracker.emit(progress_tx, true);
                        return Err(InscribeError::Cancelled);
                    }
                    Err(e) => {
                        tracker.set_phase(FlashPhase::Failed, 1, self.options.expected_size);
                        tracker.emit(progress_tx, true);
                        return Err(e);
                    }
                };

            if target_capacity > 0 && offset + bytes_in_buf as u64 > target_capacity {
                tracker.set_phase(FlashPhase::Failed, 1, self.options.expected_size);
                tracker.emit(progress_tx, true);
                return Err(InscribeError::InsufficientCapacity {
                    target_bytes: target_capacity,
                    image_bytes: offset + bytes_in_buf as u64,
                });
            }

            let slice = &buffer[..bytes_in_buf];
            if let Err(e) = target.write_block(offset, slice) {
                tracker.set_phase(FlashPhase::Failed, 1, self.options.expected_size);
                tracker.emit(progress_tx, true);
                return Err(e);
            }

            verifier.update(slice);
            offset += bytes_in_buf as u64;

            tracker.update_progress(offset);
            tracker.emit(progress_tx, false);
        }

        // Synchronize all writes to persistent media
        if let Err(e) = target.sync() {
            tracker.set_phase(FlashPhase::Failed, 1, self.options.expected_size);
            tracker.emit(progress_tx, true);
            return Err(e);
        }

        let pass1_hashes = verifier.finalize();

        // Optional SHA-256 verification against expected value
        if let Some(ref expected_sha) = self.options.expected_sha256 {
            if let Some(ref actual_sha) = pass1_hashes.sha256 {
                if !actual_sha.eq_ignore_ascii_case(expected_sha) {
                    tracker.set_phase(FlashPhase::Failed, 1, self.options.expected_size);
                    tracker.emit(progress_tx, true);
                    return Err(InscribeError::VerificationFailed {
                        offset: 0,
                        expected: expected_sha.clone(),
                        actual: actual_sha.clone(),
                    });
                }
            }
        }

        // -------------------------------------------------------------
        // PASS 2: Invalidate Cache -> Read Back -> BLAKE3 Verification
        // -------------------------------------------------------------
        if self.options.verify {
            if let Err(e) = target.invalidate_cache() {
                tracker.set_phase(FlashPhase::Failed, 2, Some(offset));
                tracker.emit(progress_tx, true);
                return Err(e);
            }

            let exact_bytes = offset;
            tracker.set_phase(FlashPhase::Verifying, 2, Some(exact_bytes));
            tracker.emit(progress_tx, true);

            let mut read_verifier =
                ReadVerifier::new(pass1_hashes.blake3.clone(), pass1_hashes.chunk_hashes);
            let mut read_offset = 0u64;

            while read_offset < exact_bytes {
                if cancel_token.load(Ordering::Relaxed) {
                    tracker.set_phase(FlashPhase::Cancelled, 2, Some(exact_bytes));
                    tracker.emit(progress_tx, true);
                    return Err(InscribeError::Cancelled);
                }

                let chunk_len = ((exact_bytes - read_offset) as usize).min(chunk_size);
                if let Err(e) = target.read_block(read_offset, &mut buffer[..chunk_len]) {
                    tracker.set_phase(FlashPhase::Failed, 2, Some(exact_bytes));
                    tracker.emit(progress_tx, true);
                    return Err(e);
                }

                if let Err(e) = read_verifier.update(read_offset, &buffer[..chunk_len]) {
                    tracker.set_phase(FlashPhase::Failed, 2, Some(exact_bytes));
                    tracker.emit(progress_tx, true);
                    return Err(e);
                }

                read_offset += chunk_len as u64;
                tracker.update_progress(read_offset);
                tracker.emit(progress_tx, false);
            }

            if let Err(e) = read_verifier.finalize() {
                tracker.set_phase(FlashPhase::Failed, 2, Some(exact_bytes));
                tracker.emit(progress_tx, true);
                return Err(e);
            }
        }

        // -------------------------------------------------------------
        // COMPLETION
        // -------------------------------------------------------------
        let duration = overall_start.elapsed();
        let total_written = offset;
        let average_mb_per_sec = if duration.as_secs_f64() > 0.0 {
            (total_written as f64 / (1024.0 * 1024.0)) / duration.as_secs_f64()
        } else {
            0.0
        };

        tracker.set_phase(FlashPhase::Completed, total_passes, Some(total_written));
        tracker.update_progress(total_written);
        tracker.emit(progress_tx, true);

        Ok(FlashSummary {
            bytes_written: total_written,
            blake3_hash: pass1_hashes.blake3,
            sha256_hash: pass1_hashes.sha256,
            duration,
            average_mb_per_sec,
        })
    }
}

/// Reads from `reader` into `buf` until `buf` is completely filled or EOF is reached,
/// checking `cancel_token` before each read operation.
fn read_chunk_or_eof<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    cancel_token: &AtomicBool,
) -> Result<usize, InscribeError> {
    let mut bytes_read = 0;
    while bytes_read < buf.len() {
        if cancel_token.load(Ordering::Relaxed) {
            return Err(InscribeError::Cancelled);
        }
        let n = reader.read(&mut buf[bytes_read..])?;
        if n == 0 {
            break;
        }
        bytes_read += n;
    }
    Ok(bytes_read)
}
