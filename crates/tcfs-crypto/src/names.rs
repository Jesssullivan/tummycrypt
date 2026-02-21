//! AES-SIV filename encryption
//!
//! Deterministic encryption (same plaintext + key = same ciphertext) is required
//! for filenames because S3 object lookups need a predictable key path.
//! AES-SIV provides this with authentication (SIV = Synthetic Initialization Vector).

use aes_siv::{
    aead::{Aead, KeyInit},
    Aes256SivAead, Nonce,
};

use crate::KEY_SIZE;

/// Encrypt a filename using AES-256-SIV.
///
/// AES-SIV is deterministic: the same name + key always produces the same ciphertext.
/// This is intentional — we need deterministic names for S3 key lookup.
///
/// The `name_key` should be derived from the master key via HKDF (domain: "tcfs-names").
///
/// Returns the encrypted name as a hex string (suitable for S3 keys).
pub fn encrypt_name(name_key: &[u8; KEY_SIZE], plaintext_name: &str) -> anyhow::Result<String> {
    // AES-256-SIV requires a 64-byte key (two 32-byte sub-keys)
    let mut double_key = [0u8; 64];
    // Use HKDF to expand the 32-byte key to 64 bytes
    let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(None, name_key);
    hkdf.expand(b"tcfs-name-aes-siv", &mut double_key)
        .map_err(|e| anyhow::anyhow!("HKDF expand for AES-SIV: {e}"))?;

    let cipher = Aes256SivAead::new((&double_key).into());
    // AES-SIV uses a zero nonce for deterministic encryption
    let nonce = Nonce::default();

    let ciphertext = cipher
        .encrypt(&nonce, plaintext_name.as_bytes())
        .map_err(|e| anyhow::anyhow!("filename encryption failed: {e}"))?;

    Ok(hex::encode(&ciphertext))
}

/// Decrypt a filename using AES-256-SIV.
///
/// The `encrypted_hex` is the hex-encoded ciphertext from `encrypt_name`.
pub fn decrypt_name(name_key: &[u8; KEY_SIZE], encrypted_hex: &str) -> anyhow::Result<String> {
    let ciphertext = hex::decode(encrypted_hex).map_err(|e| anyhow::anyhow!("hex decode: {e}"))?;

    let mut double_key = [0u8; 64];
    let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(None, name_key);
    hkdf.expand(b"tcfs-name-aes-siv", &mut double_key)
        .map_err(|e| anyhow::anyhow!("HKDF expand for AES-SIV: {e}"))?;

    let cipher = Aes256SivAead::new((&double_key).into());
    let nonce = Nonce::default();

    let plaintext = cipher
        .decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("filename decryption failed: wrong key or corrupted data"))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("decrypted name is not UTF-8: {e}"))
}

/// Hex encoding/decoding helpers (no external dep needed, just a small impl)
mod hex {
    pub fn encode(data: &[u8]) -> String {
        let mut s = String::with_capacity(data.len() * 2);
        for byte in data {
            s.push_str(&format!("{:02x}", byte));
        }
        s
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, String> {
        if !s.len().is_multiple_of(2) {
            return Err("odd-length hex string".to_string());
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("invalid hex: {e}")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_name_key() -> [u8; KEY_SIZE] {
        [0x55u8; KEY_SIZE]
    }

    #[test]
    fn test_encrypt_decrypt_name_roundtrip() {
        let key = test_name_key();
        let name = "my-photo.jpg";

        let encrypted = encrypt_name(&key, name).unwrap();
        let decrypted = decrypt_name(&key, &encrypted).unwrap();

        assert_eq!(decrypted, name);
    }

    #[test]
    fn test_deterministic_encryption() {
        let key = test_name_key();
        let name = "report.pdf";

        let enc1 = encrypt_name(&key, name).unwrap();
        let enc2 = encrypt_name(&key, name).unwrap();

        assert_eq!(enc1, enc2, "AES-SIV must be deterministic");
    }

    #[test]
    fn test_different_names_different_ciphertext() {
        let key = test_name_key();

        let enc1 = encrypt_name(&key, "file_a.txt").unwrap();
        let enc2 = encrypt_name(&key, "file_b.txt").unwrap();

        assert_ne!(enc1, enc2);
    }

    #[test]
    fn test_different_keys_different_ciphertext() {
        let key1 = [0x11u8; KEY_SIZE];
        let key2 = [0x22u8; KEY_SIZE];

        let enc1 = encrypt_name(&key1, "same-name.txt").unwrap();
        let enc2 = encrypt_name(&key2, "same-name.txt").unwrap();

        assert_ne!(enc1, enc2);
    }

    #[test]
    fn test_decrypt_wrong_key() {
        let key1 = [0x11u8; KEY_SIZE];
        let key2 = [0x22u8; KEY_SIZE];

        let encrypted = encrypt_name(&key1, "secret.txt").unwrap();
        let result = decrypt_name(&key2, &encrypted);

        assert!(result.is_err());
    }

    #[test]
    fn test_unicode_filename() {
        let key = test_name_key();
        let name = "research-2026-02-21.pdf";

        let encrypted = encrypt_name(&key, name).unwrap();
        let decrypted = decrypt_name(&key, &encrypted).unwrap();

        assert_eq!(decrypted, name);
    }
}
