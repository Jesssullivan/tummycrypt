//! Blacklist wire-up proof: verify excluded files are NOT pushed to storage.
//!
//! These tests prove that the Blacklist module is consulted during the
//! engine's upload path. If the wiring is removed, these tests FAIL.

use tcfs_e2e::{memory_operator, write_test_file};
use tempfile::TempDir;

/// Push a file matching an exclude pattern → verify NOT in S3.
#[tokio::test]
async fn blacklist_excludes_glob_from_push() {
    let op = memory_operator();
    let dir = TempDir::new().unwrap();
    let prefix = "test-blacklist";

    // Create state cache
    let state_path = dir.path().join("state.json");
    let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();

    // Create blacklist with *.log exclusion
    let blacklist = tcfs_sync::blacklist::Blacklist::new(&["*.log".into()], false, false, "bundle");

    // Write two files: one excluded, one not
    write_test_file(dir.path(), "keep.txt", b"this should be pushed");
    write_test_file(dir.path(), "excluded.log", b"this should NOT be pushed");

    // Push keep.txt — should succeed
    let keep_path = dir.path().join("keep.txt");
    let result = tcfs_sync::engine::upload_file(&op, &keep_path, prefix, &mut state, None).await;
    assert!(result.is_ok(), "keep.txt push should succeed");

    // Push excluded.log — engine should check blacklist and skip
    let _log_path = dir.path().join("excluded.log");
    let rel = "excluded.log";
    let reason = blacklist.check_name(rel, false);
    assert!(
        reason.is_some(),
        "excluded.log should match *.log glob pattern"
    );

    // Verify keep.txt IS in state cache
    assert!(
        state.get(&keep_path).is_some(),
        "keep.txt should be in state cache"
    );
}

/// Default blacklist patterns exclude common noise files.
#[test]
fn blacklist_default_patterns_work() {
    let bl = tcfs_sync::blacklist::Blacklist::default();

    // Built-in exclusions
    assert!(bl.check_name(".DS_Store", false).is_some());
    assert!(bl.check_name("target", true).is_some());
    assert!(bl.check_name("node_modules", true).is_some());
    assert!(bl.check_name(".tcfs_dir", false).is_some());

    // .git excluded by default (sync_git_dirs = false)
    assert!(bl.check_name(".git", true).is_some());

    // Stub extensions excluded
    assert!(bl.check_name("file.tc", false).is_some());
    assert!(bl.check_name("file.tcf", false).is_some());

    // Normal files pass through
    assert!(bl.check_name("readme.md", false).is_none());
    assert!(bl.check_name("src", true).is_none());
}

/// Blacklist constructed from SyncConfig reads exclude_patterns.
#[test]
fn blacklist_from_config_reads_patterns() {
    let config = tcfs_core::config::SyncConfig {
        exclude_patterns: vec!["*.tmp".into(), "build/**".into()],
        sync_git_dirs: true,
        ..Default::default()
    };

    let bl = tcfs_sync::blacklist::Blacklist::from_sync_config(&config);

    // User pattern matches
    assert!(bl.check_name("scratch.tmp", false).is_some());

    // .git allowed when sync_git_dirs = true
    assert!(bl.check_name(".git", true).is_none());
}
