//! Direct block device writer implementing `BlockTarget` with cross-platform direct I/O.

use crate::drive::DriveInfo;
use crate::drive::guard::{DriveGuard, is_system_mount};
use crate::error::InscribeError;
use crate::target::BlockTarget;
use crate::writer::aligned::AlignedBuffer;
use std::fs::File;
use std::path::Path;

/// macOS disk ioctl request code to query logical block size in bytes.
/// Defined in `<sys/disk.h>`: `_IOR('d', 24, uint32_t)`
pub const MACOS_DKIOCGETBLOCKSIZE: u64 = 0x4004_6418;

/// macOS disk ioctl request code to query total block count.
/// Defined in `<sys/disk.h>`: `_IOR('d', 25, uint64_t)`
pub const MACOS_DKIOCGETBLOCKCOUNT: u64 = 0x4008_6419;

/// Computes block device capacity in bytes from sector block size and count with overflow safety.
#[must_use]
pub fn compute_macos_block_capacity(block_size: u32, block_count: u64) -> Option<u64> {
    if block_size == 0 || block_count == 0 {
        return None;
    }
    block_count.checked_mul(block_size as u64)
}

/// Configuration options for `BlockDeviceWriter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterOptions {
    /// Enable unbuffered direct I/O (`O_DIRECT` on Linux, `FILE_FLAG_NO_BUFFERING` on Windows, `F_NOCACHE` on macOS).
    pub direct_io: bool,
    /// Physical sector/block size boundary in bytes (typically 4096 for 4Kn/Advanced Format or 512).
    pub block_size: u32,
    /// Open target in read-only mode (for verification passes).
    pub read_only: bool,
    /// Optional explicit capacity override in bytes.
    pub capacity: Option<u64>,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            direct_io: true,
            block_size: 4096,
            read_only: false,
            capacity: None,
        }
    }
}

impl WriterOptions {
    /// Creates a configuration with direct I/O enabled and 4096-byte alignment.
    #[must_use]
    pub fn direct() -> Self {
        Self::default()
    }

    /// Creates a standard buffered configuration (useful for regular test files).
    #[must_use]
    pub fn buffered() -> Self {
        Self {
            direct_io: false,
            ..Self::default()
        }
    }
}

/// Hardware or virtual block device writer implementing `BlockTarget`.
///
/// Features:
/// - Direct unbuffered I/O: Linux `O_DIRECT`, macOS `F_NOCACHE`, Windows `FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH`.
/// - Automatic trailing sector padding: Trailing bytes that do not align with physical sector size (4096B)
///   are padded with `0x00` using `AlignedBuffer` to avoid `EINVAL`.
/// - Strict Drive Guard enforcement preventing accidental target writes to system partitions or internal disks.
#[derive(Debug)]
pub struct BlockDeviceWriter {
    id: String,
    file: File,
    options: WriterOptions,
    total_bytes_written: u64,
}

impl BlockDeviceWriter {
    /// Opens a block storage device for a validated `DriveInfo`.
    ///
    /// Strictly validates the target with `DriveGuard` before acquiring file handles.
    ///
    /// # Errors
    /// Returns `InscribeError::DriveGuardViolation` if the target is an internal or system disk.
    pub fn open_drive(
        drive: &DriveInfo,
        options: &WriterOptions,
        allow_external_usb_ssd: bool,
    ) -> Result<Self, InscribeError> {
        DriveGuard::validate_target_with_options(drive, allow_external_usb_ssd)?;
        Self::open_path(&drive.path, options)
    }

    /// Opens a target block device or image file by path.
    ///
    /// Rejects root and known critical system paths.
    pub fn open_path(
        path: impl AsRef<Path>,
        options: &WriterOptions,
    ) -> Result<Self, InscribeError> {
        let p = path.as_ref();

        // Enforce Drive Guard safety check on critical system root mount paths
        if is_system_mount(p) {
            return Err(InscribeError::drive_guard_violation(
                p,
                format!("Target path '{}' is a protected system mount", p.display()),
            ));
        }

        let file = Self::open_file_with_options(p, options)?;
        let id = p.to_string_lossy().into_owned();

        Ok(Self {
            id,
            file,
            options: options.clone(),
            total_bytes_written: 0,
        })
    }

    /// Creates a `BlockDeviceWriter` from an already opened `File`.
    pub fn from_file(
        file: File,
        id: impl Into<String>,
        options: WriterOptions,
    ) -> Result<Self, InscribeError> {
        Ok(Self {
            id: id.into(),
            file,
            options,
            total_bytes_written: 0,
        })
    }

    /// Total number of bytes written to this target.
    #[must_use]
    pub fn total_bytes_written(&self) -> u64 {
        self.total_bytes_written
    }

    /// Returns the configured options.
    #[must_use]
    pub fn options(&self) -> &WriterOptions {
        &self.options
    }

    /// Returns a reference to the underlying `File`.
    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Returns a mutable reference to the underlying `File`.
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Consumes the writer and returns the underlying `File`.
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }

    #[cfg(target_os = "linux")]
    fn open_file_with_options(path: &Path, options: &WriterOptions) -> Result<File, InscribeError> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.read(true).write(!options.read_only);

        if options.direct_io {
            open_opts.custom_flags(libc::O_DIRECT);
        }

        open_opts.open(path).map_err(InscribeError::Io)
    }

    #[cfg(target_os = "macos")]
    fn open_file_with_options(path: &Path, options: &WriterOptions) -> Result<File, InscribeError> {
        use std::os::unix::io::AsRawFd;
        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.read(true).write(!options.read_only);

        let file = open_opts.open(path).map_err(InscribeError::Io)?;

        if options.direct_io {
            let fd = file.as_raw_fd();
            let ret = unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) };
            if ret == -1 {
                let err = std::io::Error::last_os_error();
                tracing::warn!("Failed to set F_NOCACHE on {}: {err}", path.display());
            }
        }

        Ok(file)
    }

    #[cfg(target_os = "windows")]
    fn open_file_with_options(path: &Path, options: &WriterOptions) -> Result<File, InscribeError> {
        use std::os::windows::fs::OpenOptionsExt;
        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.read(true).write(!options.read_only);

        if options.direct_io {
            const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
            const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
            open_opts.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
        }

        open_opts.open(path).map_err(InscribeError::Io)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn open_file_with_options(path: &Path, options: &WriterOptions) -> Result<File, InscribeError> {
        let mut open_opts = std::fs::OpenOptions::new();
        open_opts.read(true).write(!options.read_only);
        open_opts.open(path).map_err(InscribeError::Io)
    }
}

impl BlockTarget for BlockDeviceWriter {
    fn id(&self) -> &str {
        &self.id
    }

    fn capacity(&self) -> Result<u64, InscribeError> {
        if let Some(cap) = self.options.capacity {
            return Ok(cap);
        }

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            // Linux ioctl: _IOR(0x12, 114, size_t) -> BLKGETSIZE64
            const BLKGETSIZE64: u64 = 0x8008_1272;
            let mut dev_size: u64 = 0;
            // Query physical device size in bytes for raw block devices
            let ret =
                unsafe { libc::ioctl(self.file.as_raw_fd(), BLKGETSIZE64 as _, &mut dev_size) };
            if ret == 0 && dev_size > 0 {
                return Ok(dev_size);
            }
        }

        #[cfg(target_os = "macos")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self.file.as_raw_fd();
            let mut block_size: u32 = 0;
            let mut block_count: u64 = 0;

            let size_ret =
                unsafe { libc::ioctl(fd, MACOS_DKIOCGETBLOCKSIZE as _, &mut block_size) };
            let count_ret =
                unsafe { libc::ioctl(fd, MACOS_DKIOCGETBLOCKCOUNT as _, &mut block_count) };

            if size_ret == 0 && count_ret == 0 {
                if let Some(total) = compute_macos_block_capacity(block_size, block_count) {
                    return Ok(total);
                }
            }
        }

        let meta = self.file.metadata().map_err(InscribeError::Io)?;
        Ok(meta.len())
    }

    fn block_size(&self) -> u32 {
        self.options.block_size
    }

    fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError> {
        if data.is_empty() {
            return Ok(());
        }

        let block_size = self.options.block_size as usize;

        // In direct I/O mode, offset must be sector-aligned
        if self.options.direct_io && (offset % self.options.block_size as u64 != 0) {
            return Err(InscribeError::BlockWriteError {
                offset,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "Direct I/O write offset {offset} is not aligned to block size {block_size}"
                    ),
                ),
            });
        }

        let rem = data.len() % block_size;
        let is_aligned_memory = (data.as_ptr() as usize) % block_size == 0;

        // Determine if bounce buffer is required:
        // 1. Partial trailing sector requires 0x00 padding to 4096-byte boundary to avoid EINVAL
        // 2. Direct I/O requires aligned memory buffer address
        if rem != 0 || (self.options.direct_io && !is_aligned_memory) {
            let padded_buf = AlignedBuffer::from_slice_padded(data, block_size)?;
            self.write_raw(offset, &padded_buf)?;
            self.total_bytes_written += padded_buf.len() as u64;
        } else {
            self.write_raw(offset, data)?;
            self.total_bytes_written += data.len() as u64;
        }

        Ok(())
    }

    fn sync(&mut self) -> Result<(), InscribeError> {
        self.file.sync_all().map_err(InscribeError::Io)?;

        #[cfg(target_os = "linux")]
        if self.options.direct_io {
            use std::os::unix::io::AsRawFd;
            // Linux ioctl: _IO(0x12, 97) -> BLKFLSBUF
            const BLKFLSBUF: u64 = 0x1261;
            unsafe {
                libc::ioctl(self.file.as_raw_fd(), BLKFLSBUF as _);
            }
        }

        Ok(())
    }

    fn read_block(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), InscribeError> {
        if buffer.is_empty() {
            return Ok(());
        }

        let block_size = self.options.block_size as usize;
        let block_size_u64 = self.options.block_size as u64;

        let sector_start = (offset / block_size_u64) * block_size_u64;
        let intra_offset = (offset - sector_start) as usize;
        let is_aligned_memory = (buffer.as_ptr() as usize) % block_size == 0;
        let is_exact_blocks = intra_offset == 0 && (buffer.len() % block_size == 0);

        if is_exact_blocks && (!self.options.direct_io || is_aligned_memory) {
            self.read_raw(offset, buffer)?;
        } else {
            let total_needed = intra_offset
                .checked_add(buffer.len())
                .ok_or_else(|| InscribeError::Other("Read buffer length overflow".to_string()))?;
            let rem = total_needed % block_size;
            let padded_len = if rem == 0 {
                total_needed
            } else {
                total_needed.checked_add(block_size - rem).ok_or_else(|| {
                    InscribeError::Other("Read buffer length overflow".to_string())
                })?
            };

            let mut bounce = AlignedBuffer::with_capacity_and_alignment(padded_len, block_size)?;
            bounce.set_len(padded_len)?;

            let file_len = self.capacity().unwrap_or(u64::MAX);
            if sector_start >= file_len {
                return Err(InscribeError::BlockReadError {
                    offset,
                    source: std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Read offset is beyond storage capacity",
                    ),
                });
            }

            let max_readable = ((file_len - sector_start) as usize).min(padded_len);
            if max_readable < intra_offset + buffer.len() {
                return Err(InscribeError::BlockReadError {
                    offset,
                    source: std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Read range exceeds storage capacity",
                    ),
                });
            }

            let read_target_len = if self.options.direct_io {
                let rem_read = max_readable % block_size;
                if rem_read == 0 {
                    max_readable
                } else {
                    max_readable.saturating_sub(rem_read)
                }
            } else {
                max_readable
            };

            if read_target_len > 0 {
                self.read_raw(sector_start, &mut bounce[..read_target_len])?;
            }

            buffer.copy_from_slice(&bounce[intra_offset..intra_offset + buffer.len()]);
        }

        Ok(())
    }

    fn invalidate_cache(&mut self) -> Result<(), InscribeError> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self.file.as_raw_fd();
            unsafe {
                libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED);
                const BLKFLSBUF: u64 = 0x1261;
                libc::ioctl(fd, BLKFLSBUF as _);
            }
        }
        Ok(())
    }
}

impl BlockDeviceWriter {
    fn write_raw(&mut self, offset: u64, buf: &[u8]) -> Result<(), InscribeError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file
                .write_all_at(buf, offset)
                .map_err(|source| InscribeError::BlockWriteError { offset, source })?;
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut written = 0;
            while written < buf.len() {
                let n = self
                    .file
                    .seek_write(&buf[written..], offset + written as u64)
                    .map_err(|source| InscribeError::BlockWriteError {
                        offset: offset + written as u64,
                        source,
                    })?;
                if n == 0 {
                    return Err(InscribeError::BlockWriteError {
                        offset: offset + written as u64,
                        source: std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "Failed to write bytes to storage target",
                        ),
                    });
                }
                written += n;
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            use std::io::{Seek, SeekFrom, Write};
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|source| InscribeError::BlockWriteError { offset, source })?;
            self.file
                .write_all(buf)
                .map_err(|source| InscribeError::BlockWriteError { offset, source })?;
        }

        Ok(())
    }

    fn read_raw(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), InscribeError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file
                .read_exact_at(buf, offset)
                .map_err(|source| InscribeError::BlockReadError { offset, source })?;
        }

        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut read_bytes = 0;
            while read_bytes < buf.len() {
                let n = self
                    .file
                    .seek_read(&mut buf[read_bytes..], offset + read_bytes as u64)
                    .map_err(|source| InscribeError::BlockReadError {
                        offset: offset + read_bytes as u64,
                        source,
                    })?;
                if n == 0 {
                    return Err(InscribeError::BlockReadError {
                        offset: offset + read_bytes as u64,
                        source: std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "Unexpected EOF reading from storage target",
                        ),
                    });
                }
                read_bytes += n;
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            use std::io::{Read, Seek, SeekFrom};
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|source| InscribeError::BlockReadError { offset, source })?;
            self.file
                .read_exact(buf)
                .map_err(|source| InscribeError::BlockReadError { offset, source })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_writer_options_defaults() {
        let opts = WriterOptions::default();
        assert!(opts.direct_io);
        assert_eq!(opts.block_size, 4096);
        assert!(!opts.read_only);
    }
}
