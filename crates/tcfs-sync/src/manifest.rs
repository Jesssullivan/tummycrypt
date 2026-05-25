//! SyncManifest v2: JSON-encoded manifest with vector clock metadata.
//!
//! Replaces the v1 newline-separated text format. v1 manifests are
//! transparently migrated on read via `from_bytes()`.

use crate::conflict::VectorClock;
use crate::index_entry::RemoteEntryKind;
use serde::{Deserialize, Serialize};

/// One per-device FileKey wrap carried by a regular-file manifest.
///
/// This is additive for TIN-1417 Phase 1. Existing manifests continue to use
/// `encrypted_file_key`; upgraded writers can dual-write this field before the
/// fleet cuts over to per-device unwrap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedFileKey {
    /// Stable TCFS device identifier this wrap is intended for.
    pub recipient_device_id: String,
    /// Public age recipient used when producing this wrap.
    pub recipient: String,
    /// Cryptographic wrap algorithm, for example `age-x25519-v1`.
    pub algorithm: String,
    /// Wrapped FileKey payload.
    pub wrapped_key: String,
}

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
    /// Unix file mode (permissions) — preserved across sync on Unix systems.
    /// Backward-compatible: old manifests deserialize with `mode: None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// Base64-encoded wrapped file key (present only when E2E encryption is enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_file_key: Option<String>,
    /// Per-device wrapped FileKeys for manifest schema v3 migration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wrapped_file_keys: Vec<WrappedFileKey>,
}

/// A manifest for a POSIX symbolic link.
///
/// Symlinks intentionally use a separate v3 shape rather than pretending to be
/// zero-byte regular files. Older clients that do not understand this shape
/// fail to hydrate it instead of silently materializing the wrong file type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkManifest {
    /// Manifest format version (3 for symlink manifests)
    pub version: u32,
    /// Object kind discriminator.
    pub kind: RemoteEntryKind,
    /// Link target text exactly as returned by `readlink` for supported paths.
    pub symlink_target: String,
    /// Vector clock at the time of writing
    pub vclock: VectorClock,
    /// Device ID that wrote this manifest
    pub written_by: String,
    /// Unix timestamp when this manifest was written
    pub written_at: u64,
    /// Relative path of the symlink.
    pub rel_path: Option<String>,
}

impl SymlinkManifest {
    pub fn new(
        symlink_target: impl Into<String>,
        vclock: VectorClock,
        written_by: String,
        written_at: u64,
        rel_path: Option<String>,
    ) -> Self {
        Self {
            version: 3,
            kind: RemoteEntryKind::Symlink,
            symlink_target: symlink_target.into(),
            vclock,
            written_by,
            written_at,
            rel_path,
        }
    }

    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        let manifest: SymlinkManifest = serde_json::from_slice(data)
            .map_err(|e| anyhow::anyhow!("parsing symlink manifest: {e}"))?;
        if manifest.version != 3 {
            anyhow::bail!("unsupported symlink manifest version: {}", manifest.version);
        }
        if manifest.kind != RemoteEntryKind::Symlink {
            anyhow::bail!("manifest is not a symlink");
        }
        Ok(manifest)
    }

    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| anyhow::anyhow!("serializing symlink manifest: {e}"))
    }
}

impl SyncManifest {
    /// Parse manifest bytes, auto-detecting v1 (text) vs v2 (JSON).
    ///
    /// v1 format: newline-separated chunk hashes (no JSON)
    /// v2 format: JSON object with version field
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        let text = String::from_utf8(data.to_vec())
            .map_err(|e| anyhow::anyhow!("manifest is not UTF-8: {e}"))?;

        // Try JSON (v2) first. JSON-shaped manifests that do not match the
        // regular-file schema must fail closed instead of being treated as v1
        // newline manifests.
        if text.trim_start().starts_with('{') {
            return serde_json::from_str::<SyncManifest>(&text)
                .map_err(|e| anyhow::anyhow!("parsing regular-file manifest JSON: {e}"));
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
            mode: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        })
    }

    /// Serialize manifest to v2 JSON bytes.
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|e| anyhow::anyhow!("serializing manifest: {e}"))
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
            mode: Some(0o644),
            encrypted_file_key: None,
            wrapped_file_keys: vec![WrappedFileKey {
                recipient_device_id: "device-a".into(),
                recipient: "age1recipient".into(),
                algorithm: "age-x25519-v1".into(),
                wrapped_key: "AGE-ENCRYPTED-PAYLOAD".into(),
            }],
        };

        let bytes = manifest.to_bytes().unwrap();
        let parsed = SyncManifest::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.file_hash, "abc123");
        assert_eq!(parsed.chunks.len(), 2);
        assert_eq!(parsed.vclock.get("yoga"), 1);
        assert_eq!(parsed.written_by, "yoga");
        assert_eq!(parsed.wrapped_file_keys.len(), 1);
        assert_eq!(parsed.wrapped_file_keys[0].recipient_device_id, "device-a");
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
    fn test_symlink_manifest_roundtrip() {
        let manifest = SymlinkManifest::new(
            "../target.txt",
            VectorClock::new(),
            "neo".to_string(),
            1000,
            Some("link.txt".to_string()),
        );

        let bytes = manifest.to_bytes().unwrap();
        let parsed = SymlinkManifest::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.kind, crate::index_entry::RemoteEntryKind::Symlink);
        assert_eq!(parsed.symlink_target, "../target.txt");
        assert!(SyncManifest::from_bytes(&bytes).is_err());
    }

    #[test]
    fn test_v1_single_chunk() {
        let v1 = "single_hash\n";
        let parsed = SyncManifest::from_bytes(v1.as_bytes()).unwrap();
        assert_eq!(parsed.chunks, vec!["single_hash"]);
    }
}

#[cfg(test)]
mod proptest_suite {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// from_bytes must never panic on arbitrary bytes — it returns Ok or Err.
        #[test]
        fn from_bytes_never_panics(data in proptest::collection::vec(any::<u8>(), 0..=4096)) {
            let _ = SyncManifest::from_bytes(&data);
        }

        /// Valid v2 manifests must roundtrip through to_bytes/from_bytes.
        #[test]
        fn v2_roundtrip(
            file_hash in "[a-f0-9]{64}",
            file_size in any::<u64>(),
            chunk_count in 1..10usize,
            written_by in "[a-z]{1,16}",
            written_at in any::<u64>(),
        ) {
            let chunks: Vec<String> = (0..chunk_count)
                .map(|i| format!("chunk_{i:016x}"))
                .collect();

            let mut vc = VectorClock::new();
            vc.tick(&written_by);

            let manifest = SyncManifest {
                version: 2,
                file_hash,
                file_size,
                chunks,
                vclock: vc,
                written_by,
                written_at,
                rel_path: None,
                mode: None,
                encrypted_file_key: None,
                wrapped_file_keys: Vec::new(),
            };

            let bytes = manifest.to_bytes().unwrap();
            let parsed = SyncManifest::from_bytes(&bytes).unwrap();

            prop_assert_eq!(parsed.version, 2);
            prop_assert_eq!(parsed.file_hash, manifest.file_hash);
            prop_assert_eq!(parsed.file_size, manifest.file_size);
            prop_assert_eq!(parsed.chunks.len(), manifest.chunks.len());
        }

        /// v1 text manifests: any non-empty newline-separated text should parse as v1.
        #[test]
        fn v1_any_lines_parse(
            lines in proptest::collection::vec("[a-f0-9]{8,64}", 1..20)
        ) {
            let text = lines.join("\n") + "\n";
            let parsed = SyncManifest::from_bytes(text.as_bytes()).unwrap();
            prop_assert!(parsed.is_legacy());
            prop_assert_eq!(parsed.chunks.len(), lines.len());
        }
    }
}
