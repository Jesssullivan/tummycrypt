//! E2E test: directory operations (push_tree with add/delete/modify/rename)
//!
//! Validates push_tree behavior when the local directory changes between
//! successive pushes: file deletion, addition, modification, rename, nested
//! directories, and empty subdirectories.

use opendal::Operator;
use std::path::Path;
use tempfile::TempDir;

use tcfs_sync::engine::push_tree;
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

/// Push a tree with 3 files, delete one, push again.
/// The deleted file should not be re-uploaded and counts should reflect
/// only the files still on disk.
#[tokio::test]
async fn push_tree_then_delete_file_and_re_push() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/dir-ops/delete-re-push";

    let src_dir = tmp.path().join("src");
    write_test_file(&src_dir, "one.txt", b"file one");
    write_test_file(&src_dir, "two.txt", b"file two");
    write_test_file(&src_dir, "three.txt", b"file three");

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // First push: all 3 files uploaded
    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("first push_tree");

    assert_eq!(uploaded, 3, "first push should upload all 3 files");
    assert_eq!(skipped, 0);

    // Delete one file from disk
    std::fs::remove_file(src_dir.join("two.txt")).expect("delete two.txt");

    // Second push: only 2 files remain, both unchanged
    let (uploaded2, skipped2, _bytes2) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("second push_tree");

    assert_eq!(
        uploaded2 + skipped2,
        2,
        "second push should only see 2 files on disk"
    );
    assert_eq!(uploaded2, 0, "unchanged files should not be re-uploaded");
    assert_eq!(skipped2, 2, "both remaining files should be skipped");
}

/// Push a deeply nested directory structure and verify all files are uploaded.
#[tokio::test]
async fn push_tree_nested_directories() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/dir-ops/nested";

    let src_dir = tmp.path().join("src");
    write_test_file(&src_dir, "a/b/c/d/file.txt", b"deeply nested content");
    write_test_file(&src_dir, "a/b/other.txt", b"mid-level content");
    write_test_file(&src_dir, "a/top.txt", b"top-level content");

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("push_tree nested");

    assert_eq!(uploaded, 3, "should upload all 3 nested files");
    assert_eq!(skipped, 0);
}

/// Push a tree that contains an empty subdirectory alongside real files.
/// Empty dirs should not cause errors and only actual files should be counted.
#[tokio::test]
async fn push_tree_empty_subdirectory() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/dir-ops/empty-subdir";

    let src_dir = tmp.path().join("src");
    write_test_file(&src_dir, "real.txt", b"real file content");
    write_test_file(&src_dir, "subdir/also_real.txt", b"also real");

    // Create an empty subdirectory
    std::fs::create_dir_all(src_dir.join("empty_dir")).unwrap();

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("push_tree with empty subdir");

    assert_eq!(uploaded, 2, "should upload only the 2 real files");
    assert_eq!(skipped, 0);
}

/// Push tree with 2 files, add a 3rd, push again.
/// First push uploads 2, second push uploads 1 new and skips 2.
#[tokio::test]
async fn push_tree_add_file_and_re_push() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/dir-ops/add-re-push";

    let src_dir = tmp.path().join("src");
    write_test_file(&src_dir, "alpha.txt", b"alpha content");
    write_test_file(&src_dir, "beta.txt", b"beta content");

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // First push: 2 files
    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("first push_tree");

    assert_eq!(uploaded, 2, "first push should upload 2 files");
    assert_eq!(skipped, 0);

    // Add a 3rd file
    write_test_file(&src_dir, "gamma.txt", b"gamma content");

    // Second push: 1 new, 2 skipped
    let (uploaded2, skipped2, _bytes2) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("second push_tree");

    assert_eq!(uploaded2, 1, "second push should upload 1 new file");
    assert_eq!(skipped2, 2, "second push should skip 2 unchanged files");
}

/// Push tree, modify a file in a nested subdir, push again.
/// Only the modified file should be re-uploaded; others should be skipped.
#[tokio::test]
async fn push_tree_file_in_subdir_modified() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/dir-ops/subdir-modify";

    let src_dir = tmp.path().join("src");
    write_test_file(&src_dir, "root.txt", b"root content");
    write_test_file(&src_dir, "sub/nested.txt", b"original nested content");
    write_test_file(&src_dir, "sub/deep/leaf.txt", b"leaf content");

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // First push: all 3
    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("first push_tree");

    assert_eq!(uploaded, 3);
    assert_eq!(skipped, 0);

    // Modify the nested file
    std::fs::write(
        src_dir.join("sub/nested.txt"),
        b"modified nested content with extra data",
    )
    .expect("modify nested file");

    // Second push: 1 uploaded (modified), 2 skipped
    let (uploaded2, skipped2, _bytes2) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("second push_tree");

    assert_eq!(
        uploaded2, 1,
        "only the modified file should be re-uploaded"
    );
    assert_eq!(skipped2, 2, "unmodified files should be skipped");
}

/// Push tree with file a.txt, then delete a.txt and create b.txt with the
/// same content. Push again. The tree should reflect only what is on disk:
/// b.txt should appear (uploaded or skipped via content dedup), and total
/// file count should be 1.
#[tokio::test]
async fn push_tree_rename_file() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let prefix = "e2e/dir-ops/rename";

    let src_dir = tmp.path().join("src");
    let content = b"content that will be renamed";
    write_test_file(&src_dir, "a.txt", content);

    let mut state = StateCache::open(&tmp.path().join("state.db")).unwrap();

    // First push: 1 file
    let (uploaded, skipped, _bytes) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("first push_tree");

    assert_eq!(uploaded, 1);
    assert_eq!(skipped, 0);

    // "Rename": delete a.txt, create b.txt with same content
    std::fs::remove_file(src_dir.join("a.txt")).expect("delete a.txt");
    write_test_file(&src_dir, "b.txt", content);

    // Second push: only b.txt is on disk
    let (uploaded2, skipped2, _bytes2) = push_tree(&op, &src_dir, prefix, &mut state, None)
        .await
        .expect("second push_tree");

    assert_eq!(
        uploaded2 + skipped2,
        1,
        "only 1 file should be on disk after rename"
    );
    // b.txt is a new path so it should be uploaded (even if content is identical,
    // state cache is keyed by local path)
    assert_eq!(uploaded2, 1, "new path b.txt should be uploaded");
}
