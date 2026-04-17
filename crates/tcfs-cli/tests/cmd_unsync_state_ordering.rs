//! Greptile P2 #3 regression: cmd_unsync must flip the persisted sync state to
//! `NotSynced` BEFORE performing destructive filesystem operations.
//!
//! If the stub write (or remove_file) later fails, the on-disk state already
//! reflects reality so the CLI never lies to the daemon about a file being
//! `Synced` when the hydrated copy is gone (or the stub is half-written).

use std::os::unix::fs::PermissionsExt;
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

    // Arrange a stub destination directory that cannot be written to so the
    // destructive fs op fails AFTER the (correctly reordered) state flush.
    let stub_dir = tmp.path().join("stubs_readonly");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let mut perms = std::fs::metadata(&stub_dir).unwrap().permissions();
    perms.set_mode(0o500);
    std::fs::set_permissions(&stub_dir, perms).unwrap();

    let stub_full = stub_dir.join("file.stub");

    let result = tcfs_cli::commands::unsync::run_for_test(&original, &stub_full, &state_path).await;

    // Restore permissions so TempDir cleanup can succeed regardless of outcome.
    let mut restore = std::fs::metadata(&stub_dir).unwrap().permissions();
    restore.set_mode(0o700);
    std::fs::set_permissions(&stub_dir, restore).unwrap();

    assert!(
        result.is_err(),
        "stub write into read-only dir should fail, got: {result:?}"
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
