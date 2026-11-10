//! Direct block target abstraction and multi-target simultaneous flashing fan-out.

use crate::error::InscribeError;

/// Abstract hardware or virtual block storage destination.
///
/// Implementations include physical raw disks (`BlockDeviceWriter` via `O_DIRECT` or Win32 volume handles),
/// test mocks, and multi-drive fan-out containers.
///
/// This trait is strictly object-safe to enable `Box<dyn BlockTarget>` and `Vec<Box<dyn BlockTarget>>`.
pub trait BlockTarget: Send {
    /// Return the unique identifier or system device path for this target.
    fn id(&self) -> &str;

    /// Return total physical storage capacity in bytes.
    fn capacity(&self) -> Result<u64, InscribeError>;

    /// Return required physical sector/block alignment in bytes (e.g. 512 or 4096).
    fn block_size(&self) -> u32;

    /// Write a chunk of data at the specified byte offset.
    /// In production direct I/O mode, `offset` and `data.len()` must align with `block_size()`.
    fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError>;

    /// Synchronize and flush all outstanding written blocks to persistent media.
    fn sync(&mut self) -> Result<(), InscribeError>;

    /// Read back a chunk of data at the specified byte offset for verification.
    fn read_block(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), InscribeError>;
}

/// Fan-out container supporting simultaneous flashing across multiple physical block targets (`Vec<Box<dyn BlockTarget>>`).
pub struct MultiBlockTarget {
    targets: Vec<Box<dyn BlockTarget>>,
    composite_id: String,
}

impl MultiBlockTarget {
    /// Create a new multi-target fan-out container.
    #[must_use]
    pub fn new(targets: Vec<Box<dyn BlockTarget>>) -> Self {
        let composite_id = if targets.is_empty() {
            "empty-multi-target".to_string()
        } else {
            let ids: Vec<&str> = targets.iter().map(|t| t.id()).collect();
            ids.join(", ")
        };
        Self {
            targets,
            composite_id,
        }
    }

    /// Add an additional target to the fan-out collection.
    pub fn add_target(&mut self, target: Box<dyn BlockTarget>) {
        if self.targets.is_empty() {
            self.composite_id = target.id().to_string();
        } else {
            self.composite_id.push_str(", ");
            self.composite_id.push_str(target.id());
        }
        self.targets.push(target);
    }

    /// Get read access to the slice of targets.
    #[must_use]
    pub fn targets(&self) -> &[Box<dyn BlockTarget>] {
        &self.targets
    }

    /// Get mutable access to the slice of targets.
    pub fn targets_mut(&mut self) -> &mut [Box<dyn BlockTarget>] {
        &mut self.targets
    }

    /// Return the number of targets in this container.
    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// Check if the container has no registered targets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

impl BlockTarget for MultiBlockTarget {
    fn id(&self) -> &str {
        &self.composite_id
    }

    fn capacity(&self) -> Result<u64, InscribeError> {
        if self.targets.is_empty() {
            return Err(InscribeError::MultiTargetError(
                "Cannot compute capacity: No block targets attached to MultiBlockTarget"
                    .to_string(),
            ));
        }

        let mut min_capacity = u64::MAX;
        for target in &self.targets {
            let cap = target.capacity()?;
            if cap < min_capacity {
                min_capacity = cap;
            }
        }
        Ok(min_capacity)
    }

    fn block_size(&self) -> u32 {
        let mut max_block_size = 512;
        for target in &self.targets {
            let bs = target.block_size();
            if bs > max_block_size {
                max_block_size = bs;
            }
        }
        max_block_size
    }

    fn write_block(&mut self, offset: u64, data: &[u8]) -> Result<(), InscribeError> {
        if self.targets.is_empty() {
            return Err(InscribeError::MultiTargetError(
                "Cannot write block: No block targets configured in MultiBlockTarget".to_string(),
            ));
        }

        for target in &mut self.targets {
            target.write_block(offset, data)?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), InscribeError> {
        for target in &mut self.targets {
            target.sync()?;
        }
        Ok(())
    }

    fn read_block(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), InscribeError> {
        let first = self.targets.first_mut().ok_or_else(|| {
            InscribeError::MultiTargetError(
                "Cannot read block: No block targets configured in MultiBlockTarget".to_string(),
            )
        })?;
        first.read_block(offset, buffer)
    }
}
