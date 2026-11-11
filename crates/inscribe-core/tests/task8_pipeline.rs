use inscribe_core::error::InscribeError;
use inscribe_core::image::{ImageFormat, open_decompressor};
use inscribe_core::pipeline::{FlashPipeline, PipelineOptions};
use inscribe_core::progress::FlashPhase;
use inscribe_core::target::BlockTarget;
use std::io::{Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// In-memory mock block target for deterministic testing.
struct VirtualBlockTarget {
    id: String,
    storage: Vec<u8>,
    block_size: u32,
    cache_invalidations: u32,
    sync_count: u32,
}

impl VirtualBlockTarget {
    fn new(capacity: usize, block_size: u32) -> Self {
        Self {
            id: "virtual-target-1".to_string(),
            storage: vec![0u8; capacity],
            block_size,
            cache_invalidations: 0,
            sync_count: 0,
        }
    }
}

impl BlockTarget for VirtualBlockTarget {
    fn id(&self) -> &str {
        &self.id
    }

    fn capacity(&self) -> Result<u64, InscribeError> {
        Ok(self.storage.len() as u64)
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError> {
        let start = offset as usize;
        let end = start + data.len();
        if end > self.storage.len() {
            return Err(InscribeError::BlockWriteError {
                offset,
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Write exceeds virtual capacity",
                ),
            });
        }
        self.storage[start..end].copy_from_slice(data);
        Ok(())
    }

    fn sync(&mut self) -> Result<(), InscribeError> {
        self.sync_count += 1;
        Ok(())
    }

    fn read_block(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), InscribeError> {
        let start = offset as usize;
        let end = start + buffer.len();
        if end > self.storage.len() {
            return Err(InscribeError::BlockReadError {
                offset,
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Read exceeds virtual capacity",
                ),
            });
        }
        buffer.copy_from_slice(&self.storage[start..end]);
        Ok(())
    }

    fn invalidate_cache(&mut self) -> Result<(), InscribeError> {
        self.cache_invalidations += 1;
        Ok(())
    }
}

/// Helper to generate deterministic non-repeating data.
fn generate_test_data(size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..size {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        data.push((x & 0xFF) as u8);
    }
    data
}

#[test]
fn test_pass1_and_pass2_bit_for_bit_blake3_and_sha256_match() {
    // 5MB of test data (crosses 4MB chunk boundary to test multi-chunk and unaligned tail)
    let payload = generate_test_data(5 * 1024 * 1024 + 1234);

    let expected_b3 = blake3::hash(&payload).to_hex().to_string();
    use sha2::Digest;
    let expected_sha256 = format!("{:x}", sha2::Sha256::digest(&payload));

    let mut target = VirtualBlockTarget::new(8 * 1024 * 1024, 4096);
    let options = PipelineOptions {
        expected_size: Some(payload.len() as u64),
        verify: true,
        expected_sha256: Some(expected_sha256.clone()),
        chunk_size: 4 * 1024 * 1024,
        telemetry_rate_hz: 60,
    };

    let (progress_tx, progress_rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut reader = Cursor::new(payload.clone());

    let summary = FlashPipeline::new(options)
        .run(&mut reader, &mut target, &progress_tx, &cancel)
        .expect("pipeline should succeed");

    assert_eq!(summary.bytes_written, payload.len() as u64);
    assert_eq!(summary.blake3_hash, expected_b3);
    assert_eq!(
        summary.sha256_hash.as_deref(),
        Some(expected_sha256.as_str())
    );
    assert!(
        target.cache_invalidations >= 1,
        "Cache must be invalidated before Pass 2"
    );
    assert!(target.sync_count >= 1, "Target must be synced");

    // Target storage should match payload byte-for-byte
    assert_eq!(&target.storage[..payload.len()], &payload[..]);

    // Check received telemetry
    let mut events = Vec::new();
    while let Ok(p) = progress_rx.try_recv() {
        events.push(p);
    }
    assert!(
        !events.is_empty(),
        "Telemetry events should have been emitted"
    );
    assert_eq!(events.last().map(|e| e.phase), Some(FlashPhase::Completed));
}

#[test]
fn test_pass2_verification_detects_bit_corruption() {
    let payload = generate_test_data(1024 * 1024);
    let target = VirtualBlockTarget::new(2 * 1024 * 1024, 4096);

    // Custom corrupting target: injects 1 corrupt byte on read_block
    struct CorruptingTarget {
        inner: VirtualBlockTarget,
        corrupt_on_read: bool,
    }
    impl BlockTarget for CorruptingTarget {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn capacity(&self) -> Result<u64, InscribeError> {
            self.inner.capacity()
        }
        fn block_size(&self) -> u32 {
            self.inner.block_size()
        }
        fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError> {
            self.inner.write_block(offset, data)
        }
        fn sync(&mut self) -> Result<(), InscribeError> {
            self.inner.sync()
        }
        fn read_block(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), InscribeError> {
            self.inner.read_block(offset, buffer)?;
            if self.corrupt_on_read && !buffer.is_empty() {
                buffer[0] ^= 0xFF; // Invert bits of first byte
            }
            Ok(())
        }
        fn invalidate_cache(&mut self) -> Result<(), InscribeError> {
            self.inner.invalidate_cache()
        }
    }

    let mut corrupt_target = CorruptingTarget {
        inner: target,
        corrupt_on_read: true,
    };

    let options = PipelineOptions {
        expected_size: Some(payload.len() as u64),
        verify: true,
        expected_sha256: None,
        chunk_size: 256 * 1024,
        telemetry_rate_hz: 60,
    };

    let (progress_tx, _) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut reader = Cursor::new(payload);

    let result =
        FlashPipeline::new(options).run(&mut reader, &mut corrupt_target, &progress_tx, &cancel);

    match result {
        Err(InscribeError::VerificationFailed {
            expected, actual, ..
        }) => {
            assert_ne!(expected, actual);
        }
        other => panic!(
            "Expected InscribeError::VerificationFailed, got: {:?}",
            other
        ),
    }
}

#[test]
fn test_sha256_verification_mismatch_fails_early() {
    let payload = b"Hello Inscribe pipeline test".to_vec();
    let mut target = VirtualBlockTarget::new(1024 * 1024, 4096);
    let options = PipelineOptions {
        expected_size: Some(payload.len() as u64),
        verify: true,
        expected_sha256: Some(
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        ),
        chunk_size: 4096,
        telemetry_rate_hz: 60,
    };

    let (progress_tx, _) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut reader = Cursor::new(payload);

    let result = FlashPipeline::new(options).run(&mut reader, &mut target, &progress_tx, &cancel);

    assert!(matches!(
        result,
        Err(InscribeError::VerificationFailed { .. })
    ));
}

#[test]
fn test_telemetry_emits_at_60hz_with_speed_and_eta() {
    let payload = generate_test_data(2 * 1024 * 1024);
    let mut target = VirtualBlockTarget::new(4 * 1024 * 1024, 4096);

    let options = PipelineOptions {
        expected_size: Some(payload.len() as u64),
        verify: true,
        expected_sha256: None,
        chunk_size: 64 * 1024,
        telemetry_rate_hz: 60,
    };

    let (progress_tx, progress_rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut reader = Cursor::new(payload);

    let summary = FlashPipeline::new(options)
        .run(&mut reader, &mut target, &progress_tx, &cancel)
        .expect("run should succeed");

    assert!(summary.average_mb_per_sec > 0.0);

    let mut phases_seen = Vec::new();
    while let Ok(progress) = progress_rx.try_recv() {
        if !phases_seen.contains(&progress.phase) {
            phases_seen.push(progress.phase);
        }
        if progress.phase == FlashPhase::Flashing {
            assert!(progress.percentage().is_some());
            assert!(progress.current_pass == 1);
        } else if progress.phase == FlashPhase::Verifying {
            assert!(progress.current_pass == 2);
        }
    }

    assert!(phases_seen.contains(&FlashPhase::Flashing));
    assert!(phases_seen.contains(&FlashPhase::Verifying));
    assert!(phases_seen.contains(&FlashPhase::Completed));
}

#[test]
fn test_cancellation_halts_pipeline_safely_within_100ms() {
    // 20MB payload to ensure execution takes enough time to cancel
    let payload = vec![0xAB; 20 * 1024 * 1024];
    let target = VirtualBlockTarget::new(32 * 1024 * 1024, 4096);

    let options = PipelineOptions {
        expected_size: Some(payload.len() as u64),
        verify: true,
        expected_sha256: None,
        chunk_size: 64 * 1024,
        telemetry_rate_hz: 60,
    };

    let reader = Cursor::new(payload);
    let handle = FlashPipeline::spawn(reader, target, options);

    // Sleep briefly so worker starts writing
    std::thread::sleep(Duration::from_millis(5));

    let cancel_start = Instant::now();
    handle.cancel();

    let result = handle.join();
    let elapsed = cancel_start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "Cancellation must halt within 100ms, took {:?}",
        elapsed
    );
    assert!(matches!(result, Err(InscribeError::Cancelled)));
}

#[test]
fn test_insufficient_capacity_rejected() {
    let payload_size = 10 * 1024 * 1024; // 10MB
    let mut target = VirtualBlockTarget::new(2 * 1024 * 1024, 4096); // only 2MB

    let options = PipelineOptions {
        expected_size: Some(payload_size),
        verify: true,
        expected_sha256: None,
        chunk_size: 4 * 1024 * 1024,
        telemetry_rate_hz: 60,
    };

    let (progress_tx, _) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut reader = Cursor::new(vec![0u8; 100]);

    let result = FlashPipeline::new(options).run(&mut reader, &mut target, &progress_tx, &cancel);

    match result {
        Err(InscribeError::InsufficientCapacity {
            target_bytes,
            image_bytes,
        }) => {
            assert_eq!(target_bytes, 2 * 1024 * 1024);
            assert_eq!(image_bytes, 10 * 1024 * 1024);
        }
        other => panic!("Expected InsufficientCapacity, got {:?}", other),
    }
}

#[test]
fn test_pipeline_zero_disk_buffering_with_compressed_image() {
    // Generate raw payload and compress it in memory using flate2 (gzip)
    let raw_payload = generate_test_data(512 * 1024);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw_payload).expect("gzip encode failed");
    let compressed = encoder.finish().expect("gzip finish failed");

    // Open streaming decompressor without creating any host files
    let mut decompressor = open_decompressor(Cursor::new(compressed), ImageFormat::Gz)
        .expect("open_decompressor failed");

    let initial_temp_entries = std::fs::read_dir(std::env::temp_dir())
        .map(|d| d.count())
        .unwrap_or(0);

    let mut target = VirtualBlockTarget::new(1024 * 1024, 4096);
    let options = PipelineOptions {
        expected_size: Some(raw_payload.len() as u64),
        verify: true,
        expected_sha256: None,
        chunk_size: 64 * 1024,
        telemetry_rate_hz: 60,
    };

    let (progress_tx, _) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));

    let summary = FlashPipeline::new(options)
        .run(&mut decompressor, &mut target, &progress_tx, &cancel)
        .expect("pipeline should succeed");

    let post_temp_entries = std::fs::read_dir(std::env::temp_dir())
        .map(|d| d.count())
        .unwrap_or(0);

    // Verify zero temporary files created
    assert_eq!(initial_temp_entries, post_temp_entries);
    assert_eq!(summary.bytes_written, raw_payload.len() as u64);
    assert_eq!(&target.storage[..raw_payload.len()], &raw_payload[..]);
}
