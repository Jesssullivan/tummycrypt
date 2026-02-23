//! Unit tests for auto-pull vclock comparison logic.
//!
//! These test the vector clock comparison and AutoResolver at the unit level
//! without any I/O — pure conflict detection and resolution logic.

use tcfs_sync::conflict::{
    compare_clocks, AutoResolver, ConflictResolver, Resolution, SyncOutcome, VectorClock,
};

/// When the remote vclock dominates local, outcome should be RemoteNewer.
#[test]
fn auto_pull_remote_newer() {
    let mut local = VectorClock::new();
    local.tick("device-a"); // {a: 1}

    let mut remote = VectorClock::new();
    remote.tick("device-a"); // {a: 1}
    remote.tick("device-b"); // {a: 1, b: 1}

    let outcome = compare_clocks(
        &local,
        &remote,
        "hash_local_111",
        "hash_remote_222",
        "test.txt",
        "device-a",
        "device-b",
    );

    match outcome {
        SyncOutcome::RemoteNewer => {} // expected
        other => panic!("expected RemoteNewer, got: {other:?}"),
    }
}

/// When clocks are concurrent and content differs, AutoResolver should
/// deterministically pick a winner based on device name ordering.
#[test]
fn auto_pull_conflict_auto_resolver() {
    let mut local = VectorClock::new();
    local.tick("device-a"); // {a: 1}

    let mut remote = VectorClock::new();
    remote.tick("device-b"); // {b: 1}

    // Concurrent clocks, different content → Conflict
    let outcome = compare_clocks(
        &local,
        &remote,
        "local_hash_aaa",
        "remote_hash_bbb",
        "data.bin",
        "device-a",
        "device-b",
    );

    let conflict = match outcome {
        SyncOutcome::Conflict(info) => info,
        other => panic!("expected Conflict, got: {other:?}"),
    };

    // AutoResolver: lexicographically smaller device wins
    let resolver = AutoResolver;
    let resolution = resolver
        .resolve(&conflict)
        .expect("should produce resolution");

    // "device-a" < "device-b" → KeepLocal
    assert_eq!(resolution, Resolution::KeepLocal);
}

/// When device-b is the local device and device-a is remote, device-a wins
/// because "device-a" < "device-b", so resolution should be KeepRemote.
#[test]
fn auto_pull_conflict_remote_wins_by_name() {
    let mut local = VectorClock::new();
    local.tick("device-b"); // {b: 1}

    let mut remote = VectorClock::new();
    remote.tick("device-a"); // {a: 1}

    let outcome = compare_clocks(
        &local,
        &remote,
        "local_hash",
        "remote_hash",
        "config.toml",
        "device-b",
        "device-a",
    );

    let conflict = match outcome {
        SyncOutcome::Conflict(info) => info,
        other => panic!("expected Conflict, got: {other:?}"),
    };

    let resolver = AutoResolver;
    let resolution = resolver.resolve(&conflict).expect("should resolve");

    // "device-b" > "device-a" → KeepRemote (remote device has smaller name)
    assert_eq!(resolution, Resolution::KeepRemote);
}

/// When both sides have the same content hash, outcome is UpToDate
/// regardless of vclock ordering.
#[test]
fn auto_pull_same_content_is_up_to_date() {
    let mut local = VectorClock::new();
    local.tick("device-a");

    let mut remote = VectorClock::new();
    remote.tick("device-b");

    let same_hash = "abc123def456";

    let outcome = compare_clocks(
        &local,
        &remote,
        same_hash,
        same_hash,
        "identical.txt",
        "device-a",
        "device-b",
    );

    match outcome {
        SyncOutcome::UpToDate => {} // expected
        other => panic!("expected UpToDate for same content, got: {other:?}"),
    }
}

/// When local vclock dominates remote, outcome should be LocalNewer.
#[test]
fn auto_pull_local_newer() {
    let mut local = VectorClock::new();
    local.tick("device-a"); // {a: 1}
    local.tick("device-a"); // {a: 2}
    local.tick("device-b"); // {a: 2, b: 1}

    let mut remote = VectorClock::new();
    remote.tick("device-a"); // {a: 1}

    let outcome = compare_clocks(
        &local,
        &remote,
        "local_hash_newer",
        "remote_hash_older",
        "readme.md",
        "device-a",
        "device-b",
    );

    match outcome {
        SyncOutcome::LocalNewer => {} // expected
        other => panic!("expected LocalNewer, got: {other:?}"),
    }
}

/// Merge operation should produce pointwise maximum.
#[test]
fn vclock_merge_produces_pointwise_max() {
    let mut a = VectorClock::new();
    a.tick("x"); // {x: 1}
    a.tick("x"); // {x: 2}
    a.tick("y"); // {x: 2, y: 1}

    let mut b = VectorClock::new();
    b.tick("x"); // {x: 1}
    b.tick("z"); // {x: 1, z: 1}
    b.tick("z"); // {x: 1, z: 2}

    a.merge(&b); // {x: 2, y: 1, z: 2}

    assert_eq!(a.get("x"), 2);
    assert_eq!(a.get("y"), 1);
    assert_eq!(a.get("z"), 2);
}
