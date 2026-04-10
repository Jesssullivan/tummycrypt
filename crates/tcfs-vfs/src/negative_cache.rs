//! Negative dentry cache — suppresses repeated ENOENT lookups for missing paths.
//!
//! Critical for git-intensive workloads: `git status` triggers hundreds of
//! `stat()` calls for files that don't exist. Without a negative cache each
//! miss causes a remote SeaweedFS lookup. With a TTL-bounded cache (default 30s)
//! repeated misses are answered instantly in-process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Thread-safe negative dentry cache with TTL-based expiry.
pub struct NegativeCache {
    entries: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    insert_count: AtomicU64,
}

impl NegativeCache {
    /// Create a new negative cache with the given TTL.
    pub fn new(ttl: Duration) -> Self {
        NegativeCache {
            entries: Mutex::new(HashMap::new()),
            ttl,
            insert_count: AtomicU64::new(0),
        }
    }

    /// Record that `path` does not exist (ENOENT).
    pub fn insert(&self, path: &str) {
        let mut map = self.entries.lock().unwrap();
        map.insert(path.to_string(), Instant::now());

        // Periodic eviction: every 100 inserts, remove expired entries
        let count = self.insert_count.fetch_add(1, Ordering::Relaxed);
        if count.is_multiple_of(100) {
            map.retain(|_, inserted_at| inserted_at.elapsed() < self.ttl);
        }
    }

    /// Returns true if `path` is known to be absent and the TTL has not expired.
    pub fn is_negative(&self, path: &str) -> bool {
        let map = self.entries.lock().unwrap();
        match map.get(path) {
            Some(&inserted_at) => inserted_at.elapsed() < self.ttl,
            None => false,
        }
    }

    /// Remove a path from the negative cache (called when a file is created).
    pub fn remove(&self, path: &str) {
        let mut map = self.entries.lock().unwrap();
        map.remove(path);
    }

    /// Evict all entries whose TTL has expired. Call periodically to avoid unbounded growth.
    pub fn evict_expired(&self) {
        let mut map = self.entries.lock().unwrap();
        map.retain(|_, inserted_at| inserted_at.elapsed() < self.ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn insert_and_check() {
        let cache = NegativeCache::new(Duration::from_secs(30));
        assert!(!cache.is_negative("/foo/bar.tc"));
        cache.insert("/foo/bar.tc");
        assert!(cache.is_negative("/foo/bar.tc"));
    }

    #[test]
    fn remove_clears_entry() {
        let cache = NegativeCache::new(Duration::from_secs(30));
        cache.insert("/foo");
        cache.remove("/foo");
        assert!(!cache.is_negative("/foo"));
    }

    #[test]
    fn ttl_expiry() {
        let cache = NegativeCache::new(Duration::from_millis(50));
        cache.insert("/tmp/test");
        assert!(cache.is_negative("/tmp/test"));
        thread::sleep(Duration::from_millis(80));
        assert!(!cache.is_negative("/tmp/test"));
    }

    #[test]
    fn eviction_bounds_cache_size() {
        let cache = NegativeCache::new(Duration::from_millis(50));

        // Insert 200 entries
        for i in 0..200 {
            cache.insert(&format!("path/{i}"));
        }

        // Wait for TTL to expire
        thread::sleep(Duration::from_millis(100));

        // Insert one more to trigger eviction
        cache.insert("trigger");

        // The cache should have evicted expired entries
        // Only "trigger" should be non-expired
        let map = cache.entries.lock().unwrap();
        assert!(
            map.len() < 50,
            "cache should have evicted expired entries, got {}",
            map.len()
        );
    }
}
