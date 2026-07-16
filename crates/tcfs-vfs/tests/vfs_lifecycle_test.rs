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
    tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op)
        .expect("register Memory conditional-write emulation");
    TcfsVfs::new(
        op,
        prefix.to_string(),
        cache_dir,
        64 * 1024 * 1024,
        Duration::from_secs(30),
        "test-device".to_string(),
    )
}

async fn publish_symlink(op: &Operator, prefix: &str, rel_path: &str, target: &str) {
    let manifest = tcfs_sync::manifest::SymlinkManifest::new(
        target,
        tcfs_sync::conflict::VectorClock::new(),
        "test-device".into(),
        0,
        Some(rel_path.into()),
    );
    let bytes = manifest.to_bytes().expect("serialize symlink manifest");
    let object_id = tcfs_sync::index_entry::manifest_object_id(&bytes);
    let entry = tcfs_sync::index_entry::RemoteIndexEntry::new_symlink(object_id, target);
    let baseline = tcfs_sync::engine::bind_indexed_publish_baseline(op, prefix, rel_path)
        .await
        .expect("bind symlink publish baseline");
    tcfs_sync::engine::publish_indexed_manifest(op, prefix, rel_path, bytes, entry, baseline)
        .await
        .expect("publish symlink");
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

#[tokio::test]
async fn encrypted_mount_refuses_plaintext_writes_until_key_is_unlocked() {
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op)
        .expect("register Memory conditional-write emulation");
    let cache = tempfile::tempdir().unwrap();
    let key = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let vfs = TcfsVfs::new(
        op.clone(),
        "encrypted-gate".into(),
        cache.path().join("cache"),
        64 * 1024 * 1024,
        Duration::from_secs(30),
        "test-device".into(),
    )
    .with_shared_master_key(key.clone())
    .require_encryption_for_writes(true);

    let error = vfs
        .create("/", OsStr::new("secret.txt"), 0o600)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("unlocked master key"));
    assert!(op.list("encrypted-gate/").await.unwrap().is_empty());

    *key.lock().await = Some(tcfs_crypto::MasterKey::from_bytes([7u8; 32]));
    let (fh, _) = vfs
        .create("/", OsStr::new("secret.txt"), 0o600)
        .await
        .unwrap();
    vfs.write(fh, 0, b"encrypted content").await.unwrap();
    *key.lock().await = None;
    let release_error = vfs.release(fh).await.unwrap_err();
    assert!(format!("{release_error:#}").contains("unlocked master key"));
    assert!(op.read("encrypted-gate/index/secret.txt").await.is_err());
    *key.lock().await = Some(tcfs_crypto::MasterKey::from_bytes([7u8; 32]));
    vfs.release(fh).await.unwrap();
    let index = op
        .read("encrypted-gate/index/secret.txt")
        .await
        .unwrap()
        .to_bytes();
    let entry = tcfs_sync::index_entry::parse_index_entry(&index).unwrap();
    let manifest = op
        .read(&format!("encrypted-gate/manifests/{}", entry.manifest_hash))
        .await
        .unwrap()
        .to_bytes();
    let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(&manifest).unwrap();
    assert!(manifest.encrypted_file_key.is_some());
}

#[tokio::test]
async fn hydration_only_client_rejects_mutations() {
    let vfs = memory_vfs("read-only").hydration_only();
    let error = vfs.mkdir("/", OsStr::new("nope"), 0o755).await.unwrap_err();
    assert!(format!("{error:#}").contains("hydration-only"));
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
async fn real_tc_extension_is_not_treated_as_stub_suffix() {
    let vfs = memory_vfs("test");

    vfs.mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = vfs
        .create("/docs", OsStr::new("ftrace.tc"), 0o644)
        .await
        .expect("create real .tc file");
    vfs.write(fh, 0, b"real project file with .tc extension")
        .await
        .expect("write real .tc file");
    vfs.release(fh).await.expect("release real .tc file");

    let entries = vfs.readdir("/docs").await.expect("readdir /docs");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"ftrace.tc"),
        "real .tc filenames should remain visible: {names:?}"
    );
    assert!(
        !names.contains(&"ftrace"),
        "real .tc filenames must not be exposed as stripped stub names: {names:?}"
    );

    assert!(
        vfs.getattr("/docs/ftrace").await.is_err(),
        "clean path without .tc should not alias a real .tc file"
    );
    let attr = vfs
        .getattr("/docs/ftrace.tc")
        .await
        .expect("getattr exact .tc file");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::RegularFile);

    let (read_fh, data) = vfs
        .open("/docs/ftrace.tc")
        .await
        .expect("open exact .tc file");
    assert_eq!(&data, b"real project file with .tc extension");
    vfs.release(read_fh).await.expect("release exact .tc file");
}

#[tokio::test]
async fn readdir_getattr_and_readlink_preserve_symlink_entries() {
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let cache = tempfile::tempdir().unwrap();
    let vfs = memory_vfs_with_op(op.clone(), "test", cache.path().join("cache"));

    publish_symlink(&op, "test", "link.txt", "target.txt").await;

    let entries = vfs.readdir("/").await.expect("readdir root");
    let link = entries
        .iter()
        .find(|entry| entry.name == "link.txt")
        .expect("link entry");
    assert_eq!(link.kind, tcfs_vfs::types::VfsFileType::Symlink);

    let attr = vfs.getattr("/link.txt").await.expect("getattr symlink");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::Symlink);
    assert_eq!(attr.size, "target.txt".len() as u64);

    let target = vfs.readlink("/link.txt").await.expect("readlink");
    assert_eq!(target, "target.txt");
}

#[tokio::test]
async fn readlink_rejects_index_manifest_path_forgery_and_unsafe_target() {
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let cache = tempfile::tempdir().unwrap();
    let vfs = memory_vfs_with_op(op.clone(), "test", cache.path().join("cache"));

    let manifest = tcfs_sync::manifest::SymlinkManifest::new(
        "../../.ssh/authorized_keys",
        tcfs_sync::conflict::VectorClock::new(),
        "peer".into(),
        0,
        Some("different-link.txt".into()),
    );
    let bytes = manifest.to_bytes().unwrap();
    let object_id = tcfs_sync::index_entry::manifest_object_id(&bytes);
    op.write(&format!("test/manifests/{object_id}"), bytes)
        .await
        .unwrap();
    let forged = tcfs_sync::index_entry::RemoteIndexEntry::new_symlink(
        object_id,
        "../../.ssh/authorized_keys",
    );
    tcfs_sync::index_entry::write_committed_index_entry(
        &op,
        "test",
        "test/index/link.txt",
        &forged,
    )
    .await
    .unwrap();

    let error = vfs.readlink("/link.txt").await.unwrap_err();
    assert!(format!("{error:#}").contains("rel_path mismatch"));

    let unsafe_manifest = tcfs_sync::manifest::SymlinkManifest::new(
        "../../.ssh/authorized_keys",
        tcfs_sync::conflict::VectorClock::new(),
        "peer".into(),
        0,
        Some("link.txt".into()),
    );
    let unsafe_bytes = unsafe_manifest.to_bytes().unwrap();
    let unsafe_id = tcfs_sync::index_entry::manifest_object_id(&unsafe_bytes);
    op.write(
        &format!("safe-target-check/manifests/{unsafe_id}"),
        unsafe_bytes,
    )
    .await
    .unwrap();
    tcfs_sync::index_entry::write_committed_index_entry(
        &op,
        "safe-target-check",
        "safe-target-check/index/link.txt",
        &tcfs_sync::index_entry::RemoteIndexEntry::new_symlink(
            unsafe_id,
            "../../.ssh/authorized_keys",
        ),
    )
    .await
    .unwrap();
    let guarded = memory_vfs_with_op(op, "safe-target-check", cache.path().join("guarded-cache"));
    let error = guarded.readlink("/link.txt").await.unwrap_err();
    assert!(format!("{error:#}").contains("refusing symlink target"));
}

#[cfg(unix)]
#[tokio::test]
async fn pushed_symlink_json_index_reads_through_vfs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_root = tmp.path().join("source");
    let docs = source_root.join("docs");
    std::fs::create_dir_all(&docs).expect("create source docs");
    std::fs::write(docs.join("target.txt"), b"pushed symlink target").expect("write target");
    std::os::unix::fs::symlink("target.txt", docs.join("link.txt")).expect("create symlink");

    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "pushed-symlink-json-contract";
    tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op)
        .expect("register Memory conditional-write emulation");
    let mut state =
        tcfs_sync::state::StateCache::open(&tmp.path().join("state.json")).expect("state cache");
    let collect = tcfs_sync::engine::CollectConfig {
        preserve_symlinks: true,
        sync_empty_dirs: false,
        ..Default::default()
    };

    let (uploaded, skipped, _) = tcfs_sync::engine::push_tree_with_device(
        &op,
        &source_root,
        prefix,
        &mut state,
        None,
        "",
        Some(&collect),
        None,
    )
    .await
    .expect("push source tree");
    assert_eq!(uploaded, 2);
    assert_eq!(skipped, 0);

    let raw_index = op
        .read(&format!("{prefix}/index/docs/link.txt"))
        .await
        .expect("read symlink index")
        .to_bytes();
    let raw_index = String::from_utf8_lossy(&raw_index);
    assert!(
        raw_index.trim_start().starts_with('{'),
        "sync push should write versioned JSON for symlinks: {raw_index}"
    );
    assert!(
        raw_index.contains(r#""kind": "symlink""#),
        "symlink index should carry v3 kind discriminator: {raw_index}"
    );

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let entries = reader.readdir("/docs").await.expect("readdir /docs");
    let link = entries
        .iter()
        .find(|entry| entry.name == "link.txt")
        .expect("link entry from pushed index");
    assert_eq!(link.kind, tcfs_vfs::types::VfsFileType::Symlink);

    let attr = reader
        .getattr("/docs/link.txt")
        .await
        .expect("getattr pushed symlink");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::Symlink);
    assert_eq!(attr.size, "target.txt".len() as u64);

    let target = reader
        .readlink("/docs/link.txt")
        .await
        .expect("readlink pushed symlink");
    assert_eq!(target, "target.txt");
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
async fn identical_content_at_two_paths_keeps_distinct_bound_manifests() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "path-bound-duplicate-content";
    let content = b"identical bytes must not collapse path-bound metadata";
    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));

    for name in ["a.txt", "b.txt"] {
        let (fh, _) = writer
            .create("/", OsStr::new(name), 0o644)
            .await
            .expect("create duplicate-content path");
        writer.write(fh, 0, content).await.expect("write content");
        writer.release(fh).await.expect("flush bound manifest");
    }

    let a_index = op
        .read(&format!("{prefix}/index/a.txt"))
        .await
        .expect("read a index")
        .to_bytes();
    let b_index = op
        .read(&format!("{prefix}/index/b.txt"))
        .await
        .expect("read b index")
        .to_bytes();
    let a_entry = tcfs_sync::index_entry::parse_index_entry(&a_index).expect("parse a index");
    let b_entry = tcfs_sync::index_entry::parse_index_entry(&b_index).expect("parse b index");
    assert_ne!(
        a_entry.manifest_hash, b_entry.manifest_hash,
        "path-bound manifest object ids must differ even when file bytes match"
    );

    for (name, entry) in [("a.txt", a_entry), ("b.txt", b_entry)] {
        let bytes = op
            .read(&format!("{prefix}/manifests/{}", entry.manifest_hash))
            .await
            .expect("read path-bound manifest")
            .to_bytes();
        let manifest =
            tcfs_sync::manifest::SyncManifest::from_bytes(&bytes).expect("parse manifest");
        assert_eq!(manifest.rel_path.as_deref(), Some(name));
    }

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    for name in ["a.txt", "b.txt"] {
        let (fh, hydrated) = reader
            .open(&format!("/{name}"))
            .await
            .expect("hydrate independently bound duplicate-content path");
        assert_eq!(hydrated, content);
        reader.release(fh).await.expect("release duplicate path");
    }
}

#[tokio::test]
async fn forged_cross_path_index_binding_fails_closed_before_hydration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "forged-cross-path-binding";
    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    let (fh, _) = writer
        .create("/", OsStr::new("source.txt"), 0o644)
        .await
        .expect("create source");
    writer
        .write(fh, 0, b"source bytes")
        .await
        .expect("write source");
    writer.release(fh).await.expect("flush source");

    let source_index = op
        .read(&format!("{prefix}/index/source.txt"))
        .await
        .expect("read source index")
        .to_vec();
    op.write(&format!("{prefix}/index/forged.txt"), source_index)
        .await
        .expect("forge cross-path index pointer");

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let error = reader
        .open("/forged.txt")
        .await
        .expect_err("cross-path manifest substitution must fail closed");
    assert!(
        format!("{error:#}").contains("manifest rel_path mismatch"),
        "unexpected error: {error:#}"
    );
    let alias_error = reader
        .open("/forged.txt.tc")
        .await
        .expect_err("legacy alias must retain logical-path binding checks");
    assert!(
        format!("{alias_error:#}").contains("manifest rel_path mismatch"),
        "unexpected legacy-alias error: {alias_error:#}"
    );
    assert_eq!(
        reader
            .disk_cache()
            .stats()
            .await
            .expect("cache stats")
            .entry_count,
        0,
        "failed binding must not populate the plaintext cache"
    );
}

#[tokio::test]
async fn unsync_path_evicts_cache_and_rehydrates_on_demand() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "mounted-unsync-rehydrate-contract";
    let content = b"cached bytes should leave the machine and rehydrate on demand";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = writer
        .create("/docs", OsStr::new("remote.txt"), 0o644)
        .await
        .expect("create remote file");
    writer
        .write(fh, 0, content)
        .await
        .expect("write remote file");
    writer.release(fh).await.expect("flush remote file");

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let (read_fh, hydrated) = reader
        .open("/docs/remote.txt")
        .await
        .expect("initial hydrate");
    assert_eq!(&hydrated, content);
    reader.release(read_fh).await.expect("release initial read");
    assert_eq!(
        reader
            .disk_cache()
            .stats()
            .await
            .expect("cache stats after initial hydrate")
            .entry_count,
        1,
        "initial open should populate the reader cache"
    );

    let unsynced = reader
        .unsync_path("/docs/remote.txt")
        .await
        .expect("unsync mounted path");
    assert_eq!(unsynced.path, "/docs/remote.txt");
    assert!(unsynced.was_cached, "unsync should evict cached bytes");
    assert_eq!(unsynced.bytes_freed, content.len() as u64);
    assert_eq!(
        reader
            .disk_cache()
            .stats()
            .await
            .expect("cache stats after unsync")
            .entry_count,
        0,
        "unsync should leave no local cached content"
    );

    let entries = reader.readdir("/docs").await.expect("readdir after unsync");
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert!(
        names.contains(&"remote.txt"),
        "unsynced remote entry should remain listable through the clean mounted name: {names:?}"
    );
    assert!(
        !names.contains(&"remote.txt.tc"),
        "mounted unsync should not expose a physical stub suffix: {names:?}"
    );

    let already_unsynced = reader
        .unsync_path("/docs/remote.txt")
        .await
        .expect("unsync already-uncached mounted path");
    assert!(
        !already_unsynced.was_cached,
        "second unsync should report no local bytes to evict"
    );
    assert_eq!(already_unsynced.bytes_freed, 0);

    let (rehydrated_fh, rehydrated) = reader
        .open("/docs/remote.txt")
        .await
        .expect("rehydrate after unsync");
    assert_eq!(&rehydrated, content);
    reader
        .release(rehydrated_fh)
        .await
        .expect("release rehydrated file");
    assert_eq!(
        reader
            .disk_cache()
            .stats()
            .await
            .expect("cache stats after rehydrate")
            .entry_count,
        1,
        "open after unsync should restore exactly one cached entry"
    );
}

#[tokio::test]
async fn remote_create_after_negative_lookup_hydrates_after_invalidate_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "mounted-negative-cache-invalidate-contract";
    let content = b"peer-created bytes after negative lookup";

    let reader = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("reader-cache"));
    assert!(
        reader.getattr("/docs/new.txt").await.is_err(),
        "missing path should seed the reader negative cache"
    );

    let writer = memory_vfs_with_op(op, prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = writer
        .create("/docs", OsStr::new("new.txt"), 0o644)
        .await
        .expect("create peer file");
    writer.write(fh, 0, content).await.expect("write peer file");
    writer.release(fh).await.expect("flush peer file");

    assert!(
        reader.getattr("/docs/new.txt").await.is_err(),
        "negative cache should suppress the new peer file until invalidated"
    );

    reader.invalidate_path("/docs/new.txt");

    let attr = reader
        .getattr("/docs/new.txt")
        .await
        .expect("getattr after invalidation");
    assert_eq!(attr.kind, tcfs_vfs::types::VfsFileType::RegularFile);
    assert_eq!(attr.size, content.len() as u64);

    let entries = reader
        .readdirplus("/docs")
        .await
        .expect("readdirplus after invalidation");
    let entry = entries
        .iter()
        .find(|entry| entry.name == "new.txt")
        .expect("new file entry");
    assert_eq!(entry.kind, tcfs_vfs::types::VfsFileType::RegularFile);
    assert_eq!(
        entry.attr.as_ref().map(|attr| attr.size),
        Some(content.len() as u64)
    );

    let (read_fh, hydrated) = reader
        .open("/docs/new.txt")
        .await
        .expect("hydrate peer-created file after invalidation");
    assert_eq!(&hydrated, content);
    reader.release(read_fh).await.expect("release peer file");
}

#[tokio::test]
async fn remote_delete_while_unhydrated_drops_mounted_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "mounted-delete-peer-unsynced-contract";
    let content = b"remote bytes that should never hydrate before delete";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = writer
        .create("/docs", OsStr::new("doomed.txt"), 0o644)
        .await
        .expect("create doomed");
    writer.write(fh, 0, content).await.expect("write doomed");
    writer.release(fh).await.expect("flush doomed");

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let before = reader
        .readdir("/docs")
        .await
        .expect("readdir before delete");
    let before_names: Vec<&str> = before.iter().map(|entry| entry.name.as_str()).collect();
    assert!(before_names.contains(&"doomed.txt"));
    assert_eq!(
        reader
            .disk_cache()
            .stats()
            .await
            .expect("cache stats before delete")
            .entry_count,
        0,
        "readdir should leave the peer-unhydrated reader cache empty"
    );

    writer
        .unlink("/docs", OsStr::new("doomed.txt"))
        .await
        .expect("remote delete");

    let after = reader.readdir("/docs").await.expect("readdir after delete");
    let after_names: Vec<&str> = after.iter().map(|entry| entry.name.as_str()).collect();
    assert!(
        !after_names.contains(&"doomed.txt"),
        "deleted remote entry should disappear from mounted view: {after_names:?}"
    );
    assert!(
        reader.open("/docs/doomed.txt").await.is_err(),
        "deleted unhydrated entry should not open from a stale listing"
    );
}

#[tokio::test]
async fn remote_rename_while_unhydrated_hydrates_new_mounted_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "mounted-rename-peer-unsynced-contract";
    let content = b"remote bytes should hydrate only through the renamed path";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = writer
        .create("/docs", OsStr::new("old.txt"), 0o644)
        .await
        .expect("create old");
    writer.write(fh, 0, content).await.expect("write old");
    writer.release(fh).await.expect("flush old");

    let reader = memory_vfs_with_op(op, prefix, tmp.path().join("reader-cache"));
    let before = reader
        .readdir("/docs")
        .await
        .expect("readdir before rename");
    let before_names: Vec<&str> = before.iter().map(|entry| entry.name.as_str()).collect();
    assert!(before_names.contains(&"old.txt"));
    assert!(!before_names.contains(&"new.txt"));
    assert_eq!(
        reader
            .disk_cache()
            .stats()
            .await
            .expect("cache stats before rename")
            .entry_count,
        0,
        "reader should still be unhydrated before peer rename"
    );

    writer
        .rename(
            "/docs",
            OsStr::new("old.txt"),
            "/docs",
            OsStr::new("new.txt"),
        )
        .await
        .expect("remote rename");

    let after = reader.readdir("/docs").await.expect("readdir after rename");
    let after_names: Vec<&str> = after.iter().map(|entry| entry.name.as_str()).collect();
    assert!(
        !after_names.contains(&"old.txt"),
        "old path should disappear after peer rename: {after_names:?}"
    );
    assert!(
        after_names.contains(&"new.txt"),
        "new path should appear after peer rename: {after_names:?}"
    );
    assert!(
        reader.open("/docs/old.txt").await.is_err(),
        "old unhydrated path should not hydrate after rename"
    );

    let (fh, hydrated) = reader
        .open("/docs/new.txt")
        .await
        .expect("hydrate renamed path");
    assert_eq!(&hydrated, content);
    reader.release(fh).await.expect("release renamed path");
}

#[tokio::test]
async fn hydrated_remote_file_edit_flushes_exact_content() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "mounted-edit-contract";
    let original = b"original remote-backed content";
    let edited = b"edited through mounted view with longer exact content";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = writer
        .create("/docs", OsStr::new("remote.txt"), 0o644)
        .await
        .expect("create remote file");
    writer.write(fh, 0, original).await.expect("write original");
    writer.release(fh).await.expect("flush original");

    let editor = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("editor-cache"));
    let (edit_fh, hydrated) = editor
        .open("/docs/remote.txt")
        .await
        .expect("hydrate remote file for edit");
    assert_eq!(&hydrated, original);
    editor
        .write(edit_fh, 0, edited)
        .await
        .expect("write edited content");
    editor.release(edit_fh).await.expect("flush edited content");

    let verifier = memory_vfs_with_op(op, prefix, tmp.path().join("verifier-cache"));
    let (verify_fh, verified) = verifier
        .open("/docs/remote.txt")
        .await
        .expect("hydrate edited remote file");
    assert_eq!(&verified, edited);
    verifier.release(verify_fh).await.expect("release verifier");
}

#[tokio::test]
async fn hydrated_remote_file_replacement_truncates_old_tail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    let prefix = "mounted-replace-truncate-contract";
    let original = b"longer original remote-backed content with tail";
    let edited = b"short edit";

    let writer = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("writer-cache"));
    writer
        .mkdir("/", OsStr::new("docs"), 0o755)
        .await
        .expect("mkdir docs");
    let (fh, _) = writer
        .create("/docs", OsStr::new("remote.txt"), 0o644)
        .await
        .expect("create remote file");
    writer.write(fh, 0, original).await.expect("write original");
    writer.release(fh).await.expect("flush original");

    let editor = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("editor-cache"));
    let (edit_fh, hydrated) = editor
        .open("/docs/remote.txt")
        .await
        .expect("hydrate remote file for replacement");
    assert_eq!(&hydrated, original);
    editor
        .truncate(Some("/docs/remote.txt"), Some(edit_fh), 0)
        .await
        .expect("truncate before replacement");
    editor
        .write(edit_fh, 0, edited)
        .await
        .expect("write shorter replacement");
    editor
        .release(edit_fh)
        .await
        .expect("flush shorter replacement");

    let verifier = memory_vfs_with_op(op, prefix, tmp.path().join("verifier-cache"));
    let (verify_fh, verified) = verifier
        .open("/docs/remote.txt")
        .await
        .expect("hydrate replaced remote file");
    assert_eq!(&verified, edited);
    verifier.release(verify_fh).await.expect("release verifier");
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
    tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op)
        .expect("register Memory conditional-write emulation");
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
async fn concurrent_vfs_flush_preserves_the_advanced_remote_version() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let op = Operator::new(opendal::services::Memory::default())
        .unwrap()
        .finish();
    tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op)
        .expect("register Memory conditional-write emulation");
    let prefix = "vfs-concurrent-flush";
    let first = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("first-cache"));
    let second = memory_vfs_with_op(op.clone(), prefix, tmp.path().join("second-cache"));

    let (seed_fh, _) = first
        .create("/", OsStr::new("doc.txt"), 0o644)
        .await
        .expect("create seed");
    first.write(seed_fh, 0, b"base").await.expect("write seed");
    first.release(seed_fh).await.expect("publish seed");

    let (second_fh, second_base) = second.open("/doc.txt").await.expect("second opens base");
    assert_eq!(second_base.as_slice(), b"base");
    let (first_fh, _) = first.open("/doc.txt").await.expect("first opens base");
    first
        .truncate(None, Some(first_fh), 0)
        .await
        .expect("truncate first edit");
    first
        .write(first_fh, 0, b"first wins")
        .await
        .expect("write first edit");
    first.release(first_fh).await.expect("publish first edit");

    second
        .truncate(None, Some(second_fh), 0)
        .await
        .expect("truncate concurrent edit");
    second
        .write(second_fh, 0, b"second loses")
        .await
        .expect("write concurrent edit");
    let error = second.release(second_fh).await.unwrap_err();
    assert!(format!("{error:#}").contains("concurrent remote file update"));

    let verifier = memory_vfs_with_op(op, prefix, tmp.path().join("verifier-cache"));
    let (_, bytes) = verifier.open("/doc.txt").await.expect("read winner");
    assert_eq!(bytes.as_slice(), b"first wins");
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

    let root_entries = reader.readdirplus("/").await.expect("readdirplus root");
    let docs_entries: Vec<_> = root_entries
        .iter()
        .filter(|entry| entry.name == "docs")
        .collect();
    assert_eq!(
        docs_entries.len(),
        1,
        "directory prefix placeholder should not create duplicate docs entries: {root_entries:?}"
    );
    assert_eq!(
        docs_entries[0].kind,
        tcfs_vfs::types::VfsFileType::Directory
    );
    assert_eq!(
        docs_entries[0].attr.as_ref().map(|attr| attr.kind),
        Some(tcfs_vfs::types::VfsFileType::Directory)
    );

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
async fn exact_tc_file_wins_over_legacy_stub_fallback() {
    let vfs = memory_vfs("test");

    let (clean_fh, _) = vfs
        .create("/", OsStr::new("dupe.txt"), 0o644)
        .await
        .expect("create clean file");
    vfs.write(clean_fh, 0, b"clean file content")
        .await
        .expect("write clean file");
    vfs.release(clean_fh).await.expect("release clean file");

    let (tc_fh, _) = vfs
        .create("/", OsStr::new("dupe.txt.tc"), 0o644)
        .await
        .expect("create exact .tc file");
    vfs.write(tc_fh, 0, b"exact .tc file content")
        .await
        .expect("write exact .tc file");
    vfs.release(tc_fh).await.expect("release exact .tc file");

    let (read_fh, data) = vfs.open("/dupe.txt.tc").await.expect("open exact .tc file");
    assert_eq!(
        &data, b"exact .tc file content",
        "exact remote .tc entry should win over legacy clean-path fallback"
    );
    vfs.release(read_fh).await.expect("release exact .tc read");
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
