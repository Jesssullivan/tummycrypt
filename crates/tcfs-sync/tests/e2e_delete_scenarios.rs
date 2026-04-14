//! E2E test: delete-related sync scenarios
//!
//! Tests delete workflows in TCFS sync:
//!   1. Local delete + state removal
//!   2. Delete and recreate at same path
//!   3. Delete vs modify cross-device conflict
//!   4. Remote persistence after local delete
//!   5. Selective delete among multiple files
//!   6. Recreate deleted file with different size

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::conflict::{compare_clocks, SyncOutcome};
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

/// Test case 1: Upload file, verify state entry exists, delete file + remove
/// from state, verify state cache is empty for that path.
#[tokio::test]
async fn delete_synced_file_locally_then_state_remove() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/delete/state-remove";

    let content = b"file that will be deleted from disk and state";
    let src = write_test_file(tmp.path(), "src/deleteme.txt", content);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Upload
    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("deleteme.txt"),
        None,
    )
    .await
    .expect("upload");

    assert!(!upload.skipped, "first upload should not be skipped");

    // Verify state cache entry exists
    let cached = state.get(&src);
    assert!(
        cached.is_some(),
        "state cache should have entry after upload"
    );

    // Delete file from disk
    std::fs::remove_file(&src).expect("delete file from disk");
    assert!(!src.exists(), "file should be gone from disk");

    // Remove from state cache
    state.remove(&src);

    // Verify state cache is now empty for that path
    let cached_after = state.get(&src);
    assert!(
        cached_after.is_none(),
        "state cache should be empty after remove"
    );
}

/// Test case 2: Upload file with content "v1", delete, create new file at same
/// path with content "v2", upload again. New hash should differ from original.
#[tokio::test]
async fn delete_and_recreate_same_path() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/delete/recreate";

    let content_v1 = b"v1 content for delete-and-recreate test";
    let src = write_test_file(tmp.path(), "src/cycle.txt", content_v1);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Upload v1
    let upload_v1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("cycle.txt"),
        None,
    )
    .await
    .expect("upload v1");

    assert!(!upload_v1.skipped);
    let hash_v1 = upload_v1.hash.clone();
    let previous_state = state.get(&src).expect("state after v1 upload").clone();

    // Delete the file from disk and state
    std::fs::remove_file(&src).expect("delete v1 from disk");
    state.remove(&src);

    // Recreate at the same path with different content. Preserve the prior
    // sync lineage and tick the local clock to model a new local edit at the
    // same rel_path after the deletion.
    write_test_file(tmp.path(), "src/cycle.txt", content_v1);
    let mut recreated_state = previous_state;
    recreated_state.vclock.tick("device-a");
    state.set(&src, recreated_state);
    let content_v2 = b"v2 content is completely different from v1";
    std::fs::write(&src, content_v2).expect("write recreated v2 content");

    // Upload v2
    let upload_v2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("cycle.txt"),
        None,
    )
    .await
    .expect("upload v2");

    assert!(
        !upload_v2.skipped,
        "recreated file should be uploaded, got outcome: {:?}",
        upload_v2.outcome
    );
    assert_ne!(
        upload_v2.hash, hash_v1,
        "new content should produce a different hash"
    );
}

/// Test case 3: Device A uploads, Device B downloads. Device A "deletes" (removes
/// from state + ticks vclock). Device B modifies and pushes. The vclocks should
/// be concurrent (conflict) since A's delete and B's modify are independent.
#[tokio::test]
async fn delete_vs_modify_cross_device_conflict() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/delete/cross-device-conflict";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    // Device A uploads initial version
    let content_v1 = b"shared document for delete-vs-modify test";
    let src_a = write_test_file(tmp.path(), "src_a/shared.txt", content_v1);

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

    // Device B downloads the file
    let dst_b = tmp.path().join("dst_b/shared.txt");
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

    assert_eq!(std::fs::read(&dst_b).unwrap(), content_v1);

    // Device A "deletes": remove from state and tick vclock manually
    let mut vclock_a_delete = state_a.get(&src_a).expect("A state").vclock.clone();
    state_a.remove(&src_a);
    std::fs::remove_file(&src_a).expect("delete from A's disk");
    vclock_a_delete.tick("device-a"); // tick for the delete event

    // Device B modifies and pushes independently. Seed B's local path with the
    // downloaded state and tick its clock to model an edit after observing A's
    // prior version.
    let src_b = write_test_file(tmp.path(), "src_b/shared.txt", content_v1);
    let mut b_edit_state = state_b.get(&dst_b).expect("B downloaded state").clone();
    b_edit_state.vclock.tick("device-b");
    state_b.set(&src_b, b_edit_state);
    let content_b_v2 = b"device B modified this document while A deleted it";
    std::fs::write(&src_b, content_b_v2).expect("device-b writes modified content");

    let upload_b = upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("shared.txt"),
        None,
    )
    .await
    .expect("device-b upload v2");

    assert!(
        !upload_b.skipped,
        "device-b edit should upload, got outcome: {:?}",
        upload_b.outcome
    );

    // Get device B's vclock after upload
    let vclock_b = state_b.get(&src_b).expect("B state").vclock.clone();

    // The vclocks should be concurrent (A's delete and B's modify are independent)
    assert!(
        vclock_a_delete.is_concurrent(&vclock_b),
        "delete vs modify should produce concurrent vclocks: A={:?}, B={:?}",
        vclock_a_delete,
        vclock_b
    );

    // compare_clocks should detect Conflict
    let outcome = compare_clocks(
        &vclock_a_delete,
        &vclock_b,
        "deleted",      // A's "hash" (file is gone)
        &upload_b.hash, // B's hash
        "shared.txt",
        "device-a",
        "device-b",
    );

    assert!(
        matches!(outcome, SyncOutcome::Conflict(_)),
        "delete vs modify should be a Conflict, got: {:?}",
        outcome
    );
}

/// Test case 4: Device A uploads, then deletes locally + removes from state.
/// Device B can still pull the file from remote storage.
#[tokio::test]
async fn remote_file_survives_local_delete() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/delete/remote-survives";

    let content = b"this file will persist in remote storage after local delete";
    let src_a = write_test_file(tmp.path(), "src_a/persist.txt", content);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");

    // Device A uploads
    let upload = upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("persist.txt"),
        None,
    )
    .await
    .expect("device-a upload");

    let remote_path = upload.remote_path.clone();

    // Device A deletes locally
    std::fs::remove_file(&src_a).expect("delete from A's disk");
    state_a.remove(&src_a);
    assert!(!src_a.exists(), "file should be gone from A's disk");
    assert!(state_a.get(&src_a).is_none(), "A's state should be empty");

    // Device B can still pull from remote
    let dst_b = tmp.path().join("dst_b/persist.txt");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    let download = download_file_with_device(
        &op,
        &remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("device-b should still be able to pull");

    let downloaded = std::fs::read(&dst_b).unwrap();
    assert_eq!(
        downloaded, content,
        "remote content should match original despite A's local delete"
    );
    assert_eq!(download.bytes, content.len() as u64);
}

/// Test case 5: Upload 3 files. Delete file #2 from disk + state. Verify files
/// #1 and #3 remain in state. Re-upload #1 and #3 — both should be skipped.
#[tokio::test]
async fn delete_one_of_multiple_files() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/delete/selective";

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Upload 3 files
    let src1 = write_test_file(tmp.path(), "src/file1.txt", b"content of file one");
    let src2 = write_test_file(tmp.path(), "src/file2.txt", b"content of file two");
    let src3 = write_test_file(tmp.path(), "src/file3.txt", b"content of file three");

    upload_file_with_device(
        &op,
        &src1,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("file1.txt"),
        None,
    )
    .await
    .expect("upload file1");

    upload_file_with_device(
        &op,
        &src2,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("file2.txt"),
        None,
    )
    .await
    .expect("upload file2");

    upload_file_with_device(
        &op,
        &src3,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("file3.txt"),
        None,
    )
    .await
    .expect("upload file3");

    // Verify all 3 are in state
    assert!(state.get(&src1).is_some(), "file1 should be in state");
    assert!(state.get(&src2).is_some(), "file2 should be in state");
    assert!(state.get(&src3).is_some(), "file3 should be in state");

    // Delete file #2 from disk and state
    std::fs::remove_file(&src2).expect("delete file2 from disk");
    state.remove(&src2);

    // Verify file #2 is gone, #1 and #3 remain
    assert!(state.get(&src1).is_some(), "file1 should still be in state");
    assert!(
        state.get(&src2).is_none(),
        "file2 should be gone from state"
    );
    assert!(state.get(&src3).is_some(), "file3 should still be in state");

    // Re-upload #1 and #3 — both should be skipped (unchanged)
    let reupload1 = upload_file_with_device(
        &op,
        &src1,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("file1.txt"),
        None,
    )
    .await
    .expect("re-upload file1");

    let reupload3 = upload_file_with_device(
        &op,
        &src3,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("file3.txt"),
        None,
    )
    .await
    .expect("re-upload file3");

    assert!(reupload1.skipped, "file1 should be skipped (unchanged)");
    assert!(reupload3.skipped, "file3 should be skipped (unchanged)");
}

/// Test case 6: Upload 1KB file, delete from disk + state, create 10KB file at
/// same path, upload again. New upload should reflect 10KB size.
#[tokio::test]
async fn recreate_deleted_file_with_different_size() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/delete/size-change";

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Create and upload 1KB file
    let content_1kb: Vec<u8> = (0u64..1024)
        .map(|i| (i.wrapping_mul(7) ^ (i >> 3)) as u8)
        .collect();
    let src = write_test_file(tmp.path(), "src/resized.bin", &content_1kb);

    let upload_1kb = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("resized.bin"),
        None,
    )
    .await
    .expect("upload 1KB");

    assert!(!upload_1kb.skipped);
    assert_eq!(upload_1kb.bytes, 1024);
    let previous_state = state.get(&src).expect("state after 1KB upload").clone();

    // Delete from disk and state
    std::fs::remove_file(&src).expect("delete 1KB file");
    state.remove(&src);

    // Recreate the tracked path and preserve/tick the prior lineage so the new
    // upload is compared as a descendant edit rather than an unrelated file.
    write_test_file(tmp.path(), "src/resized.bin", &content_1kb);
    let mut recreated_state = previous_state;
    recreated_state.vclock.tick("device-a");
    state.set(&src, recreated_state);

    // Create 10KB file at the same path
    let content_10kb: Vec<u8> = (0u64..10240)
        .map(|i| (i.wrapping_mul(13) ^ (i >> 5)) as u8)
        .collect();
    std::fs::write(&src, &content_10kb).expect("write recreated 10KB file");

    // Upload the 10KB file
    let upload_10kb = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("resized.bin"),
        None,
    )
    .await
    .expect("upload 10KB");

    assert!(
        !upload_10kb.skipped,
        "new 10KB file should not be skipped, got outcome: {:?}",
        upload_10kb.outcome
    );
    assert_eq!(upload_10kb.bytes, 10240, "upload should reflect 10KB size");
    assert_ne!(
        upload_10kb.hash, upload_1kb.hash,
        "different size/content should produce different hash"
    );
}
