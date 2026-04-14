//! Directory reconciliation pipeline — plan-then-execute bidirectional sync.
//!
//! `reconcile()` diffs a local directory tree against the remote index and
//! produces a `ReconcilePlan` (pure data, no side effects). `execute_plan()`
//! then performs the actual I/O using existing engine primitives.
//!
//! This separation enables dry-run mode, TUI preview, and deterministic testing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use opendal::Operator;
use tracing::{debug, info, warn};

use crate::blacklist::Blacklist;
use crate::conflict::{compare_clocks, ConflictInfo};
use crate::engine::{self, OptionalEncryption, ProgressFn};
use crate::index_entry::{resolve_visible_index_entry, RemoteIndexEntry};
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

/// Visibility report for chunk objects that are no longer referenced by any
/// manifest under a remote prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanedChunkReport {
    pub orphaned_chunks: Vec<String>,
    pub referenced_chunks: usize,
    pub scanned_chunks: usize,
}

/// Cleanup report for orphaned chunk objects under a remote prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanedChunkCleanupReport {
    pub orphaned_chunks_found: usize,
    pub deleted_chunks: Vec<String>,
    pub skipped_within_grace: Vec<String>,
    pub skipped_missing_last_modified: Vec<String>,
    pub delete_errors: Vec<(String, String)>,
    pub referenced_chunks: usize,
    pub scanned_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteChunkObject {
    object_key: String,
    chunk_hash: String,
    last_modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PlannedOrphanCleanup {
    orphaned_chunks_found: usize,
    deletable: Vec<RemoteChunkObject>,
    skipped_within_grace: Vec<String>,
    skipped_missing_last_modified: Vec<String>,
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
        let rel_path = crate::engine::normalize_rel_path_text(
            full_key.strip_prefix(&index_prefix).unwrap_or(full_key),
        );

        // Skip directory markers and empty paths
        if rel_path.is_empty() || rel_path.ends_with('/') {
            continue;
        }

        match op.read(full_key).await {
            Ok(_) => {
                let manifest_prefix = format!("{}/manifests", remote_prefix.trim_end_matches('/'));
                match resolve_visible_index_entry(op, full_key, &manifest_prefix).await {
                    Ok(Some(visible)) => {
                        result.insert(rel_path, visible);
                    }
                    Ok(None) => {
                        debug!(key = full_key, "skipping non-visible index entry");
                    }
                    Err(e) => {
                        warn!(key = full_key, error = %e, "skipping unreadable index entry");
                    }
                }
            }
            Err(e) => {
                warn!(key = full_key, error = %e, "skipping unreadable index entry");
            }
        }
    }

    debug!(count = result.len(), "fetched remote index");
    Ok(result)
}

/// Scan manifests and chunk objects under a prefix and report chunks that are
/// no longer referenced by any reachable manifest.
pub async fn find_orphaned_chunks(
    op: &Operator,
    remote_prefix: &str,
) -> Result<OrphanedChunkReport> {
    let scan = scan_remote_chunks(op, remote_prefix).await?;
    let mut orphaned_chunks: Vec<String> = scan
        .chunk_objects
        .iter()
        .filter(|entry| !scan.referenced_chunks.contains(&entry.chunk_hash))
        .map(|entry| entry.chunk_hash.clone())
        .collect();
    orphaned_chunks.sort();

    Ok(OrphanedChunkReport {
        orphaned_chunks,
        referenced_chunks: scan.referenced_chunks.len(),
        scanned_chunks: scan.chunk_objects.len(),
    })
}

/// Delete orphaned chunks only after they have aged past a grace period.
///
/// Chunks without usable last-modified metadata are left in place so cleanup
/// stays conservative on backends that do not expose object timestamps.
pub async fn cleanup_orphaned_chunks(
    op: &Operator,
    remote_prefix: &str,
    grace_period: Duration,
    now: SystemTime,
) -> Result<OrphanedChunkCleanupReport> {
    let scan = scan_remote_chunks(op, remote_prefix).await?;
    let plan = plan_orphaned_chunk_cleanup(
        &scan.chunk_objects,
        &scan.referenced_chunks,
        grace_period,
        now,
    );

    let mut deleted_chunks = Vec::new();
    let mut delete_errors = Vec::new();

    for entry in plan.deletable {
        match op.delete(&entry.object_key).await {
            Ok(()) => deleted_chunks.push(entry.chunk_hash),
            Err(e) => delete_errors.push((entry.chunk_hash, e.to_string())),
        }
    }

    deleted_chunks.sort();
    delete_errors.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(OrphanedChunkCleanupReport {
        orphaned_chunks_found: plan.orphaned_chunks_found,
        deleted_chunks,
        skipped_within_grace: plan.skipped_within_grace,
        skipped_missing_last_modified: plan.skipped_missing_last_modified,
        delete_errors,
        referenced_chunks: scan.referenced_chunks.len(),
        scanned_chunks: scan.chunk_objects.len(),
    })
}

async fn scan_remote_chunks(op: &Operator, remote_prefix: &str) -> Result<RemoteChunkScan> {
    let prefix = remote_prefix.trim_end_matches('/');
    let manifest_prefix = format!("{prefix}/manifests/");
    let chunk_prefix = format!("{prefix}/chunks/");

    let manifest_entries = op
        .list_with(&manifest_prefix)
        .recursive(true)
        .await
        .context("listing remote manifests")?;

    let mut referenced_chunks = HashSet::new();
    for entry in manifest_entries {
        let key = entry.path();
        if key.ends_with('/') {
            continue;
        }

        match op.read(key).await {
            Ok(data) => match SyncManifest::from_bytes(&data.to_vec()) {
                Ok(manifest) => {
                    referenced_chunks.extend(manifest.chunks);
                }
                Err(e) => {
                    warn!(manifest = key, error = %e, "skipping unparseable manifest during orphan scan")
                }
            },
            Err(e) => {
                warn!(manifest = key, error = %e, "skipping unreadable manifest during orphan scan")
            }
        }
    }

    let chunk_entries = op
        .list_with(&chunk_prefix)
        .recursive(true)
        .await
        .context("listing remote chunks")?;

    let mut chunk_objects = Vec::new();
    for entry in chunk_entries {
        let key = entry.path();
        let chunk_hash = key.strip_prefix(&chunk_prefix).unwrap_or(key);
        if chunk_hash.is_empty() || chunk_hash.ends_with('/') {
            continue;
        }

        let last_modified = if referenced_chunks.contains(chunk_hash) {
            entry.metadata().last_modified().map(SystemTime::from)
        } else {
            match entry.metadata().last_modified() {
                Some(last_modified) => Some(SystemTime::from(last_modified)),
                None => chunk_last_modified(op, key).await,
            }
        };

        chunk_objects.push(RemoteChunkObject {
            object_key: key.to_string(),
            chunk_hash: chunk_hash.to_string(),
            last_modified,
        });
    }

    Ok(RemoteChunkScan {
        referenced_chunks,
        chunk_objects,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RemoteChunkScan {
    referenced_chunks: HashSet<String>,
    chunk_objects: Vec<RemoteChunkObject>,
}

async fn chunk_last_modified(op: &Operator, key: &str) -> Option<SystemTime> {
    op.stat(key)
        .await
        .ok()
        .and_then(|meta| meta.last_modified())
        .map(SystemTime::from)
}

fn plan_orphaned_chunk_cleanup(
    chunk_objects: &[RemoteChunkObject],
    referenced_chunks: &HashSet<String>,
    grace_period: Duration,
    now: SystemTime,
) -> PlannedOrphanCleanup {
    let mut plan = PlannedOrphanCleanup::default();

    for entry in chunk_objects {
        if referenced_chunks.contains(&entry.chunk_hash) {
            continue;
        }

        plan.orphaned_chunks_found += 1;
        match entry.last_modified {
            Some(last_modified) => match now.duration_since(last_modified) {
                Ok(age) if age >= grace_period => plan.deletable.push(entry.clone()),
                Ok(_) | Err(_) => plan.skipped_within_grace.push(entry.chunk_hash.clone()),
            },
            None => plan
                .skipped_missing_last_modified
                .push(entry.chunk_hash.clone()),
        }
    }

    plan.deletable
        .sort_by(|a, b| a.chunk_hash.cmp(&b.chunk_hash));
    plan.skipped_within_grace.sort();
    plan.skipped_missing_last_modified.sort();
    plan
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
                if let Err(e) =
                    engine::delete_remote_file(op, rel_path, remote_prefix, state, Some(local_root))
                        .await
                {
                    result
                        .errors
                        .push((rel_path.clone(), format!("remote delete failed: {e}")));
                } else {
                    result.deleted_remote += 1;
                    continue;
                }
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
            let rel_str = crate::engine::normalize_rel_path_text(&rel.to_string_lossy());
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
        .map(|rel| crate::engine::normalize_rel_path_text(&rel.to_string_lossy()))
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
            RemoteIndexEntry::new("aaa", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/index/file2.txt",
            RemoteIndexEntry::new("bbb", 200, 2).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/aaa",
            br#"{"version":2,"file_hash":"aaa","file_size":100,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/bbb",
            br#"{"version":2,"file_hash":"bbb","file_size":200,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        let index = list_remote_index(&op, "data").await.unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index["file1.txt"].manifest_hash, "aaa");
        assert_eq!(index["file2.txt"].manifest_hash, "bbb");
        assert_eq!(index["file2.txt"].size, 200);
    }

    #[tokio::test]
    async fn find_orphaned_chunks_empty_when_every_chunk_is_referenced() {
        let op = memory_op();
        let manifest = SyncManifest {
            version: 2,
            file_hash: "file-hash".into(),
            file_size: 11,
            chunks: vec!["chunk-a".into()],
            vclock: crate::conflict::VectorClock::new(),
            written_by: "neo".into(),
            written_at: 0,
            rel_path: Some("doc.txt".into()),
            mode: None,
            encrypted_file_key: None,
        };

        op.write("data/manifests/file-hash", manifest.to_bytes().unwrap())
            .await
            .unwrap();
        op.write("data/chunks/chunk-a", b"hello world".to_vec())
            .await
            .unwrap();

        let report = find_orphaned_chunks(&op, "data").await.unwrap();
        assert_eq!(report.referenced_chunks, 1);
        assert_eq!(report.scanned_chunks, 1);
        assert!(report.orphaned_chunks.is_empty());
    }

    #[tokio::test]
    async fn find_orphaned_chunks_reports_unreferenced_chunk_keys() {
        let op = memory_op();
        let manifest = SyncManifest {
            version: 2,
            file_hash: "file-hash".into(),
            file_size: 11,
            chunks: vec!["chunk-a".into()],
            vclock: crate::conflict::VectorClock::new(),
            written_by: "neo".into(),
            written_at: 0,
            rel_path: Some("doc.txt".into()),
            mode: None,
            encrypted_file_key: None,
        };

        op.write("data/manifests/file-hash", manifest.to_bytes().unwrap())
            .await
            .unwrap();
        op.write("data/chunks/chunk-a", b"hello world".to_vec())
            .await
            .unwrap();
        op.write("data/chunks/chunk-orphan", b"left behind".to_vec())
            .await
            .unwrap();

        let report = find_orphaned_chunks(&op, "data").await.unwrap();
        assert_eq!(report.referenced_chunks, 1);
        assert_eq!(report.scanned_chunks, 2);
        assert_eq!(report.orphaned_chunks, vec!["chunk-orphan".to_string()]);
    }

    #[test]
    fn plan_orphaned_chunk_cleanup_respects_grace_period() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let referenced_chunks = std::collections::HashSet::from(["chunk-ref".to_string()]);
        let chunk_objects = vec![
            RemoteChunkObject {
                object_key: "data/chunks/chunk-ref".into(),
                chunk_hash: "chunk-ref".into(),
                last_modified: Some(now - Duration::from_secs(7_200)),
            },
            RemoteChunkObject {
                object_key: "data/chunks/chunk-old".into(),
                chunk_hash: "chunk-old".into(),
                last_modified: Some(now - Duration::from_secs(7_200)),
            },
            RemoteChunkObject {
                object_key: "data/chunks/chunk-fresh".into(),
                chunk_hash: "chunk-fresh".into(),
                last_modified: Some(now - Duration::from_secs(30)),
            },
            RemoteChunkObject {
                object_key: "data/chunks/chunk-future".into(),
                chunk_hash: "chunk-future".into(),
                last_modified: Some(now + Duration::from_secs(30)),
            },
            RemoteChunkObject {
                object_key: "data/chunks/chunk-unknown".into(),
                chunk_hash: "chunk-unknown".into(),
                last_modified: None,
            },
        ];

        let plan = plan_orphaned_chunk_cleanup(
            &chunk_objects,
            &referenced_chunks,
            Duration::from_secs(3_600),
            now,
        );

        assert_eq!(plan.orphaned_chunks_found, 4);
        assert_eq!(
            plan.deletable
                .iter()
                .map(|entry| entry.chunk_hash.as_str())
                .collect::<Vec<_>>(),
            vec!["chunk-old"]
        );
        assert_eq!(
            plan.skipped_within_grace,
            vec!["chunk-fresh".to_string(), "chunk-future".to_string()]
        );
        assert_eq!(
            plan.skipped_missing_last_modified,
            vec!["chunk-unknown".to_string()]
        );
    }

    #[tokio::test]
    async fn cleanup_orphaned_chunks_skips_missing_last_modified() {
        let op = memory_op();
        let manifest = SyncManifest {
            version: 2,
            file_hash: "file-hash".into(),
            file_size: 11,
            chunks: vec!["chunk-a".into()],
            vclock: crate::conflict::VectorClock::new(),
            written_by: "neo".into(),
            written_at: 0,
            rel_path: Some("doc.txt".into()),
            mode: None,
            encrypted_file_key: None,
        };

        op.write("data/manifests/file-hash", manifest.to_bytes().unwrap())
            .await
            .unwrap();
        op.write("data/chunks/chunk-a", b"hello world".to_vec())
            .await
            .unwrap();
        op.write("data/chunks/chunk-orphan", b"left behind".to_vec())
            .await
            .unwrap();

        let report = cleanup_orphaned_chunks(&op, "data", Duration::ZERO, SystemTime::now())
            .await
            .unwrap();

        assert_eq!(report.orphaned_chunks_found, 1);
        assert!(report.deleted_chunks.is_empty());
        assert!(report.skipped_within_grace.is_empty());
        assert_eq!(
            report.skipped_missing_last_modified,
            vec!["chunk-orphan".to_string()]
        );
        assert!(report.delete_errors.is_empty());
        assert!(op.read("data/chunks/chunk-orphan").await.is_ok());
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
            RemoteIndexEntry::new("abc123", 50, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/abc123",
            br#"{"version":2,"file_hash":"abc123","file_size":50,"chunks":[],"vclock":{"clocks":{"neo":1}},"written_by":"neo","written_at":0}"#.to_vec(),
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
        let index_entry =
            RemoteIndexEntry::new(hash.clone(), content.len() as u64, 1).to_legacy_bytes();
        op.write("data/index/same.txt", index_entry).await.unwrap();

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

    #[tokio::test]
    async fn reconcile_unicode_variants_round_trip_as_up_to_date() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();

        let content = b"matching unicode content";
        let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
        let local_file = local_root.join("cafe\u{301}.txt");
        std::fs::write(&local_file, content).unwrap();

        let index_entry =
            RemoteIndexEntry::new(hash.clone(), content.len() as u64, 1).to_legacy_bytes();
        op.write("data/index/caf\u{e9}.txt", index_entry)
            .await
            .unwrap();

        let manifest_json = serde_json::json!({
            "version": 2,
            "file_hash": hash,
            "file_size": content.len(),
            "chunks": [],
            "vclock": {"clocks": {"neo": 1}},
            "written_by": "neo",
            "written_at": 0,
            "rel_path": "caf\u{e9}.txt"
        });
        op.write(
            &format!("data/manifests/{hash}"),
            serde_json::to_vec(&manifest_json).unwrap(),
        )
        .await
        .unwrap();

        let state_path = dir.path().join("state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
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

        assert_eq!(plan.summary.up_to_date, 1);
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
