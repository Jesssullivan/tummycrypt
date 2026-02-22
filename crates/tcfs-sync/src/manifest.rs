//! SyncManifest v2: JSON-encoded manifest with vector clock metadata.
//!
//! Replaces the v1 newline-separated text format. v1 manifests are
//! transparently migrated on read via `from_bytes()`.

use crate::conflict::VectorClock;
use serde::{Deserialize, Serialize};

/// A manifest describing a synced file's chunks and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncManifest {
    /// Manifest format version (2 for vclock-era)
    pub version: u32,
    /// BLAKE3 hash of the complete file content
    pub file_hash: String,
    /// File size in bytes
    pub file_size: u64,
    /// Ordered list of chunk BLAKE3 hashes
    pub chunks: Vec<String>,
    /// Vector clock at the time of writing
    pub vclock: VectorClock,
    /// Device ID that wrote this manifest
    pub written_by: String,
    /// Unix timestamp when this manifest was written
    pub written_at: u64,
    /// Relative path of the file (for cross-device lookup)
    pub rel_path: Option<String>,
}

impl SyncManifest {
    /// Parse manifest bytes, auto-detecting v1 (text) vs v2 (JSON).
    ///
    /// v1 format: newline-separated chunk hashes (no JSON)
    /// v2 format: JSON object with version field
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        let text = String::from_utf8(data.to_vec())
            .map_err(|e| anyhow::anyhow!("manifest is not UTF-8: {e}"))?;

        // Try JSON (v2) first
        if let Ok(manifest) = serde_json::from_str::<SyncManifest>(&text) {
            return Ok(manifest);
        }

        // Fall back to v1 text format: newline-separated chunk hashes
        let chunks: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();

        if chunks.is_empty() {
            anyhow::bail!("manifest is empty");
        }

        Ok(SyncManifest {
            version: 1,
            file_hash: String::new(),
            file_size: 0,
            chunks,
            vclock: VectorClock::new(),
            written_by: String::new(),
            written_at: 0,
            rel_path: None,
        })
    }

    /// Serialize manifest to v2 JSON bytes.
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| anyhow::anyhow!("serializing manifest: {e}"))
    }

    /// Extract the ordered chunk hashes (compatible with v1 consumer code).
    pub fn chunk_hashes(&self) -> &[String] {
        &self.chunks
    }

    /// Check if this is a v1 (legacy) manifest.
    pub fn is_legacy(&self) -> bool {
        self.version < 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_roundtrip() {
        let mut vc = VectorClock::new();
        vc.tick("yoga");

        let manifest = SyncManifest {
            version: 2,
            file_hash: "abc123".into(),
            file_size: 1024,
            chunks: vec!["chunk1".into(), "chunk2".into()],
            vclock: vc,
            written_by: "yoga".into(),
            written_at: 1000,
            rel_path: Some("docs/readme.md".into()),
        };

        let bytes = manifest.to_bytes().unwrap();
        let parsed = SyncManifest::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.file_hash, "abc123");
        assert_eq!(parsed.chunks.len(), 2);
        assert_eq!(parsed.vclock.get("yoga"), 1);
        assert_eq!(parsed.written_by, "yoga");
    }

    #[test]
    fn test_v1_migration() {
        let v1_content = "hash_aaa\nhash_bbb\nhash_ccc\n";
        let parsed = SyncManifest::from_bytes(v1_content.as_bytes()).unwrap();

        assert!(parsed.is_legacy());
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.chunks, vec!["hash_aaa", "hash_bbb", "hash_ccc"]);
        assert!(parsed.vclock.clocks.is_empty());
    }

    #[test]
    fn test_empty_manifest_fails() {
        let result = SyncManifest::from_bytes(b"");
        assert!(result.is_err());
    }

    #[test]
    fn test_v1_single_chunk() {
        let v1 = "single_hash\n";
        let parsed = SyncManifest::from_bytes(v1.as_bytes()).unwrap();
        assert_eq!(parsed.chunks, vec!["single_hash"]);
    }
}
