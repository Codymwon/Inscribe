use inscribe_core::image::{
    CountingReader, ImageFormat, detect_format, inspect_image, open_decompressor,
    open_seekable_decompressor,
};
use std::io::{Cursor, Read, Write};
use std::path::Path;

// Helper to create sample compressed payloads in memory
fn create_gzip_data(data: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).expect("gzip write failed");
    encoder.finish().expect("gzip finish failed")
}

fn create_xz_data(data: &[u8]) -> Vec<u8> {
    use lzma_rust2::{XzOptions, XzWriter};
    let mut writer =
        XzWriter::new(Vec::new(), XzOptions::default()).expect("xz writer init failed");
    writer.write_all(data).expect("xz write failed");
    writer.finish().expect("xz finish failed")
}

fn create_zstd_data(data: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(data, 1).expect("zstd compression failed")
}

fn create_bzip2_data(data: &[u8]) -> Vec<u8> {
    use bzip2::Compression;
    use bzip2::write::BzEncoder;
    let mut encoder = BzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).expect("bzip2 write failed");
    encoder.finish().expect("bzip2 finish failed")
}

fn create_zip_data(filename: &str, data: &[u8]) -> Vec<u8> {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;
    let mut cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(&mut cursor);
    zip.start_file(filename, SimpleFileOptions::default())
        .expect("zip start_file failed");
    zip.write_all(data).expect("zip write failed");
    zip.finish().expect("zip finish failed");
    cursor.into_inner()
}

#[test]
fn test_magic_byte_format_detection() {
    let raw_mbr = {
        let mut buf = vec![0u8; 512];
        buf[510] = 0x55;
        buf[511] = 0xAA;
        buf
    };
    assert_eq!(detect_format(&raw_mbr, None), ImageFormat::Raw);

    let gz_data = create_gzip_data(b"test data");
    assert_eq!(detect_format(&gz_data, None), ImageFormat::Gz);

    let xz_data = create_xz_data(b"test data");
    assert_eq!(detect_format(&xz_data, None), ImageFormat::Xz);

    let zstd_data = create_zstd_data(b"test data");
    assert_eq!(detect_format(&zstd_data, None), ImageFormat::Zstd);

    let bzip2_data = create_bzip2_data(b"test data");
    assert_eq!(detect_format(&bzip2_data, None), ImageFormat::Bzip2);

    let zip_data = create_zip_data("disk.img", b"test data");
    assert_eq!(detect_format(&zip_data, None), ImageFormat::Zip);
}

#[test]
fn test_file_extension_fallback_detection() {
    let empty: [u8; 0] = [];
    assert_eq!(
        detect_format(&empty, Some(Path::new("ubuntu.iso"))),
        ImageFormat::Raw
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("arch.img"))),
        ImageFormat::Raw
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("image.raw"))),
        ImageFormat::Raw
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("image.bin"))),
        ImageFormat::Raw
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("image.img.gz"))),
        ImageFormat::Gz
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("image.img.xz"))),
        ImageFormat::Xz
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("image.img.zst"))),
        ImageFormat::Zstd
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("image.img.bz2"))),
        ImageFormat::Bzip2
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("raspios.zip"))),
        ImageFormat::Zip
    );
    assert_eq!(
        detect_format(&empty, Some(Path::new("unknown.xyz"))),
        ImageFormat::Unknown
    );
}

#[test]
fn test_uncompressed_size_inspection() {
    let payload = b"Hello, transparent streaming decompression for Inscribe!";

    // RAW size is exact
    let mut raw_cursor = Cursor::new(payload.to_vec());
    let raw_meta = inspect_image(
        &mut raw_cursor,
        ImageFormat::Raw,
        Some(Path::new("test.img")),
    )
    .expect("inspect raw failed");
    assert_eq!(raw_meta.uncompressed_size, Some(payload.len() as u64));
    assert!(raw_meta.has_deterministic_size());

    // XZ size extracted via xz index
    let xz_data = create_xz_data(payload);
    let mut xz_cursor = Cursor::new(xz_data);
    let xz_meta = inspect_image(&mut xz_cursor, ImageFormat::Xz, Some(Path::new("test.xz")))
        .expect("inspect xz failed");
    assert_eq!(xz_meta.uncompressed_size, Some(payload.len() as u64));

    // ZIP size extracted from zip central directory
    let zip_data = create_zip_data("os.img", payload);
    let mut zip_cursor = Cursor::new(zip_data);
    let zip_meta = inspect_image(
        &mut zip_cursor,
        ImageFormat::Zip,
        Some(Path::new("test.zip")),
    )
    .expect("inspect zip failed");
    assert_eq!(zip_meta.uncompressed_size, Some(payload.len() as u64));

    // GZ size extracted via ISIZE footer hint
    let gz_data = create_gzip_data(payload);
    let mut gz_cursor = Cursor::new(gz_data);
    let gz_meta = inspect_image(&mut gz_cursor, ImageFormat::Gz, Some(Path::new("test.gz")))
        .expect("inspect gz failed");
    assert_eq!(gz_meta.uncompressed_size, Some(payload.len() as u64));

    // ZSTD size extracted via frame content size header
    let zstd_data = create_zstd_data(payload);
    let mut zstd_cursor = Cursor::new(zstd_data);
    let zstd_meta = inspect_image(
        &mut zstd_cursor,
        ImageFormat::Zstd,
        Some(Path::new("test.zst")),
    )
    .expect("inspect zstd failed");
    assert_eq!(zstd_meta.uncompressed_size, Some(payload.len() as u64));

    // Bzip2 has no size metadata -> indeterminate None
    let bzip2_data = create_bzip2_data(payload);
    let mut bzip2_cursor = Cursor::new(bzip2_data);
    let bz2_meta = inspect_image(
        &mut bzip2_cursor,
        ImageFormat::Bzip2,
        Some(Path::new("test.bz2")),
    )
    .expect("inspect bz2 failed");
    assert_eq!(bz2_meta.uncompressed_size, None);
    assert!(!bz2_meta.has_deterministic_size());
}

#[test]
fn test_decompression_byte_for_byte_match() {
    let source_data = (0..65536).map(|i| (i % 251) as u8).collect::<Vec<u8>>();

    // Test RAW
    let mut raw_reader = open_decompressor(Cursor::new(source_data.clone()), ImageFormat::Raw)
        .expect("open raw failed");
    let mut decompressed = Vec::new();
    raw_reader
        .read_to_end(&mut decompressed)
        .expect("read raw failed");
    assert_eq!(decompressed, source_data);

    // Test GZ
    let gz_data = create_gzip_data(&source_data);
    let mut gz_reader =
        open_decompressor(Cursor::new(gz_data), ImageFormat::Gz).expect("open gz failed");
    let mut decompressed = Vec::new();
    gz_reader
        .read_to_end(&mut decompressed)
        .expect("read gz failed");
    assert_eq!(decompressed, source_data);

    // Test XZ with seekable reader (multi-threaded XzReaderMt)
    let xz_data = create_xz_data(&source_data);
    let mut xz_seekable_reader =
        open_seekable_decompressor(Cursor::new(xz_data.clone()), ImageFormat::Xz)
            .expect("open seekable xz failed");
    let mut decompressed = Vec::new();
    xz_seekable_reader
        .read_to_end(&mut decompressed)
        .expect("read xz seekable failed");
    assert_eq!(decompressed, source_data);

    // Test XZ with stream reader (forward streaming XzReader)
    let mut xz_stream_reader =
        open_decompressor(Cursor::new(xz_data), ImageFormat::Xz).expect("open stream xz failed");
    let mut decompressed = Vec::new();
    xz_stream_reader
        .read_to_end(&mut decompressed)
        .expect("read xz stream failed");
    assert_eq!(decompressed, source_data);

    // Test ZSTD
    let zstd_data = create_zstd_data(&source_data);
    let mut zstd_reader =
        open_decompressor(Cursor::new(zstd_data), ImageFormat::Zstd).expect("open zstd failed");
    let mut decompressed = Vec::new();
    zstd_reader
        .read_to_end(&mut decompressed)
        .expect("read zstd failed");
    assert_eq!(decompressed, source_data);

    // Test BZIP2
    let bzip2_data = create_bzip2_data(&source_data);
    let mut bzip2_reader =
        open_decompressor(Cursor::new(bzip2_data), ImageFormat::Bzip2).expect("open bzip2 failed");
    let mut decompressed = Vec::new();
    bzip2_reader
        .read_to_end(&mut decompressed)
        .expect("read bzip2 failed");
    assert_eq!(decompressed, source_data);

    // Test ZIP
    let zip_data = create_zip_data("image.img", &source_data);
    let mut zip_reader = open_seekable_decompressor(Cursor::new(zip_data), ImageFormat::Zip)
        .expect("open zip failed");
    let mut decompressed = Vec::new();
    zip_reader
        .read_to_end(&mut decompressed)
        .expect("read zip failed");
    assert_eq!(decompressed, source_data);
}

#[test]
fn test_counting_reader_tracks_indeterminate_progress() {
    let payload = vec![0x42; 100_000];
    let cursor = Cursor::new(payload);
    let mut counting = CountingReader::new(cursor);

    let mut buf = [0u8; 4096];
    let mut total_read = 0;
    loop {
        let n = counting.read(&mut buf).expect("read failed");
        if n == 0 {
            break;
        }
        total_read += n as u64;
        assert_eq!(counting.bytes_read(), total_read);
    }
    assert_eq!(total_read, 100_000);
    assert_eq!(counting.bytes_read(), 100_000);
}

#[test]
fn test_corrupt_data_returns_decompression_failed() {
    let corrupt_gz = vec![0x1F, 0x8B, 0x08, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0xFF, 0xFF];
    let mut reader = open_decompressor(Cursor::new(corrupt_gz), ImageFormat::Gz)
        .expect("open should succeed initially");
    let mut out = Vec::new();
    let err = reader
        .read_to_end(&mut out)
        .expect_err("should fail on corrupt gzip");
    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
        ),
        "Expected InvalidData or UnexpectedEof, got {:?}",
        err.kind()
    );
}

// Struct implementing Read + Send without Seek to simulate non-seekable streams
struct NonSeekableStream<R>(R);

impl<R: Read> Read for NonSeekableStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

#[test]
fn test_streaming_zip_skips_directories_and_macos_metadata() {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let mut cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(&mut cursor);

    // Entry 1: Directory header
    zip.add_directory("distro/", SimpleFileOptions::default())
        .expect("add_directory failed");

    // Entry 2: macOS metadata directory
    zip.add_directory("__MACOSX/", SimpleFileOptions::default())
        .expect("add __MACOSX failed");

    // Entry 3: macOS resource fork dot-underscore file
    zip.start_file("__MACOSX/._distro", SimpleFileOptions::default())
        .expect("add resource fork failed");
    zip.write_all(b"RESOURCE_FORK_METADATA")
        .expect("write failed");

    // Entry 4: another dot-underscore file inside distro/
    zip.start_file("distro/._os.raw", SimpleFileOptions::default())
        .expect("add dot-underscore file failed");
    zip.write_all(b"METADATA_HEADER").expect("write failed");

    // Entry 5: Actual disk image payload
    let real_payload = b"GENUINE_OPERATING_SYSTEM_RAW_DISK_IMAGE_PAYLOAD";
    zip.start_file("distro/os.raw", SimpleFileOptions::default())
        .expect("add real file failed");
    zip.write_all(real_payload).expect("write failed");

    zip.finish().expect("zip finish failed");
    let zip_bytes = cursor.into_inner();

    // Pass non-seekable stream to open_decompressor
    let non_seekable = NonSeekableStream(Cursor::new(zip_bytes));
    let mut decompressor = open_decompressor(non_seekable, ImageFormat::Zip)
        .expect("open_decompressor streaming zip failed");

    let mut decompressed = Vec::new();
    decompressor
        .read_to_end(&mut decompressed)
        .expect("streaming zip read_to_end failed");

    assert_eq!(
        decompressed, real_payload,
        "Streaming non-seekable ZIP decompressor must skip folder headers and macOS metadata to find the real image"
    );
}

// Mock seekable reader to simulate large Gzip files (> 4 GiB) without allocating host disk/RAM
struct MockGzipStream {
    file_size: u64,
    cursor_pos: u64,
    header: [u8; 10],
    isize_footer: [u8; 4],
}

impl MockGzipStream {
    fn new(file_size: u64, reported_isize: u32) -> Self {
        Self {
            file_size,
            cursor_pos: 0,
            header: [0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0, 0xFF],
            isize_footer: reported_isize.to_le_bytes(),
        }
    }
}

impl Read for MockGzipStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cursor_pos >= self.file_size {
            return Ok(0);
        }

        let mut bytes_read = 0;
        for byte in buf.iter_mut() {
            if self.cursor_pos >= self.file_size {
                break;
            }
            if self.cursor_pos < 10 {
                *byte = self.header[self.cursor_pos as usize];
            } else if self.cursor_pos >= self.file_size - 4 {
                let footer_idx = (self.cursor_pos - (self.file_size - 4)) as usize;
                *byte = self.isize_footer[footer_idx];
            } else {
                *byte = 0x55; // Dummy compressed payload byte
            }
            self.cursor_pos += 1;
            bytes_read += 1;
        }

        Ok(bytes_read)
    }
}

impl std::io::Seek for MockGzipStream {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            std::io::SeekFrom::Start(p) => {
                self.cursor_pos = p.min(self.file_size);
            }
            std::io::SeekFrom::End(p) => {
                let target = (self.file_size as i64) + p;
                self.cursor_pos = target.max(0) as u64;
            }
            std::io::SeekFrom::Current(p) => {
                let target = (self.cursor_pos as i64) + p;
                self.cursor_pos = target.max(0) as u64;
            }
        }
        Ok(self.cursor_pos)
    }
}

#[test]
fn test_inspect_gzip_greater_than_4gib_falls_back_to_indeterminate() {
    const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;

    // Case 1: Compressed file is >= 4 GiB (4.5 GiB)
    // RFC 1952 32-bit ISIZE modulo wraps -> must fall back to None
    let mut large_gz = MockGzipStream::new(FOUR_GIB + 500 * 1024 * 1024, 1_000_000_000);
    let meta = inspect_image(&mut large_gz, ImageFormat::Gz, Some(Path::new("large.gz")))
        .expect("inspect failed");
    assert_eq!(
        meta.uncompressed_size, None,
        "Gzip file >= 4 GiB must return None for uncompressed size (indeterminate)"
    );

    // Case 2: Compressed file is 2 GiB, but ISIZE reports 500 MB (isize < file_size)
    // Suggests modulo 2^32 wrap -> must fall back to None
    let mut wrapped_gz = MockGzipStream::new(2 * 1024 * 1024 * 1024, 500 * 1024 * 1024);
    let meta_wrapped = inspect_image(
        &mut wrapped_gz,
        ImageFormat::Gz,
        Some(Path::new("wrapped.gz")),
    )
    .expect("inspect failed");
    assert_eq!(
        meta_wrapped.uncompressed_size, None,
        "Gzip file with ISIZE < file_size must return None (modulo wrap)"
    );

    // Case 3: Normal Gzip file (500 MB compressed, 1.2 GB uncompressed)
    let mut normal_gz = MockGzipStream::new(500 * 1024 * 1024, 1_200_000_000);
    let meta_normal = inspect_image(
        &mut normal_gz,
        ImageFormat::Gz,
        Some(Path::new("normal.gz")),
    )
    .expect("inspect failed");
    assert_eq!(
        meta_normal.uncompressed_size,
        Some(1_200_000_000),
        "Normal Gzip under 4 GiB must return Some(isize)"
    );
}

#[test]
fn test_zip_decompressor_selects_image_file_over_preceding_documentation() {
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let mut cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(&mut cursor);

    // Entry 1: Documentation text file (README)
    zip.start_file("README.txt", SimpleFileOptions::default())
        .expect("add README failed");
    zip.write_all(b"Please read this readme file before using the disk image.")
        .expect("write failed");

    // Entry 2: License text file
    zip.start_file("LICENSE.txt", SimpleFileOptions::default())
        .expect("add LICENSE failed");
    zip.write_all(b"Apache License 2.0").expect("write failed");

    // Entry 3: Actual OS disk image payload
    let real_disk_image = b"RAW_DISK_IMAGE_PAYLOAD_FOR_USB_FLASHING_TEST";
    zip.start_file("distro/os.img", SimpleFileOptions::default())
        .expect("add os.img failed");
    zip.write_all(real_disk_image).expect("write failed");

    zip.finish().expect("zip finish failed");
    let zip_bytes = cursor.into_inner();

    // 1. Seekable decompressor (used by CLI flash)
    let mut seekable_decompressor =
        open_seekable_decompressor(Cursor::new(zip_bytes.clone()), ImageFormat::Zip)
            .expect("open_seekable_decompressor failed");
    let mut seekable_output = Vec::new();
    seekable_decompressor
        .read_to_end(&mut seekable_output)
        .expect("read seekable failed");
    assert_eq!(
        seekable_output, real_disk_image,
        "Seekable ZIP decompressor must select os.img rather than preceding README.txt"
    );

    // 2. Non-seekable stream decompressor
    let non_seekable = NonSeekableStream(Cursor::new(zip_bytes));
    let mut stream_decompressor =
        open_decompressor(non_seekable, ImageFormat::Zip).expect("open_decompressor failed");
    let mut stream_output = Vec::new();
    stream_decompressor
        .read_to_end(&mut stream_output)
        .expect("read stream failed");
    assert_eq!(
        stream_output, real_disk_image,
        "Sequential streaming ZIP decompressor must skip preceding documentation files to select disk image"
    );
}
