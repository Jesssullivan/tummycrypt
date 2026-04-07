//! E2E: VFS filesystem operations with in-memory backend
//!
//! Tests the VFS layer (create, write, read, mkdir, rename, unlink, rmdir)
//! as an end-to-end user would interact with the filesystem.

use std::ffi::OsStr;

use tcfs_e2e::{memory_operator, vfs_from_operator};
use tcfs_vfs::vfs::VirtualFilesystem;
use tempfile::TempDir;

#[tokio::test]
async fn create_write_read_release_cycle() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-vfs", &tmp.path().join("cache"));

    let (fh, attr) = vfs
        .create("/", OsStr::new("hello.txt"), 0o644)
        .await
        .expect("create");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::RegularFile);

    let content = b"Hello from E2E VFS test!";
    let written = vfs.write(fh, 0, content).await.expect("write");
    assert_eq!(written as usize, content.len());

    let data = vfs.read(fh, 0, content.len() as u32).await.expect("read");
    assert_eq!(&data, content);

    vfs.release(fh).await.expect("release");
}

#[tokio::test]
async fn directory_tree_operations() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-dirs", &tmp.path().join("cache"));

    // Build a directory tree
    vfs.mkdir("/", OsStr::new("src"), 0o755)
        .await
        .expect("mkdir src");
    vfs.mkdir("/src", OsStr::new("lib"), 0o755)
        .await
        .expect("mkdir lib");
    vfs.mkdir("/src", OsStr::new("bin"), 0o755)
        .await
        .expect("mkdir bin");

    // Verify tree
    let root = vfs.readdir("/").await.expect("readdir /");
    assert!(root.iter().any(|e| e.name == "src"));

    let src = vfs.readdir("/src").await.expect("readdir /src");
    let names: Vec<&str> = src.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"lib"), "missing lib in {names:?}");
    assert!(names.contains(&"bin"), "missing bin in {names:?}");
}

#[tokio::test]
async fn create_file_in_subdirectory() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-subdir", &tmp.path().join("cache"));

    vfs.mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir");

    let (fh, _) = vfs
        .create("/docs", OsStr::new("README.md"), 0o644)
        .await
        .expect("create in subdir");
    vfs.write(fh, 0, b"# Title").await.expect("write");
    vfs.release(fh).await.expect("release");

    let entries = vfs.readdir("/docs").await.expect("readdir /docs");
    assert!(!entries.is_empty(), "docs should contain README");
}

#[tokio::test]
async fn unlink_then_readdir() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-unlink", &tmp.path().join("cache"));

    let (fh, _) = vfs
        .create("/", OsStr::new("delete-me.txt"), 0o644)
        .await
        .expect("create");
    vfs.write(fh, 0, b"will be deleted").await.expect("write");
    vfs.release(fh).await.expect("release");

    vfs.unlink("/", OsStr::new("delete-me.txt"))
        .await
        .expect("unlink");

    let entries = vfs.readdir("/").await.expect("readdir");
    assert!(
        !entries.iter().any(|e| e.name == "delete-me.txt"),
        "file should be gone"
    );
}

#[tokio::test]
async fn rmdir_empty_then_readdir() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-rmdir", &tmp.path().join("cache"));

    vfs.mkdir("/", OsStr::new("empty-dir"), 0o755)
        .await
        .expect("mkdir");
    vfs.rmdir("/", OsStr::new("empty-dir"))
        .await
        .expect("rmdir");

    let entries = vfs.readdir("/").await.expect("readdir");
    assert!(
        !entries.iter().any(|e| e.name == "empty-dir"),
        "dir should be gone"
    );
}

#[tokio::test]
async fn rename_file_across_read() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-rename", &tmp.path().join("cache"));

    let (fh, _) = vfs
        .create("/", OsStr::new("old.txt"), 0o644)
        .await
        .expect("create");
    vfs.write(fh, 0, b"renamed content").await.expect("write");
    vfs.release(fh).await.expect("release");

    vfs.rename("/", OsStr::new("old.txt"), "/", OsStr::new("new.txt"))
        .await
        .expect("rename");

    let entries = vfs.readdir("/").await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&"old.txt"), "old name should be gone");
}

#[tokio::test]
async fn write_at_offset_preserves_data() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-offset", &tmp.path().join("cache"));

    let (fh, _) = vfs
        .create("/", OsStr::new("offset.bin"), 0o644)
        .await
        .expect("create");

    vfs.write(fh, 0, b"AAAA").await.expect("write1");
    vfs.write(fh, 2, b"BB").await.expect("write2");

    let data = vfs.read(fh, 0, 10).await.expect("read");
    assert_eq!(&data, b"AABB");

    vfs.release(fh).await.expect("release");
}

#[tokio::test]
async fn statfs_basic() {
    let tmp = TempDir::new().unwrap();
    let op = memory_operator();
    let vfs = vfs_from_operator(op, "e2e-statfs", &tmp.path().join("cache"));

    let stats = vfs.statfs().await.expect("statfs");
    assert!(stats.bsize > 0);
}
