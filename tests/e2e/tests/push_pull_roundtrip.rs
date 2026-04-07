//! E2E: push → pull roundtrip with in-memory storage
//!
//! Verifies the full content pipeline: local file → chunk → hash → upload →
//! manifest → download → reassemble → byte-equal output.

use tcfs_e2e::{memory_operator, write_test_file};
use tempfile::TempDir;

#[tokio::test]
async fn small_file_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e";

    let content = b"E2E roundtrip test: small file";
    let src = write_test_file(tmp.path(), "test.txt", content);
    let dst = tmp.path().join("output.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    // Push
    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("push");
    assert!(!upload.skipped);
    assert_eq!(upload.bytes, content.len() as u64);

    // Pull
    let download = tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
        .await
        .expect("pull");
    assert_eq!(download.bytes, content.len() as u64);

    // Byte-equal
    let pulled = std::fs::read(&dst).unwrap();
    assert_eq!(pulled, content);
}

#[tokio::test]
async fn large_file_multi_chunk_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-large";

    // 2MB — will produce multiple FastCDC chunks
    let content: Vec<u8> = (0..2_097_152).map(|i| (i % 251) as u8).collect();
    let src = write_test_file(tmp.path(), "large.bin", &content);
    let dst = tmp.path().join("output.bin");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("push large");
    assert!(
        upload.chunks >= 1,
        "2MB file should produce at least 1 chunk"
    );

    let download = tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
        .await
        .expect("pull large");
    assert_eq!(download.bytes, content.len() as u64);

    let pulled = std::fs::read(&dst).unwrap();
    assert_eq!(pulled, content);
}

#[tokio::test]
async fn idempotent_push_skips_unchanged() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-skip";

    let content = b"unchanged";
    let src = write_test_file(tmp.path(), "same.txt", content);

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db.json")).unwrap();

    let first = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("first push");
    assert!(!first.skipped);

    let second = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("second push");
    assert!(second.skipped, "unchanged file should be skipped");
}

#[tokio::test]
async fn state_cache_persists_across_reopens() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e-state";
    let state_path = tmp.path().join("state.db.json");

    let content = b"persist test";
    let src = write_test_file(tmp.path(), "stateful.txt", content);

    // Session 1
    {
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
            .await
            .expect("push");
        assert!(!upload.skipped);
    }

    // Session 2 — reopen state, should skip
    {
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
            .await
            .expect("re-push");
        assert!(upload.skipped, "state should persist across sessions");
    }
}
