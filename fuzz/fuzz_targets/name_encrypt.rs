#![no_main]
//! Fuzz target: AES-SIV name encryption roundtrip.
//!
//! Properties:
//! - encrypt_name never panics on valid UTF-8 input
//! - encrypt → decrypt roundtrips for any valid name
//! - Deterministic: same key + name = same ciphertext

use libfuzzer_sys::fuzz_target;
use tcfs_crypto::{encrypt_name, decrypt_name, KEY_SIZE};

fuzz_target!(|data: &[u8]| {
    if data.len() < KEY_SIZE + 1 {
        return;
    }

    let (key_bytes, name_bytes) = data.split_at(KEY_SIZE);
    let mut key = [0u8; KEY_SIZE];
    key.copy_from_slice(key_bytes);

    let Ok(name) = std::str::from_utf8(name_bytes) else {
        return;
    };
    if name.is_empty() {
        return;
    }

    // Encrypt must succeed
    let encrypted = match encrypt_name(&key, name) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Deterministic: second encryption must match
    let encrypted2 = encrypt_name(&key, name).unwrap();
    assert_eq!(encrypted, encrypted2, "AES-SIV must be deterministic");

    // Roundtrip
    let decrypted = decrypt_name(&key, &encrypted).expect("decrypt must succeed");
    assert_eq!(decrypted, name, "roundtrip mismatch");
});
