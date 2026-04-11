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

        // Atomic write
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, data)
            .await
            .with_context(|| format!("writing cache tmp: {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .await
            .with_context(|| format!("renaming cache entry: {}", path.display()))?;

        // Best-effort eviction; failure is non-fatal
        let _ = self.evict_if_needed().await;

        Ok(())
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
