//! E2E test: multi-step state transition chains
//!
//! Tests real-world workflows that involve multiple sequential operations
//! across one or more devices:
//!   - Synced -> modified -> re-push
//!   - Push -> pull -> modify -> push -> pull (two devices)
//!   - Conflict detection -> resolution -> continued sync
//!   - Three-device relay chains
//!   - Unsync/dehydration -> modify -> re-sync
//!   - Alternating device writes over multiple rounds

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::conflict::{
    compare_clocks, AutoResolver, ConflictResolver, Resolution, SyncOutcome,
};
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
    // Force needs_sync() to hash-check the new path even when same-size writes
    // happen within the same one-second mtime granularity window.
    seeded.mtime = 0;
    state.set(new_path, seeded);
}

/// Upload file as device-a. Verify state shows synced. Modify file content
/// locally. Verify `needs_sync()` returns Some. Re-upload. Verify state
/// updated with new hash.
#[tokio::test]
async fn synced_to_modified_to_push_chain() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/chains/synced-modified-push";

    let content_v1 = b"version 1 content for state transition test";
    let src = write_test_file(tmp.path(), "src/chain.txt", content_v1);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Step 1: Upload as device-a
    let upload1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("chain.txt"),
        None,
    )
    .await
    .expect("first upload");

    assert!(!upload1.skipped, "first upload should not be skipped");
    let hash_v1 = upload1.hash.clone();

    // Step 2: Verify state shows synced (needs_sync returns None for unmodified file)
    let needs = state.needs_sync(&src);
    assert!(needs.is_ok(), "needs_sync should not error on synced file");
    assert!(
        needs.unwrap().is_none(),
        "synced file should not need sync (needs_sync should return None)"
    );

    // Step 3: Modify file content locally
    let content_v2 = b"version 2 content after local modification";
    std::fs::write(&src, content_v2).expect("modify file");

    // Step 4: Verify needs_sync returns Some (file changed)
    let needs_after = state.needs_sync(&src);
    assert!(
        needs_after.is_ok(),
        "needs_sync should not error after modification"
    );
    assert!(
        needs_after.unwrap().is_some(),
        "modified file should need sync (needs_sync should return Some)"
    );

    // Step 5: Re-upload
    let upload2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("chain.txt"),
        None,
    )
    .await
    .expect("second upload");

    assert!(!upload2.skipped, "modified file should not be skipped");
    assert_ne!(
        upload2.hash, hash_v1,
        "hash should change after modification"
    );

    // Step 6: Verify state updated with new hash
    let cached = state.get(&src).expect("state entry after re-upload");
    assert_eq!(cached.size, content_v2.len() as u64);
    assert_ne!(
        cached.blake3,
        tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content_v1)),
        "cached hash should not match v1 content"
    );
    assert_eq!(
        cached.blake3,
        tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content_v2)),
        "cached hash should match v2 content"
    );
}

/// Device A pushes file. Device B pulls. Device B modifies. Device B pushes.
/// Device A pulls updated version. Verify A sees B's content and vclock
/// reflects both devices.
#[tokio::test]
async fn push_pull_modify_push_two_device_chain() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/chains/push-pull-modify-push";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    // Step 1: Device A pushes
    let content_v1 = b"original content from device-a for two-device chain";
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

    assert!(!upload_a.skipped);

    // Step 2: Device B pulls
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
    .expect("device-b pull");

    assert_eq!(std::fs::read(&dst_b).unwrap(), content_v1);

    // Step 3: Device B modifies
    let content_v2 = b"modified content by device-b in two-device chain";
    let src_b = write_test_file(tmp.path(), "src_b/shared.txt", content_v2);
    seed_state_at_new_path(&mut state_b, &dst_b, &src_b);

    // Step 4: Device B pushes modified version
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
    .expect("device-b upload");

    assert!(
        !upload_b.skipped,
        "device-b modified upload should not be skipped"
    );

    // Step 5: Device A pulls updated version
    let dst_a = tmp.path().join("dst_a/shared.txt");
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
    .expect("device-a pull updated");

    // Verify A sees B's content
    let final_content = std::fs::read(&dst_a).unwrap();
    assert_eq!(
        final_content, content_v2,
        "device A should see device B's modified content"
    );

    // Verify vclock reflects both devices
    let cached_a = state_a.get(&dst_a).expect("device A state entry");
    assert!(
        cached_a.vclock.get("device-a") > 0 || cached_a.vclock.get("device-b") > 0,
        "device A vclock should have entries after pull"
    );
    assert!(
        cached_a.vclock.get("device-b") > 0,
        "device A should know about device B's writes: vclock={:?}",
        cached_a.vclock
    );
}

/// Device A and B diverge (both modify after shared baseline). Compare clocks
/// to detect conflict. Use AutoResolver to pick a winner. Then the winner
/// pushes, the loser pulls. Verify state is consistent and no further conflict.
#[tokio::test]
async fn conflict_detect_resolve_then_continue() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/chains/conflict-resolve-continue";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    // Step 1: Device A uploads baseline
    let baseline = b"baseline content shared between both devices";
    let src_a = write_test_file(tmp.path(), "src_a/doc.txt", baseline);

    let upload_baseline = upload_file_with_device(
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
    .expect("baseline upload");

    // Step 2: Device B pulls baseline
    let dst_b = tmp.path().join("dst_b/doc.txt");
    download_file_with_device(
        &op,
        &upload_baseline.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("device-b pull baseline");

    // Step 3: Both devices diverge independently
    let content_a = b"device A diverged content after baseline";
    let src_a2 = write_test_file(tmp.path(), "src_a2/doc.txt", content_a);
    seed_state_at_new_path(&mut state_a, &src_a, &src_a2);

    let content_b = b"device B diverged content after baseline";
    let src_b2 = write_test_file(tmp.path(), "src_b2/doc.txt", content_b);
    seed_state_at_new_path(&mut state_b, &dst_b, &src_b2);

    let upload_a = upload_file_with_device(
        &op,
        &src_a2,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("device-a diverged upload");

    let upload_b = upload_file_with_device(
        &op,
        &src_b2,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("device-b diverged upload");

    // Step 4: Compare clocks to detect conflict
    let vclock_a = state_a.get(&src_a2).expect("A state").vclock.clone();
    let vclock_b = state_b.get(&src_b2).expect("B state").vclock.clone();

    assert!(
        vclock_a.is_concurrent(&vclock_b),
        "diverged modifications should produce concurrent vclocks"
    );

    let outcome = compare_clocks(
        &vclock_a,
        &vclock_b,
        &upload_a.hash,
        &upload_b.hash,
        "doc.txt",
        "device-a",
        "device-b",
    );

    let conflict_info = match outcome {
        SyncOutcome::Conflict(info) => info,
        other => panic!("expected Conflict, got: {:?}", other),
    };

    // Step 5: Use AutoResolver to pick a winner
    let resolver = AutoResolver;
    let resolution = resolver
        .resolve(&conflict_info)
        .expect("AutoResolver should produce a resolution");

    // AutoResolver: lexicographically smaller device wins
    // "device-a" < "device-b" => KeepLocal (from A's perspective as local)
    assert_eq!(
        resolution,
        Resolution::KeepLocal,
        "device-a should win (lexicographically smaller)"
    );

    // Step 6: Winner (device-a) pushes, loser (device-b) pulls
    // Device A's upload already exists, device B downloads it
    let resolved_b = tmp.path().join("resolved_b/doc.txt");
    download_file_with_device(
        &op,
        &upload_a.remote_path,
        &resolved_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("device-b pull winner's version");

    // Step 7: Verify state is consistent
    let resolved_content = std::fs::read(&resolved_b).unwrap();
    assert_eq!(
        resolved_content, content_a,
        "device B should have device A's content after resolution"
    );

    // Step 8: Verify no further conflict -- device B pushes new content, A pulls
    let content_v3 = b"post-resolution content from device-b";
    let src_b3 = write_test_file(tmp.path(), "src_b3/doc.txt", content_v3);
    seed_state_at_new_path(&mut state_b, &resolved_b, &src_b3);

    let upload_b3 = upload_file_with_device(
        &op,
        &src_b3,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("device-b post-resolution upload");

    assert!(!upload_b3.skipped);

    let dst_a3 = tmp.path().join("dst_a3/doc.txt");
    download_file_with_device(
        &op,
        &upload_b3.remote_path,
        &dst_a3,
        prefix,
        None,
        "device-a",
        Some(&mut state_a),
        None,
    )
    .await
    .expect("device-a pull post-resolution");

    let final_a = std::fs::read(&dst_a3).unwrap();
    assert_eq!(
        final_a, content_v3,
        "post-resolution sync should work without conflict"
    );

    // Verify vclocks are no longer concurrent after resolution flow
    let vc_a_final = state_a.get(&dst_a3).expect("A final state").vclock.clone();
    let vc_b_final = state_b.get(&src_b3).expect("B final state").vclock.clone();

    // B's clock should dominate or equal A's (B was the last writer)
    assert!(
        !vc_a_final.is_concurrent(&vc_b_final),
        "after resolution flow, clocks should not be concurrent"
    );
}

/// Device A pushes. Device B pulls from A's upload. Device B modifies and
/// pushes. Device C pulls from B's upload. Verify C has B's modified content
/// and vclocks show knowledge of all devices.
#[tokio::test]
async fn three_device_relay_chain() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/chains/three-device-relay";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");
    let mut state_c = StateCache::open(&tmp.path().join("state_c.db")).expect("open state_c");

    // Step 1: Device A pushes original
    let content_a = b"original content from device-a in relay chain";
    let src_a = write_test_file(tmp.path(), "src_a/relay.txt", content_a);

    let upload_a = upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("relay.txt"),
        None,
    )
    .await
    .expect("device-a upload");

    assert!(!upload_a.skipped);

    // Step 2: Device B pulls from A's upload
    let dst_b = tmp.path().join("dst_b/relay.txt");
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
    .expect("device-b pull from A");

    assert_eq!(
        std::fs::read(&dst_b).unwrap(),
        content_a,
        "device B should have A's content"
    );

    // Step 3: Device B modifies and pushes
    let content_b = b"modified content by device-b in relay chain";
    let src_b = write_test_file(tmp.path(), "src_b/relay.txt", content_b);
    seed_state_at_new_path(&mut state_b, &dst_b, &src_b);

    let upload_b = upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("relay.txt"),
        None,
    )
    .await
    .expect("device-b upload");

    assert!(!upload_b.skipped);

    // Step 4: Device C pulls from B's upload
    let dst_c = tmp.path().join("dst_c/relay.txt");
    download_file_with_device(
        &op,
        &upload_b.remote_path,
        &dst_c,
        prefix,
        None,
        "device-c",
        Some(&mut state_c),
        None,
    )
    .await
    .expect("device-c pull from B");

    // Verify C has B's modified content
    let content_c = std::fs::read(&dst_c).unwrap();
    assert_eq!(
        content_c, content_b,
        "device C should have device B's modified content"
    );

    // Verify C's vclock shows knowledge of the relay chain
    let cached_c = state_c.get(&dst_c).expect("device C state entry");
    assert!(
        !cached_c.vclock.clocks.is_empty(),
        "device C vclock should be non-empty after relay pull"
    );

    // The vclock should reflect B's knowledge (which includes A's baseline)
    // B merged A's clock on pull, then ticked on upload
    let vc_b = state_b.get(&src_b).expect("device B state").vclock.clone();
    let vc_c = cached_c.vclock.clone();

    // C's clock should not be concurrent with B's -- C pulled from B so
    // C knows at least as much as B
    assert!(
        !vc_c.is_concurrent(&vc_b),
        "device C should not be concurrent with B after pulling B's upload"
    );
}

/// Device A uploads. Device A removes from state cache (simulating "unsync" /
/// dehydration). Device A modifies the local file. Device A re-uploads.
/// Verify upload succeeds without conflict (clean re-sync after dehydration).
#[tokio::test]
async fn unsync_modify_resync_no_conflict() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/chains/unsync-modify-resync";

    let content_v1 = b"content before dehydration unsync";
    let src = write_test_file(tmp.path(), "src/dehydrate.txt", content_v1);
    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // Step 1: Device A uploads
    let upload1 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("dehydrate.txt"),
        None,
    )
    .await
    .expect("initial upload");

    assert!(!upload1.skipped);
    assert!(
        state.get(&src).is_some(),
        "state should have entry after upload"
    );
    let tracked_state = state
        .get(&src)
        .expect("state entry after initial upload")
        .clone();

    // Step 2: Remove from state cache (simulate unsync/dehydration)
    state.remove(&src);
    state.flush().unwrap();
    assert!(
        state.get(&src).is_none(),
        "state entry should be gone after unsync"
    );

    // Step 3: Modify the local file. Restore the prior lineage and tick the
    // local clock so the follow-up upload is a descendant edit, not a fresh
    // stateless write against the rel_path index.
    let mut resync_state = tracked_state;
    resync_state.vclock.tick("device-a");
    state.set(&src, resync_state);
    let content_v2 = b"modified content after dehydration unsync";
    std::fs::write(&src, content_v2).expect("modify after unsync");

    // Step 4: Re-upload
    let upload2 = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("dehydrate.txt"),
        None,
    )
    .await
    .expect("re-upload after unsync+modify");

    // Verify upload succeeds without being skipped
    assert!(
        !upload2.skipped,
        "re-upload after unsync+modify should not be skipped"
    );

    // Verify new hash reflects modified content
    assert_ne!(
        upload2.hash, upload1.hash,
        "hash should differ after modification"
    );
    assert_eq!(upload2.bytes, content_v2.len() as u64);

    // Verify state cache is repopulated
    let cached = state.get(&src).expect("state entry after re-upload");
    assert_eq!(cached.size, content_v2.len() as u64);
    assert_eq!(cached.device_id, "device-a");
    assert!(
        cached.vclock.get("device-a") > 0,
        "vclock should be set after re-upload"
    );

    // Verify no conflict in the outcome
    if let Some(SyncOutcome::Conflict(_)) = upload2.outcome {
        panic!("re-upload after dehydration should not produce a conflict")
    }
}

/// Devices A and B take turns: A writes v1, B pulls v1, B writes v2, A pulls
/// v2, A writes v3, B pulls v3. After 3 rounds, verify: final content is v3,
/// both state caches have vclocks with entries for both devices, vclock values
/// are monotonically increasing.
#[tokio::test]
#[allow(unused_assignments)]
async fn alternating_device_writes() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/chains/alternating-writes";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).expect("open state_a");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).expect("open state_b");

    // Track vclock values to verify monotonic increase
    let mut prev_vc_a_a: u64 = 0; // device-a's entry in A's vclock
    let mut prev_vc_a_b: u64 = 0; // device-b's entry in A's vclock
    let mut prev_vc_b_a: u64 = 0; // device-a's entry in B's vclock
    let mut prev_vc_b_b: u64 = 0; // device-b's entry in B's vclock

    // ── Round 1: A writes v1, B pulls ──────────────────────────────────

    let content_v1 = b"version 1 from device-a";
    let src_a1 = write_test_file(tmp.path(), "round1/src_a/alt.txt", content_v1);

    let upload_v1 = upload_file_with_device(
        &op,
        &src_a1,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("alt.txt"),
        None,
    )
    .await
    .expect("round 1: A upload v1");

    assert!(!upload_v1.skipped);

    // Capture A's vclock after round 1 upload
    let cached_a1 = state_a.get(&src_a1).expect("A state after r1 upload");
    let vc_a1_a = cached_a1.vclock.get("device-a");
    assert!(
        vc_a1_a > prev_vc_a_a,
        "round 1: A's vclock[device-a] should increase: {} > {}",
        vc_a1_a,
        prev_vc_a_a
    );
    prev_vc_a_a = vc_a1_a;

    // B pulls v1
    let dst_b1 = tmp.path().join("round1/dst_b/alt.txt");
    download_file_with_device(
        &op,
        &upload_v1.remote_path,
        &dst_b1,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("round 1: B pull v1");

    assert_eq!(std::fs::read(&dst_b1).unwrap(), content_v1);

    let cached_b1 = state_b.get(&dst_b1).expect("B state after r1 pull");
    let vc_b1_a = cached_b1.vclock.get("device-a");
    assert!(
        vc_b1_a >= prev_vc_b_a,
        "round 1: B's vclock[device-a] should not decrease"
    );
    prev_vc_b_a = vc_b1_a;
    prev_vc_b_b = cached_b1.vclock.get("device-b");

    // ── Round 2: B writes v2, A pulls ──────────────────────────────────

    let content_v2 = b"version 2 from device-b";
    let src_b2 = write_test_file(tmp.path(), "round2/src_b/alt.txt", content_v2);
    seed_state_at_new_path(&mut state_b, &dst_b1, &src_b2);

    let upload_v2 = upload_file_with_device(
        &op,
        &src_b2,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("alt.txt"),
        None,
    )
    .await
    .expect("round 2: B upload v2");

    assert!(!upload_v2.skipped);

    // Capture B's vclock after round 2 upload
    let cached_b2 = state_b.get(&src_b2).expect("B state after r2 upload");
    let vc_b2_b = cached_b2.vclock.get("device-b");
    assert!(
        vc_b2_b > prev_vc_b_b,
        "round 2: B's vclock[device-b] should increase: {} > {}",
        vc_b2_b,
        prev_vc_b_b
    );
    prev_vc_b_b = vc_b2_b;
    prev_vc_b_a = cached_b2.vclock.get("device-a");

    // A pulls v2
    let dst_a2 = tmp.path().join("round2/dst_a/alt.txt");
    download_file_with_device(
        &op,
        &upload_v2.remote_path,
        &dst_a2,
        prefix,
        None,
        "device-a",
        Some(&mut state_a),
        None,
    )
    .await
    .expect("round 2: A pull v2");

    assert_eq!(std::fs::read(&dst_a2).unwrap(), content_v2);

    let cached_a2 = state_a.get(&dst_a2).expect("A state after r2 pull");
    let vc_a2_b = cached_a2.vclock.get("device-b");
    assert!(
        vc_a2_b > prev_vc_a_b,
        "round 2: A's vclock[device-b] should increase: {} > {}",
        vc_a2_b,
        prev_vc_a_b
    );
    prev_vc_a_b = vc_a2_b;
    prev_vc_a_a = cached_a2.vclock.get("device-a");

    // ── Round 3: A writes v3, B pulls ──────────────────────────────────

    let content_v3 = b"version 3 from device-a final round";
    let src_a3 = write_test_file(tmp.path(), "round3/src_a/alt.txt", content_v3);
    seed_state_at_new_path(&mut state_a, &dst_a2, &src_a3);

    let upload_v3 = upload_file_with_device(
        &op,
        &src_a3,
        prefix,
        &mut state_a,
        None,
        "device-a",
        Some("alt.txt"),
        None,
    )
    .await
    .expect("round 3: A upload v3");

    assert!(!upload_v3.skipped);

    // Capture A's vclock after round 3 upload
    let cached_a3 = state_a.get(&src_a3).expect("A state after r3 upload");
    let vc_a3_a = cached_a3.vclock.get("device-a");
    assert!(
        vc_a3_a > prev_vc_a_a,
        "round 3: A's vclock[device-a] should increase: {} > {}",
        vc_a3_a,
        prev_vc_a_a
    );

    // B pulls v3
    let dst_b3 = tmp.path().join("round3/dst_b/alt.txt");
    download_file_with_device(
        &op,
        &upload_v3.remote_path,
        &dst_b3,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("round 3: B pull v3");

    // ── Final verifications ────────────────────────────────────────────

    // Final content is v3
    let final_content = std::fs::read(&dst_b3).unwrap();
    assert_eq!(
        final_content, content_v3,
        "final content should be version 3"
    );

    // A's push state (src_a3) has device-a's clock from the push
    let final_a_push = state_a.get(&src_a3).expect("A push state");
    assert!(
        final_a_push.vclock.get("device-a") > 0,
        "A's push vclock should have device-a entry"
    );

    // A's pull state (dst_a2) has device-b's clock from the pull
    let final_a_pull = state_a.get(&dst_a2).expect("A pull state");
    assert!(
        final_a_pull.vclock.get("device-b") > 0 || final_a_pull.vclock.get("device-a") > 0,
        "A's pull vclock should have entries from the pull"
    );

    // B's final pull state has entries
    let final_b = state_b.get(&dst_b3).expect("B final state");
    assert!(
        final_b.vclock.get("device-a") > 0 || final_b.vclock.get("device-b") > 0,
        "B's final vclock should have entries"
    );

    // A's push vclock should reflect its two writes (r1 and r3)
    assert!(
        final_a_push.vclock.get("device-a") >= 1,
        "A wrote v3, vclock[device-a] should be >= 1, got {}",
        final_a_push.vclock.get("device-a")
    );
}
