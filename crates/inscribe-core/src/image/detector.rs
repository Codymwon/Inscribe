//! Magic-byte header detection and uncompressed payload size inspection.

use super::{ImageFormat, ImageMetadata};
use crate::error::InscribeError;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Magic byte headers for supported container and compression formats.
pub const XZ_MAGIC: &[u8] = &[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00];
pub const GZ_MAGIC: &[u8] = &[0x1F, 0x8B];
pub const ZSTD_MAGIC: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
pub const BZIP2_MAGIC: &[u8] = b"BZh";
pub const ZIP_LOCAL_MAGIC: &[u8] = &[0x50, 0x4B, 0x03, 0x04];
pub const ZIP_EMPTY_MAGIC: &[u8] = &[0x50, 0x4B, 0x05, 0x06];
pub const ZIP_SPANNED_MAGIC: &[u8] = &[0x50, 0x4B, 0x07, 0x08];

/// Detects the image format by examining initial header magic bytes, falling back to file extension.
#[must_use]
pub fn detect_format(header: &[u8], path: Option<&Path>) -> ImageFormat {
    if header.starts_with(XZ_MAGIC) {
        return ImageFormat::Xz;
    }
    if header.starts_with(GZ_MAGIC) {
        return ImageFormat::Gz;
    }
    if header.starts_with(ZSTD_MAGIC) {
        return ImageFormat::Zstd;
    }
    if header.starts_with(BZIP2_MAGIC) {
        return ImageFormat::Bzip2;
    }
    if header.starts_with(ZIP_LOCAL_MAGIC)
        || header.starts_with(ZIP_EMPTY_MAGIC)
        || header.starts_with(ZIP_SPANNED_MAGIC)
    {
        return ImageFormat::Zip;
    }

    // Check for MBR partition table boot signature at offset 510..512
    if header.len() >= 512 && header[510] == 0x55 && header[511] == 0xAA {
        return ImageFormat::Raw;
    }

    // Check for GPT partition table header at offset 512..520
    if header.len() >= 520 && &header[512..520] == b"EFI PART" {
        return ImageFormat::Raw;
    }

    // Check for ISO 9660 Primary Volume Descriptor standard identifier at offset 32769..32774
    if header.len() >= 32774 && &header[32769..32774] == b"CD001" {
        return ImageFormat::Raw;
    }

    // Fall back to path extension when magic bytes are absent or indeterminate
    if let Some(path) = path {
        if let Some(format) = detect_from_path(path) {
            return format;
        }
    }

    ImageFormat::Unknown
}

/// Detects image format based solely on file extension.
#[must_use]
pub fn detect_from_path(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "iso" | "img" | "raw" | "bin" => Some(ImageFormat::Raw),
        "gz" => Some(ImageFormat::Gz),
        "xz" => Some(ImageFormat::Xz),
        "zst" | "zstd" => Some(ImageFormat::Zstd),
        "bz2" => Some(ImageFormat::Bzip2),
        "zip" => Some(ImageFormat::Zip),
        _ => None,
    }
}

/// Inspects a seekable image stream, extracting total file size and uncompressed payload size if available.
///
/// Official Sources / Specifications:
/// - XZ uncompressed size: `lzma_rust2::xz_uncompressed_size` parses the backward index at end of stream.
///   Source: <https://docs.rs/lzma-rust2/latest/lzma_rust2/fn.xz_uncompressed_size.html>
/// - GZIP ISIZE hint: RFC 1952 Section 2.3.1 - the last 4 bytes store uncompressed size modulo 2^32 in LE.
///   Source: <https://www.rfc-editor.org/rfc/rfc1952#page-5>
/// - Zstandard frame content size: `zstd::zstd_safe::get_frame_content_size` reads the frame header.
///   Source: <https://docs.rs/zstd/latest/zstd/stream/read/struct.Decoder.html>
/// - ZIP central directory headers: `zip::ZipArchive` reads entry metadata and uncompressed size.
///   Source: <https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html>
pub fn inspect_image<R: Read + Seek>(
    reader: &mut R,
    format: ImageFormat,
    path: Option<&Path>,
) -> Result<ImageMetadata, InscribeError> {
    let file_size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    let uncompressed_size = match format {
        ImageFormat::Raw => Some(file_size),
        ImageFormat::Xz => {
            // Source: https://docs.rs/lzma-rust2/latest/lzma_rust2/fn.xz_uncompressed_size.html
            let size = lzma_rust2::xz_uncompressed_size(&mut *reader).ok();
            reader.seek(SeekFrom::Start(0))?;
            size
        }
        ImageFormat::Zip => {
            // Source: https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html
            let size = if let Ok(mut archive) = zip::ZipArchive::new(&mut *reader) {
                super::decompress::find_best_zip_entry_index(&mut archive)
                    .and_then(|idx| archive.by_index(idx).ok().map(|f| f.size()))
            } else {
                None
            };
            reader.seek(SeekFrom::Start(0))?;
            size
        }
        ImageFormat::Gz => {
            // RFC 1952 Gzip footer ISIZE hint (last 4 bytes, little-endian u32).
            // Source: https://www.rfc-editor.org/rfc/rfc1952#page-5
            // ISIZE is modulo 2^32 (4 GiB). If compressed file size >= 4 GiB or
            // the reported ISIZE is less than compressed file size, the uncompressed
            // size has wrapped modulo 2^32 or is indeterminate. Fall back to None.
            const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
            let size = if file_size >= 10 + 8 {
                reader.seek(SeekFrom::Start(file_size - 4))?;
                let mut buf = [0u8; 4];
                if reader.read_exact(&mut buf).is_ok() {
                    let isize = u32::from_le_bytes(buf) as u64;
                    // In DEFLATE (RFC 1951), compressed container overhead can exceed uncompressed
                    // payload size only for small files (< 64 KiB). If compressed file_size >= 4 GiB
                    // or file_size exceeds reported ISIZE by more than 64 KiB, ISIZE has wrapped
                    // modulo 2^32 or is indeterminate.
                    if file_size >= FOUR_GIB || file_size > isize.saturating_add(64 * 1024) {
                        None
                    } else {
                        Some(isize)
                    }
                } else {
                    None
                }
            } else {
                None
            };
            reader.seek(SeekFrom::Start(0))?;
            size
        }
        ImageFormat::Zstd => {
            // Read frame header (up to 512 bytes) to extract uncompressed frame size
            let mut header_buf = [0u8; 512];
            let n = reader.read(&mut header_buf)?;
            reader.seek(SeekFrom::Start(0))?;
            zstd::zstd_safe::get_frame_content_size(&header_buf[..n])
                .ok()
                .flatten()
        }
        ImageFormat::Bzip2 | ImageFormat::Unknown => None,
    };

    Ok(ImageMetadata::new(
        path.map(Path::to_path_buf),
        file_size,
        uncompressed_size,
        format,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_magic_detection_all_formats() {
        assert_eq!(detect_format(XZ_MAGIC, None), ImageFormat::Xz);
        assert_eq!(detect_format(GZ_MAGIC, None), ImageFormat::Gz);
        assert_eq!(detect_format(ZSTD_MAGIC, None), ImageFormat::Zstd);
        assert_eq!(detect_format(BZIP2_MAGIC, None), ImageFormat::Bzip2);
        assert_eq!(detect_format(ZIP_LOCAL_MAGIC, None), ImageFormat::Zip);
        assert_eq!(detect_format(ZIP_EMPTY_MAGIC, None), ImageFormat::Zip);
        assert_eq!(detect_format(ZIP_SPANNED_MAGIC, None), ImageFormat::Zip);
    }

    #[test]
    fn test_iso_and_partition_magic() {
        let mut mbr = [0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        assert_eq!(detect_format(&mbr, None), ImageFormat::Raw);

        let mut gpt = [0u8; 520];
        gpt[512..520].copy_from_slice(b"EFI PART");
        assert_eq!(detect_format(&gpt, None), ImageFormat::Raw);

        let mut iso = vec![0u8; 32774];
        iso[32769..32774].copy_from_slice(b"CD001");
        assert_eq!(detect_format(&iso, None), ImageFormat::Raw);
    }

    #[test]
    fn test_inspect_raw_image() {
        let data = vec![0xAB; 8192];
        let mut cursor = Cursor::new(data);
        let meta = inspect_image(&mut cursor, ImageFormat::Raw, Some(Path::new("test.raw")))
            .expect("inspect raw failed");

        assert_eq!(meta.file_size, 8192);
        assert_eq!(meta.uncompressed_size, Some(8192));
        assert_eq!(meta.format, ImageFormat::Raw);
        assert_eq!(meta.path, Some(Path::new("test.raw").to_path_buf()));
    }
}
