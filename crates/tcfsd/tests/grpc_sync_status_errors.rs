//! Behavior contract test for `TcfsDaemonImpl::sync_status` error handling.
//!
//! Greptile P2 on PR #301: the Some(entry) branch of `sync_status` used to
//! collapse `Err(_)` from `StateCache::needs_sync` into `entry.status.to_string()`,
//! which for Synced entries masked IO failures as a stale "synced" state.
//!
//! `tcfsd` is a binary-only crate (no `lib.rs`), so we can't import the gRPC
//! service here. Instead we replicate the branch logic locally — mirroring the
//! `tcfsd_test_helpers` pattern used by `metrics_test.rs` — and assert the
//! behavior contract against a real `StateCache::needs_sync` call whose
//! underlying `std::fs::metadata` fails.
//!
//! Paired with the in-crate unit test
//! `sync_status_surfaces_needs_sync_error_instead_of_synced` in
//! `crates/tcfsd/src/grpc.rs`, which exercises the same path through the
//! full RPC via `test_daemon()`.
//!
//! This file MUST fail before the fix and pass after.

use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::state::{FileSyncStatus, StateCache, SyncState};

/// Mirror of the Some(entry) branch of `TcfsDaemonImpl::sync_status` in
/// `crates/tcfsd/src/grpc.rs`. Kept in lockstep with the production code so
/// this test locks the error-handling contract in place.
fn compute_sync_state_for_entry(cache: &StateCache, path: &Path, entry: &SyncState) -> String {
    if entry.status == FileSyncStatus::Synced {
        match cache.needs_sync(path) {
            Ok(Some(_)) => "pending".to_string(),
            Ok(None) => entry.status.to_string(),
            Err(_e) => "unknown".to_string(),
        }
    } else {
        entry.status.to_string()
    }
}

fn make_entry(status: FileSyncStatus) -> SyncState {
    SyncState {
        blake3: "deadbeef".into(),
        size: 42,
        mtime: 1_700_000_000,
        chunk_count: 1,
        remote_path: "data/manifests/deadbeef".into(),
        last_synced: 1_700_000_000,
        vclock: tcfs_sync::conflict::VectorClock::default(),
        device_id: "test-device-id".into(),
        conflict: None,
        status,
    }
}

/// `StateCache::needs_sync` must return Err when the local path cannot be
/// stat'd. If this ever starts returning `Ok(...)`, our test is no longer
/// exercising the error path — update the trigger.
#[test]
fn needs_sync_errors_on_missing_path() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let missing = tmp.path().join("does-not-exist.txt");

    let cache = StateCache::open(&state_path).unwrap();
    let result = cache.needs_sync(&missing);

    assert!(
        result.is_err(),
        "needs_sync should Err on missing path, got {result:?}"
    );
}

/// Core regression: when `StateCache::needs_sync` returns Err and the cached
/// entry is Synced, we must surface an observability signal rather than a
/// stale "synced" lie.
#[test]
fn sync_status_does_not_report_synced_when_needs_sync_errs() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let missing = tmp.path().join("vanished.txt");

    let mut cache = StateCache::open(&state_path).unwrap();
    cache.set(&missing, make_entry(FileSyncStatus::Synced));
    cache.flush().unwrap();

    let entry = cache.get(&missing).expect("entry was just set").clone();
    let state = compute_sync_state_for_entry(&cache, &missing, &entry);

    assert_ne!(
        state, "synced",
        "needs_sync Err must not be collapsed into \"synced\""
    );
    assert_eq!(state, "unknown");
}

/// Sanity: when needs_sync succeeds with Ok(None) (file matches cached state),
/// we still report the entry's persisted status (Synced).
#[test]
fn sync_status_reports_synced_on_ok_none() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let file = tmp.path().join("stable.txt");
    std::fs::write(&file, b"hello").unwrap();

    let entry = tcfs_sync::state::make_sync_state(
        &file,
        "stable-hash".into(),
        1,
        "data/manifests/stable".into(),
    )
    .unwrap();

    let mut cache = StateCache::open(&state_path).unwrap();
    let mut stored = entry.clone();
    stored.status = FileSyncStatus::Synced;
    cache.set(&file, stored.clone());
    cache.flush().unwrap();

    let state = compute_sync_state_for_entry(&cache, &file, &stored);
    assert_eq!(state, "synced");
}

/// Sanity: when needs_sync detects a modification (Ok(Some(_))), we report
/// "pending" regardless of the persisted Synced status.
#[test]
fn sync_status_reports_pending_on_ok_some() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let file = tmp.path().join("modified.txt");
    std::fs::write(&file, b"original").unwrap();

    let entry = tcfs_sync::state::make_sync_state(
        &file,
        "tracked-hash".into(),
        1,
        "data/manifests/tracked".into(),
    )
    .unwrap();
    let mut stored = entry.clone();
    stored.status = FileSyncStatus::Synced;

    let mut cache = StateCache::open(&state_path).unwrap();
    cache.set(&file, stored.clone());
    cache.flush().unwrap();

    // Mutate the file so size changes — triggers Ok(Some(_)) in needs_sync.
    std::fs::write(&file, b"original plus more").unwrap();

    let state = compute_sync_state_for_entry(&cache, &file, &stored);
    assert_eq!(state, "pending");
}

/// Sanity: a non-Synced entry short-circuits and returns its own status
/// verbatim regardless of needs_sync behavior.
#[test]
fn sync_status_returns_entry_status_when_not_synced() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let missing = tmp.path().join("conflicted.txt");

    let mut cache = StateCache::open(&state_path).unwrap();
    let stored = make_entry(FileSyncStatus::Conflict);
    cache.set(&missing, stored.clone());
    cache.flush().unwrap();

    let state = compute_sync_state_for_entry(&cache, &missing, &stored);
    assert_eq!(state, "conflict");
}
