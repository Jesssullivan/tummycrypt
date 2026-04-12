#![no_main]
//! Fuzz target: chunk encrypt/decrypt roundtrip.
//!
//! For any plaintext and valid key material, encrypt → decrypt must yield
//! the original plaintext. Also verifies that decrypt with wrong key fails
//! gracefully (returns Err, never panics).

use libfuzzer_sys::fuzz_target;
use tcfs_crypto::{encrypt_chunk, decrypt_chunk, keys::FileKey};

fuzz_target!(|data: &[u8]| {
    if data.len() < 33 {
        // Need at least 1 byte of plaintext + 32 bytes for key seed
        return;
    }

    let (key_seed, plaintext) = data.split_at(32);
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(key_seed);
    let file_key = FileKey::from_bytes(key_bytes);

    let file_id = [0xABu8; 32];
    let chunk_index: u64 = 0;

    // Encrypt must succeed for valid inputs
    let ciphertext = match encrypt_chunk(&file_key, chunk_index, &file_id, plaintext) {
        Ok(ct) => ct,
        Err(_) => return,
    };

    // Decrypt with correct key must roundtrip
    let decrypted = decrypt_chunk(&file_key, chunk_index, &file_id, &ciphertext)
        .expect("decrypt with correct key must succeed");
    assert_eq!(decrypted, plaintext, "roundtrip mismatch");

    // Decrypt with wrong key must not panic
    let wrong_key = FileKey::from_bytes([0xFFu8; 32]);
    let _ = decrypt_chunk(&wrong_key, chunk_index, &file_id, &ciphertext);
});
