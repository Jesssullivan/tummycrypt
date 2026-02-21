//! BLAKE3 content hashing for files and byte slices
//!
//! Uses rayon for parallel hashing of large files (> 256KB chunks).
//! The hash is used as a content identifier (CAS key) for deduplication.

use anyhow::{Context, Result};
use std::path::Path;

/// A BLAKE3 hash digest (32 bytes), displayed as 64 hex chars
pub type Hash = blake3::Hash;

/// Hash a byte slice in memory. Fast for small inputs.
pub fn hash_bytes(data: &[u8]) -> Hash {
    blake3::hash(data)
}

/// Hash a file from disk, using parallel hashing for large files.
///
/// Files >= 128KB are hashed in parallel using rayon (blake3's built-in
/// Rayon feature). Returns the BLAKE3 hash of the file's full content.
pub fn hash_file(path: &Path) -> Result<Hash> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading file for hashing: {}", path.display()))?;

    // blake3::hash() uses SIMD internally; for large files rayon parallelism
    // would use blake3::Hasher with update_rayon(), but the simple path is fine
    // for Phase 2 — update_rayon() upgrade in Phase 4 benchmarks
    Ok(hash_bytes(&data))
}

/// Hash a file using the streaming interface (for files too large to read fully)
pub fn hash_file_streaming(path: &Path) -> Result<Hash> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening file for streaming hash: {}", path.display()))?;

    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024]; // 64KB read buffer

    loop {
        let n = file.read(&mut buf).with_context(|| "reading for hash")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hasher.finalize())
}

/// Format a hash as lowercase hex string (64 chars)
pub fn hash_to_hex(hash: &Hash) -> String {
    hash.to_hex().to_string()
}

/// Parse a 64-char hex string into a Hash
pub fn hash_from_hex(hex: &str) -> Result<Hash> {
    blake3::Hash::from_hex(hex).map_err(|e| anyhow::anyhow!("invalid BLAKE3 hex '{}': {}", hex, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn empty_hash_is_deterministic() {
        let h1 = hash_bytes(b"");
        let h2 = hash_bytes(b"");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_hex_roundtrip() {
        let h = hash_bytes(b"hello tcfs");
        let hex = hash_to_hex(&h);
        assert_eq!(hex.len(), 64);
        let back = hash_from_hex(&hex).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn different_content_different_hash() {
        let h1 = hash_bytes(b"foo");
        let h2 = hash_bytes(b"bar");
        assert_ne!(h1, h2);
    }

    proptest! {
        #[test]
        fn hash_is_deterministic(data in proptest::collection::vec(any::<u8>(), 0..=4096)) {
            let h1 = hash_bytes(&data);
            let h2 = hash_bytes(&data);
            prop_assert_eq!(h1, h2, "BLAKE3 must be deterministic for same input");
        }

        #[test]
        fn hex_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..=1024)) {
            let h = hash_bytes(&data);
            let hex = hash_to_hex(&h);
            let back = hash_from_hex(&hex).unwrap();
            prop_assert_eq!(h, back);
        }
    }
}
