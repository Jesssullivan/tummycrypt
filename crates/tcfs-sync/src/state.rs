//! Local sync state cache — tracks which files have been uploaded and their content hashes.
//!
//! Two backends are available:
//!   - **JSON** (default): loads entirely into memory, flushed atomically via temp+rename.
//!   - **RocksDB** (behind `full` feature): write-through to RocksDB with in-memory mirror.
//!
//! Both implement `StateCacheBackend`, so callers can use either transparently.
//!
//! Each entry records: blake3 hash, file size, mtime, chunk count, remote path,
//! and last sync timestamp. This allows re-push to detect unchanged files in O(1)
//! per file (stat + hash comparison against cached hash).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::conflict::VectorClock;

// ── FileSyncStatus ──────────────────────────────────────────────────────────

/// Per-file sync status, modeled after odrive's FileSyncState.
///
/// Unlike `SyncState` (which is persisted), this is a transient runtime status
/// that reflects what is happening to a file *right now*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSyncStatus {
    /// File exists only as a stub/placeholder (not hydrated).
    NotSynced,
    /// File content matches remote — fully synchronized.
    Synced,
    /// File is actively being uploaded or downloaded.
    Active,
    /// File is locked by another operation and cannot be modified.
    Locked,
    /// Local and remote versions diverged (vector clock conflict).
    Conflict,
}

impl std::fmt::Display for FileSyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileSyncStatus::NotSynced => write!(f, "not_synced"),
            FileSyncStatus::Synced => write!(f, "synced"),
            FileSyncStatus::Active => write!(f, "active"),
            FileSyncStatus::Locked => write!(f, "locked"),
            FileSyncStatus::Conflict => write!(f, "conflict"),
        }
    }
}

// ── PathLocks ───────────────────────────────────────────────────────────────

/// Per-path locking to prevent concurrent operations on the same file.
///
/// Multiple files can be processed concurrently, but a single file cannot
/// be pushed + pulled + unsynced simultaneously.
#[derive(Debug, Clone)]
pub struct PathLocks {
    inner: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Default for PathLocks {
    fn default() -> Self {
        Self::new()
    }
}

impl PathLocks {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Acquire a lock for the given path. Returns a guard that releases on drop.
    ///
    /// If the path is already locked by another task, this will wait.
    pub async fn lock(&self, path: &Path) -> PathLockGuard {
        let key = path_key(path);
        let mutex = {
            let mut map = self.inner.lock().await;
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = mutex.lock_owned().await;
        PathLockGuard {
            _guard: guard,
            key,
            inner: self.inner.clone(),
        }
    }

    /// Try to acquire a lock without waiting. Returns None if already locked.
    pub async fn try_lock(&self, path: &Path) -> Option<PathLockGuard> {
        let key = path_key(path);
        let mutex = {
            let mut map = self.inner.lock().await;
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        match mutex.try_lock_owned() {
            Ok(guard) => Some(PathLockGuard {
                _guard: guard,
                key,
                inner: self.inner.clone(),
            }),
            Err(_) => None,
        }
    }

    /// Check if a path is currently locked (non-blocking).
    pub async fn is_locked(&self, path: &Path) -> bool {
        let key = path_key(path);
        let map = self.inner.lock().await;
        if let Some(mutex) = map.get(&key) {
            mutex.try_lock().is_err()
        } else {
            false
        }
    }
}

/// RAII guard for a per-path lock. Cleans up the lock entry when no other
/// references exist to avoid unbounded memory growth.
pub struct PathLockGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    key: String,
    inner: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Drop for PathLockGuard {
    fn drop(&mut self) {
        // Best-effort cleanup: remove the entry if we're the last holder.
        if let Ok(mut map) = self.inner.try_lock() {
            if let Some(mutex) = map.get(&self.key) {
                // strong_count == 2: the map entry + this guard's clone.
                if Arc::strong_count(mutex) <= 2 {
                    map.remove(&self.key);
                }
            }
        }
    }
}

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
    /// Conflict info if a vclock divergence was detected during sync.
    /// Set by the sync engine, cleared by `tcfs resolve`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<crate::conflict::ConflictInfo>,
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

    /// Reload entries from disk, merging any new entries written by other processes.
    /// Existing in-memory entries are NOT overwritten (in-memory wins).
    pub fn reload_from_disk(&mut self) -> Result<()> {
        if !self.db_path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&self.db_path)
            .with_context(|| format!("reloading state cache: {}", self.db_path.display()))?;
        let disk_entries: HashMap<String, SyncState> = serde_json::from_str(&content)
            .with_context(|| format!("parsing state cache: {}", self.db_path.display()))?;
        for (key, state) in disk_entries {
            self.entries.entry(key).or_insert(state);
        }
        Ok(())
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

    /// Find a state entry by relative path (for NATS auto-pull lookups).
    ///
    /// Searches cache keys (canonical local paths) by suffix match, then
    /// falls back to remote_path matching. Cache keys are absolute paths
    /// like `/home/jess/tcfs/dir/file.txt` — matching the suffix against
    /// the normalized rel_path (`dir/file.txt`) handles cross-host home
    /// directory differences.
    pub fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)> {
        let normalized = rel_path.trim_start_matches('/');
        // Primary: match cache keys (canonical local paths) by suffix
        self.entries
            .iter()
            .find(|(key, _)| {
                key.ends_with(&format!("/{}", normalized)) || *key == normalized
            })
            // Fallback: match remote_path (manifest path) for backward compat
            .or_else(|| {
                self.entries.iter().find(|(_, state)| {
                    state.remote_path.ends_with(&format!("/{}", normalized))
                        || state.remote_path == normalized
                })
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

    /// Find all entries whose key starts with the given directory prefix.
    pub fn children_with_prefix(&self, dir_path: &Path) -> Vec<(String, &SyncState)> {
        let prefix = path_key(dir_path);
        let prefix_slash = if prefix.ends_with('/') {
            prefix
        } else {
            format!("{}/", prefix)
        };
        self.entries
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix_slash))
            .map(|(k, v)| (k.clone(), v))
            .collect()
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

/// Trait for state cache backends (JSON and RocksDB).
pub trait StateCacheBackend {
    /// Look up the sync state for a local file path.
    fn get(&self, local_path: &Path) -> Option<&SyncState>;
    /// Update (or insert) the sync state for a local file.
    fn set(&mut self, local_path: &Path, state: SyncState);
    /// Remove the sync state for a file.
    fn remove(&mut self, local_path: &Path);
    /// Flush pending changes to durable storage.
    fn flush(&mut self) -> Result<()>;
    /// Return all entries as (key, state) pairs.
    fn all_entries(&self) -> Vec<(String, &SyncState)>;
    /// Find a state entry by its remote path suffix.
    fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)>;
    /// Check if a file needs sync (returns reason or None if up-to-date).
    fn needs_sync(&self, local_path: &Path) -> Result<Option<String>>;
    /// Number of tracked files.
    fn len(&self) -> usize;
    /// Whether the cache is empty.
    fn is_empty(&self) -> bool;
    /// Find all entries whose path starts with the given directory prefix.
    /// Used for dirty-child checks before folder unsync.
    fn children_with_prefix(&self, dir_path: &Path) -> Vec<(String, &SyncState)>;
}

impl StateCacheBackend for StateCache {
    fn get(&self, local_path: &Path) -> Option<&SyncState> {
        self.get(local_path)
    }
    fn set(&mut self, local_path: &Path, state: SyncState) {
        self.set(local_path, state);
    }
    fn remove(&mut self, local_path: &Path) {
        self.remove(local_path);
    }
    fn flush(&mut self) -> Result<()> {
        self.flush()
    }
    fn all_entries(&self) -> Vec<(String, &SyncState)> {
        self.entries.iter().map(|(k, v)| (k.clone(), v)).collect()
    }
    fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)> {
        self.get_by_rel_path(rel_path)
    }
    fn needs_sync(&self, local_path: &Path) -> Result<Option<String>> {
        self.needs_sync(local_path)
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn is_empty(&self) -> bool {
        self.is_empty()
    }
    fn children_with_prefix(&self, dir_path: &Path) -> Vec<(String, &SyncState)> {
        self.children_with_prefix(dir_path)
    }
}

// ── RocksDB backend ──────────────────────────────────────────────────────────

#[cfg(feature = "full")]
mod rocksdb_backend {
    use super::*;

    /// RocksDB-backed state cache with in-memory mirror for API compatibility.
    ///
    /// On `open()`, all keys are loaded into a `HashMap` mirror so that
    /// `get()` can return `&SyncState` references. Writes go through to
    /// RocksDB immediately (write-through), so `flush()` is a no-op.
    pub struct RocksDbStateCache {
        db: rocksdb::DB,
        /// In-memory mirror loaded on open, updated on set/remove.
        entries: HashMap<String, SyncState>,
        /// Device ID for this machine.
        pub device_id: String,
        /// Last NATS JetStream sequence processed.
        pub last_nats_seq: u64,
    }

    impl RocksDbStateCache {
        /// Open or create a RocksDB state cache at the given path.
        pub fn open(db_path: &Path) -> Result<Self> {
            let mut opts = rocksdb::Options::default();
            opts.create_if_missing(true);

            let db = rocksdb::DB::open(&opts, db_path)
                .with_context(|| format!("opening RocksDB: {}", db_path.display()))?;

            // Load all entries into memory mirror
            let mut entries = HashMap::new();
            let iter = db.iterator(rocksdb::IteratorMode::Start);
            for item in iter {
                let (key_bytes, value_bytes) = item.with_context(|| "iterating RocksDB entries")?;
                let key = String::from_utf8_lossy(&key_bytes).to_string();
                if let Ok(state) = serde_json::from_slice::<SyncState>(&value_bytes) {
                    entries.insert(key, state);
                }
            }

            Ok(RocksDbStateCache {
                db,
                entries,
                device_id: String::new(),
                last_nats_seq: 0,
            })
        }
    }

    impl StateCacheBackend for RocksDbStateCache {
        fn get(&self, local_path: &Path) -> Option<&SyncState> {
            let key = super::path_key(local_path);
            self.entries.get(&key)
        }

        fn set(&mut self, local_path: &Path, state: SyncState) {
            let key = super::path_key(local_path);
            // Write-through to RocksDB
            if let Ok(json) = serde_json::to_vec(&state) {
                if let Err(e) = self.db.put(key.as_bytes(), &json) {
                    tracing::warn!("RocksDB put failed for {key}: {e}");
                }
            }
            self.entries.insert(key, state);
        }

        fn remove(&mut self, local_path: &Path) {
            let key = super::path_key(local_path);
            if let Err(e) = self.db.delete(key.as_bytes()) {
                tracing::warn!("RocksDB delete failed for {key}: {e}");
            }
            self.entries.remove(&key);
        }

        fn flush(&mut self) -> Result<()> {
            // Write-through means nothing to flush; RocksDB WAL handles durability.
            Ok(())
        }

        fn all_entries(&self) -> Vec<(String, &SyncState)> {
            self.entries.iter().map(|(k, v)| (k.clone(), v)).collect()
        }

        fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)> {
            self.entries
                .iter()
                .find(|(_, state)| {
                    state.remote_path.ends_with(&format!("/{}", rel_path))
                        || state.remote_path == rel_path
                })
                .map(|(k, v)| (k.as_str(), v))
        }

        fn needs_sync(&self, local_path: &Path) -> Result<Option<String>> {
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
                        return Ok(Some(format!("size changed: {} -> {}", cached.size, size)));
                    }
                    if cached.mtime != mtime {
                        let hash = tcfs_chunks::hash_file(local_path)?;
                        let hash_hex = tcfs_chunks::hash_to_hex(&hash);
                        if hash_hex != cached.blake3 {
                            return Ok(Some("content changed (hash mismatch)".into()));
                        }
                    }
                    Ok(None)
                }
            }
        }

        fn len(&self) -> usize {
            self.entries.len()
        }

        fn is_empty(&self) -> bool {
            self.entries.is_empty()
        }

        fn children_with_prefix(&self, dir_path: &Path) -> Vec<(String, &SyncState)> {
            let prefix = super::path_key(dir_path);
            let prefix_slash = if prefix.ends_with('/') {
                prefix
            } else {
                format!("{}/", prefix)
            };
            self.entries
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix_slash))
                .map(|(k, v)| (k.clone(), v))
                .collect()
        }
    }
}

#[cfg(feature = "full")]
pub use rocksdb_backend::RocksDbStateCache;

/// Dispatch enum that wraps either a JSON or RocksDB state cache.
///
/// Used by `tcfsd` to select backend at runtime based on config path.
pub enum StateBackend {
    Json(StateCache),
    #[cfg(feature = "full")]
    Rocks(RocksDbStateCache),
}

impl StateBackend {
    /// Open the appropriate backend based on path extension.
    ///
    /// Paths ending in `.json` use the JSON backend; otherwise RocksDB (if compiled with `full`).
    pub fn open(db_path: &Path) -> Result<Self> {
        let is_json = db_path
            .extension()
            .map(|ext| ext == "json")
            .unwrap_or(false);

        #[cfg(feature = "full")]
        if !is_json {
            return Ok(StateBackend::Rocks(RocksDbStateCache::open(db_path)?));
        }

        #[cfg(not(feature = "full"))]
        if !is_json {
            tracing::warn!(
                "RocksDB not compiled in (missing 'full' feature), falling back to JSON backend"
            );
        }

        Ok(StateBackend::Json(StateCache::open(db_path)?))
    }

    /// Get the device_id.
    pub fn device_id(&self) -> &str {
        match self {
            StateBackend::Json(c) => &c.device_id,
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => &c.device_id,
        }
    }

    /// Set the device_id.
    pub fn set_device_id(&mut self, id: String) {
        match self {
            StateBackend::Json(c) => c.device_id = id,
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.device_id = id,
        }
    }

    /// Get the last NATS sequence.
    pub fn last_nats_seq(&self) -> u64 {
        match self {
            StateBackend::Json(c) => c.last_nats_seq,
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.last_nats_seq,
        }
    }

    /// Set the last NATS sequence.
    pub fn set_last_nats_seq(&mut self, seq: u64) {
        match self {
            StateBackend::Json(c) => c.last_nats_seq = seq,
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.last_nats_seq = seq,
        }
    }
}

impl StateCacheBackend for StateBackend {
    fn get(&self, local_path: &Path) -> Option<&SyncState> {
        match self {
            StateBackend::Json(c) => c.get(local_path),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.get(local_path),
        }
    }
    fn set(&mut self, local_path: &Path, state: SyncState) {
        match self {
            StateBackend::Json(c) => c.set(local_path, state),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.set(local_path, state),
        }
    }
    fn remove(&mut self, local_path: &Path) {
        match self {
            StateBackend::Json(c) => c.remove(local_path),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.remove(local_path),
        }
    }
    fn flush(&mut self) -> Result<()> {
        match self {
            StateBackend::Json(c) => c.flush(),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.flush(),
        }
    }
    fn all_entries(&self) -> Vec<(String, &SyncState)> {
        match self {
            StateBackend::Json(c) => StateCacheBackend::all_entries(c),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.all_entries(),
        }
    }
    fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)> {
        match self {
            StateBackend::Json(c) => StateCacheBackend::get_by_rel_path(c, rel_path),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.get_by_rel_path(rel_path),
        }
    }
    fn needs_sync(&self, local_path: &Path) -> Result<Option<String>> {
        match self {
            StateBackend::Json(c) => StateCacheBackend::needs_sync(c, local_path),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.needs_sync(local_path),
        }
    }
    fn len(&self) -> usize {
        match self {
            StateBackend::Json(c) => StateCacheBackend::len(c),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.len(),
        }
    }
    fn is_empty(&self) -> bool {
        match self {
            StateBackend::Json(c) => StateCacheBackend::is_empty(c),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.is_empty(),
        }
    }
    fn children_with_prefix(&self, dir_path: &Path) -> Vec<(String, &SyncState)> {
        match self {
            StateBackend::Json(c) => StateCacheBackend::children_with_prefix(c, dir_path),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.children_with_prefix(dir_path),
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
        conflict: None,
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
                conflict: None,
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
                conflict: None,
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
                    conflict: None,
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

    #[test]
    fn test_file_sync_status_display() {
        assert_eq!(FileSyncStatus::NotSynced.to_string(), "not_synced");
        assert_eq!(FileSyncStatus::Synced.to_string(), "synced");
        assert_eq!(FileSyncStatus::Active.to_string(), "active");
        assert_eq!(FileSyncStatus::Locked.to_string(), "locked");
        assert_eq!(FileSyncStatus::Conflict.to_string(), "conflict");
    }

    #[test]
    fn test_file_sync_status_serde() {
        let status = FileSyncStatus::Conflict;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"conflict\"");
        let parsed: FileSyncStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, status);
    }

    #[test]
    fn test_children_with_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        // Create a directory structure
        let sub = dir.path().join("project");
        std::fs::create_dir_all(&sub).unwrap();
        let f1 = sub.join("a.txt");
        let f2 = sub.join("b.txt");
        let f3 = dir.path().join("root.txt");
        std::fs::write(&f1, b"a").unwrap();
        std::fs::write(&f2, b"b").unwrap();
        std::fs::write(&f3, b"r").unwrap();

        let make = |name: &str| SyncState {
            blake3: format!("hash_{name}"),
            size: 1,
            mtime: 1000,
            chunk_count: 1,
            remote_path: format!("bucket/{name}"),
            last_synced: 9999,
            vclock: VectorClock::new(),
            device_id: String::new(),
            conflict: None,
        };

        cache.set(&f1, make("a"));
        cache.set(&f2, make("b"));
        cache.set(&f3, make("root"));

        let children = cache.children_with_prefix(&sub);
        assert_eq!(children.len(), 2);

        let children_root = cache.children_with_prefix(dir.path());
        // All 3 files are children of dir (sub/a.txt, sub/b.txt, root.txt)
        assert_eq!(children_root.len(), 3);
    }

    #[tokio::test]
    async fn test_path_locks_concurrent() {
        let locks = PathLocks::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"data").unwrap();

        // Acquire lock
        let guard = locks.lock(&path).await;
        assert!(locks.is_locked(&path).await);

        // try_lock should fail while held
        assert!(locks.try_lock(&path).await.is_none());

        // Different path should be lockable
        let other = dir.path().join("other.txt");
        std::fs::write(&other, b"data").unwrap();
        assert!(!locks.is_locked(&other).await);
        let _other_guard = locks.lock(&other).await;

        // Drop first guard, should unlock
        drop(guard);
        assert!(!locks.is_locked(&path).await);
    }
}
