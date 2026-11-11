//! 4096-byte aligned memory buffer for direct I/O and zero-copy block writes.

use crate::error::InscribeError;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Universal sector boundary alignment required for 4Kn direct I/O (4096 bytes).
pub const DEFAULT_ALIGNMENT: usize = 4096;

/// Default streaming buffer capacity (4MB).
pub const DEFAULT_BUFFER_SIZE: usize = 4 * 1024 * 1024;

/// Memory buffer allocated on a strict physical boundary (typically 4096 bytes).
///
/// Direct I/O (`O_DIRECT` on Linux, `FILE_FLAG_NO_BUFFERING` on Windows, and `F_NOCACHE` on macOS)
/// mandates that both memory pointer addresses and transfer lengths align with the underlying
/// storage sector size (512 or 4096 bytes). `AlignedBuffer` guarantees memory alignment and
/// initializes allocations cleanly with `0x00`.
#[derive(Debug)]
pub struct AlignedBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
    len: usize,
}

// Safety: AlignedBuffer uniquely owns its allocated heap memory and has no interior mutability.
unsafe impl Send for AlignedBuffer {}
unsafe impl Sync for AlignedBuffer {}

impl AlignedBuffer {
    /// Allocates a new 4MB buffer with 4096-byte alignment.
    ///
    /// The entire capacity is pre-initialized with zeros (`0x00`).
    pub fn new() -> Result<Self, InscribeError> {
        Self::with_capacity_and_alignment(DEFAULT_BUFFER_SIZE, DEFAULT_ALIGNMENT)
    }

    /// Allocates a buffer of the given capacity aligned to 4096 bytes.
    pub fn with_capacity(capacity: usize) -> Result<Self, InscribeError> {
        Self::with_capacity_and_alignment(capacity, DEFAULT_ALIGNMENT)
    }

    /// Allocates a buffer of the given capacity and custom alignment.
    ///
    /// # Errors
    /// Returns `InscribeError::Other` if alignment is not a power of two, or if memory allocation fails.
    pub fn with_capacity_and_alignment(
        capacity: usize,
        alignment: usize,
    ) -> Result<Self, InscribeError> {
        if capacity == 0 {
            return Err(InscribeError::Other(
                "Cannot allocate zero-capacity AlignedBuffer".to_string(),
            ));
        }

        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(InscribeError::Other(format!(
                "Invalid alignment {alignment}: must be a non-zero power of two"
            )));
        }

        // Layout requires size to be rounded up to multiple of align
        let layout_size = if capacity % alignment == 0 {
            capacity
        } else {
            capacity
                .checked_add(alignment - (capacity % alignment))
                .ok_or_else(|| {
                    InscribeError::Other(
                        "Buffer capacity overflow during alignment calculation".to_string(),
                    )
                })?
        };

        let layout = Layout::from_size_align(layout_size, alignment).map_err(|e| {
            InscribeError::Other(format!(
                "Invalid layout for capacity {layout_size} and alignment {alignment}: {e}"
            ))
        })?;

        // Safety: layout is verified valid and size > 0
        let raw_ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw_ptr).ok_or_else(|| {
            InscribeError::Other(format!(
                "Memory allocation failed for {layout_size} bytes aligned to {alignment}"
            ))
        })?;

        Ok(Self {
            ptr,
            layout,
            len: capacity,
        })
    }

    /// Creates an `AlignedBuffer` containing a copy of `data`, with partial trailing sectors padded to `alignment` with `0x00`.
    ///
    /// If `data.len()` is not an integer multiple of `alignment`, the resulting buffer
    /// is padded with zeroes up to the nearest multiple of `alignment`.
    pub fn from_slice_padded(data: &[u8], alignment: usize) -> Result<Self, InscribeError> {
        if data.is_empty() {
            return Err(InscribeError::Other(
                "Cannot create AlignedBuffer from empty slice".to_string(),
            ));
        }

        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(InscribeError::Other(format!(
                "Invalid alignment {alignment}: must be a non-zero power of two"
            )));
        }

        let rem = data.len() % alignment;
        let padded_len = if rem == 0 {
            data.len()
        } else {
            data.len().checked_add(alignment - rem).ok_or_else(|| {
                InscribeError::Other("Integer overflow during padding calculation".to_string())
            })?
        };

        let mut buf = Self::with_capacity_and_alignment(padded_len, alignment)?;
        buf.len = padded_len;

        // Copy source data into aligned buffer; trailing bytes remain 0x00 from alloc_zeroed
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.ptr.as_ptr(), data.len());
        }

        Ok(buf)
    }

    /// Checks if the allocated memory address is strictly aligned to the configured layout alignment.
    #[must_use]
    pub fn is_aligned(&self) -> bool {
        (self.ptr.as_ptr() as usize) % self.layout.align() == 0
    }

    /// Returns the raw const pointer to the start of the aligned buffer.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    /// Returns the raw mutable pointer to the start of the aligned buffer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Total allocated capacity in bytes.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.layout.size()
    }

    /// Current active logical length of the buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the buffer's logical length is zero.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Sets the active logical length of the buffer.
    ///
    /// # Errors
    /// Returns error if `new_len` exceeds allocated capacity.
    pub fn set_len(&mut self, new_len: usize) -> Result<(), InscribeError> {
        if new_len > self.layout.size() {
            return Err(InscribeError::Other(format!(
                "Requested length {new_len} exceeds buffer capacity {}",
                self.layout.size()
            )));
        }
        self.len = new_len;
        Ok(())
    }

    /// Pads the current logical length up to the nearest multiple of `alignment` with `0x00`.
    pub fn pad_to_alignment(&mut self, alignment: usize) -> Result<(), InscribeError> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(InscribeError::Other(format!(
                "Invalid alignment {alignment}: must be a non-zero power of two"
            )));
        }

        let rem = self.len % alignment;
        if rem != 0 {
            let padded_len = self
                .len
                .checked_add(alignment - rem)
                .ok_or_else(|| InscribeError::Other("Padding overflow".to_string()))?;

            if padded_len > self.layout.size() {
                return Err(InscribeError::Other(format!(
                    "Padded length {padded_len} exceeds buffer capacity {}",
                    self.layout.size()
                )));
            }

            // Zero out trailing sector bytes
            unsafe {
                std::ptr::write_bytes(self.ptr.as_ptr().add(self.len), 0x00, padded_len - self.len);
            }
            self.len = padded_len;
        }
        Ok(())
    }

    /// Returns immutable slice of active buffer content.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // Safety: ptr is valid, aligned, non-null, and len <= layout.size()
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// Returns mutable slice of active buffer content.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // Safety: ptr is valid, aligned, non-null, and len <= layout.size()
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Deref for AlignedBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        // Safety: ptr was allocated with self.layout and is non-null
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aligned_buffer_allocation_and_zero_init() {
        let buf = AlignedBuffer::new().expect("Failed to allocate default buffer");
        assert_eq!(buf.len(), DEFAULT_BUFFER_SIZE);
        assert_eq!(buf.capacity(), DEFAULT_BUFFER_SIZE);
        assert!(buf.is_aligned());
        assert_eq!(buf.as_ptr() as usize % DEFAULT_ALIGNMENT, 0);
        assert_eq!(buf[0], 0x00);
        assert_eq!(buf[DEFAULT_BUFFER_SIZE - 1], 0x00);
    }

    #[test]
    fn test_aligned_buffer_deref_mut_and_slice() {
        let mut buf = AlignedBuffer::with_capacity(4096).expect("Allocation failed");
        buf[0] = 0xAA;
        buf[1] = 0xBB;
        assert_eq!(&buf[0..2], &[0xAA, 0xBB]);
        assert_eq!(buf[2], 0x00);
    }

    #[test]
    fn test_aligned_buffer_pad_to_alignment() {
        let mut buf = AlignedBuffer::with_capacity(8192).expect("Allocation failed");
        buf.set_len(100).expect("set_len failed");
        buf[0] = 0x55;
        buf.pad_to_alignment(4096).expect("pad failed");
        assert_eq!(buf.len(), 4096);
        assert_eq!(buf[0], 0x55);
        assert_eq!(buf[100], 0x00);
        assert_eq!(buf[4095], 0x00);
    }
}
