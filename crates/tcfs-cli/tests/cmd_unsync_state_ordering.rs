//! Greptile P2 #3 regression: cmd_unsync must flip the persisted sync state to
//! `NotSynced` BEFORE performing destructive filesystem operations.
//!
//! If the stub write (or remove_file) later fails, the on-disk state already
//! reflects reality so the CLI never lies to the daemon about a file being
//! `Synced` when the hydrated copy is gone (or the stub is half-written).

use tempfile::TempDir;

use tcfs_sync::conflict::VectorClock;
use tcfs_sync::state::{FileSyncStatus, StateCache, SyncState};

fn seed_synced(state_path: &std::path::Path, file: &std::path::Path) {
    let mut cache = StateCache::open(state_path).unwrap();
    cache.set(
        file,
        SyncState {
            blake3: "deadbeef".into(),
            size: 7,
            mtime: 0,
            chunk_count: 1,
            remote_path: "bucket/file.bin".into(),
            last_synced: 0,
            vclock: VectorClock::new(),
            device_id: String::new(),
            conflict: None,
            status: FileSyncStatus::Synced,
        },
    );
    cache.flush().unwrap();
}

#[tokio::test]
async fn unsync_flips_status_before_destructive_ops() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");
    let original = tmp.path().join("file.bin");
    std::fs::write(&original, b"payload").unwrap();

    // Seed state: file is Synced.
    seed_synced(&state_path, &original);

    // Arrange a stub parent that is a regular file. Creating a child beneath
    // it fails with ENOTDIR for every uid, unlike mode-bit denial, which root
    // can bypass on sanctioned ARC runners. The failure still occurs only
    // AFTER the (correctly reordered) state flush.
    let stub_parent = tmp.path().join("stub_parent_file");
    std::fs::write(&stub_parent, b"not a directory").unwrap();
    let stub_full = stub_parent.join("file.stub");

    let result = tcfs_cli::commands::unsync::run_for_test(&original, &stub_full, &state_path).await;

    assert!(
        result.is_err(),
        "stub write beneath a regular file should fail, got: {result:?}"
    );
    assert!(
        original.exists(),
        "stub write must fail before the original is removed"
    );

    // Invariant: persisted state must be NotSynced, not Synced, even though
    // the fs op failed immediately after the state flush.
    let cache = StateCache::open(&state_path).unwrap();
    let status = cache.get(&original).map(|s| s.status);
    assert_eq!(
        status,
        Some(FileSyncStatus::NotSynced),
        "state must have been flipped to NotSynced before destructive ops"
    );
}
