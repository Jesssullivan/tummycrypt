//! tcfs-chunks: content-addressed chunking, BLAKE3 hashing, and seekable zstd compression
//!
//! # Overview
//! - `blake3`: deterministic file/slice hashing (content identity)
//! - `fastcdc`: content-defined chunking — stable boundaries even with inserts
//! - `seekable_zstd`: frame-based compression enabling random-access decompression
//! - `delta`: rsync rolling-hash delta sync (Phase 4 stub)

pub mod blake3;
pub mod delta;
pub mod fastcdc;
pub mod seekable_zstd;

// Convenience re-exports for the most common operations
pub use blake3::{hash_bytes, hash_file, hash_from_hex, hash_to_hex, Hash};
pub use fastcdc::{chunk_data, chunk_file, chunk_slice, Chunk, ChunkSizes};
pub use seekable_zstd::{compress, decompress_all, decompress_range, SeekEntry, SeekableBlob};
