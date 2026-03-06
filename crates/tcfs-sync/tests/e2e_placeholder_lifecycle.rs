//! E2E test: placeholder lifecycle (push → verify state → unsync → rehydrate)
//!
//! Validates the full lifecycle of a synced file:
//!   1. Push a file via the sync engine
//!   2. Verify state cache has the entry
//!   3. Simulate "unsync" by removing from state cache (placeholder behavior)
//!   4. Re-download (hydrate) and verify content matches original

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::engine::{
    download_file, download_file_with_device, upload_file, upload_file_with_device,
};
use tcfs_sync::state::StateCache;

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

/// Full lifecycle: push → verify state → unsync → rehydrate → verify content.
#[tokio::test]
async fn placeholder_lifecycle_basic() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/placeholder/basic";

    let original = b"important document that will go through the placeholder lifecycle";
    let src = write_test_file(tmp.path(), "document.txt", original);
    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Step 1: Push the file
    let upload = upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload should succeed");

    assert!(!upload.skipped);
    assert!(upload.chunks > 0);

    // Step 2: Verify state cache has the entry
    let cached = state
        .get(&src)
        .expect("state cache should have entry after push");
    assert_eq!(cached.size, original.len() as u64);
    assert!(!cached.blake3.is_empty(), "blake3 hash should be recorded");
    assert!(
        !cached.remote_path.is_empty(),
        "remote path should be recorded"
    );

    let remote_path = cached.remote_path.clone();
    let original_hash = cached.blake3.clone();

    // Step 3: Simulate "unsync" — remove from state cache (like converting to placeholder)
    state.remove(&src);
    state.flush().unwrap();

    assert!(
        state.get(&src).is_none(),
        "entry should be gone after unsync"
    );
    // Local file still exists (placeholder would be a stub, but file stays on disk)
    assert!(src.exists(), "local file should still exist after unsync");

    // Step 4: Re-download (hydrate) — pull from remote to a new local path
    let hydrated_path = tmp.path().join("hydrated/document.txt");
    let download = download_file(&op, &remote_path, &hydrated_path, prefix, None)
        .await
        .expect("rehydrate download should succeed");

    assert_eq!(download.bytes, original.len() as u64);

    // Step 5: Verify content matches original
    let hydrated_content = std::fs::read(&hydrated_path).unwrap();
    assert_eq!(
        hydrated_content, original,
        "rehydrated content must match original"
    );

    // Verify the file hash of hydrated content matches
    let hydrated_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&hydrated_content));
    assert_eq!(
        hydrated_hash, original_hash,
        "rehydrated file hash must match original"
    );
}

/// Lifecycle with device identity and vclock tracking.
#[tokio::test]
async fn placeholder_lifecycle_with_device() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/placeholder/device";
    let device_id = "laptop-001";

    let original = b"device-aware placeholder lifecycle test content";
    let src = write_test_file(tmp.path(), "project/readme.md", original);
    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push with device identity
    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("project/readme.md"),
        None,
    )
    .await
    .expect("upload with device");

    assert!(!upload.skipped);

    // Verify state has vclock entry
    let cached = state.get(&src).expect("state entry");
    assert!(
        !cached.vclock.clocks.is_empty(),
        "vclock should be non-empty"
    );
    assert!(
        cached.vclock.get(device_id) > 0,
        "vclock should track the device"
    );

    let remote_path = cached.remote_path.clone();

    // Unsync
    state.remove(&src);
    state.flush().unwrap();
    assert!(state.get(&src).is_none());

    // Rehydrate with device identity
    let hydrated = tmp.path().join("hydrated/project/readme.md");
    download_file_with_device(
        &op,
        &remote_path,
        &hydrated,
        prefix,
        None,
        device_id,
        Some(&mut state),
        None,
    )
    .await
    .expect("rehydrate with device");

    // Content matches
    let content = std::fs::read(&hydrated).unwrap();
    assert_eq!(content, original);

    // State cache re-populated with vclock
    let recached = state.get(&hydrated).expect("rehydrated state entry");
    assert!(
        !recached.vclock.clocks.is_empty(),
        "vclock should be restored after rehydrate"
    );
}

/// Multiple files: push tree, unsync one, rehydrate just that one.
#[tokio::test]
async fn placeholder_lifecycle_selective_rehydrate() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/placeholder/selective";

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // Push three files
    let files: Vec<(&str, &[u8])> = vec![
        ("alpha.txt", b"content of alpha"),
        ("beta.txt", b"content of beta"),
        ("gamma.txt", b"content of gamma"),
    ];

    let mut remote_paths = Vec::new();
    for (name, content) in &files {
        let src = write_test_file(tmp.path(), &format!("src/{name}"), content);
        let upload = upload_file(&op, &src, prefix, &mut state, None)
            .await
            .expect("upload");
        remote_paths.push((name.to_string(), upload.remote_path, src));
    }

    assert_eq!(state.len(), 3, "should have 3 entries in state");

    // Unsync only beta.txt
    let (_, beta_remote, beta_src) = &remote_paths[1];
    state.remove(beta_src);
    state.flush().unwrap();

    assert_eq!(state.len(), 2, "should have 2 entries after unsync");
    assert!(state.get(beta_src).is_none(), "beta should be unsynced");
    // alpha and gamma still tracked
    assert!(
        state.get(&remote_paths[0].2).is_some(),
        "alpha still in state"
    );
    assert!(
        state.get(&remote_paths[2].2).is_some(),
        "gamma still in state"
    );

    // Rehydrate beta
    let hydrated_beta = tmp.path().join("hydrated/beta.txt");
    download_file(&op, beta_remote, &hydrated_beta, prefix, None)
        .await
        .expect("rehydrate beta");

    let beta_content = std::fs::read(&hydrated_beta).unwrap();
    assert_eq!(
        beta_content, b"content of beta",
        "rehydrated beta content must match"
    );
}

/// Verify that unsync + rehydrate preserves file integrity for binary data.
#[tokio::test]
async fn placeholder_lifecycle_binary() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/placeholder/binary";

    // 128 KiB of pseudo-random binary data
    let original: Vec<u8> = (0u64..131072)
        .map(|i| (i.wrapping_mul(11) ^ (i >> 4)) as u8)
        .collect();

    let src = write_test_file(tmp.path(), "binary.dat", &original);
    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    let _upload = upload_file(&op, &src, prefix, &mut state, None)
        .await
        .expect("upload binary");

    let remote_path = state.get(&src).unwrap().remote_path.clone();

    // Unsync
    state.remove(&src);
    state.flush().unwrap();

    // Rehydrate
    let hydrated = tmp.path().join("hydrated/binary.dat");
    download_file(&op, &remote_path, &hydrated, prefix, None)
        .await
        .expect("rehydrate binary");

    let hydrated_content = std::fs::read(&hydrated).unwrap();
    assert_eq!(hydrated_content.len(), original.len());
    assert_eq!(
        hydrated_content, original,
        "binary data must survive unsync/rehydrate cycle"
    );
}
