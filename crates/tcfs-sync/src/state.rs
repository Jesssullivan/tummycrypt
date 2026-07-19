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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::conflict::VectorClock;

fn state_parent_path(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn validate_existing_state_parent(path: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let parent = state_parent_path(path);
        let metadata = std::fs::symlink_metadata(parent)
            .with_context(|| format!("inspecting state parent: {}", parent.display()))?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink() && metadata.is_dir(),
            "state parent must be a real directory: {}",
            parent.display()
        );
        crate::conflict_git::validate_trusted_configured_path(parent)
            .with_context(|| format!("validating trusted state parent: {}", parent.display()))?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = path;
    Ok(())
}

fn validate_state_parent_if_present(path: &Path) -> Result<()> {
    let parent = state_parent_path(path);
    match std::fs::symlink_metadata(parent) {
        Ok(_) => validate_existing_state_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("inspecting state parent path: {}", parent.display())),
    }
}

fn ensure_trusted_state_parent(path: &Path) -> Result<()> {
    let parent = state_parent_path(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(parent)
            .with_context(|| format!("creating private state directory: {}", parent.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating state directory: {}", parent.display()))?;
    validate_existing_state_parent(path)
}

fn reject_state_path_write_acls(path: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        validate_existing_state_parent(path)?;
        if path.exists() {
            crate::path_acl::reject_write_grant_acl(path)
                .with_context(|| format!("validating state file ACL: {}", path.display()))?;
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = path;
    Ok(())
}

fn state_path_entry_exists(path: &Path) -> Result<bool> {
    validate_state_parent_if_present(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspecting private state path: {}", path.display()))
        }
    }
}

#[cfg(unix)]
fn validate_private_state_unix_fields(
    path: &Path,
    owner_uid: u32,
    link_count: u64,
    mode: u32,
    effective_uid: u32,
) -> Result<()> {
    anyhow::ensure!(
        owner_uid == effective_uid,
        "private state file must be owned by effective uid {effective_uid}, got uid {owner_uid}: {}",
        path.display()
    );
    anyhow::ensure!(
        link_count == 1,
        "refusing to read hardlinked state file {} (link count {link_count})",
        path.display()
    );
    anyhow::ensure!(
        mode & 0o077 == 0,
        "private state file must be mode 0600 or stricter: {}",
        path.display()
    );
    Ok(())
}

fn validate_opened_state_file(file: &std::fs::File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("reading private state file metadata: {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!(
            "private state path is not a regular file: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        // SAFETY: `geteuid` has no preconditions and only reads process identity.
        let effective_uid = unsafe { libc::geteuid() };
        validate_private_state_unix_fields(
            path,
            metadata.uid(),
            metadata.nlink(),
            metadata.mode(),
            effective_uid,
        )?;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    crate::path_acl::reject_write_grant_acl_fd(file, path)
        .with_context(|| format!("validating opened state file ACL: {}", path.display()))?;
    Ok(())
}

fn secure_open_state_file(path: &Path) -> Result<std::fs::File> {
    reject_state_path_write_acls(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting private state file: {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("refusing to read state-cache symlink: {}", path.display());
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("opening private state file for read: {}", path.display()))?;
    validate_opened_state_file(&file, path)?;
    Ok(file)
}

fn read_opened_state_file(mut file: std::fs::File, display_path: &Path) -> Result<Vec<u8>> {
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .with_context(|| format!("reading private state file: {}", display_path.display()))?;
    validate_opened_state_file(&file, display_path)?;
    Ok(contents)
}

fn secure_read_file_bytes(path: &Path) -> Result<Vec<u8>> {
    read_opened_state_file(secure_open_state_file(path)?, path)
}

fn secure_read_file(path: &Path) -> Result<String> {
    String::from_utf8(secure_read_file_bytes(path)?)
        .with_context(|| format!("decoding private state file as UTF-8: {}", path.display()))
}

fn secure_write_new_file(path: &Path, contents: &[u8]) -> Result<()> {
    reject_state_path_write_acls(path)?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening private state file: {}", path.display()))?;
    validate_opened_state_file(&file, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing private state file: {}", path.display()))?;
    }
    validate_opened_state_file(&file, path)?;
    file.set_len(0)
        .with_context(|| format!("truncating private state file: {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("writing private state file: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing private state file: {}", path.display()))?;
    Ok(())
}

fn secure_atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if state_path_entry_exists(path)? {
        // Validate an existing destination before creating a replacement. Rename
        // would safely replace a symlink rather than following it, but treating a
        // pre-placed redirect or hardlink as an ordinary cache generation hides
        // evidence that the state boundary was tampered with.
        drop(secure_open_state_file(path)?);
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let tmp_path = PathBuf::from(tmp_name);
    if let Err(error) = secure_write_new_file(&tmp_path, contents) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error).with_context(|| format!("renaming state cache: {}", path.display()));
    }
    sync_parent_directory(path)
        .with_context(|| format!("syncing state-cache rename directory: {}", path.display()))?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)
        .with_context(|| format!("opening parent directory for sync: {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("syncing parent directory: {}", parent.display()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn sync_parent_directory(_path: &Path) -> Result<()> {
    Ok(())
}

// ── StateFileLock ─────────────────────────────────────────────────────────

/// Cross-process advisory lock for one JSON state cache.
///
/// [`StateCache::flush`] replaces the JSON inode with an atomic rename, so the
/// lock lives in a stable sibling (`<state>.lock`) and is held across the full
/// open/read/mutate/flush transaction. This coordinates scheduled CLI
/// reconciliation with daemon-side operations on registered roots.
#[derive(Debug)]
pub struct StateFileLock {
    _file: std::fs::File,
}

/// Result of probing the existing writer lock without creating or modifying it.
///
/// Read-only status surfaces use this instead of [`StateFileLock::acquire`]:
/// creating or chmodding a sidecar is itself an unexpected write for an
/// inventory RPC. When the lock is absent, callers must securely read a stable
/// state-file descriptor and probe again afterward; writers create this
/// persistent sibling before replacing the state cache.
#[derive(Debug)]
pub enum ExistingStateFileLock {
    /// No lock inode exists. No file was created.
    Missing,
    /// The existing lock is held by this guard until it is dropped.
    Acquired(StateFileLock),
    /// Another process currently holds the existing lock.
    Contended,
}

impl Drop for StateFileLock {
    fn drop(&mut self) {
        // Be explicit instead of relying only on descriptor close semantics;
        // this also avoids platform-specific ambiguity after another process
        // (or test thread) has attempted the same advisory lock.
        let _ = self._file.unlock();
    }
}

impl StateFileLock {
    /// Stable sibling path used by every state-cache writer.
    pub fn lock_path(state_path: &Path) -> PathBuf {
        let mut path = state_path.as_os_str().to_os_string();
        path.push(".lock");
        PathBuf::from(path)
    }

    /// Acquire an exclusive lock without waiting.
    ///
    /// Contention normally means a scheduled reconcile is mid-cycle. Failing
    /// fast keeps an attended conflict ceremony from queuing behind an
    /// unbounded storage operation; the operator can retry after that cycle.
    pub fn acquire(state_path: &Path) -> Result<Self> {
        let lock_path = Self::lock_path(state_path);
        ensure_trusted_state_parent(&lock_path).with_context(|| {
            format!(
                "preparing trusted state lock parent: {}",
                lock_path.display()
            )
        })?;
        reject_state_path_write_acls(&lock_path)?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(&lock_path)
            .with_context(|| format!("opening state lock: {}", lock_path.display()))?;
        validate_opened_state_file(&file, &lock_path)
            .with_context(|| format!("validating opened state lock: {}", lock_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("securing state lock: {}", lock_path.display()))?;
        }
        validate_opened_state_file(&file, &lock_path)
            .with_context(|| format!("revalidating opened state lock: {}", lock_path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
                "state cache {} is locked by another process; retry after the current reconcile cycle (lock: {})",
                state_path.display(),
                lock_path.display()
            ),
            Err(std::fs::TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("locking state cache via {}", lock_path.display())),
        }
    }

    /// Try to acquire an existing state-cache lock without creating, truncating,
    /// chmodding, or otherwise modifying the lock inode.
    ///
    /// A missing lock is a typed result rather than a request to create one.
    /// This is intended for observational APIs; writers must continue to use
    /// [`StateFileLock::acquire`].
    pub fn try_acquire_existing(state_path: &Path) -> Result<ExistingStateFileLock> {
        let lock_path = Self::lock_path(state_path);
        validate_state_parent_if_present(&lock_path).with_context(|| {
            format!(
                "validating existing state lock parent: {}",
                lock_path.display()
            )
        })?;
        reject_state_path_write_acls(&lock_path)?;

        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ExistingStateFileLock::Missing);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("opening existing state lock: {}", lock_path.display())
                });
            }
        };
        validate_opened_state_file(&file, &lock_path)
            .with_context(|| format!("validating existing state lock: {}", lock_path.display()))?;

        match file.try_lock() {
            Ok(()) => Ok(ExistingStateFileLock::Acquired(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(ExistingStateFileLock::Contended),
            Err(std::fs::TryLockError::Error(error)) => Err(error).with_context(|| {
                format!("locking existing state cache via {}", lock_path.display())
            }),
        }
    }
}

// ── FileSyncStatus ──────────────────────────────────────────────────────────

/// Persisted per-file sync status, modeled after odrive's FileSyncState.
///
/// `Active` and `Locked` can describe an in-flight operation, but the enum is a
/// field of persisted [`SyncState`]. Inventory APIs therefore report these as
/// counts from one durable cache snapshot, never as live task telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSyncStatus {
    /// File exists only as a stub/placeholder (not hydrated).
    #[default]
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
        let guard = mutex.clone().lock_owned().await;
        PathLockGuard {
            guard: Some(guard),
            mutex,
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
        match mutex.clone().try_lock_owned() {
            Ok(guard) => Some(PathLockGuard {
                guard: Some(guard),
                mutex,
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
///
/// Cleanup is scheduled after the underlying mutex guard is released, so entry
/// removal does not depend on winning a synchronous `try_lock()` race on the
/// path-lock map.
pub struct PathLockGuard {
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    mutex: Arc<tokio::sync::Mutex<()>>,
    key: String,
    inner: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Drop for PathLockGuard {
    fn drop(&mut self) {
        // Release the per-path mutex before checking whether the entry can be
        // removed from the path-lock map.
        drop(self.guard.take());

        let key = self.key.clone();
        let inner = self.inner.clone();
        let mutex = self.mutex.clone();

        let cleanup = async move {
            let mut map = inner.lock().await;
            if let Some(current) = map.get(&key) {
                // strong_count == 2 means only the map entry and this cleanup task
                // still reference the mutex, so no holder or waiter remains.
                if Arc::ptr_eq(current, &mutex) && Arc::strong_count(&mutex) == 2 {
                    map.remove(&key);
                }
            }
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(cleanup);
        } else if let Ok(mut map) = self.inner.try_lock() {
            if let Some(current) = map.get(&self.key) {
                // strong_count == 2 means only the map entry and this guard still
                // reference the mutex in this synchronous fallback path.
                if Arc::ptr_eq(current, &self.mutex) && Arc::strong_count(&self.mutex) == 2 {
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
    /// Persisted sync status captured in the state-cache snapshot.
    #[serde(default)]
    pub status: FileSyncStatus,
}

/// On-disk format wrapping entries plus metadata that must survive restarts.
///
/// Backwards-compatible: if the file is a raw `HashMap<String, SyncState>`, we
/// still load it and default the metadata.
#[derive(Debug, Serialize, Deserialize)]
struct StateCacheOnDisk {
    #[serde(default)]
    last_nats_seq: u64,
    #[serde(default)]
    device_id: String,
    entries: HashMap<String, SyncState>,
}

/// In-memory state cache, persisted to a JSON file
pub struct StateCache {
    /// Path to the JSON state file on disk
    db_path: PathBuf,
    /// In-memory map: canonicalized local path → SyncState
    entries: HashMap<String, SyncState>,
    /// Whether there are unsaved changes
    dirty: bool,
    /// The in-memory state came from the last known-good backup because the
    /// primary was content-corrupt or absent. Until a durable primary rewrite
    /// succeeds, backup rotation must preserve that recovery source verbatim.
    recovered_from_backup: bool,
    /// Last NATS JetStream sequence processed (for catch-up on restart)
    last_nats_seq: u64,
    /// Device ID for this machine
    device_id: String,
    /// When the cache was last flushed, used by periodic best-effort flushing.
    last_flush: Instant,
}

/// Immutable view of one primary JSON state cache.
///
/// Unlike [`StateCache::open`], this loader never creates an empty cache,
/// consults a recovery backup, marks recovery state, flushes, or writes on
/// drop. It exists for status/inventory RPCs whose read-only contract requires
/// the primary cache to remain byte-identical.
#[derive(Debug, Clone)]
pub struct StateCacheSnapshot {
    entries: HashMap<String, SyncState>,
    last_nats_seq: u64,
    device_id: String,
}

impl StateCacheSnapshot {
    /// Securely read only the primary cache.
    ///
    /// `Ok(None)` means the primary is absent, even when a `.bak` file exists.
    /// Corrupt primary content is an error and is never replaced from backup.
    pub fn read_primary(db_path: &Path) -> Result<Option<Self>> {
        if !state_path_entry_exists(db_path)? {
            return Ok(None);
        }

        let primary_bytes = secure_read_file_bytes(db_path)
            .with_context(|| format!("securely reading state snapshot: {}", db_path.display()))?;
        let (entries, last_nats_seq, device_id) =
            StateCache::parse_file_bytes(db_path, &primary_bytes)
                .with_context(|| format!("parsing state snapshot: {}", db_path.display()))?;
        Ok(Some(Self {
            entries,
            last_nats_seq,
            device_id,
        }))
    }

    /// Iterate over every cache entry without exposing mutation.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &SyncState)> {
        self.entries
            .iter()
            .map(|(cache_key, state)| (cache_key.as_str(), state))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn last_nats_seq(&self) -> u64 {
        self.last_nats_seq
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }
}

/// In-memory rollback snapshot for a bounded set of state-cache keys.
#[derive(Debug, Clone)]
pub struct StateCacheKeySnapshot {
    entries: HashMap<String, Option<SyncState>>,
    dirty: bool,
    recovered_from_backup: bool,
}

impl StateCache {
    /// Load or create a state cache at the given path.
    ///
    /// Supports two on-disk formats:
    /// - new: `{"last_nats_seq": N, "device_id": "...", "entries": {...}}`
    /// - legacy: raw `HashMap<String, SyncState>`
    ///
    /// If the securely-read primary content is corrupt, falls back to `.bak`
    /// when present. Topology, ownership, permission, ACL, and read-I/O errors
    /// fail closed instead of being reclassified as recoverable corruption.
    pub fn open(db_path: &Path) -> Result<Self> {
        validate_state_parent_if_present(db_path)?;
        let bak_path = db_path.with_extension("json.bak");
        let primary_exists = state_path_entry_exists(db_path)?;
        let (entries, last_nats_seq, device_id, recovered_from_backup) = if primary_exists {
            // Security/topology/ACL/read-I/O failures are not content corruption
            // and must never be converted into a stale-backup rollback.
            let primary_bytes = secure_read_file_bytes(db_path)
                .with_context(|| format!("securely reading state cache: {}", db_path.display()))?;
            match Self::parse_file_bytes(db_path, &primary_bytes) {
                Ok((entries, last_nats_seq, device_id)) => {
                    if state_path_entry_exists(&bak_path)? {
                        drop(secure_open_state_file(&bak_path).with_context(|| {
                            format!(
                                "validating existing state-cache backup: {}",
                                bak_path.display()
                            )
                        })?);
                    }
                    (entries, last_nats_seq, device_id, false)
                }
                Err(primary_parse_err) => {
                    if state_path_entry_exists(&bak_path)? {
                        tracing::warn!(
                            path = %db_path.display(),
                            error = %primary_parse_err,
                            "state cache content corrupt, recovering from backup"
                        );
                        let (entries, last_nats_seq, device_id) =
                            Self::load_from_backup_file(&bak_path).with_context(|| {
                                format!(
                                    "state cache content is corrupt and backup failed to load securely: {}",
                                    db_path.display()
                                )
                            })?;
                        (entries, last_nats_seq, device_id, true)
                    } else {
                        return Err(primary_parse_err).with_context(|| {
                            format!("parsing state cache: {}", db_path.display())
                        });
                    }
                }
            }
        } else if state_path_entry_exists(&bak_path)? {
            tracing::warn!(
                path = %db_path.display(),
                backup = %bak_path.display(),
                "state cache primary missing, recovering from backup"
            );
            let (entries, last_nats_seq, device_id) = Self::load_from_backup_file(&bak_path)
                .with_context(|| {
                    format!(
                        "state cache primary is missing and backup failed to load: {}",
                        db_path.display()
                    )
                })?;
            (entries, last_nats_seq, device_id, true)
        } else {
            (HashMap::new(), 0, String::new(), false)
        };

        Ok(StateCache {
            db_path: db_path.to_path_buf(),
            entries,
            dirty: false,
            recovered_from_backup,
            last_nats_seq,
            device_id,
            last_flush: Instant::now(),
        })
    }

    /// Machine-local directory that owns this state cache.
    ///
    /// Git recovery artifacts such as keep-both / loser-guard undo bundles must
    /// live here, not under a sync root.
    pub fn state_dir(&self) -> PathBuf {
        self.db_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn load_from_file(path: &Path) -> Result<(HashMap<String, SyncState>, u64, String)> {
        let content = secure_read_file(path)?;
        Self::parse_file_content(path, &content)
    }

    fn load_from_backup_file(path: &Path) -> Result<(HashMap<String, SyncState>, u64, String)> {
        let content = secure_read_file(path)?;
        Self::parse_file_content(path, &content)
    }

    fn parse_file_bytes(
        path: &Path,
        content: &[u8],
    ) -> Result<(HashMap<String, SyncState>, u64, String)> {
        let content = std::str::from_utf8(content)
            .with_context(|| format!("decoding state cache as UTF-8: {}", path.display()))?;
        Self::parse_file_content(path, content)
    }

    fn parse_file_content(
        path: &Path,
        content: &str,
    ) -> Result<(HashMap<String, SyncState>, u64, String)> {
        if let Ok(data) = serde_json::from_str::<StateCacheOnDisk>(content) {
            return Ok((data.entries, data.last_nats_seq, data.device_id));
        }

        let entries: HashMap<String, SyncState> = serde_json::from_str(content)
            .with_context(|| format!("parsing state cache: {}", path.display()))?;
        Ok((entries, 0, String::new()))
    }

    /// Reload entries from disk, merging any new entries written by other processes.
    /// Existing in-memory entries are NOT overwritten (in-memory wins).
    pub fn reload_from_disk(&mut self) -> Result<()> {
        if !state_path_entry_exists(&self.db_path)? {
            return Ok(());
        }
        let (disk_entries, seq, device_id) = Self::load_from_file(&self.db_path)?;
        for (key, state) in disk_entries {
            self.entries.entry(key).or_insert(state);
        }
        if seq > self.last_nats_seq {
            self.last_nats_seq = seq;
        }
        if self.device_id.is_empty() && !device_id.is_empty() {
            self.device_id = device_id;
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

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn set_device_id(&mut self, id: String) {
        if self.device_id != id {
            self.device_id = id;
            self.dirty = true;
        }
    }

    pub fn last_nats_seq(&self) -> u64 {
        self.last_nats_seq
    }

    pub fn set_last_nats_seq(&mut self, seq: u64) {
        if self.last_nats_seq != seq {
            self.last_nats_seq = seq;
            self.dirty = true;
        }
    }

    /// Find a state entry by relative path (for NATS auto-pull lookups).
    ///
    /// Searches cache keys (canonical local paths) by suffix match, then
    /// falls back to remote_path matching. Cache keys are absolute paths
    /// like `/home/jess/tcfs/dir/file.txt` — matching the suffix against
    /// the normalized rel_path (`dir/file.txt`) handles cross-host home
    /// directory differences.
    pub fn get_by_rel_path(&self, rel_path: &str) -> Option<(&str, &SyncState)> {
        let normalized = crate::engine::normalize_rel_path_text(rel_path.trim_start_matches('/'));
        // Primary: match cache keys (canonical local paths) by suffix
        self.entries
            .iter()
            .find(|(key, _)| {
                let normalized_key = crate::engine::normalize_rel_path_text(key);
                normalized_key.ends_with(&format!("/{}", normalized))
                    || normalized_key == normalized
            })
            // Fallback: match remote_path (manifest path) for backward compat
            .or_else(|| {
                self.entries.iter().find(|(_, state)| {
                    let normalized_remote =
                        crate::engine::normalize_rel_path_text(&state.remote_path);
                    normalized_remote.ends_with(&format!("/{}", normalized))
                        || normalized_remote == normalized
                })
            })
            .map(|(k, v)| (k.as_str(), v))
    }

    /// Read-only view of every entry that currently carries a recorded
    /// conflict, as `(cache key, state)` pairs.
    ///
    /// The cache key is the normalized (canonical-parent) local path; the
    /// recorded `ConflictInfo` (with its repo-relative `rel_path`) lives on the
    /// returned [`SyncState`]. Used by `tcfs conflicts` to enumerate and group
    /// conflicts without any daemon RPC. Order is unspecified (HashMap).
    pub fn conflicts(&self) -> Vec<(&str, &SyncState)> {
        self.entries
            .iter()
            .filter(|(_, s)| s.conflict.is_some())
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    }

    /// Capture a bounded set of cache entries so callers can roll back
    /// in-memory mutations if their outer atomic operation fails.
    pub fn snapshot_cache_keys<'a, I>(&self, cache_keys: I) -> StateCacheKeySnapshot
    where
        I: IntoIterator<Item = &'a str>,
    {
        let entries = cache_keys
            .into_iter()
            .map(|key| (key.to_string(), self.entries.get(key).cloned()))
            .collect();
        StateCacheKeySnapshot {
            entries,
            dirty: self.dirty,
            recovered_from_backup: self.recovered_from_backup,
        }
    }

    /// Restore entries captured by [`StateCache::snapshot_cache_keys`].
    pub fn restore_cache_key_snapshot(&mut self, snapshot: &StateCacheKeySnapshot) {
        for (key, state) in &snapshot.entries {
            match state {
                Some(state) => {
                    self.entries.insert(key.clone(), state.clone());
                }
                None => {
                    self.entries.remove(key);
                }
            }
        }
        self.dirty = snapshot.dirty;
        self.recovered_from_backup = snapshot.recovered_from_backup;
    }

    /// Flush dirty changes to disk using an atomic write (write then rename).
    ///
    /// Persists cache metadata alongside entries so restart recovery does not
    /// replay stale NATS state or forget the current device identity.
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty && !self.recovered_from_backup {
            return Ok(());
        }

        ensure_trusted_state_parent(&self.db_path).with_context(|| {
            format!(
                "preparing trusted state directory: {}",
                self.db_path.display()
            )
        })?;

        if !self.recovered_from_backup && state_path_entry_exists(&self.db_path)? {
            let bak_path = self.db_path.with_extension("json.bak");
            let previous = secure_read_file_bytes(&self.db_path).with_context(|| {
                format!("reading state cache for backup: {}", self.db_path.display())
            })?;
            // Never rotate arbitrary bytes into the recovery slot. If a normal
            // primary changed into content-corrupt state after open, preserve the
            // last known-good backup and fail this flush closed.
            Self::parse_file_bytes(&self.db_path, &previous).with_context(|| {
                format!(
                    "refusing to back up content-corrupt state cache: {}",
                    self.db_path.display()
                )
            })?;
            secure_atomic_write(&bak_path, &previous)
                .with_context(|| format!("writing state cache backup: {}", bak_path.display()))?;
        }

        let on_disk = StateCacheOnDisk {
            last_nats_seq: self.last_nats_seq,
            device_id: self.device_id.clone(),
            entries: self.entries.clone(),
        };
        let json = serde_json::to_string_pretty(&on_disk).context("serializing state cache")?;

        // Create a unique 0600 temp inode before writing any JSON, then rename
        // it over the cache. Predictable umask-created `.tmp` crash artifacts
        // and symlink-following backup writes are not acceptable for named
        // conflict state.
        secure_atomic_write(&self.db_path, json.as_bytes())?;

        self.dirty = false;
        self.recovered_from_backup = false;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// Flush dirty state when the last successful flush is older than `interval`.
    pub fn flush_if_stale(&mut self, interval: Duration) -> Result<()> {
        if (self.dirty || self.recovered_from_backup) && self.last_flush.elapsed() >= interval {
            self.flush()
        } else {
            Ok(())
        }
    }

    /// Transition a file's sync status without removing its metadata.
    ///
    /// Used by unsync/dehydration: marks as `NotSynced` while preserving
    /// blake3, remote_path, size, etc. for future re-hydration.
    pub fn set_status(&mut self, local_path: &Path, status: FileSyncStatus) {
        let key = path_key(local_path);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.status = status;
            self.dirty = true;
        }
    }

    /// Mark an entry as conflicted while preserving its existing metadata.
    ///
    /// Conflict payload and status must move together. Setting only the conflict
    /// info leaves callers with an internally inconsistent entry that still
    /// appears synced.
    pub fn mark_conflict(
        &mut self,
        local_path: &Path,
        conflict: crate::conflict::ConflictInfo,
    ) -> bool {
        let key = path_key(local_path);
        if let Some(entry) = self.entries.get_mut(&key) {
            let mut conflict = conflict;
            if conflict.remote_manifest_key.is_none() {
                if let Some(existing) = entry.conflict.as_ref() {
                    conflict.remote_manifest_key = existing.remote_manifest_key.clone();
                }
            }
            entry.conflict = Some(conflict);
            entry.status = FileSyncStatus::Conflict;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Clear conflict state after successful resolution.
    ///
    /// Sets `conflict` to `None` and `status` to `Synced`. Both fields
    /// must be updated together — clearing only `conflict` leaves the file
    /// flagged as conflicted in UI badges and FileProvider decorations.
    pub fn resolve_conflict(&mut self, local_path: &Path) -> bool {
        let key = path_key(local_path);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.conflict = None;
            entry.status = FileSyncStatus::Synced;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Clear conflict state for an already-known cache key and replace its
    /// vector clock with the resolution clock.
    ///
    /// Repo-group `.git` resolution operates on conflict groups discovered via
    /// [`StateCache::conflicts`]. Those records already carry canonical cache
    /// keys; using the key directly avoids re-canonicalizing paths while the
    /// resolver is mutating refs under `.git/tcfs.lock`.
    pub fn resolve_conflict_by_cache_key(
        &mut self,
        cache_key: &str,
        vclock: VectorClock,
        device_id: String,
    ) -> bool {
        if let Some(entry) = self.entries.get_mut(cache_key) {
            entry.conflict = None;
            entry.status = FileSyncStatus::Synced;
            entry.vclock = vclock;
            entry.device_id = device_id;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Remove stale entries whose `remote_path` doesn't match `expected_prefix`
    /// or whose local path (under /tmp/) no longer exists on disk.
    ///
    /// Returns the number of entries removed. Caller should `flush()` if > 0.
    pub fn purge_stale(&mut self, expected_prefix: &str) -> usize {
        let prefix_slash = format!("{}/", expected_prefix.trim_end_matches('/'));
        let before = self.entries.len();

        self.entries.retain(|key, state| {
            // Never drop an entry that carries an unresolved conflict, whatever
            // its prefix. Cross-prefix roam (e.g. `git-roam/*`) records a
            // conflict under a non-default prefix; purging it on boot would
            // silently delete the record before the operator can resolve it.
            if state.conflict.is_some() {
                return true;
            }
            // Keep entries whose remote_path starts with the expected prefix
            if !state.remote_path.starts_with(&prefix_slash) {
                return false;
            }
            // Remove entries for tmp files that no longer exist
            if (key.starts_with("/tmp/") || key.starts_with("/private/tmp/"))
                && !std::path::Path::new(key).exists()
            {
                return false;
            }
            true
        });

        let removed = before - self.entries.len();
        if removed > 0 {
            self.dirty = true;
        }
        removed
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
        if self.dirty || self.recovered_from_backup {
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
            StateBackend::Json(c) => c.device_id(),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => &c.device_id,
        }
    }

    /// Set the device_id.
    pub fn set_device_id(&mut self, id: String) {
        match self {
            StateBackend::Json(c) => c.set_device_id(id),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.device_id = id,
        }
    }

    /// Get the last NATS sequence.
    pub fn last_nats_seq(&self) -> u64 {
        match self {
            StateBackend::Json(c) => c.last_nats_seq(),
            #[cfg(feature = "full")]
            StateBackend::Rocks(c) => c.last_nats_seq,
        }
    }

    /// Set the last NATS sequence.
    pub fn set_last_nats_seq(&mut self, seq: u64) {
        match self {
            StateBackend::Json(c) => c.set_last_nats_seq(seq),
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

/// Convert a path to a normalized string key for the HashMap.
///
/// Canonicalizes the parent directory (for example `/var` -> `/private/var` on
/// macOS) but preserves the final path component. That keeps first-class
/// symlinks keyed by the link path instead of the link target, while still
/// making delete/remove lookups stable after the file itself is gone.
fn path_key(path: &Path) -> String {
    path.parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map(|parent| parent.join(path.file_name().unwrap_or_default()))
        .or_else(|| std::fs::canonicalize(path).ok())
        .unwrap_or_else(|| path.to_path_buf())
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
        status: FileSyncStatus::Synced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private_state_file(path: &Path, contents: &[u8]) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn write_recovery_backup(primary_path: &Path, sequence: u64, device_id: &str) -> PathBuf {
        let backup_path = primary_path.with_extension("json.bak");
        let on_disk = StateCacheOnDisk {
            last_nats_seq: sequence,
            device_id: device_id.to_string(),
            entries: HashMap::new(),
        };
        write_private_state_file(
            &backup_path,
            serde_json::to_string(&on_disk).unwrap().as_bytes(),
        );
        backup_path
    }

    #[cfg(unix)]
    fn create_fifo(path: &Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed for {}", path.display());
    }

    #[test]
    fn open_nonexistent_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let cache = StateCache::open(&path).unwrap();
        assert!(cache.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn primary_read_rejects_symlink_hardlink_and_permissive_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let symlink_dir = tempfile::tempdir().unwrap();
        let symlink_target = symlink_dir.path().join("target.json");
        let symlink_primary = symlink_dir.path().join("state.json");
        write_private_state_file(&symlink_target, b"{}");
        symlink(&symlink_target, &symlink_primary).unwrap();
        write_recovery_backup(&symlink_primary, 7, "must-not-recover");
        let error = StateCache::open(&symlink_primary)
            .err()
            .expect("symlinked primary must fail closed");
        assert!(
            format!("{error:#}").contains("state-cache symlink"),
            "{error:#}"
        );

        let hardlink_dir = tempfile::tempdir().unwrap();
        let hardlink_target = hardlink_dir.path().join("target.json");
        let hardlink_primary = hardlink_dir.path().join("state.json");
        write_private_state_file(&hardlink_target, b"{}");
        std::fs::hard_link(&hardlink_target, &hardlink_primary).unwrap();
        write_recovery_backup(&hardlink_primary, 7, "must-not-recover");
        let error = StateCache::open(&hardlink_primary)
            .err()
            .expect("hardlinked primary must fail closed");
        assert!(
            format!("{error:#}").contains("hardlinked state file"),
            "{error:#}"
        );

        let permissive_dir = tempfile::tempdir().unwrap();
        let permissive_primary = permissive_dir.path().join("state.json");
        write_private_state_file(&permissive_primary, b"{}");
        std::fs::set_permissions(&permissive_primary, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        write_recovery_backup(&permissive_primary, 7, "must-not-recover");
        let error = StateCache::open(&permissive_primary)
            .err()
            .expect("permissive primary must fail closed");
        assert!(
            format!("{error:#}").contains("mode 0600 or stricter"),
            "{error:#}"
        );

        let fifo_dir = tempfile::tempdir().unwrap();
        let fifo_primary = fifo_dir.path().join("state.json");
        create_fifo(&fifo_primary);
        write_recovery_backup(&fifo_primary, 7, "must-not-recover");
        let error = StateCache::open(&fifo_primary)
            .err()
            .expect("FIFO primary must fail closed even with a valid backup");
        assert!(
            format!("{error:#}").contains("not a regular file"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_primary_symlink_is_not_treated_as_first_start() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        symlink(dir.path().join("missing-target.json"), &path).unwrap();

        let error = StateCache::open(&path)
            .err()
            .expect("dangling primary symlink must fail closed");
        assert!(
            format!("{error:#}").contains("state-cache symlink"),
            "{error:#}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn state_cache_rejects_unsafe_or_symlinked_parent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let unsafe_parent = dir.path().join("unsafe-parent");
        std::fs::create_dir(&unsafe_parent).unwrap();
        std::fs::set_permissions(&unsafe_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let unsafe_state = unsafe_parent.join("state.json");
        let error = StateCache::open(&unsafe_state)
            .err()
            .expect("group/world-writable state parent must fail closed");
        assert!(
            format!("{error:#}").contains("group/world-writable"),
            "{error:#}"
        );
        std::fs::set_permissions(&unsafe_parent, std::fs::Permissions::from_mode(0o700)).unwrap();

        let real_parent = dir.path().join("real-parent");
        let linked_parent = dir.path().join("linked-parent");
        std::fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &linked_parent).unwrap();
        let linked_state = linked_parent.join("state.json");
        let error = StateCache::open(&linked_state)
            .err()
            .expect("symlinked state parent must fail closed");
        assert!(
            format!("{error:#}").contains("state parent must be a real directory"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn wrong_owner_policy_is_fail_closed() {
        // SAFETY: `geteuid` has no preconditions and only reads process identity.
        let effective_uid = unsafe { libc::geteuid() };
        let foreign_uid = effective_uid ^ 1;
        let error = validate_private_state_unix_fields(
            Path::new("state.json"),
            foreign_uid,
            1,
            0o600,
            effective_uid,
        )
        .unwrap_err();
        assert!(error.to_string().contains("must be owned by effective uid"));
    }

    #[cfg(unix)]
    #[test]
    fn secure_read_remains_bound_to_opened_inode_after_path_swap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let parked = dir.path().join("opened.json");
        let trusted = br#"{"last_nats_seq":7,"device_id":"trusted","entries":{}}"#;
        let forged = br#"{"last_nats_seq":99,"device_id":"forged","entries":{}}"#;
        write_private_state_file(&path, trusted);

        let opened = secure_open_state_file(&path).unwrap();
        std::fs::rename(&path, &parked).unwrap();
        write_private_state_file(&path, forged);
        let bytes = read_opened_state_file(opened, &path).unwrap();

        assert_eq!(bytes, trusted);
        assert_eq!(std::fs::read(&path).unwrap(), forged);
    }

    #[cfg(unix)]
    #[test]
    fn reload_and_flush_backup_capture_reject_primary_symlinks() {
        use std::os::unix::fs::symlink;

        let reload_dir = tempfile::tempdir().unwrap();
        let reload_path = reload_dir.path().join("state.json");
        let mut cache = StateCache::open(&reload_path).unwrap();
        let reload_target = reload_dir.path().join("reload-target.json");
        write_private_state_file(&reload_target, b"{}");
        symlink(&reload_target, &reload_path).unwrap();
        let error = cache.reload_from_disk().unwrap_err();
        assert!(
            format!("{error:#}").contains("state-cache symlink"),
            "{error:#}"
        );

        let flush_dir = tempfile::tempdir().unwrap();
        let flush_path = flush_dir.path().join("state.json");
        let mut cache = StateCache::open(&flush_path).unwrap();
        cache.set_last_nats_seq(1);
        let flush_target = flush_dir.path().join("flush-target.json");
        write_private_state_file(&flush_target, b"{}");
        symlink(&flush_target, &flush_path).unwrap();
        let error = cache.flush().unwrap_err();
        assert!(
            format!("{error:#}").contains("state-cache symlink"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&flush_target).unwrap(), b"{}");
        assert!(!flush_dir.path().join("state.json.bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn state_keys_preserve_terminal_symlink_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink("target.txt", &link).unwrap();

        let mut cache = StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &link,
            SyncState {
                blake3: "hash".into(),
                size: 10,
                mtime: 0,
                chunk_count: 0,
                remote_path: "data/manifests/hash".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );

        assert!(cache.get(&link).is_some());
        assert!(cache.get(&target).is_none());
        assert!(cache.get_by_rel_path("link.txt").is_some());
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
                status: Default::default(),
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
                status: Default::default(),
            },
        );
        assert_eq!(cache.len(), 1);

        cache.remove(&fake_path);
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&fake_path).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_remove_entry_after_delete_through_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let real_dir = dir.path().join("real");
        let link_dir = dir.path().join("link");
        std::fs::create_dir_all(&real_dir).unwrap();
        symlink(&real_dir, &link_dir).unwrap();

        let linked_path = link_dir.join("to_remove.txt");
        let real_path = real_dir.join("to_remove.txt");
        std::fs::write(&linked_path, b"data").unwrap();

        cache.set(
            &linked_path,
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
                status: Default::default(),
            },
        );
        assert_eq!(cache.len(), 1);

        std::fs::remove_file(&real_path).unwrap();
        cache.remove(&linked_path);

        assert_eq!(cache.len(), 0);
        assert!(cache.get(&linked_path).is_none());
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
                    status: Default::default(),
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
            status: Default::default(),
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

    #[test]
    fn state_file_lock_serializes_process_writers() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("root.json");
        std::fs::write(&state_path, b"{}").unwrap();

        let first = StateFileLock::acquire(&state_path).expect("first state lock");
        let error = StateFileLock::acquire(&state_path)
            .expect_err("second state lock must fail fast")
            .to_string();
        assert!(error.contains("locked by another process"), "{error}");
        assert_eq!(
            StateFileLock::lock_path(&state_path),
            dir.path().join("root.json.lock")
        );

        drop(first);
        StateFileLock::acquire(&state_path).expect("lock releases on drop");
    }

    #[test]
    fn existing_state_lock_probe_never_creates_or_modifies_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let lock_path = StateFileLock::lock_path(&state_path);

        assert!(matches!(
            StateFileLock::try_acquire_existing(&state_path).unwrap(),
            ExistingStateFileLock::Missing
        ));
        assert!(
            !lock_path.exists(),
            "read-only lock probe must not create a sidecar"
        );

        write_private_state_file(&lock_path, b"lock-sentinel");
        let before = std::fs::read(&lock_path).unwrap();
        let probe = StateFileLock::try_acquire_existing(&state_path).unwrap();
        assert!(matches!(probe, ExistingStateFileLock::Acquired(_)));
        assert_eq!(std::fs::read(&lock_path).unwrap(), before);
    }

    #[test]
    fn existing_state_lock_probe_reports_contention_without_string_matching() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let writer = StateFileLock::acquire(&state_path).expect("writer lock");

        assert!(matches!(
            StateFileLock::try_acquire_existing(&state_path).unwrap(),
            ExistingStateFileLock::Contended
        ));

        drop(writer);
        assert!(matches!(
            StateFileLock::try_acquire_existing(&state_path).unwrap(),
            ExistingStateFileLock::Acquired(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn state_file_lock_rejects_fifo_without_blocking_and_hardlink_alias() {
        let fifo_dir = tempfile::tempdir().unwrap();
        let fifo_state = fifo_dir.path().join("fifo.json");
        let fifo_lock = StateFileLock::lock_path(&fifo_state);
        create_fifo(&fifo_lock);
        let error = StateFileLock::acquire(&fifo_state)
            .expect_err("FIFO lock path must fail instead of blocking")
            .to_string();
        assert!(
            error.contains("opening state lock") || error.contains("not a regular file"),
            "{error}"
        );

        let hardlink_dir = tempfile::tempdir().unwrap();
        let hardlink_state = hardlink_dir.path().join("hardlink.json");
        let outside = hardlink_dir.path().join("outside.lock");
        std::fs::write(&outside, b"").unwrap();
        std::fs::hard_link(&outside, StateFileLock::lock_path(&hardlink_state)).unwrap();
        let error = StateFileLock::acquire(&hardlink_state)
            .expect_err("hardlinked lock path must fail closed");
        let error = format!("{error:#}");
        assert!(error.contains("hardlinked state file"), "{error}");
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

    async fn wait_for_lock_entry_count(locks: &PathLocks, expected: usize) {
        for _ in 0..50 {
            if locks.inner.lock().await.len() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }

        let actual = locks.inner.lock().await.len();
        panic!("expected {expected} path-lock entries, found {actual}");
    }

    #[tokio::test]
    async fn test_path_lock_cleanup_survives_map_contention() {
        let locks = PathLocks::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contended.txt");
        std::fs::write(&path, b"data").unwrap();

        let guard = locks.lock(&path).await;
        assert_eq!(locks.inner.lock().await.len(), 1);

        let map_guard = locks.inner.lock().await;
        drop(guard);

        assert_eq!(map_guard.len(), 1, "entry remains while cleanup waits");
        drop(map_guard);

        wait_for_lock_entry_count(&locks, 0).await;
        assert!(!locks.is_locked(&path).await);
    }

    #[tokio::test]
    async fn test_path_lock_entry_persists_for_waiter_then_cleans_up() {
        let locks = PathLocks::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queued.txt");
        std::fs::write(&path, b"data").unwrap();

        let guard = locks.lock(&path).await;
        let acquired = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let locks_clone = locks.clone();
        let path_clone = path.clone();
        let acquired_clone = acquired.clone();
        let release_clone = release.clone();

        let waiter = tokio::spawn(async move {
            let _guard = locks_clone.lock(&path_clone).await;
            acquired_clone.notify_one();
            release_clone.notified().await;
        });

        tokio::task::yield_now().await;
        drop(guard);

        acquired.notified().await;
        assert_eq!(
            locks.inner.lock().await.len(),
            1,
            "entry must remain while another task still holds the path lock"
        );

        release.notify_waiters();
        waiter.await.unwrap();

        wait_for_lock_entry_count(&locks, 0).await;
        assert!(!locks.is_locked(&path).await);
    }

    #[test]
    fn set_status_preserves_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let state = SyncState {
            blake3: "abc123".into(),
            size: 1024,
            mtime: 12345,
            chunk_count: 3,
            remote_path: "prefix/manifests/abc123".into(),
            last_synced: 12345,
            vclock: VectorClock::new(),
            device_id: "dev1".into(),
            conflict: None,
            status: FileSyncStatus::Synced,
        };
        cache.set(&file_path, state);

        // Transition to NotSynced
        cache.set_status(&file_path, FileSyncStatus::NotSynced);

        let entry = cache.get(&file_path).unwrap();
        assert_eq!(entry.status, FileSyncStatus::NotSynced);
        // Metadata must be preserved
        assert_eq!(entry.blake3, "abc123");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.chunk_count, 3);
        assert_eq!(entry.remote_path, "prefix/manifests/abc123");
    }

    #[test]
    fn set_status_marks_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        cache.set(
            &file_path,
            SyncState {
                blake3: "abc".into(),
                size: 5,
                mtime: 0,
                chunk_count: 1,
                remote_path: "p/m/abc".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: String::new(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        cache.flush().unwrap();

        cache.set_status(&file_path, FileSyncStatus::NotSynced);
        // Should be dirty after set_status
        assert!(cache.dirty);
    }

    // ── Conflict resolution tests ──────────────────────────────────────

    #[test]
    fn resolve_conflict_clears_both_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("conflicted.txt");
        std::fs::write(&file_path, b"data").unwrap();

        let conflict_info = crate::conflict::ConflictInfo {
            rel_path: "conflicted.txt".into(),
            local_vclock: VectorClock::new(),
            remote_vclock: VectorClock::new(),
            local_blake3: "aaa".into(),
            remote_blake3: "bbb".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            detected_at: 1700000000,
            times_recorded: 0,
            remote_manifest_key: None,
        };

        cache.set(
            &file_path,
            SyncState {
                blake3: "aaa".into(),
                size: 4,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/index/conflicted.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: Some(conflict_info),
                status: FileSyncStatus::Conflict,
            },
        );

        // Verify conflict is set
        let entry = cache.get(&file_path).unwrap();
        assert!(entry.conflict.is_some());
        assert_eq!(entry.status, FileSyncStatus::Conflict);

        // Resolve it
        let resolved = cache.resolve_conflict(&file_path);
        assert!(
            resolved,
            "resolve_conflict should return true for existing entry"
        );

        // Both fields must be cleared
        let entry = cache.get(&file_path).unwrap();
        assert!(
            entry.conflict.is_none(),
            "conflict must be None after resolve"
        );
        assert_eq!(
            entry.status,
            FileSyncStatus::Synced,
            "status must be Synced after resolve"
        );

        // Metadata preserved
        assert_eq!(entry.blake3, "aaa");
        assert_eq!(entry.remote_path, "data/index/conflicted.txt");
        assert_eq!(entry.device_id, "neo");
    }

    #[test]
    fn resolve_conflict_marks_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("file.txt");
        std::fs::write(&file_path, b"x").unwrap();

        cache.set(
            &file_path,
            SyncState {
                blake3: "abc".into(),
                size: 1,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/index/file.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: Some(crate::conflict::ConflictInfo {
                    rel_path: "file.txt".into(),
                    local_vclock: VectorClock::new(),
                    remote_vclock: VectorClock::new(),
                    local_blake3: "abc".into(),
                    remote_blake3: "def".into(),
                    local_device: "neo".into(),
                    remote_device: "honey".into(),
                    detected_at: 0,
                    times_recorded: 0,
                    remote_manifest_key: None,
                }),
                status: FileSyncStatus::Conflict,
            },
        );
        cache.flush().unwrap();

        cache.resolve_conflict(&file_path);
        assert!(cache.dirty, "cache must be dirty after resolve_conflict");
    }

    #[test]
    fn cache_key_snapshot_restores_conflict_and_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("file.txt");
        std::fs::write(&file_path, b"x").unwrap();

        cache.set(
            &file_path,
            SyncState {
                blake3: "abc".into(),
                size: 1,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/index/file.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: Some(crate::conflict::ConflictInfo {
                    rel_path: "file.txt".into(),
                    local_vclock: VectorClock::new(),
                    remote_vclock: VectorClock::new(),
                    local_blake3: "abc".into(),
                    remote_blake3: "def".into(),
                    local_device: "neo".into(),
                    remote_device: "honey".into(),
                    detected_at: 0,
                    times_recorded: 0,
                    remote_manifest_key: None,
                }),
                status: FileSyncStatus::Conflict,
            },
        );
        cache.flush().unwrap();
        assert!(!cache.dirty);

        let key = cache.conflicts()[0].0.to_string();
        let snapshot = cache.snapshot_cache_keys([key.as_str()]);
        assert!(cache.resolve_conflict_by_cache_key(&key, VectorClock::new(), "neo".into()));
        assert!(cache.dirty);
        assert!(cache.entries.get(&key).unwrap().conflict.is_none());

        cache.restore_cache_key_snapshot(&snapshot);
        let restored = cache.entries.get(&key).unwrap();
        assert_eq!(restored.status, FileSyncStatus::Conflict);
        assert!(restored.conflict.is_some());
        assert!(!cache.dirty);
    }

    #[test]
    fn resolve_conflict_returns_false_for_missing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let nonexistent = dir.path().join("nope.txt");
        assert!(
            !cache.resolve_conflict(&nonexistent),
            "should return false for missing entry"
        );
    }

    #[test]
    fn resolve_conflict_roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let file_path = dir.path().join("rt.txt");
        std::fs::write(&file_path, b"roundtrip").unwrap();

        // Write conflicted state and flush
        {
            let mut cache = StateCache::open(&path).unwrap();
            cache.set(
                &file_path,
                SyncState {
                    blake3: "rt".into(),
                    size: 9,
                    mtime: 0,
                    chunk_count: 1,
                    remote_path: "data/index/rt.txt".into(),
                    last_synced: 0,
                    vclock: VectorClock::new(),
                    device_id: "neo".into(),
                    conflict: Some(crate::conflict::ConflictInfo {
                        rel_path: "rt.txt".into(),
                        local_vclock: VectorClock::new(),
                        remote_vclock: VectorClock::new(),
                        local_blake3: "rt".into(),
                        remote_blake3: "xx".into(),
                        local_device: "neo".into(),
                        remote_device: "honey".into(),
                        detected_at: 0,
                        times_recorded: 0,
                        remote_manifest_key: None,
                    }),
                    status: FileSyncStatus::Conflict,
                },
            );
            cache.flush().unwrap();
        }

        // Reload, resolve, flush
        {
            let mut cache = StateCache::open(&path).unwrap();
            let entry = cache.get(&file_path).unwrap();
            assert!(entry.conflict.is_some(), "conflict should persist on disk");
            assert_eq!(entry.status, FileSyncStatus::Conflict);

            cache.resolve_conflict(&file_path);
            cache.flush().unwrap();
        }

        // Reload again — verify resolved state persisted
        {
            let cache = StateCache::open(&path).unwrap();
            let entry = cache.get(&file_path).unwrap();
            assert!(
                entry.conflict.is_none(),
                "conflict must be None after reload"
            );
            assert_eq!(
                entry.status,
                FileSyncStatus::Synced,
                "status must be Synced after reload"
            );
        }
    }

    #[test]
    fn mark_conflict_sets_payload_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("conflicted.txt");
        std::fs::write(&file_path, b"data").unwrap();

        cache.set(
            &file_path,
            SyncState {
                blake3: "abc".into(),
                size: 4,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/index/conflicted.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );

        let conflict = crate::conflict::ConflictInfo {
            rel_path: "conflicted.txt".into(),
            local_vclock: VectorClock::new(),
            remote_vclock: VectorClock::new(),
            local_blake3: "abc".into(),
            remote_blake3: "def".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            detected_at: 0,
            times_recorded: 0,
            remote_manifest_key: None,
        };

        assert!(cache.mark_conflict(&file_path, conflict.clone()));

        let entry = cache.get(&file_path).unwrap();
        assert_eq!(entry.status, FileSyncStatus::Conflict);
        let stored = entry.conflict.as_ref().expect("conflict payload");
        assert_eq!(stored.rel_path, conflict.rel_path);
        assert_eq!(stored.local_device, conflict.local_device);
        assert_eq!(stored.remote_device, conflict.remote_device);
    }

    #[test]
    fn mark_conflict_preserves_existing_remote_manifest_key_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        let file_path = dir.path().join("conflicted.txt");
        std::fs::write(&file_path, b"data").unwrap();

        cache.set(
            &file_path,
            SyncState {
                blake3: "abc".into(),
                size: 4,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/index/conflicted.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );

        let mut conflict = crate::conflict::ConflictInfo {
            rel_path: "conflicted.txt".into(),
            local_vclock: VectorClock::new(),
            remote_vclock: VectorClock::new(),
            local_blake3: "abc".into(),
            remote_blake3: "def".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            detected_at: 0,
            times_recorded: 0,
            remote_manifest_key: Some("data/manifests/first".into()),
        };
        assert!(cache.mark_conflict(&file_path, conflict.clone()));

        conflict.remote_manifest_key = None;
        assert!(cache.mark_conflict(&file_path, conflict));
        assert_eq!(
            cache
                .get(&file_path)
                .unwrap()
                .conflict
                .as_ref()
                .unwrap()
                .remote_manifest_key
                .as_deref(),
            Some("data/manifests/first")
        );

        let replacement = crate::conflict::ConflictInfo {
            remote_manifest_key: Some("data/manifests/replacement".into()),
            ..cache
                .get(&file_path)
                .unwrap()
                .conflict
                .as_ref()
                .unwrap()
                .clone()
        };
        assert!(cache.mark_conflict(&file_path, replacement));
        assert_eq!(
            cache
                .get(&file_path)
                .unwrap()
                .conflict
                .as_ref()
                .unwrap()
                .remote_manifest_key
                .as_deref(),
            Some("data/manifests/replacement")
        );
    }

    #[test]
    fn metadata_persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        {
            let mut cache = StateCache::open(&path).unwrap();
            cache.set_last_nats_seq(42);
            cache.set_device_id("neo".into());
            cache.flush().unwrap();
        }

        let cache = StateCache::open(&path).unwrap();
        assert_eq!(cache.last_nats_seq(), 42);
        assert_eq!(cache.device_id(), "neo");
    }

    #[test]
    fn legacy_format_loads_with_default_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut entries = HashMap::new();
        entries.insert(
            "file.txt".to_string(),
            SyncState {
                blake3: "abc".into(),
                size: 5,
                mtime: 1000,
                chunk_count: 1,
                remote_path: "bucket/file.txt".into(),
                last_synced: 100,
                vclock: VectorClock::new(),
                device_id: "test".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        write_private_state_file(
            &path,
            serde_json::to_string_pretty(&entries).unwrap().as_bytes(),
        );

        let cache = StateCache::open(&path).unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.last_nats_seq(), 0);
        assert!(cache.device_id().is_empty());
    }

    #[test]
    fn flush_creates_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let file_path = dir.path().join("f.txt");
        std::fs::write(&file_path, b"x").unwrap();

        let mut cache = StateCache::open(&path).unwrap();
        cache.set(
            &file_path,
            SyncState {
                blake3: "aaa".into(),
                size: 1,
                mtime: 0,
                chunk_count: 1,
                remote_path: "idx/f.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "d".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        cache.flush().unwrap();

        cache.set(
            &file_path,
            SyncState {
                blake3: "bbb".into(),
                size: 2,
                mtime: 1,
                chunk_count: 1,
                remote_path: "idx/f.txt".into(),
                last_synced: 1,
                vclock: VectorClock::new(),
                device_id: "d".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        cache.flush().unwrap();

        assert!(dir.path().join("state.json.bak").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(dir.path().join("state.json.bak"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn flush_never_rotates_content_corrupt_primary_into_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let backup_path = path.with_extension("json.bak");
        let mut cache = StateCache::open(&path).unwrap();

        cache.set_last_nats_seq(1);
        cache.flush().unwrap();
        cache.set_last_nats_seq(2);
        cache.flush().unwrap();
        let known_good_backup = std::fs::read(&backup_path).unwrap();

        write_private_state_file(&path, b"corrupt after open");
        cache.set_last_nats_seq(3);
        let error = cache
            .flush()
            .expect_err("content-corrupt primary must not enter backup rotation");
        assert!(
            format!("{error:#}").contains("refusing to back up content-corrupt state cache"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&backup_path).unwrap(), known_good_backup);

        // Avoid a second best-effort retry from Drop obscuring this deliberate
        // corruption fixture with an unrelated warning.
        cache.dirty = false;
    }

    #[cfg(unix)]
    #[test]
    fn flush_rejects_preplaced_backup_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let tracked = dir.path().join("tracked.txt");
        std::fs::write(&tracked, b"tracked").unwrap();
        let mut cache = StateCache::open(&path).unwrap();
        cache.set(
            &tracked,
            SyncState {
                blake3: "one".into(),
                size: 1,
                mtime: 1,
                chunk_count: 1,
                remote_path: "idx/tracked.txt".into(),
                last_synced: 1,
                vclock: VectorClock::new(),
                device_id: "device".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        cache.flush().unwrap();

        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"do not overwrite").unwrap();
        symlink(&victim, dir.path().join("state.json.bak")).unwrap();
        cache.set_status(&tracked, FileSyncStatus::Active);
        let error = cache.flush().expect_err("backup symlink must fail closed");

        assert!(error.to_string().contains("backup"), "{error:#}");
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not overwrite");
    }

    #[cfg(unix)]
    #[test]
    fn flush_rejects_preplaced_backup_fifo_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let tracked = dir.path().join("tracked.txt");
        std::fs::write(&tracked, b"tracked").unwrap();
        let mut cache = StateCache::open(&path).unwrap();
        cache.set(
            &tracked,
            SyncState {
                blake3: "one".into(),
                size: 1,
                mtime: 1,
                chunk_count: 1,
                remote_path: "idx/tracked.txt".into(),
                last_synced: 1,
                vclock: VectorClock::new(),
                device_id: "device".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        cache.flush().unwrap();

        create_fifo(&dir.path().join("state.json.bak"));
        cache.set_status(&tracked, FileSyncStatus::Active);
        let error = cache
            .flush()
            .expect_err("backup FIFO must fail instead of blocking");
        assert!(error.to_string().contains("backup"), "{error:#}");
    }

    #[test]
    fn recover_from_corrupt_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bak_path = dir.path().join("state.json.bak");

        let on_disk = StateCacheOnDisk {
            last_nats_seq: 99,
            device_id: "recovered".into(),
            entries: HashMap::new(),
        };
        write_private_state_file(
            &bak_path,
            serde_json::to_string(&on_disk).unwrap().as_bytes(),
        );
        write_private_state_file(&path, b"NOT VALID JSON {{{{");

        let cache = StateCache::open(&path).unwrap();
        assert_eq!(cache.last_nats_seq(), 99);
        assert_eq!(cache.device_id(), "recovered");
        assert!(cache.recovered_from_backup);
    }

    #[test]
    fn immutable_snapshot_never_recovers_a_missing_primary_from_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bak_path = write_recovery_backup(&path, 41, "snapshot-backup");
        let backup_before = std::fs::read(&bak_path).unwrap();

        let snapshot = StateCacheSnapshot::read_primary(&path).unwrap();

        assert!(snapshot.is_none());
        assert!(!path.exists(), "snapshot read must not repair the primary");
        assert_eq!(std::fs::read(&bak_path).unwrap(), backup_before);
    }

    #[test]
    fn immutable_snapshot_never_recovers_a_corrupt_primary_from_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bak_path = write_recovery_backup(&path, 99, "snapshot-backup");
        write_private_state_file(&path, b"NOT VALID JSON {{{{");
        let primary_before = std::fs::read(&path).unwrap();
        let backup_before = std::fs::read(&bak_path).unwrap();

        let error = StateCacheSnapshot::read_primary(&path)
            .expect_err("corrupt primary must fail without recovery");

        assert!(format!("{error:#}").contains("parsing state snapshot"));
        assert_eq!(std::fs::read(&path).unwrap(), primary_before);
        assert_eq!(std::fs::read(&bak_path).unwrap(), backup_before);
    }

    #[test]
    fn immutable_snapshot_reads_current_and_legacy_formats_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let current_path = dir.path().join("current.json");
        let legacy_path = dir.path().join("legacy.json");
        let entry = SyncState {
            blake3: "hash".into(),
            size: 7,
            mtime: 11,
            chunk_count: 1,
            remote_path: "roots/work/index/file.txt".into(),
            last_synced: 13,
            vclock: VectorClock::new(),
            device_id: "device-a".into(),
            conflict: None,
            status: FileSyncStatus::Synced,
        };
        let entries = HashMap::from([("/srv/work/file.txt".to_string(), entry)]);
        let current = StateCacheOnDisk {
            last_nats_seq: 17,
            device_id: "device-a".into(),
            entries: entries.clone(),
        };
        write_private_state_file(
            &current_path,
            serde_json::to_string(&current).unwrap().as_bytes(),
        );
        write_private_state_file(
            &legacy_path,
            serde_json::to_string(&entries).unwrap().as_bytes(),
        );
        let current_before = std::fs::read(&current_path).unwrap();
        let legacy_before = std::fs::read(&legacy_path).unwrap();

        let current_snapshot = StateCacheSnapshot::read_primary(&current_path)
            .unwrap()
            .expect("current snapshot");
        let legacy_snapshot = StateCacheSnapshot::read_primary(&legacy_path)
            .unwrap()
            .expect("legacy snapshot");

        assert_eq!(current_snapshot.len(), 1);
        assert_eq!(current_snapshot.last_nats_seq(), 17);
        assert_eq!(current_snapshot.device_id(), "device-a");
        assert_eq!(legacy_snapshot.len(), 1);
        assert_eq!(legacy_snapshot.last_nats_seq(), 0);
        assert!(legacy_snapshot.device_id().is_empty());
        assert_eq!(std::fs::read(&current_path).unwrap(), current_before);
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_before);
    }

    #[test]
    fn first_flush_after_recovery_preserves_known_good_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bak_path = write_recovery_backup(&path, 99, "recovered");
        write_private_state_file(&path, b"NOT VALID JSON {{{{");

        let mut cache = StateCache::open(&path).unwrap();
        assert!(cache.recovered_from_backup);
        cache.set_last_nats_seq(100);
        cache.flush().unwrap();

        assert!(!cache.recovered_from_backup);
        let (_, backup_sequence, backup_device) =
            StateCache::load_from_backup_file(&bak_path).unwrap();
        assert_eq!(backup_sequence, 99);
        assert_eq!(backup_device, "recovered");
        let repaired = StateCache::open(&path).unwrap();
        assert_eq!(repaired.last_nats_seq(), 100);
        assert!(!repaired.recovered_from_backup);
    }

    #[test]
    fn missing_primary_recovers_backup_instead_of_starting_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bak_path = write_recovery_backup(&path, 41, "orphan-recovery");

        let mut cache = StateCache::open(&path).unwrap();
        assert_eq!(cache.last_nats_seq(), 41);
        assert_eq!(cache.device_id(), "orphan-recovery");
        assert!(cache.recovered_from_backup);

        // Recovery itself is sufficient reason for an explicit flush to repair
        // the missing primary; no unrelated state mutation is required.
        cache.flush().unwrap();
        assert!(path.exists());
        assert!(!cache.recovered_from_backup);
        let (_, backup_sequence, _) = StateCache::load_from_backup_file(&bak_path).unwrap();
        assert_eq!(backup_sequence, 41);
        let repaired = StateCache::open(&path).unwrap();
        assert_eq!(repaired.last_nats_seq(), 41);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_permissive_backup() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bak_path = dir.path().join("state.json.bak");
        let on_disk = StateCacheOnDisk {
            last_nats_seq: 99,
            device_id: "forged".into(),
            entries: HashMap::new(),
        };
        write_private_state_file(&path, b"corrupt");
        write_private_state_file(
            &bak_path,
            serde_json::to_string(&on_disk).unwrap().as_bytes(),
        );
        std::fs::set_permissions(&bak_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = StateCache::open(&path)
            .err()
            .expect("permissive recovery backup must fail closed");
        assert!(
            format!("{error:#}").contains("mode 0600 or stricter"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlinked_and_hardlinked_backup() {
        use std::os::unix::fs::symlink;

        let valid = serde_json::to_string(&StateCacheOnDisk {
            last_nats_seq: 99,
            device_id: "recovered".into(),
            entries: HashMap::new(),
        })
        .unwrap();

        let symlink_dir = tempfile::tempdir().unwrap();
        let symlink_main = symlink_dir.path().join("state.json");
        write_private_state_file(&symlink_main, b"corrupt");
        let symlink_target = symlink_dir.path().join("target.json");
        write_private_state_file(&symlink_target, valid.as_bytes());
        symlink(&symlink_target, symlink_dir.path().join("state.json.bak")).unwrap();
        let error = StateCache::open(&symlink_main)
            .err()
            .expect("symlinked recovery backup must fail closed");
        assert!(error.to_string().contains("backup"), "{error:#}");

        let hardlink_dir = tempfile::tempdir().unwrap();
        let hardlink_main = hardlink_dir.path().join("state.json");
        write_private_state_file(&hardlink_main, b"corrupt");
        let hardlink_target = hardlink_dir.path().join("target.json");
        write_private_state_file(&hardlink_target, valid.as_bytes());
        std::fs::hard_link(&hardlink_target, hardlink_dir.path().join("state.json.bak")).unwrap();
        let error = StateCache::open(&hardlink_main)
            .err()
            .expect("hardlinked recovery backup must fail closed");
        assert!(error.to_string().contains("backup"), "{error:#}");
    }

    #[test]
    fn corrupt_main_no_backup_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_private_state_file(&path, b"GARBAGE");

        let result = StateCache::open(&path);
        assert!(result.is_err());
    }

    #[test]
    fn flush_if_stale_skips_when_recent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        cache.set_last_nats_seq(1);
        cache.flush_if_stale(Duration::from_secs(3600)).unwrap();
        assert!(cache.dirty);
    }

    #[test]
    fn flush_if_stale_flushes_when_overdue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut cache = StateCache::open(&path).unwrap();

        cache.set_last_nats_seq(1);
        cache.flush_if_stale(Duration::ZERO).unwrap();
        assert!(!cache.dirty);
        assert!(path.exists());
    }

    #[test]
    fn recovered_backup_is_repaired_by_stale_flush_and_drop() {
        let stale_dir = tempfile::tempdir().unwrap();
        let stale_path = stale_dir.path().join("state.json");
        write_recovery_backup(&stale_path, 17, "stale-recovery");
        let mut stale_cache = StateCache::open(&stale_path).unwrap();
        assert!(stale_cache.recovered_from_backup);
        stale_cache.flush_if_stale(Duration::ZERO).unwrap();
        assert!(stale_path.exists());
        assert!(!stale_cache.recovered_from_backup);

        let drop_dir = tempfile::tempdir().unwrap();
        let drop_path = drop_dir.path().join("state.json");
        write_recovery_backup(&drop_path, 23, "drop-recovery");
        {
            let cache = StateCache::open(&drop_path).unwrap();
            assert!(cache.recovered_from_backup);
        }
        assert!(drop_path.exists());
        let reopened = StateCache::open(&drop_path).unwrap();
        assert_eq!(reopened.last_nats_seq(), 23);
        assert_eq!(reopened.device_id(), "drop-recovery");
    }

    #[test]
    fn purge_stale_preserves_unresolved_conflicts_across_prefixes() {
        // TIN-2657/TIN-2658: boot-time purge must never drop an entry that
        // carries an unresolved conflict, even when its remote_path lives under
        // a foreign prefix (e.g. the `git-roam/*` roam record). A plain foreign
        // non-conflict entry is still dropped.
        let dir = tempfile::tempdir().unwrap();
        let mut cache = StateCache::open(&dir.path().join("state.json")).unwrap();

        // In-prefix ordinary entry — kept.
        cache.set(
            std::path::Path::new("/sync/keep.txt"),
            SyncState {
                blake3: "a".into(),
                size: 1,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/index/keep.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );
        // Foreign-prefix entry carrying an unresolved conflict — must survive.
        cache.set(
            std::path::Path::new("/sync/repo/.git/HEAD"),
            SyncState {
                blake3: "b".into(),
                size: 1,
                mtime: 0,
                chunk_count: 1,
                remote_path: "git-roam/repo/.git/HEAD".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: Some(crate::conflict::ConflictInfo {
                    rel_path: "repo/.git/HEAD".into(),
                    local_vclock: VectorClock::new(),
                    remote_vclock: VectorClock::new(),
                    local_blake3: "b".into(),
                    remote_blake3: "c".into(),
                    local_device: "neo".into(),
                    remote_device: "honey".into(),
                    detected_at: 0,
                    times_recorded: 1,
                    remote_manifest_key: None,
                }),
                status: FileSyncStatus::Conflict,
            },
        );
        // Foreign-prefix ordinary entry with no conflict — must be dropped.
        cache.set(
            std::path::Path::new("/sync/foreign.txt"),
            SyncState {
                blake3: "d".into(),
                size: 1,
                mtime: 0,
                chunk_count: 1,
                remote_path: "git-roam/foreign.txt".into(),
                last_synced: 0,
                vclock: VectorClock::new(),
                device_id: "neo".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );

        let removed = cache.purge_stale("data/index");

        assert_eq!(removed, 1, "only the foreign non-conflict entry is dropped");
        assert!(
            cache.get(std::path::Path::new("/sync/keep.txt")).is_some(),
            "in-prefix entry retained"
        );
        assert!(
            cache
                .get(std::path::Path::new("/sync/repo/.git/HEAD"))
                .is_some(),
            "foreign-prefix UNRESOLVED CONFLICT must be preserved across boot"
        );
        assert!(
            cache
                .get(std::path::Path::new("/sync/foreign.txt"))
                .is_none(),
            "foreign-prefix non-conflict entry still purged"
        );
    }
}
