//! E2E: Encrypted push → pull roundtrip with XChaCha20-Poly1305
//!
//! Verifies the full E2E encryption pipeline:
//! 1. Generate a master key
//! 2. Push a file with EncryptionContext (chunks encrypted, file key wrapped)
//! 3. Verify the remote chunks are NOT plaintext
//! 4. Pull with the same master key (decrypt chunks, unwrap file key)
//! 5. Verify byte-equal with original
//! 6. Attempt pull WITHOUT master key → verify failure or ciphertext

use tcfs_e2e::{memory_operator, write_test_file};
use tcfs_sync::engine::EncryptionContext;
use tempfile::TempDir;

fn generate_test_master_key() -> tcfs_crypto::MasterKey {
    let key_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];
    tcfs_crypto::MasterKey::from_bytes(key_bytes)
}

#[tokio::test]
async fn encrypted_push_pull_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-encrypted";

    let plaintext = b"This is secret content that should be encrypted at rest!";
    let src = write_test_file(tmp.path(), "secret.txt", plaintext);
    let dst = tmp.path().join("decrypted.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    let master_key = generate_test_master_key();
    let enc_ctx = EncryptionContext {
        master_key: master_key.clone(),
    };

    // Push with encryption
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "test-device",
        Some("secret.txt"),
        Some(&enc_ctx),
    )
    .await
    .expect("encrypted push");

    assert!(!upload.skipped);
    assert_eq!(upload.bytes, plaintext.len() as u64);

    // Verify: remote chunks should NOT contain plaintext
    let manifest_bytes = op.read(&upload.remote_path).await.expect("read manifest");
    let manifest_raw = manifest_bytes.to_bytes();
    let manifest_str = String::from_utf8_lossy(&manifest_raw);
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_str).expect("parse manifest JSON");

    // Manifest should have encrypted_file_key
    assert!(
        manifest.get("encrypted_file_key").is_some(),
        "manifest should contain encrypted_file_key"
    );

    // Read a chunk and verify it's NOT the plaintext
    let chunks = manifest["chunks"].as_array().expect("chunks array");
    assert!(!chunks.is_empty());

    let chunk_key = format!("{}/chunks/{}", prefix, chunks[0].as_str().unwrap());
    let chunk_data = op.read(&chunk_key).await.expect("read chunk");
    let chunk_bytes = chunk_data.to_bytes();

    // Encrypted chunks should be longer than plaintext (AEAD overhead)
    // and should NOT match the plaintext content
    assert_ne!(
        &chunk_bytes[..],
        plaintext,
        "chunks should be encrypted, not plaintext"
    );

    // Pull with decryption
    let download = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        "test-device",
        None,
        Some(&enc_ctx),
    )
    .await
    .expect("encrypted pull");

    assert_eq!(download.bytes, plaintext.len() as u64);

    // Verify byte-equal
    let decrypted = std::fs::read(&dst).unwrap();
    assert_eq!(
        &decrypted, plaintext,
        "decrypted content should match original"
    );
}

#[tokio::test]
async fn encrypted_large_file_multi_chunk() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-enc-large";

    // 512KB file — will produce chunks
    let plaintext: Vec<u8> = (0..524288).map(|i| (i % 223) as u8).collect();
    let src = write_test_file(tmp.path(), "large-secret.bin", &plaintext);
    let dst = tmp.path().join("decrypted-large.bin");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    let master_key = generate_test_master_key();
    let enc_ctx = EncryptionContext {
        master_key: master_key.clone(),
    };

    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "test-device",
        Some("large-secret.bin"),
        Some(&enc_ctx),
    )
    .await
    .expect("encrypted push large");

    assert!(!upload.skipped);

    let _download = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        "test-device",
        None,
        Some(&enc_ctx),
    )
    .await
    .expect("encrypted pull large");

    let decrypted = std::fs::read(&dst).unwrap();
    assert_eq!(decrypted.len(), plaintext.len());
    assert_eq!(decrypted, plaintext, "large file decryption mismatch");
}

#[tokio::test]
async fn pull_without_key_fails_on_encrypted_file() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-enc-nokey";

    let plaintext = b"encrypted content that requires key to read";
    let src = write_test_file(tmp.path(), "locked.txt", plaintext);
    let dst = tmp.path().join("should-fail.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    let master_key = generate_test_master_key();
    let enc_ctx = EncryptionContext {
        master_key: master_key.clone(),
    };

    // Push with encryption
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "test-device",
        Some("locked.txt"),
        Some(&enc_ctx),
    )
    .await
    .expect("push");

    // Pull WITHOUT encryption context — should fail
    let result = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        "test-device",
        None,
        None, // no encryption context
    )
    .await;

    assert!(
        result.is_err(),
        "pull without master key should fail on encrypted file"
    );
}

#[tokio::test]
async fn wrong_key_fails_decryption() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-enc-wrongkey";

    let plaintext = b"encrypted with key A, decrypted with key B should fail";
    let src = write_test_file(tmp.path(), "mismatch.txt", plaintext);
    let dst = tmp.path().join("wrong-key.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    let key_a = generate_test_master_key();
    let enc_ctx_a = EncryptionContext {
        master_key: key_a.clone(),
    };

    // Push with key A
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "test-device",
        Some("mismatch.txt"),
        Some(&enc_ctx_a),
    )
    .await
    .expect("push with key A");

    // Pull with key B (different key)
    let key_b_bytes: [u8; 32] = [0xFF; 32];
    let key_b = tcfs_crypto::MasterKey::from_bytes(key_b_bytes);
    let enc_ctx_b = EncryptionContext {
        master_key: key_b.clone(),
    };

    let result = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        "test-device",
        None,
        Some(&enc_ctx_b),
    )
    .await;

    assert!(result.is_err(), "pull with wrong master key should fail");
}
