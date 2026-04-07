//! Integration tests for TcfsVfs lifecycle: readdir, getattr, open/read/release,
//! create/write/flush, mkdir, unlink, rmdir.
//!
//! Uses opendal Memory backend — no network or FUSE mount required.

use std::ffi::OsStr;
use std::time::Duration;

use opendal::Operator;
use tcfs_vfs::vfs::VirtualFilesystem;
use tcfs_vfs::TcfsVfs;

/// Create a VFS backed by in-memory storage
fn memory_vfs(prefix: &str) -> TcfsVfs {
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    TcfsVfs::new(
        op,
        prefix.to_string(),
        std::path::PathBuf::from("/tmp/tcfs-vfs-test-cache"),
        64 * 1024 * 1024,
        Duration::from_secs(30),
        "test-device".to_string(),
    )
}

// ── getattr tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn getattr_root_returns_directory() {
    let vfs = memory_vfs("test");
    let attr = vfs.getattr("/").await.expect("root getattr");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::Directory);
}

#[tokio::test]
async fn getattr_nonexistent_file_errors() {
    let vfs = memory_vfs("test");
    let result = vfs.getattr("/nonexistent.txt.tc").await;
    assert!(result.is_err());
}

// ── readdir tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn readdir_empty_root_returns_empty() {
    let vfs = memory_vfs("test");
    let entries = vfs.readdir("/").await.expect("readdir root");
    // Fresh VFS with no index entries should be empty
    assert!(entries.is_empty());
}

// ── mkdir + readdir tests ────────────────────────────────────────────────

#[tokio::test]
async fn mkdir_creates_directory() {
    let vfs = memory_vfs("test");

    let attr = vfs
        .mkdir("/", OsStr::new("subdir"), 0o755)
        .await
        .expect("mkdir");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::Directory);
}

#[tokio::test]
async fn mkdir_appears_in_readdir() {
    let vfs = memory_vfs("test");

    vfs.mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");

    let entries = vfs.readdir("/").await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"docs"), "expected 'docs' in {names:?}");
}

#[tokio::test]
async fn nested_mkdir() {
    let vfs = memory_vfs("test");

    vfs.mkdir("/", OsStr::new("a"), 0o755)
        .await
        .expect("mkdir a");
    vfs.mkdir("/a", OsStr::new("b"), 0o755)
        .await
        .expect("mkdir a/b");

    let entries = vfs.readdir("/a").await.expect("readdir /a");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"b"), "expected 'b' in {names:?}");
}

// ── create + write + read roundtrip ──────────────────────────────────────

#[tokio::test]
async fn create_write_read_roundtrip() {
    let vfs = memory_vfs("test");

    // Create a file
    let (fh, attr) = vfs
        .create("/", OsStr::new("hello.txt"), 0o644)
        .await
        .expect("create");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::RegularFile);

    // Write content
    let data = b"Hello, TcfsVfs!";
    let written = vfs.write(fh, 0, data).await.expect("write");
    assert_eq!(written as usize, data.len());

    // Read it back from the same handle
    let read_back = vfs.read(fh, 0, data.len() as u32).await.expect("read");
    assert_eq!(&read_back, data);

    // Release
    vfs.release(fh).await.expect("release");
}

#[tokio::test]
async fn write_at_offset() {
    let vfs = memory_vfs("test");

    let (fh, _) = vfs
        .create("/", OsStr::new("offset.txt"), 0o644)
        .await
        .expect("create");

    // Write initial content
    vfs.write(fh, 0, b"AAAA").await.expect("write1");

    // Overwrite at offset 2
    vfs.write(fh, 2, b"BB").await.expect("write2");

    // Read all
    let data = vfs.read(fh, 0, 10).await.expect("read");
    assert_eq!(&data, b"AABB");

    vfs.release(fh).await.expect("release");
}

#[tokio::test]
async fn read_with_offset_and_size() {
    let vfs = memory_vfs("test");

    let (fh, _) = vfs
        .create("/", OsStr::new("slice.txt"), 0o644)
        .await
        .expect("create");

    vfs.write(fh, 0, b"0123456789").await.expect("write");

    // Read middle slice
    let slice = vfs.read(fh, 3, 4).await.expect("read slice");
    assert_eq!(&slice, b"3456");

    // Read past end returns available data
    let tail = vfs.read(fh, 8, 100).await.expect("read tail");
    assert_eq!(&tail, b"89");

    vfs.release(fh).await.expect("release");
}

// ── unlink tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn unlink_removes_file() {
    let vfs = memory_vfs("test");

    let (fh, _) = vfs
        .create("/", OsStr::new("doomed.txt"), 0o644)
        .await
        .expect("create");
    vfs.write(fh, 0, b"soon gone").await.expect("write");
    vfs.release(fh).await.expect("release");

    vfs.unlink("/", OsStr::new("doomed.txt"))
        .await
        .expect("unlink");

    // Should no longer appear in readdir
    let entries = vfs.readdir("/").await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&"doomed.txt"), "file should be gone");
}

// ── rmdir tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rmdir_empty_directory() {
    let vfs = memory_vfs("test");

    vfs.mkdir("/", OsStr::new("empty"), 0o755)
        .await
        .expect("mkdir");
    vfs.rmdir("/", OsStr::new("empty"))
        .await
        .expect("rmdir empty dir");

    let entries = vfs.readdir("/").await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&"empty"), "dir should be gone");
}

// ── rename tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn rename_file() {
    let vfs = memory_vfs("test");

    let (fh, _) = vfs
        .create("/", OsStr::new("old.txt"), 0o644)
        .await
        .expect("create");
    vfs.write(fh, 0, b"content").await.expect("write");
    vfs.release(fh).await.expect("release");

    vfs.rename("/", OsStr::new("old.txt"), "/", OsStr::new("new.txt"))
        .await
        .expect("rename");

    let entries = vfs.readdir("/").await.expect("readdir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(!names.contains(&"old.txt"), "old name should be gone");
    // new.txt should exist (as new.txt.tc in stub format)
}

// ── statfs tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn statfs_returns_defaults() {
    let vfs = memory_vfs("test");
    let stats = vfs.statfs().await.expect("statfs");
    assert!(stats.bsize > 0);
}
