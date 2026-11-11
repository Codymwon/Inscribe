//! In-flight BLAKE3 and SHA-256 streaming verification engine.
//!
//! Provides cryptographic verification for both Pass 1 (in-flight stream hashing)
//! and Pass 2 (cache-invalidated read-back verification) with chunk-level discrepancy tracking.

use crate::error::InscribeError;
use sha2::{Digest, Sha256};

/// Computed cryptographic hashes from Pass 1 streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalHashes {
    /// Overall BLAKE3 hash formatted as 64-character lowercase hex.
    pub blake3: String,
    /// Optional overall SHA-256 hash formatted as 64-character lowercase hex.
    pub sha256: Option<String>,
    /// Per-chunk BLAKE3 hashes for granular byte-offset corruption detection in Pass 2.
    pub chunk_hashes: Vec<blake3::Hash>,
}

/// In-flight cryptographic checksum generator for Pass 1.
pub struct ChecksumVerifier {
    blake3_hasher: blake3::Hasher,
    sha256_hasher: Option<Sha256>,
    chunk_hashes: Vec<blake3::Hash>,
}

impl ChecksumVerifier {
    /// Initialize a new checksum verifier.
    ///
    /// If `compute_sha256` is true, computes SHA-256 in-flight alongside BLAKE3.
    #[must_use]
    pub fn new(compute_sha256: bool) -> Self {
        Self {
            blake3_hasher: blake3::Hasher::new(),
            sha256_hasher: if compute_sha256 {
                Some(Sha256::new())
            } else {
                None
            },
            chunk_hashes: Vec::new(),
        }
    }

    /// Ingest a written chunk of data, updating the in-flight hashes and chunk log.
    pub fn update(&mut self, chunk: &[u8]) {
        self.blake3_hasher.update(chunk);
        if let Some(sha) = &mut self.sha256_hasher {
            sha.update(chunk);
        }
        self.chunk_hashes.push(blake3::hash(chunk));
    }

    /// Finalize Pass 1 hashes and produce `FinalHashes`.
    #[must_use]
    pub fn finalize(self) -> FinalHashes {
        let b3_hash = self.blake3_hasher.finalize();
        let sha256_hex = self
            .sha256_hasher
            .map(|sha| format!("{:x}", sha.finalize()));

        FinalHashes {
            blake3: b3_hash.to_hex().to_string(),
            sha256: sha256_hex,
            chunk_hashes: self.chunk_hashes,
        }
    }
}

/// Read-back checksum verifier for Pass 2.
///
/// Validates read blocks against the Pass 1 baseline with instant discrepancy detection.
pub struct ReadVerifier {
    blake3_hasher: blake3::Hasher,
    expected_blake3: String,
    expected_chunk_hashes: Vec<blake3::Hash>,
    chunk_index: usize,
}

impl ReadVerifier {
    /// Create a new read-back verifier expecting the specified BLAKE3 hash and chunk hashes.
    #[must_use]
    pub fn new(expected_blake3: String, expected_chunk_hashes: Vec<blake3::Hash>) -> Self {
        Self {
            blake3_hasher: blake3::Hasher::new(),
            expected_blake3,
            expected_chunk_hashes,
            chunk_index: 0,
        }
    }

    /// Ingest a read chunk from target, verifying it against the expected chunk hash.
    ///
    /// # Errors
    /// Returns `InscribeError::VerificationFailed` if the chunk does not match Pass 1.
    pub fn update(&mut self, offset: u64, chunk: &[u8]) -> Result<(), InscribeError> {
        let actual_chunk_hash = blake3::hash(chunk);

        if self.chunk_index < self.expected_chunk_hashes.len() {
            let expected_chunk_hash = self.expected_chunk_hashes[self.chunk_index];
            if actual_chunk_hash != expected_chunk_hash {
                return Err(InscribeError::VerificationFailed {
                    offset,
                    expected: expected_chunk_hash.to_hex().to_string(),
                    actual: actual_chunk_hash.to_hex().to_string(),
                });
            }
        }

        self.blake3_hasher.update(chunk);
        self.chunk_index += 1;
        Ok(())
    }

    /// Finalize Pass 2 verification and ensure the overall BLAKE3 matches Pass 1.
    ///
    /// # Errors
    /// Returns `InscribeError::VerificationFailed` if the overall hash does not match.
    pub fn finalize(self) -> Result<String, InscribeError> {
        let actual_b3 = self.blake3_hasher.finalize().to_hex().to_string();
        if actual_b3 != self.expected_blake3 {
            return Err(InscribeError::VerificationFailed {
                offset: 0,
                expected: self.expected_blake3,
                actual: actual_b3,
            });
        }
        Ok(actual_b3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_verifier_pass1_and_pass2_success() {
        let chunk1 = b"First chunk of 4MB data";
        let chunk2 = b"Second chunk of 4MB data";

        let mut pass1 = ChecksumVerifier::new(true);
        pass1.update(chunk1);
        pass1.update(chunk2);
        let final_hashes = pass1.finalize();

        assert!(!final_hashes.blake3.is_empty());
        assert!(final_hashes.sha256.is_some());
        assert_eq!(final_hashes.chunk_hashes.len(), 2);

        // Pass 2 read-back verification
        let mut pass2 = ReadVerifier::new(
            final_hashes.blake3.clone(),
            final_hashes.chunk_hashes.clone(),
        );
        pass2.update(0, chunk1).expect("chunk 1 should pass");
        pass2
            .update(chunk1.len() as u64, chunk2)
            .expect("chunk 2 should pass");

        let verified_b3 = pass2.finalize().expect("finalize should pass");
        assert_eq!(verified_b3, final_hashes.blake3);
    }

    #[test]
    fn test_read_verifier_detects_corrupt_chunk_at_offset() {
        let chunk1 = b"Valid chunk 1";
        let chunk2 = b"Valid chunk 2";
        let corrupt_chunk2 = b"Corrupted chunk 2";

        let mut pass1 = ChecksumVerifier::new(false);
        pass1.update(chunk1);
        pass1.update(chunk2);
        let final_hashes = pass1.finalize();

        let mut pass2 = ReadVerifier::new(final_hashes.blake3, final_hashes.chunk_hashes);
        pass2.update(0, chunk1).expect("chunk 1 valid");

        let err = pass2
            .update(1000, corrupt_chunk2)
            .expect_err("corrupt chunk must fail");

        match err {
            InscribeError::VerificationFailed { offset, .. } => {
                assert_eq!(offset, 1000);
            }
            other => panic!("Expected VerificationFailed, got {:?}", other),
        }
    }
}
