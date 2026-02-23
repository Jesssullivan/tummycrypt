//! Integration tests for E2E encryption in the push/pull pipeline.
//!
//! Verifies that when an `EncryptionContext` is provided, chunks are
//! encrypted before upload, the manifest contains a wrapped file key,
//! and decryption produces the original plaintext.

#![cfg(feature = "crypto")]

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

fn memory_operator() -> Operator {
    Operator::new(opendal::services::Memory::default())
        .expect("memory operator")
        .finish()
}

fn write_test_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write test file");
    path
}

fn test_encryption_context() -> tcfs_sync::engine::EncryptionContext {
    let master_key = tcfs_crypto::MasterKey::from_bytes([42u8; 32]);
    tcfs_sync::engine::EncryptionContext { master_key }
}

#[tokio::test]
async fn encrypted_upload_download_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/encrypted";
    let ctx = test_encryption_context();

    let original = b"hello encrypted world! this file will be chunked, encrypted, \
                     uploaded, downloaded, decrypted, and verified for integrity.";
    let src = write_test_file(tmp.path(), "secret.txt", original);
    let dst = tmp.path().join("output/secret.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push with encryption
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "test-device",
        None,
        Some(&ctx),
    )
    .await
    .expect("encrypted upload should succeed");

    assert!(!upload.skipped);
    assert!(upload.chunks > 0);
    assert_eq!(upload.bytes, original.len() as u64);

    // Pull with decryption
    let download = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        "test-device",
        None,
        Some(&ctx),
    )
    .await
    .expect("encrypted download should succeed");

    assert_eq!(download.bytes, original.len() as u64);

    // Verify content is identical
    let output = std::fs::read(&dst).unwrap();
    assert_eq!(output, original);
}

#[tokio::test]
async fn encrypted_manifest_has_wrapped_key() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/enc-manifest";
    let ctx = test_encryption_context();

    let content = b"test file for manifest verification";
    let src = write_test_file(tmp.path(), "test.txt", content);

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "dev1",
        None,
        Some(&ctx),
    )
    .await
    .expect("upload should succeed");

    // Read the manifest and verify encrypted_file_key is present
    let manifest_bytes = op.read(&upload.remote_path).await.unwrap();
    let manifest =
        tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes()).unwrap();

    assert!(
        manifest.encrypted_file_key.is_some(),
        "encrypted manifest must contain wrapped file key"
    );

    // The wrapped key should be valid base64
    let key_b64 = manifest.encrypted_file_key.unwrap();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &key_b64);
    assert!(decoded.is_ok(), "encrypted_file_key should be valid base64");
}

#[tokio::test]
async fn unencrypted_download_of_encrypted_fails() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/enc-noctx";
    let ctx = test_encryption_context();

    let content = b"encrypted file that should fail without key";
    let src = write_test_file(tmp.path(), "locked.txt", content);
    let dst = tmp.path().join("output/locked.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Upload with encryption
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "dev1",
        None,
        Some(&ctx),
    )
    .await
    .expect("encrypted upload should succeed");

    // Attempt download without encryption context — should fail
    let result = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        "dev1",
        None,
        None, // no encryption context
    )
    .await;

    assert!(
        result.is_err(),
        "downloading encrypted file without key should fail"
    );
}

#[tokio::test]
async fn mixed_encrypted_unencrypted_coexist() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/mixed";
    let ctx = test_encryption_context();

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Upload unencrypted file
    let plain_content = b"plaintext file content";
    let plain_src = write_test_file(tmp.path(), "plain.txt", plain_content);
    let plain_upload = tcfs_sync::engine::upload_file_with_device(
        &op, &plain_src, prefix, &mut state, None, "dev1", None, None,
    )
    .await
    .expect("plain upload should succeed");

    // Upload encrypted file
    let enc_content = b"encrypted file content";
    let enc_src = write_test_file(tmp.path(), "encrypted.txt", enc_content);
    let enc_upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &enc_src,
        prefix,
        &mut state,
        None,
        "dev1",
        None,
        Some(&ctx),
    )
    .await
    .expect("encrypted upload should succeed");

    // Download both
    let plain_dst = tmp.path().join("out/plain.txt");
    let enc_dst = tmp.path().join("out/encrypted.txt");

    tcfs_sync::engine::download_file_with_device(
        &op,
        &plain_upload.remote_path,
        &plain_dst,
        prefix,
        None,
        "dev1",
        None,
        None,
    )
    .await
    .expect("plain download should succeed");

    tcfs_sync::engine::download_file_with_device(
        &op,
        &enc_upload.remote_path,
        &enc_dst,
        prefix,
        None,
        "dev1",
        None,
        Some(&ctx),
    )
    .await
    .expect("encrypted download should succeed");

    assert_eq!(std::fs::read(&plain_dst).unwrap(), plain_content);
    assert_eq!(std::fs::read(&enc_dst).unwrap(), enc_content);
}
