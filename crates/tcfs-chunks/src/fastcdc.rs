//! FastCDC content-defined chunking
//!
//! Splits files into variable-size chunks whose boundaries are content-defined,
//! ensuring stable chunk boundaries even when data shifts (e.g. inserting bytes
//! near the start of a file doesn't invalidate all subsequent chunks).
//!
//! Chunk size targets:
//!   - Default (small files): min 2KB, avg 4KB, max 16KB
//!   - Pack files (.pack, .bin): min 32KB, avg 64KB, max 256KB
//!
//! Each chunk is content-addressed by its BLAKE3 hash.

use anyhow::Result;
use std::path::Path;

/// A single content-defined chunk
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Byte offset within the source file
    pub offset: u64,
    /// Chunk length in bytes
    pub length: usize,
    /// BLAKE3 hash of this chunk's data
    pub hash: crate::blake3::Hash,
}

/// Chunk size configuration
#[derive(Debug, Clone, Copy)]
pub struct ChunkSizes {
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

impl ChunkSizes {
    /// Default for most files (small-file optimized)
    pub const SMALL: ChunkSizes = ChunkSizes {
        min_size: 2 * 1024,  // 2KB
        avg_size: 4 * 1024,  // 4KB
        max_size: 16 * 1024, // 16KB
    };

    /// For pack/binary files (reduced overhead for large sequential data)
    pub const PACK: ChunkSizes = ChunkSizes {
        min_size: 32 * 1024,  // 32KB
        avg_size: 64 * 1024,  // 64KB
        max_size: 256 * 1024, // 256KB
    };

    /// Select chunk sizes based on file extension
    pub fn for_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("pack") | Some("bin") | Some("iso") | Some("img") => Self::PACK,
            _ => Self::SMALL,
        }
    }
}

/// Split `data` into content-defined chunks using FastCDC.
///
/// Returns a list of chunks. For empty data, returns an empty list.
/// The caller can then upload each chunk separately, deduplicating by hash.
pub fn chunk_data(data: &[u8], sizes: ChunkSizes) -> Vec<Chunk> {
    if data.is_empty() {
        return vec![];
    }

    let chunker =
        fastcdc::v2020::FastCDC::new(data, sizes.min_size, sizes.avg_size, sizes.max_size);

    chunker
        .map(|c| {
            let chunk_data = &data[c.offset..c.offset + c.length];
            Chunk {
                offset: c.offset as u64,
                length: c.length,
                hash: crate::blake3::hash_bytes(chunk_data),
            }
        })
        .collect()
}

/// Chunk a file from disk using auto-selected chunk sizes.
pub fn chunk_file(path: &Path) -> Result<(Vec<Chunk>, Vec<u8>)> {
    let data = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("reading file for chunking {}: {e}", path.display()))?;

    let sizes = ChunkSizes::for_path(path);
    let chunks = chunk_data(&data, sizes);
    Ok((chunks, data))
}

/// Chunk a byte slice with explicit sizes. Useful for testing.
pub fn chunk_slice(data: &[u8], sizes: ChunkSizes) -> Vec<Chunk> {
    chunk_data(data, sizes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn empty_data_yields_no_chunks() {
        let chunks = chunk_data(&[], ChunkSizes::SMALL);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_small_file_yields_chunks() {
        let data = vec![0xABu8; 64 * 1024]; // 64KB of repeated bytes
        let chunks = chunk_data(&data, ChunkSizes::SMALL);
        // Should produce multiple chunks
        assert!(!chunks.is_empty());

        // Chunks should cover the full file
        let total: usize = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, data.len());
    }

    #[test]
    fn chunk_offsets_are_contiguous() {
        let data: Vec<u8> = (0u8..=255).cycle().take(128 * 1024).collect();
        let chunks = chunk_data(&data, ChunkSizes::SMALL);

        let mut expected_offset = 0u64;
        for chunk in &chunks {
            assert_eq!(chunk.offset, expected_offset, "chunks must be contiguous");
            expected_offset += chunk.length as u64;
        }
        assert_eq!(expected_offset as usize, data.len());
    }

    proptest! {
        /// FastCDC boundary stability: same input → same chunk boundaries
        #[test]
        fn chunking_is_deterministic(data in proptest::collection::vec(any::<u8>(), 0..=32768)) {
            let c1 = chunk_data(&data, ChunkSizes::SMALL);
            let c2 = chunk_data(&data, ChunkSizes::SMALL);
            prop_assert_eq!(c1.len(), c2.len(), "chunk count must be deterministic");
            for (a, b) in c1.iter().zip(c2.iter()) {
                prop_assert_eq!(a.offset, b.offset);
                prop_assert_eq!(a.length, b.length);
                prop_assert_eq!(a.hash, b.hash, "chunk hash must be deterministic");
            }
        }

        /// Chunks must cover the full input without gaps or overlap
        #[test]
        fn chunks_cover_full_input(data in proptest::collection::vec(any::<u8>(), 1..=65536)) {
            let chunks = chunk_data(&data, ChunkSizes::SMALL);
            let total: usize = chunks.iter().map(|c| c.length).sum();
            prop_assert_eq!(total, data.len(), "chunks must cover full input");
        }
    }
}
