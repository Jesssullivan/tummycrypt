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

fn seed_state_at_new_path(state: &mut StateCache, tracked_path: &Path, new_path: &Path) {
    let mut seeded = state
        .get(tracked_path)
        .expect("tracked state for lineage seed")
        .clone();
    seeded.mtime = 0;
    state.set(new_path, seeded);
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
    seed_state_at_new_path(&mut state_b, &dst_b, &src_b);

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

/// Test case 3: Both devices modify from the same baseline. Once one device
/// publishes the rel_path, the other should be stopped by VectorClock-based
/// conflict detection rather than silently publishing a second winner.
#[tokio::test]
async fn two_device_simultaneous_conflict_detection() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/two-device/conflict";

    // Step 1: Device A uploads initial version
    let content_a_v1 = b"initial content from device-a";
    let src_a_v1 = write_test_file(tmp.path(), "src_a/shared.txt", content_a_v1);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");

    let upload_a_v1 = upload_file_with_device(
        &op,
        &src_a_v1,
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

    // Step 3: Both modify and push independently
    let content_a_v2 = b"device-a made independent changes to the document";
    let src_a_v2 = write_test_file(tmp.path(), "src_a2/shared.txt", content_a_v2);
    seed_state_at_new_path(&mut state_a, &src_a_v1, &src_a_v2);

    let content_b_v2 = b"device-b also made different independent changes";
    let src_b_v2 = write_test_file(tmp.path(), "src_b2/shared.txt", content_b_v2);
    seed_state_at_new_path(&mut state_b, &dst_b, &src_b_v2);

    // Device A publishes first. Device B should then be rejected as a
    // concurrent modifier for the same rel_path.
    let upload_a_v2 = upload_file_with_device(
        &op,
        &src_a_v2,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("shared.txt"),
        None,
    )
    .await
    .expect("device-a upload v2");

    let upload_b_v2 = upload_file_with_device(
        &op,
        &src_b_v2,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("shared.txt"),
        None,
    )
    .await
    .expect("device-b upload v2");

    assert!(!upload_a_v2.skipped);
    assert!(
        upload_b_v2.skipped,
        "second independent writer should be stopped by conflict detection"
    );
    match &upload_b_v2.outcome {
        Some(tcfs_sync::conflict::SyncOutcome::Conflict(info)) => {
            assert_eq!(info.rel_path, "shared.txt");
            assert_eq!(info.local_device, "device-b");
            assert_eq!(info.remote_device, "device-a");
        }
        other => panic!("expected Conflict for second writer, got: {:?}", other),
    }

    // Step 4: Compare the recorded local/remote clocks to verify the conflict
    // is concurrent, not a stale remote overwrite.
    let vclock_a = state_a.get(&src_a_v2).expect("A state").vclock.clone();
    let vclock_b = state_b.get(&src_b_v2).expect("B state").vclock.clone();

    // The vclocks should be concurrent (neither dominates)
    assert!(
        vclock_a.is_concurrent(&vclock_b),
        "independent modifications should produce concurrent vclocks: A={:?}, B={:?}",
        vclock_a,
        vclock_b
    );

    // compare_clocks should return Conflict
    let outcome = tcfs_sync::conflict::compare_clocks(
        &vclock_b,
        &vclock_a,
        &state_b.get(&src_b_v2).expect("B state").blake3,
        &upload_a_v2.hash,
        "shared.txt",
        "device-b",
        "device-a",
    );

    match outcome {
        tcfs_sync::conflict::SyncOutcome::Conflict(info) => {
            assert_eq!(info.rel_path, "shared.txt");
            assert_eq!(info.local_device, "device-b");
            assert_eq!(info.remote_device, "device-a");
            assert_ne!(
                info.local_blake3, info.remote_blake3,
                "conflicting versions should have different hashes"
            );
        }
        other => panic!("expected Conflict from vclock comparison, got: {:?}", other),
    }
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
    seed_state_at_new_path(&mut state_b, &dst_b, &src_b);
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
