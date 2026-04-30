//! Integration tests for TcfsVfs lifecycle: readdir, getattr, open/read/release,
//! create/write/flush, mkdir, unlink, rmdir.
//!
//! Uses opendal Memory backend — no network or FUSE mount required.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

use opendal::Operator;
use tcfs_vfs::vfs::VirtualFilesystem;
use tcfs_vfs::TcfsVfs;

/// Create a VFS backed by in-memory storage
fn memory_vfs(prefix: &str) -> TcfsVfs {
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    memory_vfs_with_op(
        op,
        prefix,
        std::path::PathBuf::from("/tmp/tcfs-vfs-test-cache"),
    )
}

fn memory_vfs_with_op(op: Operator, prefix: &str, cache_dir: PathBuf) -> TcfsVfs {
    TcfsVfs::new(
        op,
        prefix.to_string(),
        cache_dir,
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
async fn readdir_after_create_uses_clean_names() {
    let vfs = memory_vfs("test");

    vfs.mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");

    let (fh, _) = vfs
        .create("/docs", OsStr::new("README.md"), 0o644)
        .await
        .expect("create README");
    vfs.write(fh, 0, b"# Clean mounted names")
        .await
        .expect("write README");
    vfs.release(fh).await.expect("release README");

    let entries = vfs.readdir("/docs").await.expect("readdir /docs");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    assert!(
        names.contains(&"README.md"),
        "mounted VFS should expose clean filenames: {names:?}"
    );
    assert!(
        !names.contains(&"README.md.tc"),
        "mounted VFS should not expose physical stub suffixes: {names:?}"
    );
}

#[tokio::test]
async fn readdir_is_lazy_and_open_hydrates_cache() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "lazy-contract";
    let content = b"remote content should hydrate only on open";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    writer
        .mkdir("/docs", OsStr::new("deep"), 0o755)
        .await
        .expect("mkdir docs/deep");
    let (fh, _) = writer
        .create("/docs/deep", OsStr::new("remote.txt"), 0o644)
        .await
        .expect("create remote file");
    writer
        .write(fh, 0, content)
        .await
        .expect("write remote file");
    writer.release(fh).await.expect("flush remote file");

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let before = reader.disk_cache().stats().await.expect("cache stats");
    assert_eq!(before.entry_count, 0, "reader cache starts empty");

    let docs_entries = reader.readdir("/docs").await.expect("readdir /docs");
    let docs_names: Vec<&str> = docs_entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        docs_names.contains(&"deep"),
        "remote directory should be visible before hydration: {docs_names:?}"
    );

    let deep_entries = reader
        .readdir("/docs/deep")
        .await
        .expect("readdir /docs/deep");
    let deep_names: Vec<&str> = deep_entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        deep_names.contains(&"remote.txt"),
        "remote file should be visible before hydration: {deep_names:?}"
    );
    assert!(
        !deep_names.contains(&"remote.txt.tc"),
        "mounted view should not expose physical stub suffixes: {deep_names:?}"
    );

    let after_readdir = reader
        .disk_cache()
        .stats()
        .await
        .expect("cache stats after readdir");
    assert_eq!(
        after_readdir.entry_count, 0,
        "readdir should not hydrate file content"
    );
    assert_eq!(
        after_readdir.total_bytes, 0,
        "readdir should leave the content cache empty"
    );

    let (read_fh, hydrated) = reader
        .open("/docs/deep/remote.txt")
        .await
        .expect("open remote file");
    assert_eq!(&hydrated, content);
    reader.release(read_fh).await.expect("release remote file");

    let after_open = reader
        .disk_cache()
        .stats()
        .await
        .expect("cache stats after open");
    assert_eq!(after_open.entry_count, 1, "open should hydrate one file");
    assert_eq!(
        after_open.total_bytes,
        content.len() as u64,
        "cache should store the hydrated file bytes"
    );
}

#[tokio::test]
async fn sync_push_json_index_hydrates_through_vfs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let source_dir = source_root.join("docs/deep");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    let file_path = source_dir.join("from-push.txt");
    let content = b"sync engine seeded JSON index should hydrate via VFS";
    std::fs::write(&file_path, content).expect("write source file");

    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "sync-json-contract";
    let mut state =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state.json")).expect("state cache");

    let (uploaded, skipped, uploaded_bytes) =
        tcfs_sync::engine::push_tree(&op, &source_root, prefix, &mut state, None)
            .await
            .expect("push tree");
    assert_eq!(uploaded, 1);
    assert_eq!(skipped, 0);
    assert_eq!(uploaded_bytes, content.len() as u64);

    let raw_index = op
        .read(&format!("{prefix}/index/docs/deep/from-push.txt"))
        .await
        .expect("read index")
        .to_bytes();
    assert!(
        String::from_utf8_lossy(&raw_index)
            .trim_start()
            .starts_with('{'),
        "sync push should write the versioned JSON index format"
    );

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let before = reader.disk_cache().stats().await.expect("cache stats");
    assert_eq!(before.entry_count, 0, "reader cache starts empty");

    let entries = reader
        .readdir("/docs/deep")
        .await
        .expect("readdir /docs/deep");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"from-push.txt"),
        "VFS should enumerate sync-engine JSON index entries: {names:?}"
    );

    let after_readdir = reader
        .disk_cache()
        .stats()
        .await
        .expect("cache stats after readdir");
    assert_eq!(
        after_readdir.entry_count, 0,
        "readdir should not hydrate JSON-indexed file content"
    );

    let (fh, hydrated) = reader
        .open("/docs/deep/from-push.txt")
        .await
        .expect("open JSON-indexed remote file");
    assert_eq!(&hydrated, content);
    reader.release(fh).await.expect("release remote file");
}

#[tokio::test]
async fn getattr_directory_prefix_placeholder_is_not_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "directory-prefix-placeholder";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    writer
        .mkdir("/docs", OsStr::new("deep"), 0o755)
        .await
        .expect("mkdir docs/deep");
    let (fh, _) = writer
        .create("/docs/deep", OsStr::new("remote.txt"), 0o644)
        .await
        .expect("create remote file");
    writer
        .write(fh, 0, b"remote content")
        .await
        .expect("write remote file");
    writer.release(fh).await.expect("release remote file");

    let placeholder_key = format!("{prefix}/index/docs");
    op.write(&placeholder_key, Vec::<u8>::new())
        .await
        .expect("write empty directory prefix object");

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let attr = reader.getattr("/docs").await.expect("getattr /docs");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::Directory);

    let entries = reader.readdir("/docs").await.expect("readdir /docs");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"deep"),
        "directory prefix placeholder should not hide child entries: {names:?}"
    );
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
async fn open_accepts_clean_and_legacy_stub_paths() {
    let vfs = memory_vfs("test");

    let (fh, _) = vfs
        .create("/", OsStr::new("legacy.txt"), 0o644)
        .await
        .expect("create");
    vfs.write(fh, 0, b"legacy-compatible hydration")
        .await
        .expect("write");
    vfs.release(fh).await.expect("release");

    let (clean_fh, clean_data) = vfs.open("/legacy.txt").await.expect("open clean path");
    assert_eq!(&clean_data, b"legacy-compatible hydration");
    vfs.release(clean_fh).await.expect("release clean path");

    let (stub_fh, stub_data) = vfs
        .open("/legacy.txt.tc")
        .await
        .expect("open legacy stub path");
    assert_eq!(&stub_data, b"legacy-compatible hydration");
    vfs.release(stub_fh)
        .await
        .expect("release legacy stub path");
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
    assert!(
        names.contains(&"new.txt"),
        "new name should appear as a clean mounted name: {names:?}"
    );
    assert!(
        !names.contains(&"new.txt.tc"),
        "rename should not expose a physical stub suffix: {names:?}"
    );
}

// ── statfs tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn statfs_returns_defaults() {
    let vfs = memory_vfs("test");
    let stats = vfs.statfs().await.expect("statfs");
    assert!(stats.bsize > 0);
}
