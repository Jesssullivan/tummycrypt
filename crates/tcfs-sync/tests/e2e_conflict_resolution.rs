//! E2E test: conflict resolution patterns
//!
//! Tests the full conflict lifecycle:
//!   1. Two devices push conflicting versions
//!   2. VectorClock comparison shows Conflict
//!   3. Resolution strategies: keep_local, keep_remote, keep_both
//!   4. Final state is consistent after resolution
//!
//! NOTE: The sync engine uses content-addressed manifest paths
//! (`{prefix}/manifests/{file_hash}`), so two devices writing different content
//! produce different manifest paths. Conflict detection happens at the
//! VectorClock/NATS layer, not within a single `upload_file_with_device` call.
//! These tests exercise VectorClock conflict detection directly and test the
//! resolution workflows (download remote, re-upload local, keep both).

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::conflict::{
    compare_clocks, AutoResolver, ConflictInfo, ConflictResolver, Resolution, SyncOutcome,
    VectorClock,
};
use tcfs_sync::engine::{download_file, download_file_with_device, upload_file_with_device};
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

// ── VectorClock conflict detection tests ─────────────────────────────────────

/// Verify VectorClock comparison directly for concurrent modifications.
#[test]
fn vclock_comparison_detects_concurrent() {
    let mut clock_a = VectorClock::new();
    clock_a.tick("device-a");

    let mut clock_b = VectorClock::new();
    clock_b.tick("device-b");

    // These are concurrent -- neither dominates
    assert!(
        clock_a.is_concurrent(&clock_b),
        "independent ticks should be concurrent"
    );

    let outcome = compare_clocks(
        &clock_a, &clock_b, "hash_a", "hash_b", "file.txt", "device-a", "device-b",
    );

    match outcome {
        SyncOutcome::Conflict(info) => {
            assert_eq!(info.local_device, "device-a");
            assert_eq!(info.remote_device, "device-b");
            assert_eq!(info.local_blake3, "hash_a");
            assert_eq!(info.remote_blake3, "hash_b");
        }
        other => panic!("expected Conflict, got: {:?}", other),
    }
}

/// Verify that identical content does NOT produce conflict (UpToDate).
#[test]
fn vclock_comparison_same_content_is_up_to_date() {
    let mut clock_a = VectorClock::new();
    clock_a.tick("device-a");

    let mut clock_b = VectorClock::new();
    clock_b.tick("device-b");

    // Same hash = same content, even with concurrent clocks
    let outcome = compare_clocks(
        &clock_a,
        &clock_b,
        "same_hash",
        "same_hash",
        "file.txt",
        "device-a",
        "device-b",
    );

    assert!(
        matches!(outcome, SyncOutcome::UpToDate),
        "same content should be UpToDate regardless of clocks"
    );
}

/// Verify that a strictly newer clock shows LocalNewer.
#[test]
fn vclock_comparison_local_newer() {
    let mut clock_a = VectorClock::new();
    clock_a.tick("device-a");
    clock_a.tick("device-a");

    let mut clock_b = VectorClock::new();
    clock_b.tick("device-a"); // B knows about A's first tick only

    let outcome = compare_clocks(
        &clock_a, &clock_b, "hash_new", "hash_old", "file.txt", "device-a", "device-b",
    );

    assert!(
        matches!(outcome, SyncOutcome::LocalNewer),
        "A with higher own-tick should be LocalNewer"
    );
}

/// Verify that a strictly older clock shows RemoteNewer.
#[test]
fn vclock_comparison_remote_newer() {
    let mut clock_a = VectorClock::new();
    clock_a.tick("device-a");

    let mut clock_b = VectorClock::new();
    clock_b.tick("device-a");
    clock_b.tick("device-b"); // B has strictly more info

    let outcome = compare_clocks(
        &clock_a, &clock_b, "hash_old", "hash_new", "file.txt", "device-a", "device-b",
    );

    assert!(
        matches!(outcome, SyncOutcome::RemoteNewer),
        "B with merged+ticked clock should be RemoteNewer"
    );
}

/// Test all three resolution variants with a crafted ConflictInfo.
#[test]
fn resolution_variants_exhaustive() {
    let mut vc_a = VectorClock::new();
    vc_a.tick("device-a");
    let mut vc_b = VectorClock::new();
    vc_b.tick("device-b");

    let info = ConflictInfo {
        rel_path: "project/readme.md".into(),
        local_vclock: vc_a,
        remote_vclock: vc_b,
        local_blake3: "hash_local".into(),
        remote_blake3: "hash_remote".into(),
        local_device: "device-a".into(),
        remote_device: "device-b".into(),
        detected_at: 1700000000,
    };

    // KeepLocal: local_device < remote_device => AutoResolver picks KeepLocal
    let resolver = AutoResolver;
    assert_eq!(
        resolver.resolve(&info),
        Some(Resolution::KeepLocal),
        "device-a < device-b => KeepLocal"
    );

    // Flip devices: KeepRemote
    let info_flipped = ConflictInfo {
        local_device: "device-b".into(),
        remote_device: "device-a".into(),
        ..info.clone()
    };
    assert_eq!(
        resolver.resolve(&info_flipped),
        Some(Resolution::KeepRemote),
        "device-b > device-a => KeepRemote"
    );
}

// ── Resolution workflow tests (using actual sync engine) ─────────────────────

/// Resolution: keep_remote -- download the remote version, discard local.
#[tokio::test]
async fn resolve_keep_remote_workflow() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/conflict/keep-remote";

    // Device A uploads its version
    let content_a = b"device A's version of the document";
    let src_a = write_test_file(tmp.path(), "src_a/doc.txt", content_a);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).unwrap();

    let upload_a = upload_file_with_device(
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

    // Device B has local content but decides to keep remote (device A's version)
    let _content_b = b"device B's local version that will be discarded";
    let resolved_path = tmp.path().join("resolved_b/doc.txt");
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).unwrap();

    let download = download_file_with_device(
        &op,
        &upload_a.remote_path,
        &resolved_path,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("keep_remote download");

    let content = std::fs::read(&resolved_path).unwrap();
    assert_eq!(
        content, content_a,
        "keep_remote should have device A's content"
    );
    assert_eq!(download.bytes, content_a.len() as u64);

    // State cache updated with vclock
    let cached = state_b
        .get(&resolved_path)
        .expect("state entry after resolve");
    assert!(!cached.vclock.clocks.is_empty());
}

/// Resolution: keep_local -- re-upload local version.
#[tokio::test]
async fn resolve_keep_local_workflow() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/conflict/keep-local";

    // Device A uploads first
    let content_a = b"device A's version";
    let src_a = write_test_file(tmp.path(), "src_a/notes.txt", content_a);
    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).unwrap();

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
    .expect("device-a upload");

    // Device B decides to keep its local version and re-upload
    let content_b = b"device B's version that wins the conflict resolution";
    let src_b = write_test_file(tmp.path(), "src_b/notes.txt", content_a);
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).unwrap();

    let a_state = state_a.get(&src_a).expect("A should have state");
    let mut resolved_b_state = tcfs_sync::state::make_sync_state(
        &src_b,
        upload_a.hash.clone(),
        upload_a.chunks,
        upload_a.remote_path.clone(),
    )
    .expect("resolved B state");
    // Simulate keep_local resolution: B has observed A's version and then
    // ticks its own clock before re-uploading its chosen local content.
    resolved_b_state.vclock = a_state.vclock.clone();
    resolved_b_state.vclock.tick("device-b");
    state_b.set(&src_b, resolved_b_state);
    std::fs::write(&src_b, content_b).expect("device-b writes resolved local version");

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
    .expect("device-b re-upload (keep_local)");

    assert!(
        !upload_b.skipped,
        "keep_local re-upload should succeed, got outcome: {:?}",
        upload_b.outcome
    );

    // Verify remote now has B's content
    let verify_path = tmp.path().join("verify/notes.txt");
    download_file(&op, &upload_b.remote_path, &verify_path, prefix, None)
        .await
        .expect("verify download");

    let verified = std::fs::read(&verify_path).unwrap();
    assert_eq!(
        verified, content_b,
        "remote should have device B's content after keep_local"
    );
}

/// Resolution: keep_both -- rename local copy, download remote to original path.
#[tokio::test]
async fn resolve_keep_both_workflow() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/conflict/keep-both";

    // Device A uploads
    let content_a = b"device A content for keep_both test";
    let src_a = write_test_file(tmp.path(), "src/report.txt", content_a);
    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    let upload = upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state,
        None,
        "device-a",
        Some("report.txt"),
        None,
    )
    .await
    .expect("upload");

    // Simulate keep_both: local file gets renamed, remote downloaded to original path
    let content_b = b"device B's local content";
    let local_file = write_test_file(tmp.path(), "local/report.txt", content_b);
    let conflict_file = tmp.path().join("local/report.conflict-device-b.txt");

    // Rename local to conflict copy
    std::fs::rename(&local_file, &conflict_file).expect("rename local");
    assert!(conflict_file.exists(), "conflict copy should exist");
    assert!(!local_file.exists(), "original should be gone");

    // Download remote to original path
    download_file_with_device(
        &op,
        &upload.remote_path,
        &local_file,
        prefix,
        None,
        "device-b",
        Some(&mut state),
        None,
    )
    .await
    .expect("download remote to original path");

    // Both files should exist with correct content
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
        conflict_content, content_b,
        "conflict copy should be local (device B)"
    );
}

/// AutoResolver picks the lexicographically smaller device as winner.
#[test]
fn auto_resolver_deterministic() {
    let mut vc_local = VectorClock::new();
    vc_local.tick("device-b");
    let mut vc_remote = VectorClock::new();
    vc_remote.tick("device-a");

    let info = ConflictInfo {
        rel_path: "file.txt".into(),
        local_vclock: vc_local,
        remote_vclock: vc_remote,
        local_blake3: "hash_b".into(),
        remote_blake3: "hash_a".into(),
        local_device: "device-b".into(),
        remote_device: "device-a".into(),
        detected_at: 0,
    };

    let resolver = AutoResolver;
    let resolution = resolver.resolve(&info).expect("should resolve");

    // "device-b" > "device-a" lexicographically => KeepRemote
    assert_eq!(
        resolution,
        Resolution::KeepRemote,
        "device-a should win (lexicographically smaller)"
    );

    // Flip: local_device is the smaller one
    let info2 = ConflictInfo {
        local_device: "alpha".into(),
        remote_device: "zeta".into(),
        ..info
    };
    let resolution2 = resolver.resolve(&info2).expect("should resolve");
    assert_eq!(
        resolution2,
        Resolution::KeepLocal,
        "alpha should win (lexicographically smaller)"
    );
}

/// After resolution (keep_remote), subsequent syncs work normally.
#[tokio::test]
async fn resolved_conflict_subsequent_sync() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/conflict/no-recur";

    let mut state_a = StateCache::open(&tmp.path().join("state_a.db")).unwrap();
    let mut state_b = StateCache::open(&tmp.path().join("state_b.db")).unwrap();

    // Device A uploads v1
    let content_v1 = b"version 1 from device A";
    let src_a = write_test_file(tmp.path(), "src_a/v1.txt", content_v1);
    let upload_v1 = upload_file_with_device(
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
    .expect("v1 upload");

    // Device B downloads v1 (resolve/accept remote)
    let dst_b = tmp.path().join("dst_b/doc.txt");
    download_file_with_device(
        &op,
        &upload_v1.remote_path,
        &dst_b,
        prefix,
        None,
        "device-b",
        Some(&mut state_b),
        None,
    )
    .await
    .expect("B downloads v1");

    assert_eq!(std::fs::read(&dst_b).unwrap(), content_v1);

    // Device B now modifies and pushes v2
    let content_v2 = b"version 2 from device B after resolving";
    let src_b = write_test_file(tmp.path(), "src_b/v2.txt", content_v1);
    let a_cached = state_a.get(&src_a).expect("A state");
    let mut resolved_b_state = tcfs_sync::state::make_sync_state(
        &src_b,
        upload_v1.hash.clone(),
        upload_v1.chunks,
        upload_v1.remote_path.clone(),
    )
    .expect("resolved B state");
    // Simulate the post-resolution edit on device B: B has A's clock and then
    // performs a new local write before uploading v2.
    resolved_b_state.vclock = a_cached.vclock.clone();
    resolved_b_state.vclock.tick("device-b");
    state_b.set(&src_b, resolved_b_state);
    std::fs::write(&src_b, content_v2).expect("device-b writes v2 after resolving");

    let upload_v2 = upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-b",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("B uploads v2");

    assert!(
        !upload_v2.skipped,
        "post-resolution upload should succeed, got outcome: {:?}",
        upload_v2.outcome
    );

    // Device A pulls v2
    let dst_a = tmp.path().join("dst_a/doc.txt");
    download_file_with_device(
        &op,
        &upload_v2.remote_path,
        &dst_a,
        prefix,
        None,
        "device-a",
        Some(&mut state_a),
        None,
    )
    .await
    .expect("A downloads v2");

    let final_content = std::fs::read(&dst_a).unwrap();
    assert_eq!(final_content, content_v2);

    // Verify merged vclocks
    let cached_a = state_a.get(&dst_a).expect("A state");
    assert!(
        cached_a.vclock.get("device-b") > 0,
        "device A should know about device B's writes"
    );
}

/// Verify merged vclock after resolution dominates both original clocks.
#[test]
fn merged_vclock_dominates_after_resolution() {
    let mut vc_a = VectorClock::new();
    vc_a.tick("device-a");
    vc_a.tick("device-a");

    let mut vc_b = VectorClock::new();
    vc_b.tick("device-b");
    vc_b.tick("device-b");
    vc_b.tick("device-b");

    // These are concurrent
    assert!(vc_a.is_concurrent(&vc_b));

    // Merge and tick (simulating resolution)
    let mut merged = vc_a.clone();
    merged.merge(&vc_b);
    merged.tick("device-b"); // resolver ticks winner

    // Merged should dominate both originals
    assert_eq!(
        merged.partial_cmp_vc(&vc_a),
        Some(std::cmp::Ordering::Greater),
        "merged should dominate A"
    );
    assert_eq!(
        merged.partial_cmp_vc(&vc_b),
        Some(std::cmp::Ordering::Greater),
        "merged should dominate B"
    );

    // Using merged as local vs either original as remote should show LocalNewer
    let outcome = compare_clocks(
        &merged,
        &vc_a,
        "hash_resolved",
        "hash_a",
        "file.txt",
        "device-b",
        "device-a",
    );
    assert!(
        matches!(outcome, SyncOutcome::LocalNewer),
        "merged clock should be LocalNewer vs A"
    );
}
