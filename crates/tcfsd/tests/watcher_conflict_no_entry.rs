//! The watcher must record conflict state even when the cache has no prior entry.
//!
//! Regression test for Greptile P2 #2 on PR #301: `StateCache::mark_conflict`
//! returns `false` when the entry is absent, and the watcher previously
//! discarded that signal, silently dropping the conflict metadata for files
//! that were never registered in the state cache.

use tcfs_sync::conflict::{ConflictInfo, VectorClock};
use tcfs_sync::state::{FileSyncStatus, StateCache};
use tempfile::TempDir;

fn sample_info() -> ConflictInfo {
    ConflictInfo {
        rel_path: "nonregistered.bin".to_string(),
        local_vclock: VectorClock::default(),
        remote_vclock: VectorClock::default(),
        local_blake3: "aaaa".to_string(),
        remote_blake3: "bbbb".to_string(),
        local_device: "local".to_string(),
        remote_device: "remote".to_string(),
        detected_at: 0,
    }
}

#[test]
fn mark_conflict_inserts_entry_when_missing() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let mut cache = StateCache::open(&state_path).unwrap();

    let target = std::path::PathBuf::from("/tmp/tcfsd-watcher-nonregistered.bin");
    let info = sample_info();

    assert!(
        cache.get(&target).is_none(),
        "precondition: target must not be in cache"
    );

    tcfsd::daemon::test_support::watcher_record_conflict(&mut cache, &target, info.clone());

    let entry = cache.get(&target).expect("conflict record missing");
    assert_eq!(entry.status, FileSyncStatus::Conflict);
    let stored = entry
        .conflict
        .as_ref()
        .expect("conflict payload must round-trip");
    assert_eq!(stored.local_device, info.local_device);
    assert_eq!(stored.remote_device, info.remote_device);
    assert_eq!(stored.local_blake3, info.local_blake3);
    assert_eq!(stored.remote_blake3, info.remote_blake3);
}

#[test]
fn mark_conflict_preserves_existing_entry_fields() {
    use tcfs_sync::state::SyncState;

    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let mut cache = StateCache::open(&state_path).unwrap();

    let target = std::path::PathBuf::from("/tmp/tcfsd-watcher-registered.bin");
    let existing = SyncState {
        blake3: "previous".to_string(),
        size: 42,
        mtime: 1_700_000_000,
        chunk_count: 3,
        remote_path: "chunks/previous".to_string(),
        last_synced: 1_700_000_000,
        vclock: VectorClock::default(),
        device_id: "local".to_string(),
        conflict: None,
        status: FileSyncStatus::Synced,
    };
    cache.set(&target, existing);

    let info = sample_info();
    tcfsd::daemon::test_support::watcher_record_conflict(&mut cache, &target, info.clone());

    let entry = cache.get(&target).expect("entry must still exist");
    assert_eq!(entry.status, FileSyncStatus::Conflict);
    assert_eq!(entry.blake3, "previous", "pre-existing metadata preserved");
    assert_eq!(entry.remote_path, "chunks/previous");
    assert!(entry.conflict.is_some());
}
