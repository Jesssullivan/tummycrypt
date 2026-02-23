//! Integration tests: conflict resolution patterns with in-memory storage
//!
//! Verifies that the conflict resolution primitives (keep_local, keep_remote,
//! keep_both, unsync) work correctly against the sync engine and state cache.

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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).expect("write test file");
    path
}

/// Simulates keep_remote: device A uploads, device B downloads the remote version.
#[tokio::test]
async fn resolve_keep_remote_downloads_remote() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/resolve-remote";

    // Device A uploads its version
    let content_a = b"device A's version of the file";
    let src_a = write_test_file(tmp.path(), "src_a/doc.txt", content_a);
    let mut state_a = tcfs_sync::state::StateCache::open(&tmp.path().join("state_a.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("doc.txt"),
    )
    .await
    .expect("device A upload");

    assert!(!upload.skipped);

    // Device B "resolves" by downloading remote (device A's version)
    let dst_b = tmp.path().join("dst_b/doc.txt");
    let mut state_b = tcfs_sync::state::StateCache::open(&tmp.path().join("state_b.db")).unwrap();

    let download = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
    )
    .await
    .expect("device B download");

    let downloaded = std::fs::read(&dst_b).unwrap();
    assert_eq!(
        downloaded, content_a,
        "keep_remote should get device A's content"
    );
    assert_eq!(download.bytes, content_a.len() as u64);

    // State cache should have entry with vclock
    let cached = state_b.get(&dst_b).expect("state cache entry");
    assert!(!cached.vclock.clocks.is_empty());
}

/// Simulates keep_local: device B re-uploads its local version with ticked vclock.
#[tokio::test]
async fn resolve_keep_local_re_uploads() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/resolve-local";

    // Device A uploads first
    let content_a = b"original from device A";
    let src_a = write_test_file(tmp.path(), "src_a/notes.txt", content_a);
    let mut state_a = tcfs_sync::state::StateCache::open(&tmp.path().join("state_a.db")).unwrap();

    let _upload_a = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("notes.txt"),
    )
    .await
    .expect("device A upload");

    // Device B uploads its own version (keep_local scenario: re-upload with ticked clock)
    let content_b = b"device B's local version that wins";
    let src_b = write_test_file(tmp.path(), "src_b/notes.txt", content_b);
    let mut state_b = tcfs_sync::state::StateCache::open(&tmp.path().join("state_b.db")).unwrap();

    // Tick B's clock to make it newer
    let mut vclock = tcfs_sync::conflict::VectorClock::new();
    vclock.tick("device-b");
    vclock.tick("device-b"); // Tick twice so B dominates

    let upload_b = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("notes.txt"),
    )
    .await
    .expect("device B re-upload");

    assert!(!upload_b.skipped);

    // Verify the remote now has B's content
    let verify_path = tmp.path().join("verify/notes.txt");
    let dl =
        tcfs_sync::engine::download_file(&op, &upload_b.remote_path, &verify_path, prefix, None)
            .await
            .expect("verify download");

    let verified = std::fs::read(&verify_path).unwrap();
    assert_eq!(
        verified, content_b,
        "remote should have device B's content after keep_local"
    );
    assert_eq!(dl.bytes, content_b.len() as u64);
}

/// Simulates keep_both: rename local + download remote to original path.
#[tokio::test]
async fn resolve_keep_both_preserves_files() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/resolve-both";

    // Device A uploads
    let content_a = b"device A content for keep_both";
    let src_a = write_test_file(tmp.path(), "src/report.txt", content_a);
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("report.txt"),
    )
    .await
    .expect("upload");

    // Simulate keep_both: local file gets renamed, remote downloaded to original
    let local_file = write_test_file(tmp.path(), "local/report.txt", b"local B content");
    let conflict_file = tmp.path().join("local/report.conflict-device-b.txt");

    // Rename local
    std::fs::rename(&local_file, &conflict_file).expect("rename local");
    assert!(conflict_file.exists(), "conflict copy should exist");
    assert!(!local_file.exists(), "original should be gone");

    // Download remote to original path
    let dl = tcfs_sync::engine::download_file_with_device(
        &op,
        &upload.remote_path,
        &local_file,
        prefix,
        None,
        "device-b",
        Some(&mut state),
    )
    .await
    .expect("download remote to original path");

    // Both files should exist
    assert!(
        local_file.exists(),
        "original path should have remote content"
    );
    assert!(conflict_file.exists(), "conflict copy should still exist");

    let original_content = std::fs::read(&local_file).unwrap();
    let conflict_content = std::fs::read(&conflict_file).unwrap();
    assert_eq!(
        original_content, content_a,
        "original should be remote (device A)"
    );
    assert_eq!(
        conflict_content, b"local B content",
        "conflict copy should be local (device B)"
    );
    assert_eq!(dl.bytes, content_a.len() as u64);
}

/// Unsync removes a path from state cache.
#[tokio::test]
async fn unsync_removes_from_state_cache() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "test/unsync";

    let content = b"file to unsync";
    let src = write_test_file(tmp.path(), "synced.txt", content);
    let mut state = tcfs_sync::state::StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push the file
    let _upload = tcfs_sync::engine::upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload");

    assert!(state.get(&src).is_some(), "should be in state cache");

    // Unsync: remove from state cache
    state.remove(&src);
    state.flush().unwrap();

    assert!(
        state.get(&src).is_none(),
        "should be removed from state cache"
    );
    assert!(
        src.exists(),
        "local file should still exist (unsync doesn't delete)"
    );
}
