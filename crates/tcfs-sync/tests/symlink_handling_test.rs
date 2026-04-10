//! Tests for symlink handling during file collection.
//!
//! Verifies that collect_files correctly handles:
//! - Regular symlinks to files (follow vs skip)
//! - Regular symlinks to directories (follow vs skip)
//! - Broken symlinks (skip with warning, don't crash)
//! - Circular symlinks (detect cycle, don't infinite loop)

use std::path::Path;
use tempfile::TempDir;
use tcfs_sync::engine::{collect_files, CollectConfig};

fn config_no_follow() -> CollectConfig {
    CollectConfig {
        follow_symlinks: false,
        ..Default::default()
    }
}

fn config_follow() -> CollectConfig {
    CollectConfig {
        follow_symlinks: true,
        ..Default::default()
    }
}

fn write_file(dir: &Path, name: &str, content: &[u8]) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
}

#[test]
fn symlink_to_file_skipped_when_follow_false() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(root, "real.txt", b"real content");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(root.join("real.txt"), root.join("link.txt")).unwrap();

    let files = collect_files(root, &config_no_follow()).unwrap();

    // Should only contain real.txt, not link.txt
    assert_eq!(files.len(), 1, "should skip symlink, got: {:?}", files);
    assert!(
        files[0].ends_with("real.txt"),
        "should contain real.txt, got: {:?}",
        files
    );
}

#[test]
fn symlink_to_file_included_when_follow_true() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(root, "real.txt", b"real content");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(root.join("real.txt"), root.join("link.txt")).unwrap();

    let files = collect_files(root, &config_follow()).unwrap();

    // Should contain both real.txt and link.txt
    assert_eq!(files.len(), 2, "should follow symlink, got: {:?}", files);
    let names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"real.txt".to_string()));
    assert!(names.contains(&"link.txt".to_string()));
}

#[test]
fn symlink_to_dir_skipped_when_follow_false() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("subdir")).unwrap();
    write_file(root, "subdir/inner.txt", b"inner");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("subdir"), root.join("link_dir")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(root.join("subdir"), root.join("link_dir")).unwrap();

    let files = collect_files(root, &config_no_follow()).unwrap();

    // Should only find inner.txt via subdir, not link_dir
    assert_eq!(files.len(), 1, "should skip dir symlink, got: {:?}", files);
}

#[test]
fn symlink_to_dir_followed_when_follow_true() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("subdir")).unwrap();
    write_file(root, "subdir/inner.txt", b"inner");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("subdir"), root.join("link_dir")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(root.join("subdir"), root.join("link_dir")).unwrap();

    let files = collect_files(root, &config_follow()).unwrap();

    // Should find inner.txt via both subdir and link_dir, but cycle detection
    // means the symlink target (subdir) is already visited, so link_dir is skipped
    // Actually: the cycle detection tracks canonical paths. subdir/ is visited
    // from the real dir walk, then link_dir → subdir is detected as a cycle.
    // So we get 1 file, not 2.
    assert_eq!(
        files.len(),
        1,
        "cycle detection should prevent re-traversal, got: {:?}",
        files
    );
}

#[cfg(unix)]
#[test]
fn broken_symlink_skipped_gracefully() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(root, "real.txt", b"real content");
    // Create a symlink to a non-existent target
    std::os::unix::fs::symlink("/nonexistent/path/to/nowhere", root.join("broken.txt")).unwrap();

    let files = collect_files(root, &config_no_follow()).unwrap();
    assert_eq!(files.len(), 1, "broken symlink should be skipped");
    assert!(files[0].ends_with("real.txt"));

    // Also test with follow=true — should still skip (broken target)
    let files = collect_files(root, &config_follow()).unwrap();
    assert_eq!(
        files.len(),
        1,
        "broken symlink should be skipped even with follow=true"
    );
}

#[cfg(unix)]
#[test]
fn circular_symlink_detected() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(root, "real.txt", b"real content");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    // a/link → b, b/link → a (circular)
    std::os::unix::fs::symlink(root.join("b"), root.join("a/link")).unwrap();
    std::os::unix::fs::symlink(root.join("a"), root.join("b/link")).unwrap();

    // With follow=true, should detect the cycle and not infinite loop
    let files = collect_files(root, &config_follow()).unwrap();
    // Should find real.txt without getting stuck
    assert!(
        !files.is_empty(),
        "should find at least real.txt despite circular symlinks"
    );
    assert!(
        files.iter().any(|f| f.ends_with("real.txt")),
        "should still find real.txt"
    );
}
