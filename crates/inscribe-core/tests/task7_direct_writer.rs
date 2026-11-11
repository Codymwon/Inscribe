use inscribe_core::drive::{BusType, DriveInfo};
use inscribe_core::error::InscribeError;
use inscribe_core::target::{BlockTarget, MultiBlockTarget};
use inscribe_core::writer::{AlignedBuffer, BlockDeviceWriter, WriterOptions};
use std::fs::File;
use std::path::PathBuf;

#[test]
fn test_aligned_buffer_4096_alignment_and_layout() {
    // 1. Default 4MB buffer aligned to 4096 bytes
    let buf = AlignedBuffer::new().expect("Failed to allocate 4MB aligned buffer");
    assert_eq!(buf.capacity(), 4 * 1024 * 1024);
    assert_eq!(buf.len(), 4 * 1024 * 1024);
    assert!(buf.is_aligned());
    assert_eq!(buf.as_ptr() as usize % 4096, 0);
    assert!(buf.iter().all(|&b| b == 0));

    // 2. Custom capacity with 4096-byte alignment
    let custom_buf =
        AlignedBuffer::with_capacity(8192).expect("Failed to allocate custom aligned buffer");
    assert_eq!(custom_buf.capacity(), 8192);
    assert_eq!(custom_buf.len(), 8192);
    assert!(custom_buf.is_aligned());
    assert_eq!(custom_buf.as_ptr() as usize % 4096, 0);

    // 3. Mutating aligned buffer
    let mut mut_buf = AlignedBuffer::with_capacity(4096).expect("Failed to allocate");
    mut_buf[0] = 0xAA;
    mut_buf[4095] = 0x55;
    assert_eq!(mut_buf[0], 0xAA);
    assert_eq!(mut_buf[4095], 0x55);
}

#[test]
fn test_aligned_buffer_padding_to_sector_boundary() {
    // Slice of 3 bytes should be padded up to 4096 bytes with 0x00
    let data = vec![0x11, 0x22, 0x33];
    let padded_buf =
        AlignedBuffer::from_slice_padded(&data, 4096).expect("Failed to create padded buffer");

    assert_eq!(padded_buf.len(), 4096);
    assert!(padded_buf.is_aligned());
    assert_eq!(padded_buf.as_ptr() as usize % 4096, 0);
    assert_eq!(&padded_buf[0..3], &[0x11, 0x22, 0x33]);
    assert!(padded_buf[3..4096].iter().all(|&b| b == 0x00));

    // Slicing an exact multiple (e.g. 8192 bytes) retains length without extra sector padding
    let exact_data = vec![0x42; 8192];
    let exact_buf = AlignedBuffer::from_slice_padded(&exact_data, 4096)
        .expect("Failed to create exact padded buffer");
    assert_eq!(exact_buf.len(), 8192);
    assert_eq!(&exact_buf[..], &exact_data[..]);
}

#[test]
fn test_direct_writer_unaligned_payload_padding() {
    let temp_dir = std::env::temp_dir();
    let test_file_path = temp_dir.join(format!(
        "inscribe_test_unaligned_{}.img",
        std::process::id()
    ));

    // Create a blank target file
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&test_file_path)
        .expect("Failed to create test target file");

    let options = WriterOptions {
        direct_io: false, // Standard file test mode
        block_size: 4096,
        read_only: false,
        capacity: Some(10 * 1024 * 1024),
    };

    let mut writer =
        BlockDeviceWriter::from_file(file, test_file_path.to_string_lossy().to_string(), options)
            .expect("Failed to create BlockDeviceWriter");

    // Write unaligned payload size: 1,000,003 bytes
    let payload_size = 1_000_003;
    let mut payload = vec![0u8; payload_size];
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }

    writer
        .write_block(0, &payload)
        .expect("Failed to write unaligned payload");
    writer.sync().expect("Failed to sync writer");

    // Verify written file size on disk is padded to next 4096 sector boundary
    // 1,000,003 rounded up to 4096 = 1,003,520 (245 * 4096)
    let expected_padded_size = 1_003_520u64;
    let metadata = std::fs::metadata(&test_file_path).expect("Failed to inspect metadata");
    assert_eq!(metadata.len(), expected_padded_size);

    // Read back and verify exact payload bytes + trailing zeroes
    let mut read_back = vec![0u8; payload_size];
    writer
        .read_block(0, &mut read_back)
        .expect("Failed to read payload back");
    assert_eq!(read_back, payload);

    // Verify trailing bytes in sector are zeroes
    let mut trailing_sector = vec![0u8; (expected_padded_size - payload_size as u64) as usize];
    writer
        .read_block(payload_size as u64, &mut trailing_sector)
        .expect("Failed to read trailing sector");
    assert!(trailing_sector.iter().all(|&b| b == 0x00));

    // Cleanup
    let _ = std::fs::remove_file(&test_file_path);
}

#[test]
fn test_direct_writer_implements_block_target() {
    let temp_dir = std::env::temp_dir();
    let test_file_path = temp_dir.join(format!("inscribe_test_target_{}.img", std::process::id()));

    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&test_file_path)
        .expect("Failed to create test target file");

    let options = WriterOptions {
        direct_io: false,
        block_size: 4096,
        read_only: false,
        capacity: Some(2 * 1024 * 1024),
    };

    let writer = BlockDeviceWriter::from_file(file, "test-target-1".to_string(), options)
        .expect("Failed to create BlockDeviceWriter");

    // Verify trait object compatibility with BlockTarget and MultiBlockTarget
    let boxed_target: Box<dyn BlockTarget> = Box::new(writer);
    assert_eq!(boxed_target.id(), "test-target-1");
    assert_eq!(boxed_target.block_size(), 4096);
    assert_eq!(boxed_target.capacity().unwrap(), 2 * 1024 * 1024);

    let mut multi = MultiBlockTarget::new(vec![boxed_target]);
    assert_eq!(multi.len(), 1);
    assert_eq!(multi.id(), "test-target-1");

    let test_block = vec![0xEE; 4096];
    multi
        .write_block(0, &test_block)
        .expect("Multi write failed");
    multi.sync().expect("Multi sync failed");

    let mut read_buf = vec![0u8; 4096];
    multi
        .read_block(0, &mut read_buf)
        .expect("Multi read failed");
    assert_eq!(read_buf, test_block);

    let _ = std::fs::remove_file(&test_file_path);
}

#[test]
fn test_direct_writer_drive_guard_rejection() {
    let internal_nvme = DriveInfo {
        path: PathBuf::from("/dev/nvme0n1"),
        model: "Samsung SSD 980 PRO".to_string(),
        vendor: "Samsung".to_string(),
        size: 1_000_204_886_016,
        sector_size: 4096,
        bus_type: BusType::Nvme,
        is_removable: false,
        is_read_only: false,
        is_system: false,
        mount_points: vec![],
    };

    let options = WriterOptions::default();
    let res = BlockDeviceWriter::open_drive(&internal_nvme, &options, false);
    match res {
        Err(InscribeError::DriveGuardViolation { path, reason }) => {
            assert!(path.contains("nvme0n1"));
            assert!(reason.contains("Internal bus") || reason.contains("internal"));
        }
        _ => panic!("Expected DriveGuardViolation, got {res:?}"),
    }

    // System path like root "/" must also be blocked
    let root_res = BlockDeviceWriter::open_path("/", &options);
    assert!(root_res.is_err());
}

#[test]
fn test_direct_writer_sequential_4mb_writes() {
    let temp_dir = std::env::temp_dir();
    let test_file_path = temp_dir.join(format!("inscribe_test_seq_{}.img", std::process::id()));

    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&test_file_path)
        .expect("Failed to create test target file");

    let options = WriterOptions {
        direct_io: false,
        block_size: 4096,
        read_only: false,
        capacity: Some(12 * 1024 * 1024),
    };

    let mut writer = BlockDeviceWriter::from_file(file, "seq-writer".to_string(), options)
        .expect("Failed to create BlockDeviceWriter");

    let chunk_size = 4 * 1024 * 1024;
    let mut chunk1 = AlignedBuffer::new().expect("Failed to allocate 4MB buffer");
    chunk1.fill(0x11);
    let mut chunk2 = AlignedBuffer::new().expect("Failed to allocate 4MB buffer");
    chunk2.fill(0x22);

    writer
        .write_block(0, &chunk1)
        .expect("Write chunk 1 failed");
    writer
        .write_block(chunk_size as u64, &chunk2)
        .expect("Write chunk 2 failed");
    writer.sync().expect("Sync failed");

    let mut read_buf = AlignedBuffer::new().expect("Failed to allocate 4MB buffer");
    writer
        .read_block(0, &mut read_buf)
        .expect("Read chunk 1 failed");
    assert_eq!(&read_buf[..], &chunk1[..]);

    writer
        .read_block(chunk_size as u64, &mut read_buf)
        .expect("Read chunk 2 failed");
    assert_eq!(&read_buf[..], &chunk2[..]);

    let _ = std::fs::remove_file(&test_file_path);
}
