//! Local sync state cache — tracks which files have been uploaded and their content hashes.
//!
//! Backed by a JSON file in Phase 2 (no RocksDB required). The state is loaded
//! into memory at startup, updated on each sync operation, and flushed atomically.
//!
//! Each entry records: blake3 hash, file size, mtime, chunk count, remote path,
//! and last sync timestamp. This allows re-push to detect unchanged files in O(1)
//! per file (stat + hash comparison against cached hash).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::conflict::VectorClock;

/// Sync state for a single local file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    /// BLAKE3 hash of the file content at last sync (hex)
    pub blake3: String,
    /// File size at last sync
    pub size: u64,
    /// mtime as Unix timestamp (seconds) at last sync
    pub mtime: u64,
    /// Number of chunks uploaded
    pub chunk_count: usize,
    /// Remote path/key in SeaweedFS
    pub remote_path: String,
    /// Unix timestamp of last successful sync
    pub last_synced: u64,
    /// Vector clock at last sync
    #[serde(default)]
    pub vclock: VectorClock,
    /// Device ID that performed this sync
    #[serde(default)]
    pub device_id: String,
}

/// In-memory state cache, persisted to a JSON file
pub struct StateCache {
    /// Path to the JSON state file on disk
    db_path: PathBuf,
    /// In-memory map: canonicalized local path → SyncState
    entries: HashMap<String, SyncState>,
    /// Whether there are unsaved changes
    dirty: bool,
    /// Last NATS JetStream sequence processed (for catch-up on restart)
    pub last_nats_seq: u64,
    /// Device ID for this machine
    pub device_id: String,
}

impl StateCache {
    /// Load or create a state cache at the given path.
    /// If the file doesn't exist, starts with an empty cache.
    pub fn open(db_path: &Path) -> Result<Self> {
        let entries = if db_path.exists() {
            let content = std::fs::read_to_string(db_path)
                .with_context(|| format!("reading state cache: {}", db_path.display()))?;
            serde_json::from_str(&content)
                .with_context(|| format!("parsing state cache: {}", db_path.display()))?
        } else {
            HashMap::new()
        };

        Ok(StateCache {
            db_path: db_path.to_path_buf(),
            entries,
            dirty: false,
            last_nats_seq: 0,
            device_id: String::new(),
        })
    }

    /// Look up the sync state for a local file path.
    pub fn get(&self, local_path: &Path) -> Option<&SyncState> {
        let key = path_key(local_path);
        self.entries.get(&key)
    }

    /// Update (or insert) the sync state for a local file.
    pub fn set(&mut self, local_path: &Path, state: SyncState) {
        let key = path_key(local_path);
        self.entries.insert(key, state);
        self.dirty = true;
    }

    /// Remove the sync state for a file (e.g. after deletion).
    pub fn remove(&mut self, local_path: &Path) {
        let key = path_key(local_path);
        if self.entries.remove(&key).is_some() {
            self.dirty = true;
        }
    }

    /// Total number of tracked files
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find a state entry by its remote path suffix (for NATS event lookups).
    pub fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)> {
        self.entries
            .iter()
            .find(|(_, state)| {
                state
                    .remote_path
                    .ends_with(&format!("/{}", rel_path))
                    || state.remote_path == rel_path
            })
            .map(|(k, v)| (k.as_str(), v))
    }

    /// Flush dirty changes to disk using an atomic write (write then rename).
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        // Ensure parent directory exists
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state dir: {}", parent.display()))?;
        }

        let json =
            serde_json::to_string_pretty(&self.entries).context("serializing state cache")?;

        // Atomic write: write to temp file, then rename
        let tmp_path = self.db_path.with_extension("tmp");
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("writing state cache temp: {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.db_path)
            .with_context(|| format!("renaming state cache: {}", self.db_path.display()))?;

        self.dirty = false;
        Ok(())
    }

    /// Check if a file needs to be synced by comparing stat + hash.
    ///
    /// Returns `None` if the file is up to date (unchanged since last sync).
    /// Returns `Some(reason)` if the file needs to be synced.
    pub fn needs_sync(&self, local_path: &Path) -> Result<Option<String>> {
        let meta = std::fs::metadata(local_path)
            .with_context(|| format!("stat: {}", local_path.display()))?;

        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        match self.get(local_path) {
            None => Ok(Some("new file".into())),
            Some(cached) => {
                if cached.size != size {
                    return Ok(Some(format!("size changed: {} → {}", cached.size, size)));
                }
                if cached.mtime != mtime {
                    // mtime changed — verify content hash before uploading
                    let hash = tcfs_chunks::hash_file(local_path)?;
                    let hash_hex = tcfs_chunks::hash_to_hex(&hash);
                    if hash_hex != cached.blake3 {
                        return Ok(Some("content changed (hash mismatch)".into()));
                    }
                    // mtime changed but content is identical — update mtime only
                    // (will be handled by caller updating the cache)
                }
                Ok(None)
            }
        }
    }
}

impl Drop for StateCache {
    fn drop(&mut self) {
        if self.dirty {
            if let Err(e) = self.flush() {
                tracing::warn!("failed to flush state cache on drop: {e}");
            }
        }
    }
}

/// Convert a path to a normalized string key for the HashMap
fn path_key(path: &Path) -> String {
    // Use the canonicalized absolute path as the key
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Create a SyncState from a just-uploaded file
pub fn make_sync_state(
    local_path: &Path,
    hash_hex: String,
    chunk_count: usize,
    remote_path: String,
) -> Result<SyncState> {
    make_sync_state_full(
        local_path,
        hash_hex,
        chunk_count,
        remote_path,
        VectorClock::new(),
        String::new(),
    )
}

/// Create a SyncState with full vector clock and device info.
pub fn make_sync_state_full(
    local_path: &Path,
    hash_hex: String,
    chunk_count: usize,
    remote_path: String,
    vclock: VectorClock,
    device_id: String,
) -> Result<SyncState> {
    let meta = std::fs::metadata(local_path)
        .with_context(|| format!("stat for sync state: {}", local_path.display()))?;

    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(SyncState {
        blake3: hash_hex,
        size: meta.len(),
        mtime,
        chunk_count,
        remote_path,
        last_synced: now,
        vclock,
        device_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn open_nonexistent_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let cache = StateCache::open(&path).unwrap();
        assert!(cache.is_empty());
    }

    #[test]
    fn set_get_flush_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // Write a state entry and flush
        let mut cache = StateCache::open(&path).unwrap();
        let fake_path = dir.path().join("file.txt");
        std::fs::write(&fake_path, b"hello").unwrap();

        cache.set(
            &fake_path,
            SyncState {
                blake3: "abc123".into(),
                size: 5,
                mtime: 1000,
                chunk_count: 1,
                remote_path: "bucket/file.txt".into(),
                last_synced: 9999,
                vclock: VectorClock::new(),
                device_id: String::new(),
            },
        );
        cache.flush().unwrap();

        // Reload and verify
        let cache2 = StateCache::open(&path).unwrap();
        let entry = cache2.get(&fake_path).unwrap();
        assert_eq!(entry.blake3, "abc123");
        assert_eq!(entry.size, 5);
    }

    #[test]
    fn test_remove_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let fake_path = dir.path().join("to_remove.txt");
        std::fs::write(&fake_path, b"data").unwrap();

        cache.set(
            &fake_path,
            SyncState {
                blake3: "hash1".into(),
                size: 4,
                mtime: 1000,
                chunk_count: 1,
                remote_path: "bucket/to_remove.txt".into(),
                last_synced: 9999,
                vclock: VectorClock::new(),
                device_id: String::new(),
            },
        );
        assert_eq!(cache.len(), 1);

        cache.remove(&fake_path);
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&fake_path).is_none());
    }

    #[test]
    fn test_multiple_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        for i in 0..5 {
            let fake_path = dir.path().join(format!("file_{i}.txt"));
            std::fs::write(&fake_path, format!("content {i}")).unwrap();

            cache.set(
                &fake_path,
                SyncState {
                    blake3: format!("hash_{i}"),
                    size: 9,
                    mtime: 1000 + i,
                    chunk_count: 1,
                    remote_path: format!("bucket/file_{i}.txt"),
                    last_synced: 9999,
                    vclock: VectorClock::new(),
                    device_id: String::new(),
                },
            );
        }

        assert_eq!(cache.len(), 5);
        cache.flush().unwrap();

        // Reload and verify all entries
        let cache2 = StateCache::open(&path).unwrap();
        assert_eq!(cache2.len(), 5);
    }

    #[test]
    fn test_needs_sync_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let cache = StateCache::open(&path).unwrap();

        let file = dir.path().join("new.txt");
        std::fs::write(&file, b"new content").unwrap();

        let result = cache.needs_sync(&file).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "new file");
    }

    #[test]
    fn test_flush_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        // Flush empty cache — should succeed even though file doesn't exist
        cache.flush().unwrap();
        // Flush again — no-op
        cache.flush().unwrap();
    }
}
