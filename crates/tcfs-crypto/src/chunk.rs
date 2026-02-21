//! Per-chunk XChaCha20-Poly1305 encryption/decryption
//!
//! Encrypted chunk format (binary):
//! ```text
//! [24 bytes: random nonce][N bytes: ciphertext][16 bytes: Poly1305 tag]
//! AAD = chunk_index (8 bytes, big-endian) || file_id (32 bytes)
//! ```
//!
//! The AAD (Additional Authenticated Data) binds each chunk to its position
//! and file, preventing chunk reordering and cross-file substitution attacks.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;

use crate::keys::FileKey;
use crate::NONCE_SIZE;

/// Encrypt a single chunk with XChaCha20-Poly1305.
///
/// - `file_key`: The per-file encryption key
/// - `chunk_index`: Zero-based index of this chunk within the file
/// - `file_id`: 32-byte file identifier (e.g., BLAKE3 hash of plaintext)
/// - `plaintext`: The (potentially compressed) chunk data
///
/// Returns: `[24-byte nonce][ciphertext][16-byte tag]`
pub fn encrypt_chunk(
    file_key: &FileKey,
    chunk_index: u64,
    file_id: &[u8; 32],
    plaintext: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(file_key.as_bytes().into());

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let aad = build_aad(chunk_index, file_id);

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|e| anyhow::anyhow!("chunk encryption failed: {e}"))?;

    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Decrypt a single chunk with XChaCha20-Poly1305.
///
/// - `file_key`: The per-file encryption key
/// - `chunk_index`: Zero-based index of this chunk within the file
/// - `file_id`: 32-byte file identifier (must match what was used during encryption)
/// - `encrypted`: `[24-byte nonce][ciphertext][16-byte tag]`
///
/// Returns the decrypted plaintext.
pub fn decrypt_chunk(
    file_key: &FileKey,
    chunk_index: u64,
    file_id: &[u8; 32],
    encrypted: &[u8],
) -> anyhow::Result<Vec<u8>> {
    if encrypted.len() < NONCE_SIZE + 16 {
        anyhow::bail!(
            "encrypted chunk too short: {} bytes (minimum {})",
            encrypted.len(),
            NONCE_SIZE + 16
        );
    }

    let (nonce_bytes, ciphertext) = encrypted.split_at(NONCE_SIZE);
    let nonce = XNonce::from_slice(nonce_bytes);
    let cipher = XChaCha20Poly1305::new(file_key.as_bytes().into());

    let aad = build_aad(chunk_index, file_id);

    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "chunk decryption failed: invalid key, corrupted data, or wrong chunk_index/file_id"
            )
        })
}

/// Build AAD: chunk_index (8 bytes BE) || file_id (32 bytes)
fn build_aad(chunk_index: u64, file_id: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(8 + 32);
    aad.extend_from_slice(&chunk_index.to_be_bytes());
    aad.extend_from_slice(file_id);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::generate_file_key;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = generate_file_key();
        let file_id = [0xABu8; 32];
        let plaintext = b"hello, encrypted world!";

        let encrypted = encrypt_chunk(&key, 0, &file_id, plaintext).unwrap();
        let decrypted = decrypt_chunk(&key, 0, &file_id, &encrypted).unwrap();

        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_decrypt_empty() {
        let key = generate_file_key();
        let file_id = [0u8; 32];

        let encrypted = encrypt_chunk(&key, 0, &file_id, b"").unwrap();
        let decrypted = decrypt_chunk(&key, 0, &file_id, &encrypted).unwrap();

        assert_eq!(decrypted, b"");
    }

    #[test]
    fn test_decrypt_wrong_key() {
        let key1 = generate_file_key();
        let key2 = generate_file_key();
        let file_id = [0u8; 32];

        let encrypted = encrypt_chunk(&key1, 0, &file_id, b"secret data").unwrap();
        let result = decrypt_chunk(&key2, 0, &file_id, &encrypted);

        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_wrong_chunk_index() {
        let key = generate_file_key();
        let file_id = [0u8; 32];

        let encrypted = encrypt_chunk(&key, 0, &file_id, b"secret data").unwrap();
        let result = decrypt_chunk(&key, 1, &file_id, &encrypted);

        assert!(
            result.is_err(),
            "wrong chunk_index must fail (AAD mismatch)"
        );
    }

    #[test]
    fn test_decrypt_wrong_file_id() {
        let key = generate_file_key();
        let file_id_a = [0xAAu8; 32];
        let file_id_b = [0xBBu8; 32];

        let encrypted = encrypt_chunk(&key, 0, &file_id_a, b"secret data").unwrap();
        let result = decrypt_chunk(&key, 0, &file_id_b, &encrypted);

        assert!(result.is_err(), "wrong file_id must fail (AAD mismatch)");
    }

    #[test]
    fn test_encrypted_size() {
        let key = generate_file_key();
        let file_id = [0u8; 32];
        let plaintext = vec![0u8; 1000];

        let encrypted = encrypt_chunk(&key, 0, &file_id, &plaintext).unwrap();

        // nonce (24) + plaintext (1000) + tag (16) = 1040
        assert_eq!(encrypted.len(), 24 + 1000 + 16);
    }

    #[test]
    fn test_tampered_ciphertext() {
        let key = generate_file_key();
        let file_id = [0u8; 32];

        let mut encrypted = encrypt_chunk(&key, 0, &file_id, b"secret data").unwrap();
        // Flip a byte in the ciphertext (after nonce)
        encrypted[25] ^= 0xFF;

        let result = decrypt_chunk(&key, 0, &file_id, &encrypted);
        assert!(result.is_err(), "tampered ciphertext must fail");
    }
}
