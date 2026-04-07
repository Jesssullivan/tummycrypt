//! Tests for directory creation logic (ensure_dirs equivalent)
//!
//! Since ensure_dirs is not exported from the binary crate, we test the
//! same filesystem behavior: creating parent directories for socket, state,
//! cache, and sync root paths from a TcfsConfig.

use tcfs_core::config::TcfsConfig;
use tempfile::TempDir;

/// Replicates the ensure_dirs logic from daemon.rs for testing
fn ensure_dirs(config: &TcfsConfig) {
    if let Some(parent) = config.daemon.socket.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Some(parent) = config.sync.state_db.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::create_dir_all(&config.fuse.cache_dir).ok();
    if let Some(ref root) = config.sync.sync_root {
        std::fs::create_dir_all(root).ok();
    }
}

#[test]
fn creates_socket_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let mut config = TcfsConfig::default();
    config.daemon.socket = tmp.path().join("nested/deep/tcfsd.sock");

    ensure_dirs(&config);

    assert!(tmp.path().join("nested/deep").is_dir());
}

#[test]
fn creates_state_db_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let mut config = TcfsConfig::default();
    config.sync.state_db = tmp.path().join("state/cache/state.db");

    ensure_dirs(&config);

    assert!(tmp.path().join("state/cache").is_dir());
}

#[test]
fn creates_fuse_cache_dir() {
    let tmp = TempDir::new().unwrap();
    let mut config = TcfsConfig::default();
    config.fuse.cache_dir = tmp.path().join("fuse-cache");

    ensure_dirs(&config);

    assert!(tmp.path().join("fuse-cache").is_dir());
}

#[test]
fn creates_sync_root_dir() {
    let tmp = TempDir::new().unwrap();
    let mut config = TcfsConfig::default();
    config.sync.sync_root = Some(tmp.path().join("sync-root/subdir"));

    ensure_dirs(&config);

    assert!(tmp.path().join("sync-root/subdir").is_dir());
}

#[test]
fn no_sync_root_is_fine() {
    let tmp = TempDir::new().unwrap();
    let mut config = TcfsConfig::default();
    config.daemon.socket = tmp.path().join("sock/tcfsd.sock");
    config.sync.sync_root = None;

    // Should not panic
    ensure_dirs(&config);
    assert!(tmp.path().join("sock").is_dir());
}

#[test]
fn idempotent_on_existing_dirs() {
    let tmp = TempDir::new().unwrap();
    let cache_dir = tmp.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    let mut config = TcfsConfig::default();
    config.fuse.cache_dir = cache_dir.clone();

    // Should not fail on existing dir
    ensure_dirs(&config);
    assert!(cache_dir.is_dir());
}

#[test]
fn default_config_dirs_are_valid_paths() {
    let config = TcfsConfig::default();

    // Default socket path should have a parent
    assert!(config.daemon.socket.parent().is_some());
    // Default state_db should have a parent
    assert!(config.sync.state_db.parent().is_some());
    // Default cache_dir should be a valid path
    assert!(!config.fuse.cache_dir.as_os_str().is_empty());
}
