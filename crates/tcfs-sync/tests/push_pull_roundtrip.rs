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

#[tokio::test]
async fn roundtrip_empty_file() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/empty";

    let src = write_test_file(tmp.path(), "empty.txt", b"");
    let dst = tmp.path().join("output/empty.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push empty file
    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload empty file should succeed");

    assert!(!upload.skipped);
    assert_eq!(upload.chunks, 0, "empty file should produce 0 chunks");
    assert_eq!(upload.bytes, 0, "empty file should be 0 bytes");

    // Pull empty file
    let download =
        tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
            .await
            .expect("download empty file should succeed");

    assert_eq!(download.bytes, 0);

    // Verify the file exists and is empty
    let downloaded = std::fs::read(&dst).unwrap();
    assert!(downloaded.is_empty(), "downloaded empty file must be empty");
}

#[tokio::test]
async fn roundtrip_empty_file_with_device() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/empty-dev";
    let device_id = "test-device-empty";

    let src = write_test_file(tmp.path(), "empty-dev.txt", b"");
    let dst = tmp.path().join("output/empty-dev.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push with device identity
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("empty-dev.txt"),
        None,
    )
    .await
    .expect("upload empty file with device should succeed");

    assert!(!upload.skipped);
    assert_eq!(upload.chunks, 0);
    assert_eq!(upload.bytes, 0);

    // Pull with device identity and state merge
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
    .expect("download empty file with device should succeed");

    assert_eq!(download.bytes, 0);

    let downloaded = std::fs::read(&dst).unwrap();
    assert!(downloaded.is_empty(), "downloaded empty file must be empty");

    // Verify state cache has vclock entry
    let cached = state.get(&dst).expect("state cache should have entry for empty file");
    assert!(
        !cached.vclock.clocks.is_empty(),
        "vclock should be non-empty after device-aware empty file sync"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn roundtrip_preserves_executable_permission() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/perms";
    let device_id = "test-device-perms";

    // Create a file with executable permissions (0o755)
    let src = write_test_file(tmp.path(), "script.sh", b"#!/bin/sh\necho hello\n");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();

    let dst = tmp.path().join("output/script.sh");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push with device identity
    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("script.sh"),
        None,
    )
    .await
    .expect("upload executable file");

    assert!(!upload.skipped);

    // Verify manifest has mode field
    let manifest_bytes = op.read(&upload.remote_path).await.unwrap();
    let manifest =
        tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes.to_bytes()).unwrap();
    assert!(manifest.mode.is_some(), "manifest should capture file mode");
    assert_eq!(
        manifest.mode.unwrap() & 0o777,
        0o755,
        "manifest mode should be 755"
    );

    // Pull to a new location
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
    .expect("download executable file");

    // Verify content
    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(downloaded, b"#!/bin/sh\necho hello\n");
    assert_eq!(download.bytes, 21);

    // Verify permissions were preserved
    let meta = std::fs::metadata(&dst).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "executable permission should be preserved after pull");
}

#[cfg(unix)]
#[tokio::test]
async fn roundtrip_preserves_readonly_permission() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/perms-ro";

    // Create a read-only file (0o444)
    let src = write_test_file(tmp.path(), "readonly.txt", b"do not edit");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();

    let dst = tmp.path().join("output/readonly.txt");

    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push
    let upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload readonly file");

    // Pull
    tcfs_sync::engine::download_file(&op, &upload.remote_path, &dst, prefix, None)
        .await
        .expect("download readonly file");

    // Verify permissions preserved
    let meta = std::fs::metadata(&dst).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o444, "read-only permission should be preserved");
}

#[tokio::test]
async fn delete_removes_index_and_manifest() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/delete";
    let device_id = "test-device-del";

    // Push a file
    let src = write_test_file(tmp.path(), "to-delete.txt", b"delete me");
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("to-delete.txt"),
        None,
    )
    .await
    .expect("upload file to delete");

    assert!(!upload.skipped);

    // Write index entry (normally done by push_tree or cmd_push)
    let index_key = format!("{prefix}/index/to-delete.txt");
    let index_entry = format!(
        "manifest_hash={}\nsize={}\nchunks={}\n",
        upload.hash, upload.bytes, upload.chunks
    );
    op.write(&index_key, index_entry.into_bytes())
        .await
        .expect("write index entry");

    // Verify index and manifest exist
    assert!(
        op.exists(&index_key).await.unwrap(),
        "index entry should exist after push"
    );
    assert!(
        op.exists(&upload.remote_path).await.unwrap(),
        "manifest should exist after push"
    );

    // Delete the remote file
    tcfs_sync::engine::delete_remote_file(
        &op,
        "to-delete.txt",
        prefix,
        &mut state,
        Some(tmp.path()),
    )
    .await
    .expect("delete should succeed");

    // Verify index and manifest are gone
    assert!(
        !op.exists(&index_key).await.unwrap(),
        "index entry should be gone after delete"
    );
    assert!(
        !op.exists(&upload.remote_path).await.unwrap(),
        "manifest should be gone after delete"
    );

    // Verify state cache entry is gone
    assert!(
        state.get(&src).is_none(),
        "state cache entry should be removed after delete"
    );
}
