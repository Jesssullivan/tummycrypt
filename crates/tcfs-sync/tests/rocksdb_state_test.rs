//! Integration tests for the RocksDB state cache backend.
//!
//! Verifies that `RocksDbStateCache` correctly implements the
//! `StateCacheBackend` trait with proper persistence and retrieval.

#![cfg(feature = "full")]

use std::path::Path;
use tcfs_sync::state::{RocksDbStateCache, StateCacheBackend, SyncState};
use tempfile::TempDir;

fn make_state(rel_path: &str, hash: &str) -> SyncState {
    SyncState {
        local_hash: hash.to_string(),
        remote_manifest: format!("test/manifests/{hash}"),
        rel_path: rel_path.to_string(),
        last_synced: 1000,
        file_size: 42,
        vclock: Default::default(),
        device_id: "test-device".to_string(),
    }
}

#[test]
fn rocksdb_set_get_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("state.rocksdb");
    let mut cache = RocksDbStateCache::open(&db_path).expect("open rocksdb");

    let local = tmp.path().join("docs/readme.md");
    let state = make_state("docs/readme.md", "abc123");

    cache.set(&local, state.clone());

    let retrieved = cache.get(&local).expect("should exist");
    assert_eq!(retrieved.local_hash, "abc123");
    assert_eq!(retrieved.rel_path, "docs/readme.md");
    assert_eq!(retrieved.file_size, 42);
}

#[test]
fn rocksdb_persistence_across_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("state.rocksdb");
    let local = tmp.path().join("photos/cat.jpg");

    // Write data and drop
    {
        let mut cache = RocksDbStateCache::open(&db_path).expect("open");
        cache.set(&local, make_state("photos/cat.jpg", "hash_cat"));
        cache.flush().unwrap();
    }

    // Reopen and verify data persisted
    {
        let cache = RocksDbStateCache::open(&db_path).expect("reopen");
        let state = cache.get(&local).expect("should persist");
        assert_eq!(state.local_hash, "hash_cat");
        assert_eq!(state.rel_path, "photos/cat.jpg");
    }
}

#[test]
fn rocksdb_remove_deletes() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("state.rocksdb");
    let mut cache = RocksDbStateCache::open(&db_path).expect("open");

    let local = tmp.path().join("tmp/scratch.txt");
    cache.set(&local, make_state("tmp/scratch.txt", "hash_tmp"));
    assert!(cache.get(&local).is_some());

    cache.remove(&local);
    assert!(cache.get(&local).is_none());
    assert_eq!(cache.len(), 0);
}

#[test]
fn rocksdb_get_by_rel_path() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("state.rocksdb");
    let mut cache = RocksDbStateCache::open(&db_path).expect("open");

    let local_a = tmp.path().join("src/main.rs");
    let local_b = tmp.path().join("src/lib.rs");

    cache.set(&local_a, make_state("src/main.rs", "hash_main"));
    cache.set(&local_b, make_state("src/lib.rs", "hash_lib"));

    let found = cache.get_by_rel_path("src/lib.rs");
    assert!(found.is_some());
    let (_, state) = found.unwrap();
    assert_eq!(state.local_hash, "hash_lib");

    assert!(cache.get_by_rel_path("nonexistent.txt").is_none());
}
