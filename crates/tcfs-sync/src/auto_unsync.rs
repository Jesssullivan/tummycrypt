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
}
