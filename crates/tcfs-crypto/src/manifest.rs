//! Encrypted manifest format
//!
//! A manifest records the chunk layout of an encrypted file:
//! - file_id (plaintext, for lookup)
//! - wrapped file key (encrypted by master key)
//! - list of chunk hashes (BLAKE3 of ciphertext)
//!
//! The manifest itself is encrypted with the manifest-derived key.

use serde::{Deserialize, Serialize};

use crate::kdf::MasterKey;
use crate::keys::{unwrap_key, wrap_key, FileKey};

/// A single chunk entry in the manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// BLAKE3 hash of the encrypted chunk (hex)
    pub hash: String,
    /// Chunk index (0-based)
    pub index: u64,
    /// Size of the encrypted chunk in bytes (includes nonce + tag overhead)
    pub encrypted_size: u64,
}

/// An encrypted file manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedManifest {
    /// Manifest format version
    pub version: u32,
    /// File identifier (BLAKE3 of original plaintext, hex)
    pub file_id: String,
    /// Original file size in bytes
    pub original_size: u64,
    /// The file encryption key, wrapped (encrypted) by the master key (base64)
    pub wrapped_file_key: String,
    /// Ordered list of chunk entries
    pub chunks: Vec<ManifestEntry>,
}

impl EncryptedManifest {
    /// Create a new manifest, wrapping the file key with the master key.
    pub fn new(
        file_id: String,
        original_size: u64,
        master_key: &MasterKey,
        file_key: &FileKey,
        chunks: Vec<ManifestEntry>,
    ) -> anyhow::Result<Self> {
        let wrapped = wrap_key(master_key, file_key)?;
        let wrapped_b64 = base64_encode(&wrapped);

        Ok(Self {
            version: 1,
            file_id,
            original_size,
            wrapped_file_key: wrapped_b64,
            chunks,
        })
    }

    /// Extract the file key by unwrapping with the master key.
    pub fn unwrap_file_key(&self, master_key: &MasterKey) -> anyhow::Result<FileKey> {
        let wrapped = base64_decode(&self.wrapped_file_key)?;
        unwrap_key(master_key, &wrapped)
    }

    /// Serialize to JSON bytes
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| anyhow::anyhow!("manifest serialization: {e}"))
    }

    /// Deserialize from JSON bytes
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(data).map_err(|e| anyhow::anyhow!("manifest deserialization: {e}"))
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.encode(data)
}

fn base64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD
        .decode(s)
        .map_err(|e| anyhow::anyhow!("base64 decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf::MasterKey;
    use crate::keys::generate_file_key;
    use crate::KEY_SIZE;

    #[test]
    fn test_manifest_roundtrip() {
        let master = MasterKey::from_bytes([42u8; KEY_SIZE]);
        let file_key = generate_file_key();

        let manifest = EncryptedManifest::new(
            "abc123def456".to_string(),
            1024,
            &master,
            &file_key,
            vec![
                ManifestEntry {
                    hash: "chunk_hash_0".to_string(),
                    index: 0,
                    encrypted_size: 540,
                },
                ManifestEntry {
                    hash: "chunk_hash_1".to_string(),
                    index: 1,
                    encrypted_size: 524,
                },
            ],
        )
        .unwrap();

        // Serialize and deserialize
        let bytes = manifest.to_bytes().unwrap();
        let restored = EncryptedManifest::from_bytes(&bytes).unwrap();

        assert_eq!(restored.version, 1);
        assert_eq!(restored.file_id, "abc123def456");
        assert_eq!(restored.original_size, 1024);
        assert_eq!(restored.chunks.len(), 2);

        // Unwrap file key
        let unwrapped = restored.unwrap_file_key(&master).unwrap();
        assert_eq!(unwrapped.as_bytes(), file_key.as_bytes());
    }

    #[test]
    fn test_manifest_wrong_master_key() {
        let master1 = MasterKey::from_bytes([1u8; KEY_SIZE]);
        let master2 = MasterKey::from_bytes([2u8; KEY_SIZE]);
        let file_key = generate_file_key();

        let manifest =
            EncryptedManifest::new("test".to_string(), 100, &master1, &file_key, vec![]).unwrap();

        let result = manifest.unwrap_file_key(&master2);
        assert!(result.is_err());
    }
}
