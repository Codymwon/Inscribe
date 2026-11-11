//! Transparent streaming decompression implementations and byte-counting adapters.

use super::ImageFormat;
use crate::error::InscribeError;
use std::io::{Cursor, Read, Seek};

/// Byte-counting reader wrapper used to track streaming progress in indeterminate modes.
#[derive(Debug)]
pub struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
}

impl<R: Read> CountingReader<R> {
    /// Wraps a reader with a byte counter initialized to zero.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }

    /// Total number of uncompressed or processed bytes read so far.
    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Unwraps the inner reader, discarding the counter.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n as u64;
        Ok(n)
    }
}

/// Channel-backed stream reader for asynchronous in-memory decompression pipelines (e.g. ZIP entries).
pub struct ChannelReader {
    receiver: crossbeam_channel::Receiver<std::io::Result<Vec<u8>>>,
    current_chunk: Cursor<Vec<u8>>,
    _worker: Option<std::thread::JoinHandle<()>>,
}

impl ChannelReader {
    /// Creates a new `ChannelReader` consuming chunks from a receiver.
    pub fn new(
        receiver: crossbeam_channel::Receiver<std::io::Result<Vec<u8>>>,
        worker: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            receiver,
            current_chunk: Cursor::new(Vec::new()),
            _worker: Some(worker),
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = self.current_chunk.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            match self.receiver.recv() {
                Ok(Ok(chunk)) => {
                    self.current_chunk = Cursor::new(chunk);
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(0), // Channel closed / EOF
            }
        }
    }
}

/// Opens a forward streaming decompressor for any `Read + Send + 'static` stream.
///
/// Supported formats:
/// - RAW (.iso, .img): Passthrough reader
/// - GZ (.gz): `flate2::read::MultiGzDecoder` (<https://docs.rs/flate2/latest/flate2/read/struct.MultiGzDecoder.html>)
/// - XZ (.xz): `lzma_rust2::XzReader` single-stream/multi-stream decoder (<https://docs.rs/lzma-rust2/latest/lzma_rust2/struct.XzReader.html>)
/// - ZSTD (.zst): `zstd::Decoder` (<https://docs.rs/zstd/latest/zstd/stream/read/struct.Decoder.html>)
/// - BZ2 (.bz2): `bzip2::read::BzDecoder` (<https://docs.rs/bzip2/latest/bzip2/read/struct.BzDecoder.html>)
/// - ZIP (.zip): Streaming sequential entry decoder via `zip::read::read_zipfile_from_stream`
pub fn open_decompressor<R: Read + Send + 'static>(
    reader: R,
    format: ImageFormat,
) -> Result<Box<dyn Read + Send>, InscribeError> {
    match format {
        ImageFormat::Raw => Ok(Box::new(reader)),
        ImageFormat::Gz => {
            // Source: https://docs.rs/flate2/latest/flate2/read/struct.MultiGzDecoder.html
            Ok(Box::new(flate2::read::MultiGzDecoder::new(reader)))
        }
        ImageFormat::Xz => {
            // Source: https://docs.rs/lzma-rust2/latest/lzma_rust2/struct.XzReader.html
            Ok(Box::new(lzma_rust2::XzReader::new(reader, true)))
        }
        ImageFormat::Zstd => {
            // Source: https://docs.rs/zstd/latest/zstd/stream/read/struct.Decoder.html
            let decoder = zstd::Decoder::new(reader)?;
            Ok(Box::new(decoder))
        }
        ImageFormat::Bzip2 => {
            // Source: https://docs.rs/bzip2/latest/bzip2/read/struct.BzDecoder.html
            Ok(Box::new(bzip2::read::BzDecoder::new(reader)))
        }
        ImageFormat::Zip => {
            // Source: https://docs.rs/zip/latest/zip/read/fn.read_zipfile_from_stream.html
            let (tx, rx) = crossbeam_channel::bounded::<std::io::Result<Vec<u8>>>(16);
            let handle = std::thread::spawn(move || {
                let mut reader = reader;
                // Sequentially scan entries in the non-seekable stream, skipping directory
                // headers and macOS metadata to find the real disk image payload.
                loop {
                    let mut zip_file = match zip::read::read_zipfile_from_stream(&mut reader) {
                        Ok(Some(file)) => file,
                        Ok(None) => return, // End of archive stream
                        Err(e) => {
                            let _ = tx
                                .send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)));
                            return;
                        }
                    };

                    let name = zip_file.name().to_ascii_lowercase();
                    let is_dir = zip_file.is_dir() || name.ends_with('/');
                    let is_macos_meta = name.contains("__macosx")
                        || name.split('/').any(|seg| seg.starts_with("._"));
                    let is_doc = name.ends_with(".txt")
                        || name.ends_with(".md")
                        || name.ends_with(".nfo")
                        || name.ends_with(".pdf")
                        || name.ends_with(".html")
                        || name.ends_with(".htm")
                        || name.ends_with(".json")
                        || name.ends_with(".xml")
                        || name.ends_with(".yaml")
                        || name.ends_with(".yml")
                        || name.ends_with(".rtf");

                    if is_dir || is_macos_meta || is_doc {
                        // Dropping zip_file advances reader past entry payload
                        continue;
                    }

                    let mut buffer = vec![0u8; 64 * 1024];
                    loop {
                        match zip_file.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(n) => {
                                if tx.send(Ok(buffer[..n].to_vec())).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e));
                                return;
                            }
                        }
                    }
                    break;
                }
            });
            Ok(Box::new(ChannelReader::new(rx, handle)))
        }
        ImageFormat::Unknown => Err(InscribeError::InvalidImage(
            "Unsupported or unrecognized image container format".to_string(),
        )),
    }
}

/// Opens a decompressor for seekable sources (`Read + Seek + Send + 'static`), enabling multi-threaded XZ
/// decompression (`lzma_rust2::XzReaderMt`) and random-access ZIP archive decompression (`zip::ZipArchive`).
///
/// Sources / Specifications:
/// - Pure-Rust multi-threaded XZ: `lzma_rust2::XzReaderMt` distributes block decompression across CPU cores.
///   Source: <https://docs.rs/lzma-rust2/latest/lzma_rust2/struct.XzReaderMt.html>
/// - ZIP central directory extraction: `zip::ZipArchive` selects the image file and streams chunks into memory.
///   Source: <https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html>
pub fn open_seekable_decompressor<R: Read + Seek + Send + 'static>(
    reader: R,
    format: ImageFormat,
) -> Result<Box<dyn Read + Send>, InscribeError> {
    match format {
        ImageFormat::Xz => {
            let num_workers = std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(4);
            // Source: https://docs.rs/lzma-rust2/latest/lzma_rust2/struct.XzReaderMt.html
            let mt_reader =
                lzma_rust2::XzReaderMt::new(reader, true, num_workers).map_err(|e| {
                    InscribeError::DecompressionFailed(format!(
                        "Failed to initialize multi-threaded XZ reader: {e}"
                    ))
                })?;
            Ok(Box::new(mt_reader))
        }
        ImageFormat::Zip => {
            // Source: https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html
            let (tx, rx) = crossbeam_channel::bounded::<std::io::Result<Vec<u8>>>(16);
            let handle = std::thread::spawn(move || {
                let mut archive = match zip::ZipArchive::new(reader) {
                    Ok(a) => a,
                    Err(e) => {
                        let _ =
                            tx.send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)));
                        return;
                    }
                };

                // Find the best entry to flash
                let mut target_index = 0;
                let mut best_size: u64 = 0;

                for i in 0..archive.len() {
                    if let Ok(file) = archive.by_index(i) {
                        let name = file.name().to_ascii_lowercase();
                        if name.ends_with('/')
                            || name.contains("__macosx")
                            || name.split('/').any(|seg| seg.starts_with("._"))
                        {
                            continue;
                        }
                        let is_image = name.ends_with(".img")
                            || name.ends_with(".iso")
                            || name.ends_with(".raw")
                            || name.ends_with(".bin");
                        if is_image {
                            target_index = i;
                            break;
                        }
                        if file.size() > best_size {
                            best_size = file.size();
                            target_index = i;
                        }
                    }
                }

                let mut file = match archive.by_index(target_index) {
                    Ok(f) => f,
                    Err(e) => {
                        let _ =
                            tx.send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)));
                        return;
                    }
                };

                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(Ok(buffer[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            break;
                        }
                    }
                }
            });

            Ok(Box::new(ChannelReader::new(rx, handle)))
        }
        // Non-seekable or non-MT formats delegate to open_decompressor
        other => open_decompressor(reader, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counting_reader_unit() {
        let data = b"Inscribe direct streaming without temp files";
        let mut counting = CountingReader::new(&data[..]);

        let mut buf = [0u8; 8];
        let n = counting.read(&mut buf).expect("read failed");
        assert_eq!(n, 8);
        assert_eq!(counting.bytes_read(), 8);

        let mut remainder = Vec::new();
        counting
            .read_to_end(&mut remainder)
            .expect("read remaining failed");
        assert_eq!(counting.bytes_read(), data.len() as u64);

        let inner = counting.into_inner();
        assert_eq!(inner.len(), 0);
    }

    #[test]
    fn test_unknown_format_fails() {
        let data = b"Some random binary";
        let result = open_decompressor(&data[..], ImageFormat::Unknown);
        assert!(matches!(result, Err(InscribeError::InvalidImage(_))));
    }

    #[test]
    fn test_zero_temporary_files_generated() {
        let initial_files: std::collections::HashSet<_> = std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();

        let source = b"Decompressing entirely in-memory with zero host temp files!";
        let mut reader = open_decompressor(&source[..], ImageFormat::Raw).expect("open failed");
        let mut output = Vec::new();
        reader.read_to_end(&mut output).expect("read failed");
        assert_eq!(output, source);

        let new_files: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .filter(|f| !initial_files.contains(f))
            .collect();

        // Verification: Zero temporary files created on host storage during streaming
        assert!(
            new_files.is_empty(),
            "Expected zero new temporary files in temp dir, found: {new_files:?}"
        );
    }
}
