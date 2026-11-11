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

