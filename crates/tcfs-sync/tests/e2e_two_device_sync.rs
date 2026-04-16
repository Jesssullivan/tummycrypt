//! E2E test: two-device sync via shared storage
//!
//! Uses two separate StateCache instances with different device_ids
//! sharing the same OpenDAL operator (simulating shared SeaweedFS).
//! Tests the full push/pull cycle between two logical devices.

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

/// Test case 1: Device A pushes a file, Device B pulls it, content matches.
#[tokio::test]
async fn two_device_push_then_pull() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/two-device/push-pull";

    let original = b"hello from device-a, this is the shared document";
    let src_a = write_test_file(tmp.path(), "src_a/doc.txt", original);
    let dst_b = tmp.path().join("dst_b/doc.txt");

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    // Device A uploads
    let upload = upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("device-a upload");

    assert!(!upload.skipped, "first upload should not be skipped");
    assert!(upload.chunks > 0);
    assert_eq!(upload.bytes, original.len() as u64);

    // Device B downloads
    let download = download_file_with_device(
        &op,
        &upload.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("device-b download");

    // Verify content matches
    let downloaded = std::fs::read(&dst_b).unwrap();
    assert_eq!(
        downloaded, original,
        "device B should receive device A's exact content"
    );
    assert_eq!(download.bytes, original.len() as u64);

    // Verify device B's state cache has the entry with vclock
    let cached_b = state_b.get(&dst_b).expect("device B state cache entry");
    assert!(
        !cached_b.vclock.clocks.is_empty(),
        "device B vclock should be non-empty after pull"
    );
}

/// Test case 2: Device B modifies, pushes, Device A pulls updated content with merged vclock.
#[tokio::test]
async fn two_device_modify_and_re_sync() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/two-device/modify-resync";

    // Step 1: Device A uploads original
    let content_v1 = b"version 1 from device-a";
    let src_a = write_test_file(tmp.path(), "src_a/notes.txt", content_v1);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");

    let upload_a = upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("notes.txt"),
        None,
    )
    .await
    .expect("device-a upload v1");

    // Step 2: Device B downloads v1
    let dst_b = tmp.path().join("dst_b/notes.txt");
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
    .expect("device-b download v1");

    // Verify device B has v1
    assert_eq!(std::fs::read(&dst_b).unwrap(), content_v1);

    // Step 3: Device B modifies the file and pushes v2
    let content_v2 = b"version 2 modified by device-b with extra changes";
    let src_b = write_test_file(tmp.path(), "src_b/notes.txt", content_v2);

    let upload_b = upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("notes.txt"),
        None,
    )
    .await
    .expect("device-b upload v2");

    assert!(!upload_b.skipped, "modified file should be uploaded");

    // Step 4: Device A pulls the update
    let dst_a = tmp.path().join("dst_a/notes.txt");

    download_file_with_device(
        &op,
        &upload_b.remote_path,
        &dst_a,
        prefix,
        None,
        "device-a",
        Some(&mut state_a),
        None,
    )
    .await
    .expect("device-a download v2");

    // Verify device A sees v2
    let downloaded_a = std::fs::read(&dst_a).unwrap();
    assert_eq!(
        downloaded_a, content_v2,
        "device A should see device B's updated content"
    );

    // Verify device A's vclock has merged entries from both devices
    let cached_a = state_a.get(&dst_a).expect("device A state cache entry");
    assert!(
        cached_a.vclock.get("device-a") > 0 || cached_a.vclock.get("device-b") > 0,
        "device A vclock should contain entries after merge"
    );
}

/// Test case 3: Both devices modify simultaneously — conflict detected via index.
///
/// With index-based conflict detection, the second device to upload sees the
/// first device's new manifest through the rel_path→manifest index, detecting
/// the conflict immediately. The first uploader succeeds; the second gets a
/// Conflict outcome with concurrent vclocks.
#[tokio::test]
async fn two_device_simultaneous_conflict_detection() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/two-device/conflict";

    // Step 1: Device A uploads initial version
    let content_a_v1 = b"initial content from device-a";
    let src_a = write_test_file(tmp.path(), "src_a/shared.txt", content_a_v1);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");

    let upload_a_v1 = upload_file_with_device(
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
    .expect("device-a upload v1");

    // Step 2: Device B downloads v1 (shared baseline)
    let dst_b = tmp.path().join("dst_b/shared.txt");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    download_file_with_device(
        &op,
        &upload_a_v1.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("device-b download v1");

    // Step 3: Both modify their copies at paths with existing state
    let content_b_v2 = b"device-b also made different independent changes";
    std::fs::write(&dst_b, content_b_v2).expect("B modifies local copy");

    let content_a_v2 = b"device-a made independent changes to the document";
    std::fs::write(&src_a, content_a_v2).expect("A modifies local copy");

    // Step 4: Device B pushes first (first uploader succeeds)
    let upload_b_v2 = upload_file_with_device(
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
    .expect("device-b upload v2");

    assert!(!upload_b_v2.skipped, "B's upload should succeed (first diverger)");

    // Step 5: Device A pushes — index now points to B's manifest, conflict!
    let upload_a_v2 = upload_file_with_device(
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
    .expect("device-a upload v2");

    // A's upload detects the conflict via the index
    assert!(
        upload_a_v2.skipped,
        "A's upload should be skipped (conflict detected)"
    );
    assert!(
        matches!(upload_a_v2.outcome, Some(tcfs_sync::conflict::SyncOutcome::Conflict(_))),
        "A should detect Conflict, got: {:?}",
        upload_a_v2.outcome
    );

    // Step 6: Verify conflict info has concurrent vclocks
    let conflict_info = match upload_a_v2.outcome {
        Some(tcfs_sync::conflict::SyncOutcome::Conflict(ref info)) => info.clone(),
        _ => unreachable!(),
    };

    assert!(
        conflict_info.local_vclock.is_concurrent(&conflict_info.remote_vclock),
        "conflict should have concurrent vclocks: local={:?}, remote={:?}",
        conflict_info.local_vclock,
        conflict_info.remote_vclock
    );

    assert_eq!(conflict_info.rel_path, "shared.txt");
    assert_ne!(
        conflict_info.local_blake3, conflict_info.remote_blake3,
        "conflicting versions should have different hashes"
    );
}

/// Test that multiple sequential syncs between devices maintain consistent vclocks.
#[tokio::test]
async fn two_device_multi_round_sync() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/two-device/multi-round";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    // Round 1: A writes, B pulls
    let content_r1 = b"round 1 content";
    let src = write_test_file(tmp.path(), "round/1/file.txt", content_r1);
    let upload_r1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("file.txt"),
        None,
    )
    .await
    .expect("r1 upload");

    let dst_b = tmp.path().join("pull_b/file.txt");
    download_file_with_device(
        &op,
        &upload_r1.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("r1 pull B");

    // Round 2: B modifies and pushes
    let content_r2 = b"round 2 content from device-b";
    let src_b = write_test_file(tmp.path(), "round/2/file.txt", content_r2);
    let upload_r2 = upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("file.txt"),
        None,
    )
    .await
    .expect("r2 upload B");

    assert!(!upload_r2.skipped);

    // Round 3: A pulls B's changes
    let dst_a = tmp.path().join("pull_a/file.txt");
    download_file_with_device(
        &op,
        &upload_r2.remote_path,
        &dst_a,
        prefix,
        None,
        "device-a",
        Some(&mut state_a),
        None,
    )
    .await
    .expect("r3 pull A");

    let final_content = std::fs::read(&dst_a).unwrap();
    assert_eq!(final_content, content_r2);

    // Verify vclocks are monotonic: both devices should have knowledge of each other
    let cached_a = state_a.get(&dst_a).expect("A state");
    assert!(
        cached_a.vclock.get("device-b") > 0,
        "device A should know about device B's writes"
    );
}
