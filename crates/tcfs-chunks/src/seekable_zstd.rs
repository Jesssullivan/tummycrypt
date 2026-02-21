//! Seekable zstd compression using independent frames
//!
//! Standard zstd produces a single stream that must be decompressed from the
//! start to reach any offset. For large files we want random-access reads, so
//! we split the data into fixed-size frames and store a seek table.
//!
//! Frame format on disk:
//!   N compressed zstd frames (each <= frame_size uncompressed bytes), followed
//!   by a JSON-encoded seek table in chunk metadata (not appended to the blob).

use anyhow::{Context, Result};

/// Default frame size: 1MB uncompressed per frame
pub const DEFAULT_FRAME_SIZE: usize = 1024 * 1024;

/// Seek table entry for one frame
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SeekEntry {
    /// Uncompressed size of this frame
    pub uncompressed_size: u32,
    /// Compressed size (zstd frame bytes)
    pub compressed_size: u32,
    /// Byte offset of this frame in the compressed output
    pub compressed_offset: u64,
}

/// A seekable compressed blob: compressed bytes + seek table
#[derive(Debug)]
pub struct SeekableBlob {
    /// The concatenated compressed frames
    pub compressed: Vec<u8>,
    /// Seek table (one entry per frame, in order)
    pub seek_table: Vec<SeekEntry>,
}

impl SeekableBlob {
    /// Total uncompressed size
    pub fn uncompressed_size(&self) -> u64 {
        self.seek_table
            .iter()
            .map(|e| e.uncompressed_size as u64)
            .sum()
    }

    /// Number of frames
    pub fn frame_count(&self) -> usize {
        self.seek_table.len()
    }
}

/// Compress `data` into seekable frames of at most `frame_size` bytes each.
pub fn compress(data: &[u8], frame_size: usize, level: i32) -> Result<SeekableBlob> {
    let mut compressed = Vec::with_capacity(data.len() / 2 + 1024);
    let mut seek_table = Vec::new();

    for chunk in data.chunks(frame_size.max(1)) {
        let compressed_offset = compressed.len() as u64;
        let frame = zstd::encode_all(chunk, level).context("zstd compress frame")?;
        let entry = SeekEntry {
            uncompressed_size: chunk.len() as u32,
            compressed_size: frame.len() as u32,
            compressed_offset,
        };
        compressed.extend_from_slice(&frame);
        seek_table.push(entry);
    }

    Ok(SeekableBlob {
        compressed,
        seek_table,
    })
}

/// Decompress all frames back to the original data.
pub fn decompress_all(blob: &SeekableBlob) -> Result<Vec<u8>> {
    let total: usize = blob
        .seek_table
        .iter()
        .map(|e| e.uncompressed_size as usize)
        .sum();
    let mut out = Vec::with_capacity(total);

    for entry in &blob.seek_table {
        let start = entry.compressed_offset as usize;
        let end = start + entry.compressed_size as usize;
        let frame = &blob.compressed[start..end];
        let plain = zstd::decode_all(frame).context("zstd decompress frame")?;
        out.extend_from_slice(&plain);
    }

    Ok(out)
}

/// Decompress a specific byte range from the seekable blob.
///
/// `range_start` and `range_end` are offsets into the uncompressed data.
/// Only the frames overlapping the range are decompressed.
pub fn decompress_range(blob: &SeekableBlob, range_start: u64, range_end: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut frame_start: u64 = 0;

    for entry in &blob.seek_table {
        let frame_end = frame_start + entry.uncompressed_size as u64;

        if frame_end > range_start && frame_start < range_end {
            let cf_start = entry.compressed_offset as usize;
            let cf_end = cf_start + entry.compressed_size as usize;
            let plain = zstd::decode_all(&blob.compressed[cf_start..cf_end])
                .context("zstd decompress range frame")?;

            let local_start = (range_start.saturating_sub(frame_start)) as usize;
            let local_end = (range_end.min(frame_end) - frame_start) as usize;
            out.extend_from_slice(&plain[local_start..local_end]);
        }

        frame_start = frame_end;
        if frame_start >= range_end {
            break;
        }
    }

    Ok(out)
}

/// Serialize seek table to bytes (for embedding in chunk metadata)
pub fn serialize_seek_table(table: &[SeekEntry]) -> Result<Vec<u8>> {
    serde_json::to_vec(table).context("serializing seek table")
}

/// Deserialize seek table from bytes
pub fn deserialize_seek_table(data: &[u8]) -> Result<Vec<SeekEntry>> {
    serde_json::from_slice(data).context("deserializing seek table")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn round_trip_small() {
        let data = b"hello seekable zstd";
        let blob = compress(data, DEFAULT_FRAME_SIZE, 1).unwrap();
        let out = decompress_all(&blob).unwrap();
        assert_eq!(out.as_slice(), data.as_slice());
    }

    #[test]
    fn round_trip_multi_frame() {
        let data: Vec<u8> = (0u8..=255).cycle().take(4 * 1024 * 1024).collect();
        let blob = compress(&data, DEFAULT_FRAME_SIZE, 1).unwrap();
        assert!(blob.frame_count() >= 4);
        let out = decompress_all(&blob).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn range_decompress_spanning_frames() {
        let data: Vec<u8> = (0u8..=255).cycle().take(3 * 1024 * 1024).collect();
        let blob = compress(&data, 1024 * 1024, 1).unwrap();

        let range = decompress_range(&blob, 500_000, 1_500_000).unwrap();
        assert_eq!(range, &data[500_000..1_500_000]);
    }

    proptest! {
        #[test]
        fn compress_decompress_roundtrip(
            data in proptest::collection::vec(any::<u8>(), 0..=65536),
            frame_kb in 4u32..=64u32,
        ) {
            let frame_size = (frame_kb * 1024) as usize;
            let blob = compress(&data, frame_size, 1).unwrap();
            let out = decompress_all(&blob).unwrap();
            prop_assert_eq!(out, data, "round-trip must be identical");
        }
    }
}
