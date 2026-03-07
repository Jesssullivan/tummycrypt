//! E2E test: two-device sync with index-based conflict detection
//!
//! Verifies that when two devices push different content for the same rel_path
//! without syncing between pushes, the second push detects a conflict via
//! the index entry (not just same-hash manifest).

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;
use tcfs_sync::conflict::SyncOutcome;

fn memory_operator() -> Operator {
    Operator::new(opendal::services::Memory::default())
        .expect("memory operator")
        .finish()
}

fn write_test_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).expect("write test file");
    path
}

/// Two devices push to the same rel_path — second device detects conflict.
#[tokio::test]
async fn two_device_conflict_via_index() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/two-device";

    // Device A pushes "hello.txt"
    let content_a = b"device A version";
    let src_a = write_test_file(tmp.path(), "a/hello.txt", content_a);
    let mut state_a =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state_a.json")).unwrap();

    let upload_a = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-aaa",
        Some("hello.txt"),
        None,
    )
    .await
    .expect("device A upload");

    assert!(!upload_a.skipped, "device A first push should upload");
    state_a.flush().unwrap();

    // Write index entry (simulating what the CLI does after push)
    let index_key = format!("{}/index/hello.txt", prefix);
    let index_entry = format!(
        "manifest_hash={}\nsize={}\nchunks={}\n",
        upload_a.hash, upload_a.bytes, upload_a.chunks
    );
    op.write(&index_key, index_entry.into_bytes())
        .await
        .expect("write index entry");

    // Device B: first write old content, record state, then write new content
    let src_b = write_test_file(tmp.path(), "b/hello.txt", b"old B content");
    let mut state_b =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state_b.json")).unwrap();

    // Record state with old content (simulate having previously pushed)
    let mut initial_b_state = tcfs_sync::state::make_sync_state(
        &src_b,
        "old_hash".to_string(),
        1,
        "old/manifest".to_string(),
    )
    .unwrap();
    initial_b_state.vclock.tick("device-bbb");
    state_b.set(&src_b, initial_b_state);

    // Now write DIFFERENT content so needs_sync detects a change
    let content_b = b"device B divergent version";
    std::fs::write(&src_b, content_b).unwrap();

    let upload_b = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-bbb",
        Some("hello.txt"),
        None,
    )
    .await
    .expect("device B upload");

    // Should detect conflict (concurrent vclocks: A={aaa:1}, B={bbb:1})
    assert!(upload_b.skipped, "conflict should skip upload");
    match &upload_b.outcome {
        Some(SyncOutcome::Conflict(info)) => {
            assert_eq!(info.local_device, "device-bbb");
            assert_eq!(info.remote_device, "device-aaa");
        }
        other => panic!("expected Conflict, got: {:?}", other),
    }
}

/// Device B pushes after pulling A — B's vclock dominates A's, so no conflict.
#[tokio::test]
async fn sequential_push_no_conflict() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/sequential";

    // Device A pushes
    let content_a = b"first version from A";
    let src_a = write_test_file(tmp.path(), "a/doc.txt", content_a);
    let mut state_a =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state_a.json")).unwrap();

    let upload_a = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_a,
        prefix,
        &mut state_a,
        None,
        "device-aaa",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("device A upload");

    assert!(!upload_a.skipped);
    state_a.flush().unwrap();

    // Write index entry
    let index_key = format!("{}/index/doc.txt", prefix);
    let index_entry = format!(
        "manifest_hash={}\nsize={}\nchunks={}\n",
        upload_a.hash, upload_a.bytes, upload_a.chunks
    );
    op.write(&index_key, index_entry.into_bytes())
        .await
        .expect("write index entry");

    // Device B: pulls A's version, then edits locally.
    // After pull+edit, B's vclock should DOMINATE A's: B has A's clock PLUS B's tick.
    let src_b = write_test_file(tmp.path(), "b/doc.txt", content_a);
    let mut state_b =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state_b.json")).unwrap();

    let a_state = state_a.get(&src_a).expect("A should have state");
    let mut initial_b_state = tcfs_sync::state::make_sync_state(
        &src_b,
        upload_a.hash.clone(),
        upload_a.chunks,
        upload_a.remote_path.clone(),
    )
    .unwrap();
    // B's vclock = A's vclock + B's own tick (simulating pull then local edit)
    initial_b_state.vclock = a_state.vclock.clone();
    initial_b_state.vclock.tick("device-bbb"); // B edited after pulling
    state_b.set(&src_b, initial_b_state);

    // Write new content so needs_sync detects a change
    let content_b = b"updated version from B after pulling A";
    std::fs::write(&src_b, content_b).unwrap();

    let upload_b = tcfs_sync::engine::upload_file_with_device(
        &op,
        &src_b,
        prefix,
        &mut state_b,
        None,
        "device-bbb",
        Some("doc.txt"),
        None,
    )
    .await
    .expect("device B upload");

    // B's clock {aaa:1, bbb:1} dominates A's {aaa:1} → LocalNewer
    assert!(
        !upload_b.skipped,
        "sequential push should succeed, got outcome: {:?}",
        upload_b.outcome
    );
    match &upload_b.outcome {
        Some(SyncOutcome::LocalNewer) | None => {} // Both are acceptable
        other => panic!("expected LocalNewer or None, got: {:?}", other),
    }
}

/// Conflict records ConflictInfo in the state cache for tcfs resolve.
#[tokio::test]
async fn conflict_records_state() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/conflict-state";

    // Device A pushes
    let src_a = write_test_file(tmp.path(), "a/data.bin", b"alpha");
    let mut state_a =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state_a.json")).unwrap();

    let upload_a = tcfs_sync::engine::upload_file_with_device(
        &op, &src_a, prefix, &mut state_a, None, "dev-a", Some("data.bin"), None,
    )
    .await
    .expect("A upload");
    state_a.flush().unwrap();

    // Write index
    let index_key = format!("{}/index/data.bin", prefix);
    let index_entry = format!(
        "manifest_hash={}\nsize={}\nchunks={}\n",
        upload_a.hash, upload_a.bytes, upload_a.chunks
    );
    op.write(&index_key, index_entry.into_bytes())
        .await
        .unwrap();

    // Device B: write old content, record state, then write new content
    let src_b = write_test_file(tmp.path(), "b/data.bin", b"old");
    let mut state_b =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state_b.json")).unwrap();

    let mut stale_state = tcfs_sync::state::make_sync_state(
        &src_b, "stale".into(), 1, "stale/manifest".into(),
    )
    .unwrap();
    stale_state.vclock.tick("dev-b");
    state_b.set(&src_b, stale_state);

    // Write different content so needs_sync triggers
    std::fs::write(&src_b, b"bravo").unwrap();

    let upload_b = tcfs_sync::engine::upload_file_with_device(
        &op, &src_b, prefix, &mut state_b, None, "dev-b", Some("data.bin"), None,
    )
    .await
    .expect("B upload");

    assert!(upload_b.skipped);
    assert!(matches!(upload_b.outcome, Some(SyncOutcome::Conflict(_))));

    // Verify conflict is recorded in state cache
    let cached = state_b.get(&src_b).expect("state should exist after conflict");
    assert!(
        cached.conflict.is_some(),
        "conflict info should be recorded in state"
    );
    let conflict = cached.conflict.as_ref().unwrap();
    assert_eq!(conflict.local_device, "dev-b");
    assert_eq!(conflict.remote_device, "dev-a");
}
