//! Key hierarchy: master key → derived keys, file key generation, key wrapping

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::kdf::MasterKey;
use crate::{KEY_SIZE, NONCE_SIZE, TAG_SIZE};

/// Algorithm marker for file keys wrapped to age X25519 device recipients.
pub const AGE_X25519_FILE_KEY_ALGORITHM: &str = "age-x25519-v1";

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

/// An active device recipient for per-device FileKey wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgeFileKeyRecipient {
    /// Stable TCFS device identifier.
    pub device_id: String,
    /// age X25519 public recipient string (`age1...`).
    pub recipient: String,
}

/// One FileKey wrap addressed to one device recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgeWrappedFileKey {
    /// Stable TCFS device identifier this wrap is intended for.
    pub recipient_device_id: String,
    /// Public recipient used when wrapping, retained for audit/migration checks.
    pub recipient: String,
    /// Cryptographic wrap algorithm.
    pub algorithm: String,
    /// Armored age ciphertext containing the 32-byte FileKey.
    pub wrapped_key: String,
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

/// Wrap a FileKey once per active age X25519 device recipient.
///
/// This is the additive TIN-1417 primitive for manifest schema v3. Existing
/// master-key wraps remain supported during migration; callers should dual-write
/// this recipient list before cutting over reads to per-device identities.
pub fn wrap_file_key_for_age_recipients(
    file_key: &FileKey,
    recipients: &[AgeFileKeyRecipient],
) -> anyhow::Result<Vec<AgeWrappedFileKey>> {
    if recipients.is_empty() {
        anyhow::bail!("at least one recipient is required to wrap a file key");
    }

    recipients
        .iter()
        .map(|recipient| {
            let age_recipient: age::x25519::Recipient =
                recipient.recipient.parse().map_err(|e| {
                    anyhow::anyhow!("parsing age recipient for {}: {e}", recipient.device_id)
                })?;
            let wrapped_key =
                age::encrypt_and_armor(&age_recipient, file_key.as_bytes()).map_err(|e| {
                    anyhow::anyhow!("wrapping file key for {}: {e}", recipient.device_id)
                })?;
            Ok(AgeWrappedFileKey {
                recipient_device_id: recipient.device_id.clone(),
                recipient: recipient.recipient.clone(),
                algorithm: AGE_X25519_FILE_KEY_ALGORITHM.to_string(),
                wrapped_key,
            })
        })
        .collect()
}

/// Unwrap a FileKey using a local age X25519 identity.
///
/// If `device_id` is provided, only wraps addressed to that device are tried.
/// Otherwise every supported wrap is attempted. Non-matching wraps fail closed
/// and do not produce corrupted keys.
pub fn unwrap_file_key_with_age_identity(
    wrapped_keys: &[AgeWrappedFileKey],
    identity_secret: &str,
    device_id: Option<&str>,
) -> anyhow::Result<FileKey> {
    use age::armor::ArmoredReader;
    use std::io::Read;

    let identity: age::x25519::Identity = identity_secret
        .parse()
        .map_err(|e| anyhow::anyhow!("parsing age identity: {e}"))?;
    let mut saw_candidate = false;

    for wrapped in wrapped_keys {
        if wrapped.algorithm != AGE_X25519_FILE_KEY_ALGORITHM {
            continue;
        }
        if device_id.is_some_and(|id| id != wrapped.recipient_device_id) {
            continue;
        }
        saw_candidate = true;

        let armored = ArmoredReader::new(wrapped.wrapped_key.as_bytes());
        let decryptor = match age::Decryptor::new(armored) {
            Ok(decryptor) => decryptor,
            Err(_) => continue,
        };
        if decryptor.is_scrypt() {
            continue;
        }

        let mut reader = match decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity)) {
            Ok(reader) => reader,
            Err(_) => continue,
        };
        let mut plaintext = Vec::new();
        if reader.read_to_end(&mut plaintext).is_err() {
            plaintext.zeroize();
            continue;
        }

        if plaintext.len() != KEY_SIZE {
            plaintext.zeroize();
            anyhow::bail!(
                "age-wrapped file key for {} had wrong size: {} bytes",
                wrapped.recipient_device_id,
                plaintext.len()
            );
        }

        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes.copy_from_slice(&plaintext);
        plaintext.zeroize();
        return Ok(FileKey::from_bytes(key_bytes));
    }

    if saw_candidate {
        anyhow::bail!("no decryptable age-wrapped file key for this identity");
    }
    if let Some(device_id) = device_id {
        anyhow::bail!("manifest has no supported age-wrapped file key for device {device_id}");
    }
    anyhow::bail!("manifest has no supported age-wrapped file key entries")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf::MasterKey;
    use secrecy::ExposeSecret;

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

    #[test]
    fn test_age_recipient_file_key_wrap_roundtrip() {
        let device_a = age::x25519::Identity::generate();
        let device_b = age::x25519::Identity::generate();
        let outsider = age::x25519::Identity::generate();
        let file_key = generate_file_key();

        let wrapped = wrap_file_key_for_age_recipients(
            &file_key,
            &[
                AgeFileKeyRecipient {
                    device_id: "device-a".into(),
                    recipient: device_a.to_public().to_string(),
                },
                AgeFileKeyRecipient {
                    device_id: "device-b".into(),
                    recipient: device_b.to_public().to_string(),
                },
            ],
        )
        .unwrap();

        assert_eq!(wrapped.len(), 2);
        assert!(wrapped
            .iter()
            .all(|wrap| wrap.algorithm == AGE_X25519_FILE_KEY_ALGORITHM));

        let a = unwrap_file_key_with_age_identity(
            &wrapped,
            device_a.to_string().expose_secret(),
            Some("device-a"),
        )
        .unwrap();
        let b = unwrap_file_key_with_age_identity(
            &wrapped,
            device_b.to_string().expose_secret(),
            Some("device-b"),
        )
        .unwrap();

        assert_eq!(a.as_bytes(), file_key.as_bytes());
        assert_eq!(b.as_bytes(), file_key.as_bytes());

        let outsider_result =
            unwrap_file_key_with_age_identity(&wrapped, outsider.to_string().expose_secret(), None);
        assert!(outsider_result.is_err());
    }

    #[test]
    fn test_age_wrap_rejects_bad_recipient() {
        let file_key = generate_file_key();
        let result = wrap_file_key_for_age_recipients(
            &file_key,
            &[AgeFileKeyRecipient {
                device_id: "bad-device".into(),
                recipient: "age1-not-a-real-recipient".into(),
            }],
        );

        assert!(result.is_err());
    }
}
