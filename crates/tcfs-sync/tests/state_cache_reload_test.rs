//! Tests for StateCache::reload_from_disk — verifies that entries written
//! by one process (CLI) are visible to another (daemon) after reload.

use tcfs_sync::state::StateCache;
use tempfile::TempDir;

fn write_file(dir: &std::path::Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, content).unwrap();
    p
}

/// reload_from_disk merges new entries from disk into memory.
#[test]
fn reload_picks_up_new_entries() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.json");
    let file = write_file(tmp.path(), "foo.txt", b"hello");

    // Process 1 (daemon) opens empty cache
    let mut cache1 = StateCache::open(&path).unwrap();
    assert!(cache1.get(&file).is_none());

    // Process 2 (CLI) opens same file, writes an entry, flushes
    let mut cache2 = StateCache::open(&path).unwrap();
    let mut state = tcfs_sync::state::make_sync_state(
        &file,
        "abc123".into(),
        1,
        "test/manifests/abc123".into(),
    )
    .unwrap();
    state.vclock.tick("device-cli");
    cache2.set(&file, state);
    cache2.flush().unwrap();

    // Process 1 still doesn't see it in memory
    assert!(cache1.get(&file).is_none());

    // After reload, it should appear
    cache1.reload_from_disk().unwrap();
    let entry = cache1.get(&file).expect("should exist after reload");
    assert_eq!(entry.blake3, "abc123");
    assert!(!entry.vclock.clocks.is_empty());
}

/// reload_from_disk does NOT overwrite existing in-memory entries.
#[test]
fn reload_preserves_in_memory_entries() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.json");
    let file = write_file(tmp.path(), "bar.txt", b"world");

    // Process 1 has an in-memory entry
    let mut cache1 = StateCache::open(&path).unwrap();
    let state1 = tcfs_sync::state::make_sync_state(
        &file,
        "memory_hash".into(),
        2,
        "test/manifests/memory_hash".into(),
    )
    .unwrap();
    cache1.set(&file, state1);
    cache1.flush().unwrap();

    // Process 2 writes a DIFFERENT hash for the same path
    let mut cache2 = StateCache::open(&path).unwrap();
    let state2 = tcfs_sync::state::make_sync_state(
        &file,
        "disk_hash".into(),
        3,
        "test/manifests/disk_hash".into(),
    )
    .unwrap();
    cache2.set(&file, state2);
    cache2.flush().unwrap();

    // Process 1 reloads — should keep its in-memory version
    cache1.reload_from_disk().unwrap();
    let entry = cache1.get(&file).expect("should exist");
    assert_eq!(
        entry.blake3, "memory_hash",
        "in-memory should win over disk"
    );
}

/// reload_from_disk handles missing file gracefully.
#[test]
fn reload_nonexistent_file_is_ok() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("does_not_exist.json");

    let mut cache = StateCache::open(&path).unwrap();
    // Should not error
    cache.reload_from_disk().unwrap();
}
