//! Shared E2E test helpers for tcfs integration tests.

use std::path::Path;
use std::time::Duration;

use opendal::Operator;
use tcfs_vfs::TcfsVfs;

/// Create an opendal operator backed by in-memory storage.
pub fn memory_operator() -> Operator {
    Operator::new(opendal::services::Memory::default())
        .expect("memory operator")
        .finish()
}

/// Write a file with the given content and return its path.
pub fn write_test_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, content).expect("write test file");
    path
}

/// Create a TcfsVfs backed by the given operator.
pub fn vfs_from_operator(op: Operator, prefix: &str, cache_dir: &Path) -> TcfsVfs {
    TcfsVfs::new(
        op,
        prefix.to_string(),
        cache_dir.to_path_buf(),
        64 * 1024 * 1024,
        Duration::from_secs(30),
        "e2e-test-device".to_string(),
    )
}
