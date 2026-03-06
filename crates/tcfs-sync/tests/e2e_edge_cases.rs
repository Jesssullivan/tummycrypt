//! E2E test: edge cases and boundary conditions
//!
//! Tests special characters, unicode filenames, deeply nested paths,
//! content-addressed dedup, sequential uploads, empty directories,
//! and minimum file sizes.

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::engine::{download_file_with_device, push_tree, upload_file_with_device};
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

/// Test 1: Filenames with spaces and parentheses survive push/pull roundtrip.
#[tokio::test]
async fn special_characters_in_filename() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/special-chars";
    let device_id = "edge-device";

    let original = b"content with special filename";
    let src = write_test_file(tmp.path(), "hello world (copy).txt", original);
    let dst = tmp.path().join("output/hello world (copy).txt");

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("hello world (copy).txt"),
        None,
    )
    .await
    .expect("upload special chars filename");

    assert!(!upload.skipped);
    assert!(upload.chunks > 0);

    let download = download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        device_id,
        Some(&mut state),
        None,
    )
    .await
    .expect("download special chars filename");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(
        downloaded, original,
        "special-char filename content must roundtrip exactly"
    );
    assert_eq!(download.bytes, original.len() as u64);
}

/// Test 2: Unicode filenames survive push/pull roundtrip.
#[tokio::test]
async fn unicode_filename() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/unicode";
    let device_id = "edge-device";

    let original = b"unicode filename test content";
    let filename = "\u{65E5}\u{672C}\u{8A9E}\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt"; // 日本語ファイル.txt
    let src = write_test_file(tmp.path(), filename, original);
    let dst = tmp.path().join(format!("output/{filename}"));

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some(filename),
        None,
    )
    .await
    .expect("upload unicode filename");

    assert!(!upload.skipped);

    let download = download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        device_id,
        Some(&mut state),
        None,
    )
    .await
    .expect("download unicode filename");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(
        downloaded, original,
        "unicode filename content must roundtrip exactly"
    );
    assert_eq!(download.bytes, original.len() as u64);
}

/// Test 3: A 200-character filename survives push/pull roundtrip.
#[tokio::test]
async fn long_filename() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/long-name";
    let device_id = "edge-device";

    // 200-char filename (196 chars of 'a' + ".txt")
    let long_name: String = "a".repeat(196) + ".txt";
    assert_eq!(long_name.len(), 200);

    let original = b"long filename test content";
    let src = write_test_file(tmp.path(), &long_name, original);
    let dst = tmp.path().join(format!("output/{long_name}"));

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some(&long_name),
        None,
    )
    .await
    .expect("upload long filename");

    assert!(!upload.skipped);

    let download = download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        device_id,
        Some(&mut state),
        None,
    )
    .await
    .expect("download long filename");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(
        downloaded, original,
        "long filename content must roundtrip exactly"
    );
    assert_eq!(download.bytes, original.len() as u64);
}

/// Test 4: A file nested 10 directories deep uploads and is tracked in state cache.
#[tokio::test]
async fn deeply_nested_path() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/deep-nest";
    let device_id = "edge-device";

    let nested_rel = "a/b/c/d/e/f/g/h/i/j/deep.txt";
    let original = b"deeply nested file content";
    let src = write_test_file(tmp.path(), nested_rel, original);

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some(nested_rel),
        None,
    )
    .await
    .expect("upload deeply nested file");

    assert!(!upload.skipped);
    assert!(upload.chunks > 0);
    assert_eq!(upload.bytes, original.len() as u64);

    // State cache should track the file
    let cached = state.get(&src).expect("state cache should track deeply nested file");
    assert_eq!(cached.size, original.len() as u64);
    assert!(!cached.blake3.is_empty());
}

/// Test 5: Two files with identical content but different names both upload successfully.
#[tokio::test]
async fn upload_same_content_different_names() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/dedup-names";
    let device_id = "edge-device";

    let content = b"identical content for dedup test";
    let src1 = write_test_file(tmp.path(), "copy1.txt", content);
    let src2 = write_test_file(tmp.path(), "copy2.txt", content);

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let upload1 = upload_file_with_device(
        &op,
        &src1,
        prefix,
        &mut state,
        None,
        device_id,
        Some("copy1.txt"),
        None,
    )
    .await
    .expect("upload copy1");

    let upload2 = upload_file_with_device(
        &op,
        &src2,
        prefix,
        &mut state,
        None,
        device_id,
        Some("copy2.txt"),
        None,
    )
    .await
    .expect("upload copy2");

    assert!(!upload1.skipped, "copy1 should upload");
    // copy2 may be skipped at storage layer (same content hash = same manifest),
    // but the state cache should still track it as a separate file.

    // Both should have the same content hash (content-addressed)
    assert_eq!(
        upload1.hash, upload2.hash,
        "identical content should produce identical hashes"
    );

    // State cache should have entries for both files
    let cached1 = state.get(&src1).expect("state cache should have copy1");
    let cached2 = state.get(&src2).expect("state cache should have copy2");
    assert_eq!(cached1.blake3, cached2.blake3);
}

/// Test 6: Sequential uploads of 10 files, then re-push skips all unchanged files.
#[tokio::test]
async fn concurrent_uploads_same_operator() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/sequential-10";

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Create 10 files with distinct content
    for i in 0..10 {
        let name = format!("file_{i:02}.txt");
        let content = format!("content for file number {i}");
        write_test_file(&src_dir, &name, content.as_bytes());
    }

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    // First push: all 10 should upload
    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("push_tree first pass");

    assert_eq!(uploaded, 10, "first push should upload all 10 files");
    assert_eq!(skipped, 0, "first push should skip none");

    // Verify state cache has 10 entries
    assert_eq!(state.len(), 10, "state cache should have 10 entries");

    // Second push: all 10 should be skipped (unchanged)
    let (uploaded2, skipped2, _) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("push_tree second pass");

    assert_eq!(uploaded2, 0, "second push should upload nothing");
    assert_eq!(skipped2, 10, "second push should skip all 10");
}

/// Test 7: push_tree on an empty directory produces zero uploads with no errors.
#[tokio::test]
async fn empty_directory_push_tree() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/empty-dir";

    let empty_dir = tmp.path().join("empty");
    std::fs::create_dir_all(&empty_dir).unwrap();

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let (uploaded, skipped, bytes) = push_tree(&op, &empty_dir, prefix, &mut state, None)
        .await
        .expect("push_tree on empty dir should not error");

    assert_eq!(uploaded, 0, "empty dir: no files to upload");
    assert_eq!(skipped, 0, "empty dir: no files to skip");
    assert_eq!(bytes, 0, "empty dir: zero bytes");
    assert_eq!(state.len(), 0, "state cache should remain empty");
}

/// Test 8: A single-byte file roundtrips correctly through push/pull.
#[tokio::test]
async fn single_byte_file() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/edge/single-byte";
    let device_id = "edge-device";

    let original = b"x";
    let src = write_test_file(tmp.path(), "tiny.txt", original);
    let dst = tmp.path().join("output/tiny.txt");

    let mut state = StateCache::open(&tmp.path().join("state.db")).expect("open state");

    let upload = upload_file_with_device(
        &op,
        &src,
        prefix,
        &mut state,
        None,
        device_id,
        Some("tiny.txt"),
        None,
    )
    .await
    .expect("upload single byte file");

    assert!(!upload.skipped);
    assert_eq!(upload.bytes, 1);
    assert!(upload.chunks > 0, "even 1 byte should produce at least 1 chunk");

    let download = download_file_with_device(
        &op,
        &upload.remote_path,
        &dst,
        prefix,
        None,
        device_id,
        Some(&mut state),
        None,
    )
    .await
    .expect("download single byte file");

    let downloaded = std::fs::read(&dst).unwrap();
    assert_eq!(downloaded, original, "single byte content must roundtrip exactly");
    assert_eq!(download.bytes, 1);
}
