//! Directory reconciliation pipeline — plan-then-execute bidirectional sync.
//!
//! `reconcile()` diffs a local directory tree against the remote index and
//! produces a `ReconcilePlan` (pure data, no side effects). `execute_plan()`
//! then performs the actual I/O using existing engine primitives.
//!
//! This separation enables dry-run mode, TUI preview, and deterministic testing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use opendal::Operator;
use tracing::{debug, info, warn};

use crate::blacklist::Blacklist;
use crate::conflict::{compare_clocks, ConflictInfo};
use crate::engine::{self, OptionalEncryption, ProgressFn};
use crate::index_entry::{parse_index_entry, parse_index_entry_record, RemoteIndexEntry};
use crate::manifest::SyncManifest;
use crate::state::{StateCache, StateCacheBackend, SyncState};

// ── Types ────────────────────────────────────────────────────────────────────

/// Why a file needs to be pushed.
#[derive(Debug, Clone)]
pub enum PushReason {
    /// Exists locally but not in the remote index.
    NewLocal,
    /// Vector clock indicates local is ahead of remote.
    LocalNewer,
}

/// Why a file needs to be pulled.
#[derive(Debug, Clone)]
pub enum PullReason {
    /// Exists in remote index but not locally.
    NewRemote,
    /// Vector clock indicates remote is ahead of local.
    RemoteNewer,
}

/// A single reconciliation action — pure data describing what to do.
#[derive(Debug, Clone)]
pub enum ReconcileAction {
    Push {
        local_path: PathBuf,
        rel_path: String,
        reason: PushReason,
    },
    Pull {
        rel_path: String,
        manifest_hash: String,
        size: u64,
        reason: PullReason,
    },
    DeleteLocal {
        local_path: PathBuf,
        rel_path: String,
    },
    DeleteRemote {
        rel_path: String,
    },
    Conflict {
        rel_path: String,
        info: ConflictInfo,
    },
    UpToDate {
        rel_path: String,
    },
}

/// Summary statistics for a reconciliation plan.
#[derive(Debug, Clone, Default)]
pub struct ReconcileSummary {
    pub pushes: usize,
    pub pulls: usize,
    pub local_deletes: usize,
    pub remote_deletes: usize,
    pub conflicts: usize,
    pub up_to_date: usize,
}

/// Complete reconciliation plan — pure data, no side effects.
#[derive(Debug, Clone)]
pub struct ReconcilePlan {
    pub actions: Vec<ReconcileAction>,
    pub summary: ReconcileSummary,
    pub device_id: String,
    pub generated_at: u64,
}

/// Configuration controlling reconciliation behavior.
#[derive(Debug, Clone, Default)]
pub struct ReconcileConfig {
    /// Delete local files that were synced but no longer exist on remote.
    pub delete_local_orphans: bool,
    /// Delete remote files that were synced but no longer exist locally.
    pub delete_remote_orphans: bool,
}

/// Result of executing a reconciliation plan.
#[derive(Debug, Default)]
pub struct ExecutionResult {
    pub pushed: usize,
    pub pulled: usize,
    pub deleted_local: usize,
    pub deleted_remote: usize,
    pub conflicts_recorded: usize,
    pub errors: Vec<(String, String)>,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
}

// ── Remote Index ─────────────────────────────────────────────────────────────

/// Fetch the full remote index for a prefix.
///
/// Returns a map of `rel_path → RemoteIndexEntry` for every file in the index.
pub async fn list_remote_index(
    op: &Operator,
    remote_prefix: &str,
) -> Result<HashMap<String, RemoteIndexEntry>> {
    let index_prefix = format!("{}/index/", remote_prefix.trim_end_matches('/'));
    let entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .context("listing remote index")?;

    let mut result = HashMap::new();
    for entry in entries {
        let full_key = entry.path();
        let rel_path = full_key
            .strip_prefix(&index_prefix)
            .unwrap_or(full_key)
            .to_string();

        // Skip directory markers and empty paths
        if rel_path.is_empty() || rel_path.ends_with('/') {
            continue;
        }

        match op.read(full_key).await {
            Ok(data) => match parse_index_entry_record(&data.to_vec()) {
                Ok(parsed) => {
                    if let Some(visible) = parsed.visible_entry().cloned() {
                        result.insert(rel_path, visible);
                    } else {
                        debug!(
                            key = full_key,
                            state = ?parsed.state(),
                            "skipping non-visible index entry"
                        );
                    }
                }
                Err(e) => {
                    warn!(key = full_key, error = %e, "skipping unparseable index entry");
                }
            },
            Err(e) => {
                warn!(key = full_key, error = %e, "skipping unreadable index entry");
            }
        }
    }

    debug!(count = result.len(), "fetched remote index");
    Ok(result)
}

// ── Reconciliation ───────────────────────────────────────────────────────────

/// Generate a reconciliation plan by diffing local tree against remote index.
///
/// This is a **pure function** — it reads state and remote index but performs
/// no writes. The returned plan can be inspected, displayed, or executed.
pub async fn reconcile(
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &StateCache,
    device_id: &str,
    blacklist: &Blacklist,
    config: &ReconcileConfig,
) -> Result<ReconcilePlan> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // 1. Collect local files
    let local_files = collect_local_set(local_root, blacklist)?;
    debug!(count = local_files.len(), "collected local files");

    // 2. Fetch remote index
    let remote_index = list_remote_index(op, remote_prefix).await?;
    debug!(count = remote_index.len(), "fetched remote index");

    // 3. Build alignment — union of all known paths
    let mut all_paths: HashSet<String> = HashSet::new();
    all_paths.extend(local_files.keys().cloned());
    all_paths.extend(remote_index.keys().cloned());
    // Include state-tracked paths (may have been deleted from both sides)
    for (key, _entry) in StateCacheBackend::all_entries(state) {
        if let Some(rel) = extract_rel_path_from_state(&key, local_root) {
            all_paths.insert(rel);
        }
    }

    // 4. Classify each path
    let mut actions = Vec::new();
    let mut summary = ReconcileSummary::default();

    for rel_path in &all_paths {
        let local = local_files.get(rel_path);
        let remote = remote_index.get(rel_path);
        let tracked = state.get_by_rel_path(rel_path).map(|(_, s)| s);

        let action = classify_path(
            rel_path,
            local,
            remote,
            tracked,
            op,
            remote_prefix,
            device_id,
            config,
        )
        .await;

        match &action {
            ReconcileAction::Push { .. } => summary.pushes += 1,
            ReconcileAction::Pull { .. } => summary.pulls += 1,
            ReconcileAction::DeleteLocal { .. } => summary.local_deletes += 1,
            ReconcileAction::DeleteRemote { .. } => summary.remote_deletes += 1,
            ReconcileAction::Conflict { .. } => summary.conflicts += 1,
            ReconcileAction::UpToDate { .. } => summary.up_to_date += 1,
        }

        actions.push(action);
    }

    // 5. Sort: conflicts first, then pulls, pushes, deletes, up-to-date last
    actions.sort_by_key(|a| match a {
        ReconcileAction::Conflict { .. } => 0,
        ReconcileAction::Pull { .. } => 1,
        ReconcileAction::Push { .. } => 2,
        ReconcileAction::DeleteLocal { .. } => 3,
        ReconcileAction::DeleteRemote { .. } => 4,
        ReconcileAction::UpToDate { .. } => 5,
    });

    info!(
        pushes = summary.pushes,
        pulls = summary.pulls,
        conflicts = summary.conflicts,
        up_to_date = summary.up_to_date,
        "reconciliation plan generated"
    );

    Ok(ReconcilePlan {
        actions,
        summary,
        device_id: device_id.to_string(),
        generated_at: now,
    })
}

/// Classify a single path into a reconciliation action.
#[allow(clippy::too_many_arguments)]
async fn classify_path(
    rel_path: &str,
    local: Option<&PathBuf>,
    remote: Option<&RemoteIndexEntry>,
    tracked: Option<&SyncState>,
    op: &Operator,
    remote_prefix: &str,
    device_id: &str,
    config: &ReconcileConfig,
) -> ReconcileAction {
    match (local, remote, tracked) {
        // New local file — not on remote, not previously synced
        (Some(local_path), None, None) => ReconcileAction::Push {
            local_path: local_path.clone(),
            rel_path: rel_path.to_string(),
            reason: PushReason::NewLocal,
        },

        // Was synced, now deleted locally — delete from remote if configured
        (None, Some(_remote_entry), Some(_tracked_state)) => {
            if config.delete_remote_orphans {
                ReconcileAction::DeleteRemote {
                    rel_path: rel_path.to_string(),
                }
            } else {
                ReconcileAction::UpToDate {
                    rel_path: rel_path.to_string(),
                }
            }
        }

        // New remote file — not local, not previously synced
        (None, Some(remote_entry), None) => ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash: remote_entry.manifest_hash.clone(),
            size: remote_entry.size,
            reason: PullReason::NewRemote,
        },

        // Was synced, now deleted from remote — delete locally if configured
        (Some(local_path), None, Some(_tracked_state)) => {
            if config.delete_local_orphans {
                ReconcileAction::DeleteLocal {
                    local_path: local_path.clone(),
                    rel_path: rel_path.to_string(),
                }
            } else {
                ReconcileAction::UpToDate {
                    rel_path: rel_path.to_string(),
                }
            }
        }

        // Both exist — compare via vector clocks
        (Some(local_path), Some(remote_entry), tracked_opt) => {
            compare_both_exist(
                rel_path,
                local_path,
                remote_entry,
                tracked_opt,
                op,
                remote_prefix,
                device_id,
            )
            .await
        }

        // Ghost: tracked but gone from both sides
        (None, None, Some(_)) => ReconcileAction::UpToDate {
            rel_path: rel_path.to_string(),
        },

        // Nothing anywhere — shouldn't happen, but handle gracefully
        (None, None, None) => ReconcileAction::UpToDate {
            rel_path: rel_path.to_string(),
        },
        // Local exists, not remote, but was tracked — local is newer
        // (This case is covered above, but the compiler needs exhaustive matching
        //  for the tracked_opt variations. The (Some, None, None) case above handles it.)
    }
}

/// Compare when both local and remote exist — uses vector clocks.
async fn compare_both_exist(
    rel_path: &str,
    local_path: &Path,
    remote_entry: &RemoteIndexEntry,
    tracked: Option<&SyncState>,
    op: &Operator,
    remote_prefix: &str,
    device_id: &str,
) -> ReconcileAction {
    // Get local hash
    let local_hash = match tcfs_chunks::hash_file(local_path) {
        Ok(h) => tcfs_chunks::hash_to_hex(&h),
        Err(e) => {
            warn!(path = %local_path.display(), error = %e, "failed to hash local file");
            return ReconcileAction::UpToDate {
                rel_path: rel_path.to_string(),
            };
        }
    };

    // Get local vector clock from tracked state
    let local_vclock = tracked.map(|s| s.vclock.clone()).unwrap_or_default();

    // Fetch remote manifest for its vector clock and hash
    let manifest_path = format!(
        "{}/manifests/{}",
        remote_prefix.trim_end_matches('/'),
        &remote_entry.manifest_hash
    );

    let remote_manifest = match op.read(&manifest_path).await {
        Ok(data) => match SyncManifest::from_bytes(&data.to_vec()) {
            Ok(m) => m,
            Err(e) => {
                warn!(path = manifest_path, error = %e, "failed to parse remote manifest");
                return ReconcileAction::Push {
                    local_path: local_path.to_path_buf(),
                    rel_path: rel_path.to_string(),
                    reason: PushReason::NewLocal,
                };
            }
        },
        Err(e) => {
            warn!(path = manifest_path, error = %e, "failed to read remote manifest");
            return ReconcileAction::Push {
                local_path: local_path.to_path_buf(),
                rel_path: rel_path.to_string(),
                reason: PushReason::NewLocal,
            };
        }
    };

    let remote_device = remote_manifest.written_by.as_str();

    let outcome = compare_clocks(
        &local_vclock,
        &remote_manifest.vclock,
        &local_hash,
        &remote_manifest.file_hash,
        rel_path,
        device_id,
        remote_device,
    );

    match outcome {
        crate::conflict::SyncOutcome::UpToDate => ReconcileAction::UpToDate {
            rel_path: rel_path.to_string(),
        },
        crate::conflict::SyncOutcome::LocalNewer => ReconcileAction::Push {
            local_path: local_path.to_path_buf(),
            rel_path: rel_path.to_string(),
            reason: PushReason::LocalNewer,
        },
        crate::conflict::SyncOutcome::RemoteNewer => ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash: remote_entry.manifest_hash.clone(),
            size: remote_entry.size,
            reason: PullReason::RemoteNewer,
        },
        crate::conflict::SyncOutcome::Conflict(info) => ReconcileAction::Conflict {
            rel_path: rel_path.to_string(),
            info,
        },
    }
}

// ── Execution ────────────────────────────────────────────────────────────────

/// Execute a reconciliation plan, performing all I/O operations.
///
/// Errors on individual actions are collected — the plan continues past failures.
#[allow(clippy::too_many_arguments)]
pub async fn execute_plan(
    plan: &ReconcilePlan,
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    device_id: &str,
    encryption: OptionalEncryption<'_>,
    progress: Option<&ProgressFn>,
) -> Result<ExecutionResult> {
    let mut result = ExecutionResult::default();

    for action in &plan.actions {
        match action {
            ReconcileAction::Push {
                local_path,
                rel_path,
                ..
            } => match engine::upload_file_with_device(
                op,
                local_path,
                remote_prefix,
                state,
                progress,
                device_id,
                Some(rel_path.as_str()),
                encryption,
            )
            .await
            {
                Ok(upload) => {
                    if !upload.skipped {
                        result.pushed += 1;
                        result.bytes_uploaded += upload.bytes;
                    }
                }
                Err(e) => {
                    result
                        .errors
                        .push((rel_path.clone(), format!("push failed: {e}")));
                }
            },

            ReconcileAction::Pull {
                rel_path,
                manifest_hash,
                ..
            } => {
                let manifest_path = format!(
                    "{}/manifests/{}",
                    remote_prefix.trim_end_matches('/'),
                    manifest_hash
                );
                let local_path = local_root.join(rel_path);

                // Ensure parent directory exists
                if let Some(parent) = local_path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        result
                            .errors
                            .push((rel_path.clone(), format!("mkdir failed: {e}")));
                        continue;
                    }
                }

                match engine::download_file_with_device(
                    op,
                    &manifest_path,
                    &local_path,
                    remote_prefix,
                    progress,
                    device_id,
                    Some(state),
                    encryption,
                )
                .await
                {
                    Ok(download) => {
                        result.pulled += 1;
                        result.bytes_downloaded += download.bytes;
                    }
                    Err(e) => {
                        result
                            .errors
                            .push((rel_path.clone(), format!("pull failed: {e}")));
                    }
                }
            }

            ReconcileAction::DeleteLocal {
                local_path,
                rel_path,
            } => match tokio::fs::remove_file(local_path).await {
                Ok(()) => {
                    state.remove(local_path);
                    result.deleted_local += 1;
                }
                Err(e) => {
                    result
                        .errors
                        .push((rel_path.clone(), format!("local delete failed: {e}")));
                }
            },

            ReconcileAction::DeleteRemote { rel_path } => {
                let prefix = remote_prefix.trim_end_matches('/');
                let index_key = format!("{prefix}/index/{rel_path}");

                // Read index to find manifest hash before deleting
                let manifest_key = if let Ok(idx_bytes) = op.read(&index_key).await {
                    parse_index_entry(&idx_bytes.to_vec())
                        .ok()
                        .map(|entry| format!("{prefix}/manifests/{}", entry.manifest_hash))
                } else {
                    None
                };

                // Delete index entry
                if let Err(e) = op.delete(&index_key).await {
                    result
                        .errors
                        .push((rel_path.clone(), format!("remote index delete failed: {e}")));
                    continue;
                }

                // Delete manifest (prevents orphaned manifests)
                if let Some(ref mkey) = manifest_key {
                    if let Err(e) = op.delete(mkey).await {
                        tracing::warn!(
                            rel_path = %rel_path,
                            manifest = %mkey,
                            "failed to delete manifest during reconcile: {e}"
                        );
                    }
                }

                result.deleted_remote += 1;
            }

            ReconcileAction::Conflict { rel_path, info } => {
                // Record conflict in state cache for later resolution
                if let Some((key, existing)) = state.get_by_rel_path(rel_path) {
                    let key_owned = key.to_string();
                    let mut updated = existing.clone();
                    updated.conflict = Some(info.clone());
                    state.set(Path::new(&key_owned), updated);
                }
                result.conflicts_recorded += 1;
            }

            ReconcileAction::UpToDate { .. } => {
                // No-op
            }
        }
    }

    if let Err(e) = state.flush() {
        warn!(error = %e, "failed to flush state cache after plan execution");
    }

    info!(
        pushed = result.pushed,
        pulled = result.pulled,
        conflicts = result.conflicts_recorded,
        errors = result.errors.len(),
        "plan execution complete"
    );

    Ok(result)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Collect local files into a `rel_path → PathBuf` map, applying the blacklist.
fn collect_local_set(local_root: &Path, blacklist: &Blacklist) -> Result<HashMap<String, PathBuf>> {
    let config = crate::engine::CollectConfig {
        sync_git_dirs: blacklist.allows_git_dirs(),
        git_sync_mode: blacklist.git_sync_mode().to_string(),
        sync_hidden_dirs: blacklist.allows_hidden_dirs(),
        exclude_patterns: blacklist.glob_patterns(),
        follow_symlinks: false,
        sync_empty_dirs: false, // reconcile only cares about files
    };
    let result = crate::engine::collect_files(local_root, &config)?;

    let mut map = HashMap::new();
    for file in result.files {
        if let Ok(rel) = file.strip_prefix(local_root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            map.insert(rel_str, file);
        }
    }
    Ok(map)
}

/// Extract a relative path from a state cache key (canonicalized absolute path).
fn extract_rel_path_from_state(state_key: &str, local_root: &Path) -> Option<String> {
    let key_path = Path::new(state_key);
    key_path
        .strip_prefix(local_root)
        .ok()
        .or_else(|| {
            // Try canonicalized root
            std::fs::canonicalize(local_root)
                .ok()
                .and_then(|canon| key_path.strip_prefix(&canon).ok())
        })
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;

    fn memory_op() -> Operator {
        Operator::new(Memory::default()).unwrap().finish()
    }

    // ── list_remote_index ────────────────────────────────────────────────

    #[tokio::test]
    async fn list_remote_index_empty() {
        let op = memory_op();
        let index = list_remote_index(&op, "data").await.unwrap();
        assert!(index.is_empty());
    }

    #[tokio::test]
    async fn list_remote_index_finds_entries() {
        let op = memory_op();
        op.write(
            "data/index/file1.txt",
            b"manifest_hash=aaa\nsize=100\nchunks=1\n".to_vec(),
        )
        .await
        .unwrap();
        op.write(
            "data/index/file2.txt",
            b"manifest_hash=bbb\nsize=200\nchunks=2\n".to_vec(),
        )
        .await
        .unwrap();

        let index = list_remote_index(&op, "data").await.unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index["file1.txt"].manifest_hash, "aaa");
        assert_eq!(index["file2.txt"].manifest_hash, "bbb");
        assert_eq!(index["file2.txt"].size, 200);
    }

    // ── reconcile plan: local-only → push ────────────────────────────────

    #[tokio::test]
    async fn reconcile_local_only_file_generates_push() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        std::fs::write(local_root.join("new_file.txt"), b"hello").unwrap();

        let state_path = dir.path().join("state.json");
        let state = crate::state::StateCache::open(&state_path).unwrap();

        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(&op, local_root, "data", &state, "neo", &blacklist, &config)
            .await
            .unwrap();

        assert_eq!(
            plan.summary.pushes, 1,
            "local-only file should generate a push"
        );
        assert!(
            plan.actions.iter().any(|a| matches!(a, ReconcileAction::Push { rel_path, .. } if rel_path == "new_file.txt")),
            "push action should target new_file.txt"
        );
    }

    // ── reconcile plan: remote-only → pull ───────────────────────────────

    #[tokio::test]
    async fn reconcile_remote_only_file_generates_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();

        // Write a remote index entry (no local file)
        op.write(
            "data/index/remote_only.txt",
            b"manifest_hash=abc123\nsize=50\nchunks=1\n".to_vec(),
        )
        .await
        .unwrap();

        let state_path = dir.path().join("state.json");
        let state = crate::state::StateCache::open(&state_path).unwrap();

        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(&op, local_root, "data", &state, "neo", &blacklist, &config)
            .await
            .unwrap();

        assert_eq!(
            plan.summary.pulls, 1,
            "remote-only file should generate a pull"
        );
        assert!(
            plan.actions.iter().any(|a| matches!(a, ReconcileAction::Pull { rel_path, .. } if rel_path == "remote_only.txt")),
            "pull action should target remote_only.txt"
        );
    }

    // ── reconcile plan: both exist, up-to-date ───────────────────────────

    #[tokio::test]
    async fn reconcile_matching_files_up_to_date() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();

        let content = b"matching content";
        let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));

        // Write local file
        std::fs::write(local_root.join("same.txt"), content).unwrap();

        // Write matching remote index + manifest
        let index_entry = format!("manifest_hash={hash}\nsize={}\nchunks=1\n", content.len());
        op.write("data/index/same.txt", index_entry.into_bytes())
            .await
            .unwrap();

        let manifest_json = serde_json::json!({
            "version": 2,
            "file_hash": hash,
            "file_size": content.len(),
            "chunks": [],
            "vclock": {"clocks": {"neo": 1}},
            "written_by": "neo",
            "written_at": 0
        });
        op.write(
            &format!("data/manifests/{hash}"),
            serde_json::to_vec(&manifest_json).unwrap(),
        )
        .await
        .unwrap();

        // Set up state cache with matching entry
        let state_path = dir.path().join("state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let local_file = local_root.join("same.txt");
        let mut vc = crate::conflict::VectorClock::new();
        vc.tick("neo");
        let sync_state = crate::state::make_sync_state_full(
            &local_file,
            hash.clone(),
            1,
            format!("data/manifests/{hash}"),
            vc,
            "neo".into(),
        )
        .unwrap();
        state.set(&local_file, sync_state);

        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(&op, local_root, "data", &state, "neo", &blacklist, &config)
            .await
            .unwrap();

        assert_eq!(
            plan.summary.up_to_date, 1,
            "matching files should be up-to-date"
        );
        assert_eq!(plan.summary.pushes, 0);
        assert_eq!(plan.summary.pulls, 0);
        assert_eq!(plan.summary.conflicts, 0);
    }
    // ── original tests ───────────────────────────────────────────────────

    #[test]
    fn test_reconcile_summary_default() {
        let summary = ReconcileSummary::default();
        assert_eq!(summary.pushes, 0);
        assert_eq!(summary.pulls, 0);
        assert_eq!(summary.conflicts, 0);
    }

    #[test]
    fn test_reconcile_config_default_safe() {
        let config = ReconcileConfig::default();
        assert!(!config.delete_local_orphans);
        assert!(!config.delete_remote_orphans);
    }
}
