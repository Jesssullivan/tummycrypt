//! Disk cache for hydrated file content.
//!
//! Stores fully-assembled file content keyed by manifest hash. Files are
//! written atomically (temp → rename) and evicted LRU-style when the cache
//! exceeds `max_bytes`.
//!
//! Cache layout: `{cache_dir}/{hash[0..2]}/{hash}` (two-level sharding).

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::fs;

pub struct DiskCache {
    dir: PathBuf,
    max_bytes: u64,
}

impl DiskCache {
    /// Create a new disk cache at `dir` with the given capacity.
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        DiskCache { dir, max_bytes }
    }

    /// Return the cache path for a given manifest hash key.
    fn path_for(&self, key: &str) -> PathBuf {
        // Two-level sharding: first two chars as subdirectory
        let prefix = if key.len() >= 2 { &key[..2] } else { "xx" };
        self.dir.join(prefix).join(key)
    }

    /// Look up cached content by manifest hash. Returns `None` if not cached.
    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.path_for(key);
        fs::read(&path).await.ok()
    }

    /// Store content in the cache, atomically. Evicts old entries if needed.
    pub async fn put(&self, key: &str, data: &[u8]) -> Result<()> {
        let path = self.path_for(key);

        // Ensure the shard directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating cache dir: {}", parent.display()))?;
        }

        // Atomic write through a per-writer temporary name: two concurrent
        // puts of one key must never interleave into one temp file. The
        // guard removes the temp file if this future is dropped (a protocol
        // timeout) or fails before the rename.
        let mut tmp = CacheTmp::new(unique_tmp_path(&path));
        fs::write(tmp.path(), data)
            .await
            .with_context(|| format!("writing cache tmp: {}", tmp.path().display()))?;
        fs::rename(tmp.path(), &path)
            .await
            .with_context(|| format!("renaming cache entry: {}", path.display()))?;
        tmp.disarm();

        // Best-effort eviction; failure is non-fatal
        let _ = self.evict_if_needed().await;

        Ok(())
    }

    /// Open a cached entry for reading, or `None` when it is not cached.
    ///
    /// The returned descriptor keeps the content readable even if the entry
    /// is evicted or replaced afterwards.
    pub fn open_entry(&self, key: &str) -> Result<Option<std::fs::File>> {
        let path = self.path_for(key);
        match std::fs::File::open(&path) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("opening cache entry: {}", path.display()))
            }
        }
    }

    /// Create a private, uniquely named temporary file beside `key`'s entry,
    /// open for reading and writing. Commit it with [`DiskCache::commit_tmp`].
    ///
    /// The returned [`CacheTmp`] removes the file's name when it is dropped
    /// uncommitted, so an error, a panic or a cancelled future (for example a
    /// protocol timeout around hydration) never leaves an orphan behind. An
    /// open descriptor keeps the bytes readable after the name is gone.
    pub fn create_tmp(&self, key: &str) -> Result<(CacheTmp, std::fs::File)> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating cache dir: {}", parent.display()))?;
        }
        let tmp = unique_tmp_path(&path);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("creating cache tmp: {}", tmp.display()))?;
        Ok((CacheTmp::new(tmp), file))
    }

    /// Atomically publish a temporary file from [`DiskCache::create_tmp`] as
    /// `key`'s entry, then evict old entries if the cache is over capacity.
    /// On failure the temporary file is removed.
    pub async fn commit_tmp(&self, mut tmp: CacheTmp, key: &str) -> Result<()> {
        let path = self.path_for(key);
        fs::rename(tmp.path(), &path)
            .await
            .with_context(|| format!("renaming cache entry: {}", path.display()))?;
        tmp.disarm();
        let _ = self.evict_if_needed().await;
        Ok(())
    }

    /// Remove temporary files older than `older_than` from the shard
    /// directories, returning how many were removed.
    ///
    /// A writer that died mid-hydration (a killed or restarted daemon) leaves
    /// its temporary file behind, and `stats()` and eviction never count
    /// `*.tmp` names, so without a sweep they would grow without bound. A live
    /// writer keeps its file's mtime fresh, so an age threshold well above any
    /// write gap never removes a file someone is still filling.
    pub fn sweep_stale_tmp(&self, older_than: std::time::Duration) -> usize {
        let now = std::time::SystemTime::now();
        let mut removed = 0usize;
        let Ok(top) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        for shard in top.flatten() {
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Ok(inner) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in inner.flatten() {
                if !entry.file_name().to_string_lossy().ends_with(".tmp") {
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                let age = meta
                    .modified()
                    .ok()
                    .and_then(|mtime| now.duration_since(mtime).ok())
                    .unwrap_or_default();
                if age >= older_than && std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Returns true if the key is already cached.
    pub async fn contains(&self, key: &str) -> bool {
        self.path_for(key).exists()
    }

    /// Evict least-recently-used entries until total cache size is under `max_bytes`.
    async fn evict_if_needed(&self) -> Result<()> {
        let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
        let mut total: u64 = 0;

        // Walk two-level cache dirs
        let mut top = fs::read_dir(&self.dir).await?;
        while let Some(shard) = top.next_entry().await? {
            if !shard.file_type().await?.is_dir() {
                continue;
            }
            let mut inner = fs::read_dir(shard.path()).await?;
            while let Some(entry) = inner.next_entry().await? {
                let meta = entry.metadata().await?;
                if meta.is_file() && !entry.file_name().to_string_lossy().ends_with(".tmp") {
                    let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    total += meta.len();
                    entries.push((entry.path(), meta.len(), mtime));
                }
            }
        }

        if total <= self.max_bytes {
            return Ok(());
        }

        // Sort oldest access first, delete until under limit
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        for (path, size, _) in entries {
            if total <= self.max_bytes {
                break;
            }
            let _ = fs::remove_file(&path).await;
            total = total.saturating_sub(size);
        }

        Ok(())
    }

    /// Evict a specific cache entry by key. Returns bytes freed, or 0 if not cached.
    pub async fn evict(&self, key: &str) -> Result<u64> {
        let path = self.path_for(key);
        match fs::metadata(&path).await {
            Ok(meta) => {
                let size = meta.len();
                fs::remove_file(&path)
                    .await
                    .with_context(|| format!("evicting cache entry: {}", path.display()))?;
                Ok(size)
            }
            Err(_) => Ok(0), // Not cached
        }
    }

    /// Evict all cache entries whose key starts with `prefix`. Returns total bytes freed.
    ///
    /// Walks the two-level shard directories and removes matching files.
    pub async fn evict_prefix(&self, prefix: &str) -> Result<u64> {
        let mut freed = 0u64;
        let mut top = fs::read_dir(&self.dir).await?;
        while let Some(shard) = top.next_entry().await? {
            if !shard.file_type().await?.is_dir() {
                continue;
            }
            let mut inner = fs::read_dir(shard.path()).await?;
            while let Some(entry) = inner.next_entry().await? {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(prefix) {
                    if let Ok(meta) = entry.metadata().await {
                        if meta.is_file() {
                            freed += meta.len();
                            let _ = fs::remove_file(entry.path()).await;
                        }
                    }
                }
            }
        }
        Ok(freed)
    }
}

/// Statistics about the disk cache
#[derive(Debug)]
pub struct CacheStats {
    /// Total bytes used by cached entries
    pub total_bytes: u64,
    /// Maximum allowed cache size in bytes
    pub max_bytes: u64,
    /// Number of cached file entries
    pub entry_count: usize,
    /// Number of shard directories
    pub shard_count: usize,
}

impl DiskCache {
    /// Compute cache usage statistics by walking the two-level shard dirs.
    pub async fn stats(&self) -> Result<CacheStats> {
        let mut total: u64 = 0;
        let mut count: usize = 0;
        let mut shards: usize = 0;

        if !self.dir.exists() {
            return Ok(CacheStats {
                total_bytes: 0,
                max_bytes: self.max_bytes,
                entry_count: 0,
                shard_count: 0,
            });
        }

        let mut top = fs::read_dir(&self.dir).await?;
        while let Some(shard) = top.next_entry().await? {
            if !shard.file_type().await?.is_dir() {
                continue;
            }
            shards += 1;
            let mut inner = fs::read_dir(shard.path()).await?;
            while let Some(entry) = inner.next_entry().await? {
                let meta = entry.metadata().await?;
                if meta.is_file() && !entry.file_name().to_string_lossy().ends_with(".tmp") {
                    total += meta.len();
                    count += 1;
                }
            }
        }

        Ok(CacheStats {
            total_bytes: total,
            max_bytes: self.max_bytes,
            entry_count: count,
            shard_count: shards,
        })
    }
}

/// A cache temporary file's name, removed on drop unless committed.
#[derive(Debug)]
pub struct CacheTmp {
    path: PathBuf,
    armed: bool,
}

impl CacheTmp {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// The temporary file's path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CacheTmp {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A temporary sibling of `path` that no other writer can be using. The
/// `.tmp` suffix keeps it out of `stats()` and eviction accounting.
fn unique_tmp_path(path: &std::path::Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        "{name}.{}.{}.{n}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

/// Derive a safe filesystem key from a manifest path by hashing it.
/// Use the manifest hash directly when available; this is for fallback.
pub fn cache_key_for_path(manifest_path: &str) -> String {
    // manifest_path is already a hash-based path like "{prefix}/manifests/{hash}"
    // Use just the last component (the hash) as the key
    manifest_path
        .rsplit('/')
        .next()
        .unwrap_or(manifest_path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 100 * 1024 * 1024);

        cache.put("abc123", b"hello world").await.unwrap();
        let result = cache.get("abc123").await.unwrap();
        assert_eq!(result, b"hello world");
    }

    #[tokio::test]
    async fn miss_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 100 * 1024 * 1024);
        assert!(cache.get("nonexistent").await.is_none());
    }

    #[test]
    fn cache_key_extraction() {
        assert_eq!(
            cache_key_for_path("mydata/manifests/abc123def"),
            "abc123def"
        );
        assert_eq!(cache_key_for_path("abc"), "abc");
    }

    #[tokio::test]
    async fn test_stats_empty_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 100 * 1024 * 1024);
        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.shard_count, 0);
        assert_eq!(stats.max_bytes, 100 * 1024 * 1024);
    }

    #[tokio::test]
    async fn test_stats_after_put() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 100 * 1024 * 1024);

        cache.put("abc123", b"hello world").await.unwrap();
        cache.put("def456", b"foo bar baz").await.unwrap();
        cache.put("ghi789", b"test").await.unwrap();

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.entry_count, 3);
        assert_eq!(stats.total_bytes, 11 + 11 + 4);
        assert!(stats.shard_count > 0);
    }

    #[tokio::test]
    async fn test_stats_excludes_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 100 * 1024 * 1024);

        cache.put("abc123", b"hello world").await.unwrap();

        // Manually create a .tmp file in the same shard
        let shard_dir = dir.path().join("ab");
        tokio::fs::write(shard_dir.join("stale.tmp"), b"garbage")
            .await
            .unwrap();

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.total_bytes, 11);
    }

    #[tokio::test]
    async fn tmp_files_are_unique_and_commit_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 1024 * 1024);

        let (tmp_a, mut file_a) = cache.create_tmp("abc123").unwrap();
        let (tmp_b, _file_b) = cache.create_tmp("abc123").unwrap();
        assert_ne!(
            tmp_a.path(),
            tmp_b.path(),
            "concurrent writers must not share a temp file"
        );
        assert!(cache.open_entry("abc123").unwrap().is_none());

        use std::io::Write;
        file_a.write_all(b"committed").unwrap();
        cache.commit_tmp(tmp_a, "abc123").await.unwrap();
        assert_eq!(cache.get("abc123").await.unwrap(), b"committed");
        assert!(cache.open_entry("abc123").unwrap().is_some());
        assert_eq!(cache.stats().await.unwrap().entry_count, 1);
    }

    fn tmp_names(dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        for shard in std::fs::read_dir(dir).unwrap().flatten() {
            if shard.path().is_dir() {
                for entry in std::fs::read_dir(shard.path()).unwrap().flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".tmp") {
                        out.push(name);
                    }
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn uncommitted_tmp_is_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 1024 * 1024);

        let (tmp, file) = cache.create_tmp("abc123").unwrap();
        assert!(tmp.path().exists());
        drop(tmp);
        drop(file);
        assert!(
            tmp_names(dir.path()).is_empty(),
            "a dropped, uncommitted temp file must not be left behind"
        );
    }

    #[test]
    fn sweep_removes_only_stale_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 1024 * 1024);
        let shard = dir.path().join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("abc123"), b"entry").unwrap();
        let stale = shard.join("abc123.1.dead.0.tmp");
        std::fs::write(&stale, b"orphan").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let fresh = shard.join("abc123.1.live.1.tmp");
        std::fs::write(&fresh, b"in progress").unwrap();

        assert_eq!(
            cache.sweep_stale_tmp(std::time::Duration::from_secs(3600)),
            1
        );
        assert!(!stale.exists(), "a stale temp file must be swept");
        assert!(fresh.exists(), "a live writer's temp file must be kept");
        assert!(shard.join("abc123").exists(), "entries are never swept");
    }

    #[tokio::test]
    async fn evict_returns_bytes_freed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 1024 * 1024);

        let data = vec![0xABu8; 4096];
        cache.put("test_key_abc", &data).await.unwrap();
        assert!(cache.contains("test_key_abc").await);

        let freed = cache.evict("test_key_abc").await.unwrap();
        assert_eq!(freed, 4096);
        assert!(!cache.contains("test_key_abc").await);
    }

    #[tokio::test]
    async fn evict_missing_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 1024 * 1024);

        let freed = cache.evict("nonexistent").await.unwrap();
        assert_eq!(freed, 0);
    }

    #[tokio::test]
    async fn evict_prefix_removes_matching() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 1024 * 1024);

        cache.put("abc_one", b"data1").await.unwrap();
        cache.put("abc_two", b"data22").await.unwrap();
        cache.put("xyz_other", b"data333").await.unwrap();

        let freed = cache.evict_prefix("abc").await.unwrap();
        assert_eq!(freed, 5 + 6); // "data1" + "data22"
        assert!(!cache.contains("abc_one").await);
        assert!(!cache.contains("abc_two").await);
        assert!(cache.contains("xyz_other").await);
    }
}
