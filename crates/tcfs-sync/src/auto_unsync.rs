//! Auto-unsync controller — periodically scans the state cache for stale files
//! and removes them from tracking to reclaim sync state.
//!
//! Files are considered stale when `last_synced` exceeds the configured max age.
//! The sweep respects per-folder policy exemptions and skips files with unsynced
//! local modifications (dirty-child safety).
//!
//! Auto-unsync removes entries from the state cache but does NOT delete local files.
//! Untracked files will be re-synced if modified (caught by the file watcher).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{debug, info};

use crate::policy::PolicyStore;
use crate::state::{StateCache, StateCacheBackend};

/// Result of a single auto-unsync sweep.
#[derive(Debug, Default)]
pub struct SweepResult {
    /// Total entries scanned.
    pub scanned: usize,
    /// Entries removed from state cache (unsynced).
    pub unsynced: usize,
    /// Entries skipped due to policy exemption.
    pub skipped_exempt: usize,
    /// Entries skipped because they have unsynced local changes.
    pub skipped_dirty: usize,
    /// Entries skipped because the file no longer exists on disk.
    pub skipped_missing: usize,
    /// Total bytes of state entries removed (from cached size field).
    pub bytes_reclaimed: u64,
}

/// Run a single auto-unsync sweep over the state cache.
///
/// Finds files where `last_synced` is older than `max_age_secs`, respects
/// `PolicyStore` exemptions, and removes eligible entries from the state cache.
///
/// Local files are NOT deleted — only their tracking state is removed.
pub fn sweep(
    state: &mut StateCache,
    policy_store: &PolicyStore,
    max_age_secs: u64,
    dry_run: bool,
) -> SweepResult {
    let mut result = SweepResult::default();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Collect candidates (can't mutate state while iterating)
    let entries: Vec<(String, u64, u64)> = state
        .all_entries()
        .iter()
        .map(|(key, s)| (key.clone(), s.last_synced, s.size))
        .collect();

    let mut to_remove: Vec<(String, u64)> = Vec::new();

    for (key, last_synced, size) in &entries {
        result.scanned += 1;
        let path = Path::new(key);

        // Check age
        let age = now.saturating_sub(*last_synced);
        if age <= max_age_secs {
            continue;
        }

        // Check policy exemption
        if policy_store.is_auto_unsync_exempt(path) {
            result.skipped_exempt += 1;
            debug!(path = key, "auto-unsync: skipped (exempt)");
            continue;
        }

        // Check if file still exists on disk
        if !path.exists() {
            result.skipped_missing += 1;
            continue;
        }

        // Check for unsynced local changes (dirty-child safety)
        match state.needs_sync(path) {
            Ok(Some(_reason)) => {
                result.skipped_dirty += 1;
                debug!(path = key, "auto-unsync: skipped (dirty)");
                continue;
            }
            Err(_) => {
                // Can't stat file — skip rather than risk data loss
                continue;
            }
            Ok(None) => {
                // File is clean — eligible for unsync
            }
        }

        to_remove.push((key.clone(), *size));
    }

    // Execute removals
    for (key, size) in &to_remove {
        let path = Path::new(key.as_str());
        if dry_run {
            info!(
                path = key,
                age_secs = now.saturating_sub(
                    entries
                        .iter()
                        .find(|(k, _, _)| k == key)
                        .map(|(_, ls, _)| *ls)
                        .unwrap_or(0)
                ),
                "auto-unsync: would remove (dry run)"
            );
        } else {
            state.remove(path);
            debug!(path = key, "auto-unsync: removed from state cache");
        }
        result.unsynced += 1;
        result.bytes_reclaimed += size;
    }

    // Flush state if we made changes
    if !dry_run && result.unsynced > 0 {
        if let Err(e) = state.flush() {
            tracing::warn!(error = %e, "auto-unsync: failed to flush state cache");
        }
    }

    result
}

/// Result of a sweep that includes actual file dehydration.
#[derive(Debug, Default)]
pub struct DehydrationResult {
    /// Files scanned.
    pub scanned: usize,
    /// Files successfully dehydrated (converted to stubs).
    pub dehydrated: usize,
    /// Files skipped due to policy exemption.
    pub skipped_exempt: usize,
    /// Files skipped because they have unsynced local changes.
    pub skipped_dirty: usize,
    /// Files skipped because the file no longer exists on disk.
    pub skipped_missing: usize,
    /// Files where the unsync callback failed.
    pub failed: usize,
    /// Total bytes freed from disk cache.
    pub bytes_freed: u64,
}

/// Run a dehydration sweep: find stale files and call `unsync_fn` for each.
///
/// The `unsync_fn` callback performs the actual file-to-stub conversion and
/// returns bytes freed. This decouples sweep logic from VFS implementation.
pub async fn sweep_with_dehydration<F, Fut>(
    state: &mut StateCache,
    policy_store: &PolicyStore,
    max_age_secs: u64,
    max_per_sweep: usize,
    unsync_fn: F,
) -> DehydrationResult
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<u64, anyhow::Error>>,
{
    let mut result = DehydrationResult::default();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Collect candidates (can't mutate state while iterating)
    let entries: Vec<(String, u64, u64)> = state
        .all_entries()
        .iter()
        .map(|(key, s)| (key.clone(), s.last_synced, s.size))
        .collect();

    let mut to_update: Vec<String> = Vec::new();

    for (key, last_synced, _size) in &entries {
        if to_update.len() >= max_per_sweep {
            break;
        }
        result.scanned += 1;
        let path = Path::new(key);

        // Check age
        let age = now.saturating_sub(*last_synced);
        if age <= max_age_secs {
            continue;
        }

        // Check policy exemption
        if policy_store.is_auto_unsync_exempt(path) {
            result.skipped_exempt += 1;
            continue;
        }

        // Check if file still exists on disk
        if !path.exists() {
            result.skipped_missing += 1;
            continue;
        }

        // Check for unsynced local changes (dirty-child safety)
        match state.needs_sync(path) {
            Ok(Some(_)) => {
                result.skipped_dirty += 1;
                continue;
            }
            Err(_) => continue,
            Ok(None) => {} // Clean — eligible for dehydration
        }

        // Call the unsync callback
        match unsync_fn(key.clone()).await {
            Ok(freed) => {
                to_update.push(key.clone());
                result.bytes_freed += freed;
            }
            Err(e) => {
                tracing::warn!(path = key, error = %e, "auto-unsync: dehydration failed");
                result.failed += 1;
            }
        }
    }

    // Mark dehydrated files as NotSynced (preserves metadata for re-hydration)
    for key in &to_update {
        let path = Path::new(key.as_str());
        state.set_status(path, crate::state::FileSyncStatus::NotSynced);
        result.dehydrated += 1;
    }

    if result.dehydrated > 0 {
        if let Err(e) = state.flush() {
            tracing::warn!(error = %e, "auto-unsync: failed to flush state cache after dehydration");
        }
    }

    result
}

/// Check if disk usage exceeds the given threshold percentage.
///
/// Returns `true` if the filesystem containing `path` is above the threshold
/// (disk pressure detected). A threshold of 0.0 disables the check.
#[cfg(unix)]
pub fn disk_pressure_check(path: &Path, threshold_pct: f64) -> bool {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    if threshold_pct <= 0.0 {
        return false;
    }

    let c_path = match CString::new(path.to_string_lossy().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };

    unsafe {
        let mut stat = MaybeUninit::<libc::statvfs>::uninit();
        if libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return false;
        }
        let stat = stat.assume_init();
        if stat.f_blocks == 0 {
            return false;
        }
        let used = stat.f_blocks - stat.f_bfree;
        let usage_pct = used as f64 / stat.f_blocks as f64;
        usage_pct >= threshold_pct
    }
}

#[cfg(not(unix))]
pub fn disk_pressure_check(_path: &Path, _threshold_pct: f64) -> bool {
    false // Not implemented on non-Unix
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conflict::VectorClock;
    use crate::policy::FolderPolicy;
    use crate::state::SyncState;

    fn make_state(last_synced: u64) -> SyncState {
        SyncState {
            blake3: "abc123".into(),
            size: 1024,
            mtime: last_synced,
            chunk_count: 1,
            remote_path: "bucket/test".into(),
            last_synced,
            vclock: VectorClock::new(),
            device_id: String::new(),
            conflict: None,
            status: Default::default(),
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn test_sweep_skips_young_files() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        let file = dir.path().join("recent.txt");
        std::fs::write(&file, b"data").unwrap();
        state.set(&file, make_state(now_secs()));

        let result = sweep(&mut state, &policy, 3600, false);
        assert_eq!(result.scanned, 1);
        assert_eq!(result.unsynced, 0);
        assert!(state.get(&file).is_some());
    }

    #[test]
    fn test_sweep_removes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        let file = dir.path().join("old.txt");
        std::fs::write(&file, b"data").unwrap();

        // Set last_synced to 2 hours ago
        let old_time = now_secs() - 7200;
        let mut sync_state = make_state(old_time);
        // Match current file metadata so needs_sync returns None
        let meta = std::fs::metadata(&file).unwrap();
        sync_state.size = meta.len();
        sync_state.mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        sync_state.blake3 = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_file(&file).unwrap());
        state.set(&file, sync_state);

        let result = sweep(&mut state, &policy, 3600, false); // max_age = 1 hour
        assert_eq!(result.unsynced, 1);
        assert_eq!(result.bytes_reclaimed, meta.len());
        assert!(state.get(&file).is_none()); // removed from state
        assert!(file.exists()); // file still on disk
    }

    #[test]
    fn test_sweep_respects_exempt() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let policy_path = dir.path().join("policies.json");
        let mut state = StateCache::open(&state_path).unwrap();
        let mut policy = PolicyStore::open(&policy_path).unwrap();

        let folder = dir.path().join("important");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("keep.txt");
        std::fs::write(&file, b"data").unwrap();

        // Mark folder as exempt
        policy.set(
            &folder,
            FolderPolicy {
                auto_unsync_exempt: true,
                ..Default::default()
            },
        );

        // Old file in exempt folder
        state.set(&file, make_state(now_secs() - 7200));

        let result = sweep(&mut state, &policy, 3600, false);
        assert_eq!(result.skipped_exempt, 1);
        assert_eq!(result.unsynced, 0);
        assert!(state.get(&file).is_some()); // still tracked
    }

    #[test]
    fn test_sweep_skips_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        let file = dir.path().join("modified.txt");
        std::fs::write(&file, b"original").unwrap();

        // Old sync state with different content
        let mut sync_state = make_state(now_secs() - 7200);
        sync_state.size = 100; // doesn't match actual file size → dirty
        state.set(&file, sync_state);

        let result = sweep(&mut state, &policy, 3600, false);
        assert_eq!(result.skipped_dirty, 1);
        assert_eq!(result.unsynced, 0);
    }

    #[test]
    fn test_sweep_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        let file = dir.path().join("old.txt");
        std::fs::write(&file, b"data").unwrap();

        let mut sync_state = make_state(now_secs() - 7200);
        let meta = std::fs::metadata(&file).unwrap();
        sync_state.size = meta.len();
        sync_state.mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        sync_state.blake3 = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_file(&file).unwrap());
        state.set(&file, sync_state);

        let result = sweep(&mut state, &policy, 3600, true); // dry_run = true
        assert_eq!(result.unsynced, 1);
        assert!(state.get(&file).is_some()); // NOT removed (dry run)
    }

    #[test]
    fn disk_pressure_disabled_at_zero() {
        assert!(!disk_pressure_check(Path::new("/"), 0.0));
    }

    #[test]
    fn disk_pressure_unreachable_at_one() {
        // 100% threshold should never trigger on a healthy filesystem
        assert!(!disk_pressure_check(Path::new("/"), 1.0));
    }

    #[tokio::test]
    async fn sweep_with_dehydration_calls_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut cache = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        let file_path = dir.path().join("old_file.txt");
        std::fs::write(&file_path, "test data").unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_file(&file_path).unwrap());

        let mut sync_state = make_state(0); // last_synced = 0 → very old
        sync_state.size = meta.len();
        sync_state.mtime = mtime;
        sync_state.blake3 = hash;
        cache.set(&file_path, sync_state);

        let call_count = std::sync::Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();

        let result = sweep_with_dehydration(&mut cache, &policy, 60, 100, |_path| {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Ok(1024u64) // Pretend we freed 1KB
            }
        })
        .await;

        assert_eq!(result.dehydrated, 1);
        assert_eq!(result.bytes_freed, 1024);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // Verify the entry is now NotSynced (not removed)
        let entry = cache.get(&file_path).unwrap();
        assert_eq!(entry.status, crate::state::FileSyncStatus::NotSynced);
    }

    #[tokio::test]
    async fn sweep_with_dehydration_skips_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut cache = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        let file_path = dir.path().join("dirty_file.txt");
        std::fs::write(&file_path, "original content").unwrap();

        // Stale entry with mismatched size (dirty)
        let mut sync_state = make_state(0);
        sync_state.size = 999; // doesn't match real file size
        cache.set(&file_path, sync_state);

        let result = sweep_with_dehydration(&mut cache, &policy, 60, 100, |_path| async {
            panic!("callback should not be called for dirty files");
        })
        .await;

        assert_eq!(result.skipped_dirty, 1);
        assert_eq!(result.dehydrated, 0);
    }

    #[tokio::test]
    async fn sweep_with_dehydration_respects_max_per_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut cache = StateCache::open(&state_path).unwrap();
        let policy = PolicyStore::default();

        // Create 5 old files
        for i in 0..5 {
            let file_path = dir.path().join(format!("file_{i}.txt"));
            std::fs::write(&file_path, format!("content {i}")).unwrap();
            let meta = std::fs::metadata(&file_path).unwrap();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_file(&file_path).unwrap());
            let mut s = make_state(0);
            s.size = meta.len();
            s.mtime = mtime;
            s.blake3 = hash;
            cache.set(&file_path, s);
        }

        // Limit to 2 per sweep
        let result =
            sweep_with_dehydration(&mut cache, &policy, 60, 2, |_path| async { Ok(512u64) }).await;

        assert_eq!(result.dehydrated, 2);
        assert_eq!(result.bytes_freed, 1024);
    }
}
