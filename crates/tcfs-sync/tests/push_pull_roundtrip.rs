//! Integration test: push → pull round-trip with in-memory storage
//!
//! Verifies the full content pipeline: chunk → hash → upload → download →
//! verify integrity → reassemble → byte-equal output. Uses OpenDAL's
//! in-memory backend so no live SeaweedFS is required.

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

#[tokio::test]
async fn roundtrip_small_file() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/default";

    let original = b"hello world, this is a small test file for tcfs round-trip";
    let src = write_test_file(tmp.path(), "small.txt", original);
    let dst = tmp.path().join("output/small.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push
    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload should succeed");

    assert!(!upload.skipped);
    assert!(upload.chunks > 0);
    assert_eq!(upload.bytes, original.len() as u64);

    // Pull
    let download = tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
        .await
        .expect("download should succeed");

    assert_eq!(download.bytes, original.len() as u64);

    // Verify byte equality
    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(downloaded, original, "downloaded file must match original");
}

#[tokio::test]
async fn roundtrip_binary_data() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/binary";

    // Generate 256 KiB of pseudo-random binary data
    let original: Vec<u8> = (0u64..262144)
        .map(|i| (i.wrapping_mul(7) ^ (i >> 3)) as u8)
        .collect();
    let src = write_test_file(tmp.path(), "binary.bin", &original);
    let dst = tmp.path().join("output/binary.bin");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload binary");

    assert!(!upload.skipped);
    assert!(
        upload.chunks >= 1,
        "256 KiB should produce at least 1 chunk, got {}",
        upload.chunks
    );

    tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
        .await
        .expect("download binary");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(downloaded.len(), original.len());
    assert_eq!(downloaded, original, "binary round-trip must be exact");
}

#[tokio::test]
async fn roundtrip_dedup_skips_rechunk() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/dedup";

    let original = b"deduplicated content test";
    let src = write_test_file(tmp.path(), "dedup.txt", original);

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // First upload
    let first = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("first upload");
    assert!(!first.skipped);

    // Second upload of same file should be skipped (state cache hit)
    let second = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("second upload");
    assert!(second.skipped, "unchanged file should be skipped");
}

#[tokio::test]
async fn roundtrip_integrity_verification() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/integrity";

    // Upload a file
    let original = b"integrity verification test data";
    let src = write_test_file(tmp.path(), "verify.txt", original);
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload");

    // Corrupt a chunk in storage
    let manifest_bytes = op.read(&upload.remote_path).await.unwrap();
    let manifest =
        tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes()).unwrap();
    let chunk_hashes = manifest.chunk_hashes();
    let chunk_key = format!("{prefix}/chunks/{}", chunk_hashes[0]);

    // Overwrite chunk with garbage
    op.write(&chunk_key, vec![0xDE, 0xAD, 0xBE, 0xEF])
        .await
        .unwrap();

    // Pull should fail integrity check
    let dst = tmp.path().join("output/verify.txt");
    let result =
        tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None).await;

    assert!(
        result.is_err(),
        "corrupted chunk should fail integrity check"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("integrity check failed"),
        "error should mention integrity: {err}"
    );
}

#[tokio::test]
async fn roundtrip_large_file_many_chunks() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/large";

    // 1 MiB file — should produce many chunks with FastCDC
    let original: Vec<u8> = (0u64..1048576)
        .map(|i| (i.wrapping_mul(13) ^ (i >> 5)) as u8)
        .collect();
    let src = write_test_file(tmp.path(), "large.bin", &original);
    let dst = tmp.path().join("output/large.bin");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload large");

    assert!(
        upload.chunks >= 4,
        "1 MiB should produce at least 4 chunks, got {}",
        upload.chunks
    );

    let _download = tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
        .await
        .expect("download large");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(downloaded.len(), original.len());
    assert_eq!(downloaded, original, "1 MiB round-trip must be exact");
}

#[tokio::test]
async fn roundtrip_push_tree() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/tree";

    // Create a directory tree
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(src_dir.join("subdir")).unwrap();
    write_test_file(&src_dir, "a.txt", b"file a content");
    write_test_file(&src_dir, "b.txt", b"file b content");
    write_test_file(&src_dir.join("subdir"), "c.txt", b"file c in subdir");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (uploaded, skipped, _bytes) =
        tcfs_sync::engine::push_tree(&op, &src_dir, prefix, &mut state, None)
            .await
            .expect("push_tree");

    assert_eq!(uploaded, 3, "should upload 3 files");
    assert_eq!(skipped, 0);

    // Push again — should skip all
    let (uploaded2, skipped2, _) =
        tcfs_sync::engine::push_tree(&op, &src_dir, prefix, &mut state, None)
            .await
            .expect("push_tree second");

    assert_eq!(uploaded2, 0, "second push should upload nothing");
    assert_eq!(skipped2, 3, "second push should skip all 3");
}

#[tokio::test]
async fn roundtrip_with_device_identity() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/device";
    let device_id = "test-device-001";

    let original = b"device-aware upload test";
    let src = write_test_file(tmp.path(), "device.txt", original);
    let dst = tmp.path().join("output/device.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Upload with device identity
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("device.txt"),
        None,
    )
    .await
    .expect("upload with device");

    assert!(!upload.skipped);

    // Download with device identity
    let download = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        device_id,
        Some(&mut state),
        None,
    )
    .await
    .expect("download with device");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(downloaded, original);
    assert_eq!(download.bytes, original.len() as u64);

    // Verify state cache has vclock entry
    let cached = state.get(&dst).expect("state cache should have entry");
    assert!(
        !cached.vclock.clocks.is_empty(),
        "vclock should be non-empty after device-aware sync"
    );
}
