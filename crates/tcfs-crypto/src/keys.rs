//! Key hierarchy: master key → derived keys, file key generation, key wrapping

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use zeroize::Zeroize;

use crate::kdf::MasterKey;
use crate::{KEY_SIZE, NONCE_SIZE, TAG_SIZE};

/// A per-file 256-bit encryption key. Zeroized on drop.
#[derive(Clone)]
pub struct FileKey {
    bytes: [u8; KEY_SIZE],
}

impl FileKey {
    pub fn from_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.bytes
    }
}

impl Drop for FileKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl std::fmt::Debug for FileKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileKey")
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Generate a random 256-bit file encryption key.
pub fn generate_file_key() -> FileKey {
    let mut bytes = [0u8; KEY_SIZE];
    rand::thread_rng().fill_bytes(&mut bytes);
    FileKey::from_bytes(bytes)
}

/// Derive the manifest encryption key from the master key via HKDF-SHA256.
pub fn derive_manifest_key(master: &MasterKey) -> anyhow::Result<[u8; KEY_SIZE]> {
    hkdf_derive(master.as_bytes(), b"tcfs-manifest")
}

/// Derive the filename encryption key from the master key via HKDF-SHA256.
pub fn derive_name_key(master: &MasterKey) -> anyhow::Result<[u8; KEY_SIZE]> {
    hkdf_derive(master.as_bytes(), b"tcfs-names")
}

/// HKDF-SHA256 key derivation with a domain-specific info string.
fn hkdf_derive(ikm: &[u8; KEY_SIZE], info: &[u8]) -> anyhow::Result<[u8; KEY_SIZE]> {
    let hkdf = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; KEY_SIZE];
    hkdf.expand(info, &mut okm)
        .map_err(|e| anyhow::anyhow!("HKDF expand failed: {e}"))?;
    Ok(okm)
}

/// Wrap (encrypt) a file key using the master key.
///
/// Uses XChaCha20-Poly1305 with a random nonce.
/// Output: `[24-byte nonce][ciphertext + 16-byte tag]`
pub fn wrap_key(master: &MasterKey, file_key: &FileKey) -> anyhow::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(master.as_bytes().into());

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, file_key.as_bytes().as_ref())
        .map_err(|e| anyhow::anyhow!("key wrapping failed: {e}"))?;

    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Unwrap (decrypt) a file key using the master key.
///
/// Input: `[24-byte nonce][ciphertext + 16-byte tag]` (output of `wrap_key`)
pub fn unwrap_key(master: &MasterKey, wrapped: &[u8]) -> anyhow::Result<FileKey> {
    if wrapped.len() < NONCE_SIZE + KEY_SIZE + TAG_SIZE {
        anyhow::bail!(
            "wrapped key too short: {} bytes (expected at least {})",
            wrapped.len(),
            NONCE_SIZE + KEY_SIZE + TAG_SIZE
        );
    }

    let (nonce_bytes, ciphertext) = wrapped.split_at(NONCE_SIZE);
    let nonce = XNonce::from_slice(nonce_bytes);
    let cipher = XChaCha20Poly1305::new(master.as_bytes().into());

    let mut plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        anyhow::anyhow!("key unwrapping failed: invalid master key or corrupted data")
    })?;

    if plaintext.len() != KEY_SIZE {
        plaintext.zeroize();
        anyhow::bail!(
            "unwrapped key has wrong size: {} bytes (expected {})",
            plaintext.len(),
            KEY_SIZE
        );
    }

    let mut key_bytes = [0u8; KEY_SIZE];
    key_bytes.copy_from_slice(&plaintext);
    plaintext.zeroize();

    Ok(FileKey::from_bytes(key_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf::MasterKey;

    fn test_master_key() -> MasterKey {
        MasterKey::from_bytes([42u8; KEY_SIZE])
    }

    #[test]
    fn test_file_key_generation() {
        let k1 = generate_file_key();
        let k2 = generate_file_key();
        assert_ne!(k1.as_bytes(), k2.as_bytes(), "random keys must differ");
    }

    #[test]
    fn test_key_wrap_unwrap_roundtrip() {
        let master = test_master_key();
        let file_key = generate_file_key();

        let wrapped = wrap_key(&master, &file_key).unwrap();
        let unwrapped = unwrap_key(&master, &wrapped).unwrap();

        assert_eq!(file_key.as_bytes(), unwrapped.as_bytes());
    }

    #[test]
    fn test_key_unwrap_wrong_master() {
        let master1 = MasterKey::from_bytes([1u8; KEY_SIZE]);
        let master2 = MasterKey::from_bytes([2u8; KEY_SIZE]);
        let file_key = generate_file_key();

        let wrapped = wrap_key(&master1, &file_key).unwrap();
        let result = unwrap_key(&master2, &wrapped);

        assert!(result.is_err(), "unwrap with wrong master key must fail");
    }

    #[test]
    fn test_hkdf_derive_different_domains() {
        let master = test_master_key();
        let manifest_key = derive_manifest_key(&master).unwrap();
        let name_key = derive_name_key(&master).unwrap();

        assert_ne!(
            manifest_key, name_key,
            "different domains must produce different keys"
        );
    }

    #[test]
    fn test_wrapped_key_size() {
        let master = test_master_key();
        let file_key = generate_file_key();
        let wrapped = wrap_key(&master, &file_key).unwrap();

        // nonce (24) + key (32) + tag (16) = 72
        assert_eq!(wrapped.len(), NONCE_SIZE + KEY_SIZE + TAG_SIZE);
    }
}
