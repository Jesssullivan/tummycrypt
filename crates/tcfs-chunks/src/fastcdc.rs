//! FastCDC content-defined chunking
//!
//! Splits files into variable-size chunks whose boundaries are content-defined,
//! ensuring stable chunk boundaries even when data shifts (e.g. inserting bytes
//! near the start of a file doesn't invalidate all subsequent chunks).
//!
//! Chunk size targets:
//!   - Default (small files): min 2KB, avg 4KB, max 16KB
//!   - Generic index/binary files (.idx, .bin, .git/index): min 32KB,
//!     avg 64KB, max 256KB
//!   - Large sequential files (.pack, .rev, .git/objects/pack/*.idx,
//!     .git/objects/pack/tmp_pack_*, .iso, .img): min 1MB, avg 4MB, max 16MB
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

    /// For generic index/binary files (reduced overhead without fully giving
    /// up index-level dedupe granularity)
    pub const PACK: ChunkSizes = ChunkSizes {
        min_size: 32 * 1024,  // 32KB
        avg_size: 64 * 1024,  // 64KB
        max_size: 256 * 1024, // 256KB
    };

    /// For large sequential artifacts where remote object count dominates.
    ///
    /// Git `.pack` files are already Git-internal compressed packfiles. The
    /// adjacent `.rev` and `.idx` files are pack-derived index data. Git also
    /// leaves extensionless `tmp_pack_*` files in `.git/objects/pack/` during
    /// some partial/shallow clone workflows. Project-tree canaries showed that
    /// 64KB average chunks turn raw Git pack/index/temp-pack files into
    /// thousands of remote objects, which is too many for the S3/SeaweedFS
    /// posture we want to prove. This profile keeps content-defined boundaries
    /// while making large raw-Git trees operationally tractable.
    pub const LARGE_SEQUENTIAL: ChunkSizes = ChunkSizes {
        min_size: 1024 * 1024,      // 1MB
        avg_size: 4 * 1024 * 1024,  // 4MB
        max_size: 16 * 1024 * 1024, // 16MB
    };

    /// Select chunk sizes based on file extension
    pub fn for_path(path: &Path) -> Self {
        if is_git_index(path) {
            return Self::PACK;
        }

        if is_git_temp_pack(path) {
            return Self::LARGE_SEQUENTIAL;
        }

        match path.extension().and_then(|e| e.to_str()) {
            Some("idx") if is_git_pack_dir(path) => Self::LARGE_SEQUENTIAL,
            Some("pack") | Some("rev") | Some("iso") | Some("img") => Self::LARGE_SEQUENTIAL,
            Some("idx") | Some("bin") => Self::PACK,
            _ => Self::SMALL,
        }
    }
}

fn is_git_pack_dir(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();

    parts
        .windows(3)
        .any(|window| matches!(window, [".git", "objects", "pack"]))
}

fn is_git_index(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();

    parts.as_slice().ends_with(&[".git", "index"])
}

fn is_git_temp_pack(path: &Path) -> bool {
    is_git_pack_dir(path)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("tmp_pack_"))
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

/// A chunk carrying its own data — used by the streaming chunker.
///
/// Unlike `Chunk` (which references a shared buffer by offset), each
/// `ChunkWithData` owns its bytes so the full file never needs to be
/// in memory at once.
#[derive(Debug, Clone)]
pub struct ChunkWithData {
    /// Byte offset within the source file
    pub offset: u64,
    /// Owned chunk bytes
    pub data: Vec<u8>,
    /// BLAKE3 hash of this chunk's data
    pub hash: crate::blake3::Hash,
}

/// Chunk a large file using the streaming interface (bounded memory).
///
/// Uses `fastcdc::v2020::StreamCDC<File>` which reads the file in
/// chunks via `Read`, so peak memory is bounded to ~max_size instead
/// of the file size.
///
/// Returns chunks with owned data — the caller uploads each chunk
/// individually without needing the full file in memory.
pub fn chunk_file_streaming(path: &Path) -> Result<Vec<ChunkWithData>> {
    let file = std::fs::File::open(path).map_err(|e| {
        anyhow::anyhow!("opening file for streaming chunk: {}: {e}", path.display())
    })?;

    let file_len = file
        .metadata()
        .map_err(|e| anyhow::anyhow!("stat for streaming chunk: {}: {e}", path.display()))?
        .len();

    if file_len == 0 {
        return Ok(vec![]);
    }

    let sizes = ChunkSizes::for_path(path);

    let chunker =
        fastcdc::v2020::StreamCDC::new(file, sizes.min_size, sizes.avg_size, sizes.max_size);

    let mut chunks = Vec::new();

    for result in chunker {
        let entry = result.map_err(|e| anyhow::anyhow!("streaming chunk error: {e}"))?;
        let hash = crate::blake3::hash_bytes(&entry.data);
        chunks.push(ChunkWithData {
            offset: entry.offset,
            data: entry.data,
            hash,
        });
    }

    Ok(chunks)
}

/// Chunk a large file using the streaming interface, keeping only chunk
/// metadata and the whole-file hash.
pub fn chunk_file_streaming_metadata(path: &Path) -> Result<(Vec<Chunk>, crate::blake3::Hash)> {
    let file = std::fs::File::open(path).map_err(|e| {
        anyhow::anyhow!(
            "opening file for streaming chunk metadata: {}: {e}",
            path.display()
        )
    })?;

    let file_len = file
        .metadata()
        .map_err(|e| anyhow::anyhow!("stat for streaming chunk metadata: {}: {e}", path.display()))?
        .len();

    if file_len == 0 {
        return Ok((vec![], crate::blake3::hash_bytes(&[])));
    }

    let sizes = ChunkSizes::for_path(path);

    let chunker =
        fastcdc::v2020::StreamCDC::new(file, sizes.min_size, sizes.avg_size, sizes.max_size);

    let mut chunks = Vec::new();
    let mut file_hasher = blake3::Hasher::new();

    for result in chunker {
        let entry = result.map_err(|e| anyhow::anyhow!("streaming chunk metadata error: {e}"))?;
        let hash = crate::blake3::hash_bytes(&entry.data);
        file_hasher.update(&entry.data);
        chunks.push(Chunk {
            offset: entry.offset,
            length: entry.data.len(),
            hash,
        });
    }

    Ok((chunks, file_hasher.finalize()))
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

    #[test]
    fn streaming_chunker_matches_in_memory() {
        // Write a 256KB file and verify streaming produces identical boundaries
        let data: Vec<u8> = (0u64..262144)
            .map(|i| (i.wrapping_mul(7) ^ (i >> 3)) as u8)
            .collect();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &data).unwrap();

        // In-memory chunking
        let (mem_chunks, _) = chunk_file(tmp.path()).unwrap();

        // Streaming chunking
        let stream_chunks = chunk_file_streaming(tmp.path()).unwrap();

        assert_eq!(
            mem_chunks.len(),
            stream_chunks.len(),
            "chunk count must match between in-memory and streaming"
        );

        for (i, (mem, stream)) in mem_chunks.iter().zip(stream_chunks.iter()).enumerate() {
            assert_eq!(mem.offset, stream.offset, "chunk {i} offset mismatch");
            assert_eq!(mem.length, stream.data.len(), "chunk {i} length mismatch");
            assert_eq!(mem.hash, stream.hash, "chunk {i} hash mismatch");
        }
    }

    #[test]
    fn streaming_metadata_matches_streaming_chunks() {
        let data: Vec<u8> = (0u64..524288)
            .map(|i| (i.wrapping_mul(17) ^ (i >> 9)) as u8)
            .collect();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &data).unwrap();

        let stream_chunks = chunk_file_streaming(tmp.path()).unwrap();
        let (metadata_chunks, file_hash) = chunk_file_streaming_metadata(tmp.path()).unwrap();

        assert_eq!(
            crate::blake3::hash_bytes(&data),
            file_hash,
            "metadata pass should preserve the whole-file hash"
        );
        assert_eq!(metadata_chunks.len(), stream_chunks.len());

        for (i, (metadata, stream)) in metadata_chunks.iter().zip(stream_chunks.iter()).enumerate()
        {
            assert_eq!(metadata.offset, stream.offset, "chunk {i} offset mismatch");
            assert_eq!(
                metadata.length,
                stream.data.len(),
                "chunk {i} length mismatch"
            );
            assert_eq!(metadata.hash, stream.hash, "chunk {i} hash mismatch");
        }
    }

    #[test]
    fn git_pack_index_uses_large_sequential_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new(".git/objects/pack/pack-example.idx"));

        assert_eq!(sizes.min_size, ChunkSizes::LARGE_SEQUENTIAL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::LARGE_SEQUENTIAL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::LARGE_SEQUENTIAL.max_size);
    }

    #[test]
    fn generic_index_uses_pack_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new("db/search.idx"));

        assert_eq!(sizes.min_size, ChunkSizes::PACK.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::PACK.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::PACK.max_size);
    }

    #[test]
    fn git_index_uses_pack_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new(".git/index"));

        assert_eq!(sizes.min_size, ChunkSizes::PACK.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::PACK.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::PACK.max_size);
    }

    #[test]
    fn generic_extensionless_index_uses_small_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new("index"));

        assert_eq!(sizes.min_size, ChunkSizes::SMALL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::SMALL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::SMALL.max_size);
    }

    #[test]
    fn git_index_descendant_uses_small_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new(".git/index/not-a-real-index-file"));

        assert_eq!(sizes.min_size, ChunkSizes::SMALL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::SMALL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::SMALL.max_size);
    }

    #[test]
    fn git_pack_uses_large_sequential_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new(".git/objects/pack/pack-example.pack"));

        assert_eq!(sizes.min_size, ChunkSizes::LARGE_SEQUENTIAL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::LARGE_SEQUENTIAL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::LARGE_SEQUENTIAL.max_size);
    }

    #[test]
    fn git_pack_reverse_index_uses_large_sequential_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new(".git/objects/pack/pack-example.rev"));

        assert_eq!(sizes.min_size, ChunkSizes::LARGE_SEQUENTIAL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::LARGE_SEQUENTIAL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::LARGE_SEQUENTIAL.max_size);
    }

    #[test]
    fn git_temp_pack_uses_large_sequential_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new(".git/objects/pack/tmp_pack_DGh0Fb"));

        assert_eq!(sizes.min_size, ChunkSizes::LARGE_SEQUENTIAL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::LARGE_SEQUENTIAL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::LARGE_SEQUENTIAL.max_size);
    }

    #[test]
    fn generic_temp_pack_uses_small_chunk_profile() {
        let sizes = ChunkSizes::for_path(Path::new("tmp_pack_DGh0Fb"));

        assert_eq!(sizes.min_size, ChunkSizes::SMALL.min_size);
        assert_eq!(sizes.avg_size, ChunkSizes::SMALL.avg_size);
        assert_eq!(sizes.max_size, ChunkSizes::SMALL.max_size);
    }

    #[test]
    fn git_pack_index_large_profile_avoids_remote_object_explosion() {
        let data: Vec<u8> = (0u64..(64 * 1024 * 1024))
            .map(|i| (i.wrapping_mul(31) ^ (i >> 7) ^ (i >> 17)) as u8)
            .collect();

        let old_idx_chunks = chunk_data(&data, ChunkSizes::PACK).len();
        let idx_chunks = chunk_data(
            &data,
            ChunkSizes::for_path(Path::new(".git/objects/pack/pack-example.idx")),
        )
        .len();

        assert!(
            idx_chunks * 16 < old_idx_chunks,
            "large git index profile should materially reduce object count: old_idx={old_idx_chunks} idx={idx_chunks}"
        );
    }

    #[test]
    fn git_temp_pack_large_profile_avoids_remote_object_explosion() {
        let data: Vec<u8> = (0u64..(64 * 1024 * 1024))
            .map(|i| (i.wrapping_mul(37) ^ i.rotate_left(9) ^ (i >> 13) ^ 0x5A5A5A5A) as u8)
            .collect();

        let old_temp_pack_chunks = chunk_data(&data, ChunkSizes::SMALL).len();
        let temp_pack_chunks = chunk_data(
            &data,
            ChunkSizes::for_path(Path::new(".git/objects/pack/tmp_pack_DGh0Fb")),
        )
        .len();

        assert!(
            temp_pack_chunks * 32 < old_temp_pack_chunks,
            "large git temp-pack profile should materially reduce object count: old_temp_pack={old_temp_pack_chunks} temp_pack={temp_pack_chunks}"
        );
    }

    #[test]
    fn git_index_pack_profile_avoids_remote_object_explosion() {
        let data: Vec<u8> = (0u64..(16 * 1024 * 1024))
            .map(|i| (i.wrapping_mul(41) ^ i.rotate_left(5) ^ (i >> 11) ^ 0x3C3C3C3C) as u8)
            .collect();

        let old_git_index_chunks = chunk_data(&data, ChunkSizes::SMALL).len();
        let git_index_chunks =
            chunk_data(&data, ChunkSizes::for_path(Path::new(".git/index"))).len();

        assert!(
            git_index_chunks * 8 < old_git_index_chunks,
            "git index pack profile should materially reduce object count: old_git_index={old_git_index_chunks} git_index={git_index_chunks}"
        );
    }

    #[test]
    fn git_pack_large_profile_avoids_remote_object_explosion() {
        let data: Vec<u8> = (0u64..(64 * 1024 * 1024))
            .map(|i| (i.wrapping_mul(31) ^ i.rotate_left(7) ^ (i >> 19) ^ 0xA5A5A5A5) as u8)
            .collect();

        let old_pack_chunks = chunk_data(&data, ChunkSizes::PACK).len();
        let large_chunks = chunk_data(
            &data,
            ChunkSizes::for_path(Path::new(".git/objects/pack/pack-example.pack")),
        )
        .len();

        assert!(
            large_chunks * 16 < old_pack_chunks,
            "large pack profile should materially reduce object count: pack={old_pack_chunks} large={large_chunks}"
        );
    }

    #[test]
    fn git_pack_reverse_index_large_profile_avoids_remote_object_explosion() {
        let data: Vec<u8> = (0u64..(32 * 1024 * 1024))
            .map(|i| (i.wrapping_mul(29) ^ i.rotate_left(11) ^ (i >> 13) ^ 0x5A5A5A5A) as u8)
            .collect();

        let small_chunks = chunk_data(&data, ChunkSizes::SMALL).len();
        let rev_chunks = chunk_data(
            &data,
            ChunkSizes::for_path(Path::new(".git/objects/pack/pack-example.rev")),
        )
        .len();

        assert!(
            rev_chunks * 64 < small_chunks,
            "large reverse-index profile should materially reduce object count: small={small_chunks} rev={rev_chunks}"
        );
    }

    #[test]
    fn streaming_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), []).unwrap();

        let chunks = chunk_file_streaming(tmp.path()).unwrap();
        assert!(chunks.is_empty(), "empty file should yield 0 chunks");
    }

    #[test]
    fn streaming_chunks_cover_full_file() {
        let data: Vec<u8> = (0u64..524288)
            .map(|i| (i.wrapping_mul(13) ^ (i >> 5)) as u8)
            .collect();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &data).unwrap();

        let chunks = chunk_file_streaming(tmp.path()).unwrap();
        let total: usize = chunks.iter().map(|c| c.data.len()).sum();
        assert_eq!(total, data.len(), "streaming chunks must cover full file");
    }
}
