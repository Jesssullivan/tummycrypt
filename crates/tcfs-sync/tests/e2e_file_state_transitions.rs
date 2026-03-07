//! E2E test: file modification in various sync states
//!
//! Tests that the sync engine correctly detects and handles file modifications:
//!   - Modified files are re-uploaded (not skipped)
//!   - Size changes (grow/shrink/truncate) are handled
//!   - State cache reflects final content after rapid overwrites
//!   - Device identity and vclock are preserved across modifications
//!   - State cache persists correctly across open/close cycles

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::engine::{download_file_with_device, upload_file_with_device};
use tcfs_sync::state::StateCache;

fn memory_operator() -> Operator {
    Operator::new(opendal::services::Memory::default())
        .expect("memory operator")
        .finish()
}

fn write_test_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).expect("write test file");
    path
}

/// Upload a file, overwrite it with new content, upload again.
/// The second upload must NOT be skipped (engine detects modification).
#[tokio::test]
async fn modify_synced_file_triggers_re_upload() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/modify-reupload";

    let original = b"original content before modification";
    let src = write_test_file(tmp.path(), "src/doc.txt", original);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // First upload
    let upload1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("first upload");

    assert!(!upload1.skipped, "first upload should not be skipped");
    let hash1 = upload1.hash.clone();

    // Overwrite with new content
    std::fs::write(&src, b"modified content after overwrite").expect("overwrite file");

    // Second upload — must detect modification
    let upload2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("second upload");

    assert!(
        !upload2.skipped,
        "second upload should NOT be skipped after modification"
    );
    assert_ne!(
        upload2.hash, hash1,
        "hash should change after content modification"
    );
}

/// Upload a small file (100 bytes), overwrite with a much larger file (100KB).
/// Verify bytes and chunks increase on re-upload.
#[tokio::test]
async fn overwrite_with_larger_content() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/overwrite-larger";

    let small_content = vec![0x42u8; 100];
    let src = write_test_file(tmp.path(), "src/data.bin", &small_content);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Upload small file
    let upload_small = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("data.bin"),
        None,
    )
    .await
    .expect("upload small");

    assert!(!upload_small.skipped);
    assert_eq!(upload_small.bytes, 100);

    // Overwrite with 100KB
    let large_content = vec![0xABu8; 100 * 1024];
    std::fs::write(&src, &large_content).expect("overwrite with large content");

    // Re-upload
    let upload_large = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("data.bin"),
        None,
    )
    .await
    .expect("upload large");

    assert!(!upload_large.skipped, "larger file should not be skipped");
    assert_eq!(upload_large.bytes, 100 * 1024);
    assert!(
        upload_large.bytes > upload_small.bytes,
        "bytes should increase: {} > {}",
        upload_large.bytes,
        upload_small.bytes
    );
    assert!(
        upload_large.chunks >= upload_small.chunks,
        "chunks should not decrease: {} >= {}",
        upload_large.chunks,
        upload_small.chunks
    );
}

/// Upload a file with content, truncate to 0 bytes, re-upload.
/// Verify the upload succeeds with 0 bytes.
#[tokio::test]
async fn truncate_to_zero_bytes() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/truncate-zero";

    let content = b"some content that will be truncated to nothing";
    let src = write_test_file(tmp.path(), "src/truncated.txt", content);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Upload with content
    let upload1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("truncated.txt"),
        None,
    )
    .await
    .expect("upload with content");

    assert!(!upload1.skipped);
    assert!(upload1.bytes > 0);

    // Truncate to 0 bytes
    std::fs::write(&src, b"").expect("truncate file");

    // Re-upload truncated file
    let upload2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("truncated.txt"),
        None,
    )
    .await
    .expect("upload truncated");

    assert!(!upload2.skipped, "truncated file should not be skipped");
    assert_eq!(upload2.bytes, 0, "truncated file should have 0 bytes");
}

/// Device A uploads, Device B downloads, Device B removes from state cache,
/// modifies the local copy, then re-uploads. Verify the new upload succeeds
/// with updated content and hash.
#[tokio::test]
async fn unsync_then_modify_then_resync() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/unsync-modify-resync";

    let original = b"original content from device-a";
    let src_a = write_test_file(tmp.path(), "src_a/shared.txt", original);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");

    // Device A uploads
    let upload_a = upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("shared.txt"),
        None,
    )
    .await
    .expect("device-a upload");

    assert!(!upload_a.skipped);

    // Device B downloads
    let dst_b = tmp.path().join("dst_b/shared.txt");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    download_file_with_device(
        &op,
        &upload_a.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("device-b download");

    assert_eq!(std::fs::read(&dst_b).unwrap(), original);
    assert!(
        state_b.get(&dst_b).is_some(),
        "state_b should have entry after download"
    );

    // Device B removes from state cache (unsync)
    state_b.remove(&dst_b);
    state_b.flush().unwrap();
    assert!(
        state_b.get(&dst_b).is_none(),
        "entry should be gone after unsync"
    );

    // Device B modifies the local copy
    let modified = b"modified content by device-b after unsync";
    std::fs::write(&dst_b, modified).expect("modify local copy");

    // Device B re-uploads
    let upload_b = upload_file_with_device(
        &op,
        &dst_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("shared.txt"),
        None,
    )
    .await
    .expect("device-b re-upload");

    assert!(
        !upload_b.skipped,
        "re-upload after unsync+modify should not be skipped"
    );
    assert_ne!(
        upload_b.hash, upload_a.hash,
        "hash should differ after modification"
    );
    assert_eq!(upload_b.bytes, modified.len() as u64);
}

/// Write file, upload, write new content, upload, write newer content, upload.
/// Each upload should not be skipped. Final state cache hash matches last content.
#[tokio::test]
async fn rapid_overwrites_final_state_wins() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/rapid-overwrites";

    let src = write_test_file(tmp.path(), "src/rapid.txt", b"version 1");
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Upload v1
    let upload1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("rapid.txt"),
        None,
    )
    .await
    .expect("upload v1");

    assert!(!upload1.skipped, "v1 upload should not be skipped");

    // Overwrite with v2 and upload
    std::fs::write(&src, b"version 2 with more data").expect("write v2");
    let upload2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("rapid.txt"),
        None,
    )
    .await
    .expect("upload v2");

    assert!(!upload2.skipped, "v2 upload should not be skipped");

    // Overwrite with v3 and upload
    let v3_content = b"version 3 final content with even more data added";
    std::fs::write(&src, v3_content).expect("write v3");
    let upload3 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("rapid.txt"),
        None,
    )
    .await
    .expect("upload v3");

    assert!(!upload3.skipped, "v3 upload should not be skipped");

    // Verify final state cache matches v3 content
    let cached = state.get(&src).expect("state cache entry");
    let v3_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(v3_content));
    assert_eq!(
        cached.blake3, v3_hash,
        "state cache hash should match the last written content"
    );
    assert_eq!(cached.size, v3_content.len() as u64);

    // All three hashes should be different
    assert_ne!(upload1.hash, upload2.hash, "v1 and v2 hashes should differ");
    assert_ne!(upload2.hash, upload3.hash, "v2 and v3 hashes should differ");
    assert_ne!(upload1.hash, upload3.hash, "v1 and v3 hashes should differ");
}

/// Upload as device-a, modify file, re-upload as device-a.
/// Verify device_id is still device-a and vclock for device-a has incremented.
#[tokio::test]
async fn modify_does_not_change_device_id() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/device-id-stable";

    let src = write_test_file(tmp.path(), "src/owned.txt", b"initial content");
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // First upload as device-a
    let upload1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("owned.txt"),
        None,
    )
    .await
    .expect("first upload");

    assert!(!upload1.skipped);

    let cached1 = state.get(&src).expect("state entry after first upload");
    let vclock1_a = cached1.vclock.get("device-a");
    assert!(
        vclock1_a > 0,
        "vclock for device-a should be > 0 after first upload"
    );
    assert_eq!(cached1.device_id, "device-a");

    // Modify and re-upload as device-a
    std::fs::write(&src, b"modified content by same device").expect("modify file");

    let upload2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("owned.txt"),
        None,
    )
    .await
    .expect("second upload");

    assert!(!upload2.skipped, "modified file should not be skipped");

    let cached2 = state.get(&src).expect("state entry after second upload");
    assert_eq!(
        cached2.device_id, "device-a",
        "device_id should still be device-a"
    );

    let vclock2_a = cached2.vclock.get("device-a");
    assert!(
        vclock2_a > vclock1_a,
        "vclock for device-a should increment: {} > {}",
        vclock2_a,
        vclock1_a
    );
}

/// Upload a file, flush state cache, open a NEW StateCache from the same path.
/// Verify the entry persists with correct hash and vclock.
#[tokio::test]
async fn state_cache_persistence_across_opens() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/file-state/persistence";

    let content = b"content that must survive state cache reopen";
    let src = write_test_file(tmp.path(), "src/persistent.txt", content);
    let state_path = tmp.path().join("state.db");

    let hash_before;
    let vclock_a_before;
    {
        let mut state = StateCache::open(&state_path).expect("open state");

        // Upload with device identity
        let upload = upload_file_with_device(
            &op,
            &src,
            prefix,
            &mut state,
            None,
            "device-a",
            Some("persistent.txt"),
            None,
        )
        .await
        .expect("upload");

        assert!(!upload.skipped);

        let cached = state.get(&src).expect("state entry");
        hash_before = cached.blake3.clone();
        vclock_a_before = cached.vclock.get("device-a");
        assert!(!hash_before.is_empty(), "hash should be non-empty");
        assert!(vclock_a_before > 0, "vclock should be > 0");

        // Explicitly flush
        state.flush().unwrap();
    }
    // state is dropped here

    // Open a NEW StateCache from the same path
    let state2 = StateCache::open(&state_path).expect("reopen state");

    let cached2 = state2
        .get(&src)
        .expect("entry should persist across reopen");
    assert_eq!(
        cached2.blake3, hash_before,
        "hash should match after reopen"
    );
    assert_eq!(
        cached2.vclock.get("device-a"),
        vclock_a_before,
        "vclock for device-a should match after reopen"
    );
    assert_eq!(cached2.device_id, "device-a");
    assert_eq!(cached2.size, content.len() as u64);
}
