//! Reconcile wire-up proof: verify reconciliation pipeline produces correct plans.
//!
//! These tests prove the reconcile module is connected to the engine and
//! produces actionable plans. If the wiring breaks, these tests FAIL.

use tcfs_e2e::{memory_operator, write_test_file};
use tempfile::TempDir;

/// Push a file, then reconcile — should show up-to-date.
#[tokio::test]
async fn reconcile_detects_up_to_date() {
    let op = memory_operator();
    let dir = TempDir::new().unwrap();
    let prefix = "test-reconcile";

    let state_path = dir.path().join("state.json");
    let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();

    // Push a file
    write_test_file(dir.path(), "hello.txt", b"hello world");
    let hello_path = dir.path().join("hello.txt");
    tcfs_sync::engine::upload_file(&op, &hello_path, prefix, &mut state, None)
        .await
        .unwrap();

    // Reconcile — should show up-to-date
    let blacklist = tcfs_sync::blacklist::Blacklist::default();
    let config = tcfs_sync::reconcile::ReconcileConfig::default();

    let plan = tcfs_sync::reconcile::reconcile(
        &op,
        dir.path(),
        prefix,
        &state,
        "test-device",
        &blacklist,
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        plan.summary.up_to_date, 1,
        "pushed file should be up-to-date"
    );
    assert_eq!(plan.summary.pushes, 0);
    assert_eq!(plan.summary.pulls, 0);
}

/// New local file not yet pushed — reconcile should plan a push.
#[tokio::test]
async fn reconcile_detects_new_local_file() {
    let op = memory_operator();
    let dir = TempDir::new().unwrap();
    let prefix = "test-reconcile-new";

    let state_path = dir.path().join("state.json");
    let state = tcfs_sync::state::StateCache::open(&state_path).unwrap();

    // Create file but don't push it
    write_test_file(dir.path(), "unpushed.txt", b"not yet synced");

    let blacklist = tcfs_sync::blacklist::Blacklist::default();
    let config = tcfs_sync::reconcile::ReconcileConfig::default();

    let plan = tcfs_sync::reconcile::reconcile(
        &op,
        dir.path(),
        prefix,
        &state,
        "test-device",
        &blacklist,
        &config,
    )
    .await
    .unwrap();

    assert!(
        plan.summary.pushes >= 1,
        "unpushed file should appear as push action, got {} pushes",
        plan.summary.pushes
    );
}

/// Dry-run produces plan but doesn't modify storage.
#[tokio::test]
async fn reconcile_dry_run_no_side_effects() {
    let op = memory_operator();
    let dir = TempDir::new().unwrap();
    let prefix = "test-reconcile-dry";

    let state_path = dir.path().join("state.json");
    let state = tcfs_sync::state::StateCache::open(&state_path).unwrap();

    write_test_file(dir.path(), "newfile.txt", b"dry run test");

    let blacklist = tcfs_sync::blacklist::Blacklist::default();
    let config = tcfs_sync::reconcile::ReconcileConfig::default();

    // Get plan (this is always dry-run — reconcile() never mutates)
    let plan = tcfs_sync::reconcile::reconcile(
        &op,
        dir.path(),
        prefix,
        &state,
        "test-device",
        &blacklist,
        &config,
    )
    .await
    .unwrap();

    assert!(!plan.actions.is_empty(), "should have actions planned");

    // Verify no files were actually uploaded (dry-run)
    let remote_files = op.list(&format!("{prefix}/")).await.unwrap();
    assert!(
        remote_files.is_empty() || remote_files.iter().all(|e| e.path().ends_with('/')),
        "dry-run should not upload any files to storage"
    );
}
