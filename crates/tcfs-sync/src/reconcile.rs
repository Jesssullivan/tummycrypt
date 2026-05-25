//! Directory reconciliation pipeline — plan-then-execute bidirectional sync.
//!
//! `reconcile()` diffs a local directory tree against the remote index and
//! produces a `ReconcilePlan` (pure data, no side effects). `execute_plan()`
//! then performs the actual I/O using existing engine primitives.
//!
//! This separation enables dry-run mode, TUI preview, and deterministic testing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use opendal::Operator;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::blacklist::Blacklist;
use crate::conflict::{compare_clocks, ConflictInfo};
use crate::engine::{self, OptionalEncryption, ProgressFn};
use crate::index_entry::{
    manifest_key, parse_index_entry_record, resolve_visible_index_entry, RemoteIndexEntry,
};
use crate::manifest::SyncManifest;
use crate::state::{StateCache, StateCacheBackend, SyncState};

const REMOTE_INDEX_READ_CONCURRENCY: usize = 32;
const REMOTE_PULL_CONCURRENCY: usize = 16;
const DIR_MARKER: &str = ".tcfs_dir";
const DIR_MARKER_SUFFIX: &str = "/.tcfs_dir";

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
    CreateDirectory {
        rel_path: String,
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
    pub directories: usize,
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
    pub directories_created: usize,
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
    let manifest_prefix = format!("{}/manifests/", remote_prefix.trim_end_matches('/'));
    let entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .context("listing remote index")?;

    let manifest_keys = Arc::new(list_remote_manifest_keys(op, &manifest_prefix).await?);
    let read_permits = Arc::new(Semaphore::new(REMOTE_INDEX_READ_CONCURRENCY));
    let mut tasks = JoinSet::new();

    for entry in entries {
        let full_key = entry.path().to_string();
        let rel_path = crate::engine::normalize_rel_path_text(
            full_key.strip_prefix(&index_prefix).unwrap_or(&full_key),
        );

        // Skip directory markers and empty paths.
        if rel_path.is_empty()
            || rel_path.ends_with('/')
            || rel_path == DIR_MARKER
            || rel_path.ends_with("/.tcfs_dir")
        {
            continue;
        }

        let op = op.clone();
        let manifest_prefix = manifest_prefix.clone();
        let manifest_keys = Arc::clone(&manifest_keys);
        let read_permits = Arc::clone(&read_permits);
        tasks.spawn(async move {
            let _permit = read_permits
                .acquire_owned()
                .await
                .context("acquiring remote index read permit")?;
            let visible =
                read_visible_remote_index_entry(&op, &full_key, &manifest_prefix, &manifest_keys)
                    .await
                    .with_context(|| format!("reading remote index entry: {full_key}"))?;
            Ok::<_, anyhow::Error>((rel_path, full_key, visible))
        });
    }

    let mut result = HashMap::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok((rel_path, _full_key, Some(visible)))) => {
                result.insert(rel_path, visible);
            }
            Ok(Ok((_rel_path, full_key, None))) => {
                debug!(key = full_key, "skipping non-visible index entry");
            }
            Ok(Err(e)) => {
                warn!(error = %e, "skipping unreadable index entry");
            }
            Err(e) => {
                warn!(error = %e, "remote index entry task failed");
            }
        }
    }

    debug!(count = result.len(), "fetched remote index");
    Ok(result)
}

async fn list_remote_empty_dirs(op: &Operator, remote_prefix: &str) -> Result<HashSet<String>> {
    let index_prefix = format!("{}/index/", remote_prefix.trim_end_matches('/'));
    let entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .context("listing remote directory markers")?;

    let mut result = HashSet::new();
    for entry in entries {
        let full_key = entry.path();
        let rel_path = crate::engine::normalize_rel_path_text(
            full_key.strip_prefix(&index_prefix).unwrap_or(full_key),
        );

        let Some(dir_path) = rel_path.strip_suffix(DIR_MARKER_SUFFIX) else {
            continue;
        };
        if dir_path.is_empty() {
            continue;
        }
        result.insert(dir_path.to_string());
    }

    debug!(
        count = result.len(),
        "fetched remote empty directory markers"
    );
    Ok(result)
}

async fn list_remote_manifest_keys(
    op: &Operator,
    manifest_prefix: &str,
) -> Result<HashSet<String>> {
    let entries = op
        .list_with(manifest_prefix)
        .recursive(true)
        .await
        .context("listing remote manifests")?;

    let mut result = HashSet::new();
    for entry in entries {
        let key = entry.path();
        if key.is_empty() || key.ends_with('/') {
            continue;
        }
        result.insert(key.to_string());
    }

    debug!(count = result.len(), "fetched remote manifest keys");
    Ok(result)
}

async fn read_visible_remote_index_entry(
    op: &Operator,
    index_key: &str,
    manifest_prefix: &str,
    manifest_keys: &HashSet<String>,
) -> Result<Option<RemoteIndexEntry>> {
    let bytes = op
        .read(index_key)
        .await
        .with_context(|| format!("reading index entry: {index_key}"))?;
    let parsed = parse_index_entry_record(&bytes.to_vec())
        .with_context(|| format!("parsing index entry: {index_key}"))?;

    if parsed.pending_entry().is_some() {
        return resolve_visible_index_entry(op, index_key, manifest_prefix).await;
    }

    if let Some(current) = parsed.visible_entry() {
        let current_manifest_key = manifest_key(manifest_prefix, &current.manifest_hash);
        if manifest_keys.contains(&current_manifest_key) {
            return Ok(Some(current.clone()));
        }

        anyhow::bail!("index entry points to missing manifest: {current_manifest_key}");
    }

    Ok(None)
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
    let remote_empty_dirs = list_remote_empty_dirs(op, remote_prefix).await?;
    debug!(
        count = remote_empty_dirs.len(),
        "fetched remote empty directory markers"
    );

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
            ReconcileAction::CreateDirectory { .. } => summary.directories += 1,
            ReconcileAction::UpToDate { .. } => summary.up_to_date += 1,
        }

        actions.push(action);
    }

    let mut remote_empty_dirs = remote_empty_dirs.into_iter().collect::<Vec<_>>();
    remote_empty_dirs.sort();
    for rel_path in &remote_empty_dirs {
        if local_root.join(rel_path).is_dir() {
            continue;
        }
        summary.directories += 1;
        actions.push(ReconcileAction::CreateDirectory {
            rel_path: rel_path.clone(),
        });
    }

    // 5. Sort: conflicts first, then pulls, pushes, deletes, up-to-date last
    actions.sort_by_key(|a| match a {
        ReconcileAction::Conflict { .. } => 0,
        ReconcileAction::CreateDirectory { .. } => 1,
        ReconcileAction::Pull { .. } => 2,
        ReconcileAction::Push { .. } => 3,
        ReconcileAction::DeleteLocal { .. } => 4,
        ReconcileAction::DeleteRemote { .. } => 5,
        ReconcileAction::UpToDate { .. } => 6,
    });

    info!(
        pushes = summary.pushes,
        pulls = summary.pulls,
        directories = summary.directories,
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
    if encryption.is_none()
        && progress.is_none()
        && plan.actions.iter().all(|action| {
            matches!(
                action,
                ReconcileAction::Pull {
                    reason: PullReason::NewRemote,
                    ..
                } | ReconcileAction::CreateDirectory { .. }
                    | ReconcileAction::UpToDate { .. }
            )
        })
        && plan.actions.iter().any(|action| {
            matches!(
                action,
                ReconcileAction::Pull {
                    reason: PullReason::NewRemote,
                    ..
                }
            )
        })
    {
        let result = execute_new_remote_pulls_concurrent(
            plan,
            op,
            local_root,
            remote_prefix,
            state,
            device_id,
        )
        .await?;
        if let Err(e) = state.flush() {
            warn!(error = %e, "failed to flush state cache after concurrent pull execution");
        }
        info!(
            pulled = result.pulled,
            errors = result.errors.len(),
            "concurrent pull execution complete"
        );
        return Ok(result);
    }

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
                        .push((rel_path.clone(), format!("push failed: {e:#}")));
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
                            .push((rel_path.clone(), format!("pull failed: {e:#}")));
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

            ReconcileAction::CreateDirectory { rel_path } => {
                let local_path = local_root.join(rel_path);
                match std::fs::create_dir_all(&local_path) {
                    Ok(()) => {
                        result.directories_created += 1;
                    }
                    Err(e) => {
                        result
                            .errors
                            .push((rel_path.clone(), format!("create directory failed: {e}")));
                    }
                }
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

async fn execute_new_remote_pulls_concurrent(
    plan: &ReconcilePlan,
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    device_id: &str,
) -> Result<ExecutionResult> {
    let read_permits = Arc::new(Semaphore::new(REMOTE_PULL_CONCURRENCY));
    let mut tasks = JoinSet::new();
    let mut result = ExecutionResult::default();

    for action in &plan.actions {
        if let ReconcileAction::CreateDirectory { rel_path } = action {
            let local_path = local_root.join(rel_path);
            match std::fs::create_dir_all(&local_path) {
                Ok(()) => {
                    result.directories_created += 1;
                }
                Err(e) => {
                    result
                        .errors
                        .push((rel_path.clone(), format!("create directory failed: {e}")));
                }
            }
            continue;
        }

        let ReconcileAction::Pull {
            rel_path,
            manifest_hash,
            reason: PullReason::NewRemote,
            ..
        } = action
        else {
            continue;
        };

        let op = op.clone();
        let rel_path = rel_path.clone();
        let local_path = local_root.join(&rel_path);
        let remote_prefix = remote_prefix.to_string();
        let manifest_path = format!(
            "{}/manifests/{}",
            remote_prefix.trim_end_matches('/'),
            manifest_hash
        );
        let device_id = device_id.to_string();
        let read_permits = Arc::clone(&read_permits);

        tasks.spawn(async move {
            let pull_result = async {
                let _permit = read_permits
                    .acquire_owned()
                    .await
                    .context("acquiring remote pull permit")?;

                if let Some(parent) = local_path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .with_context(|| format!("creating dir: {}", parent.display()))?;
                }

                engine::download_file_with_device(
                    &op,
                    &manifest_path,
                    &local_path,
                    &remote_prefix,
                    None,
                    &device_id,
                    None,
                    None,
                )
                .await
                .with_context(|| format!("pull failed: {rel_path}"))
            }
            .await;

            (rel_path, pull_result)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((_, Ok(download))) => {
                result.pulled += 1;
                result.bytes_downloaded += download.bytes;
                if let Some(sync_state) = download.sync_state {
                    state.set(&download.local_path, sync_state);
                }
            }
            Ok((rel_path, Err(e))) => {
                result.errors.push((rel_path, format!("{e:#}")));
            }
            Err(e) => {
                result
                    .errors
                    .push(("<concurrent-pull-task>".into(), format!("{e:#}")));
            }
        }
    }

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
        preserve_symlinks: false,
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
    async fn list_remote_index_skips_directory_markers() {
        let op = memory_op();
        op.write("data/index/empty/.tcfs_dir", b"directory".to_vec())
            .await
            .unwrap();
        op.write(
            "data/index/file.txt",
            RemoteIndexEntry::new("aaa", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/aaa",
            br#"{"version":2,"file_hash":"aaa","file_size":100,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        let index = list_remote_index(&op, "data").await.unwrap();
        assert_eq!(index.len(), 1);
        assert!(index.contains_key("file.txt"));
        assert!(!index.contains_key("empty/.tcfs_dir"));
    }

    #[tokio::test]
    async fn list_remote_empty_dirs_finds_directory_markers() {
        let op = memory_op();
        op.write("data/index/empty/.tcfs_dir", b"type=directory\n".to_vec())
            .await
            .unwrap();
        op.write(
            "data/index/nested/also-empty/.tcfs_dir",
            b"type=directory\n".to_vec(),
        )
        .await
        .unwrap();
        op.write(
            "data/index/file.txt",
            RemoteIndexEntry::new("aaa", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();

        let dirs = list_remote_empty_dirs(&op, "data").await.unwrap();
        assert_eq!(dirs.len(), 2);
        assert!(dirs.contains("empty"));
        assert!(dirs.contains("nested/also-empty"));
        assert!(!dirs.contains("file.txt"));
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
            wrapped_file_keys: Vec::new(),
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
            wrapped_file_keys: Vec::new(),
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
            wrapped_file_keys: Vec::new(),
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

    #[tokio::test]
    async fn reconcile_remote_only_directory_marker_generates_create_directory() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();

        op.write("data/index/empty/.tcfs_dir", b"type=directory\n".to_vec())
            .await
            .unwrap();

        let state_path = dir.path().join("state.json");
        let state = crate::state::StateCache::open(&state_path).unwrap();
        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(&op, local_root, "data", &state, "neo", &blacklist, &config)
            .await
            .unwrap();

        assert_eq!(plan.summary.directories, 1);
        assert!(
            plan.actions.iter().any(
                |a| matches!(a, ReconcileAction::CreateDirectory { rel_path } if rel_path == "empty")
            ),
            "create-dir action should target empty/"
        );
    }

    #[tokio::test]
    async fn execute_plan_new_remote_pulls_restore_files_and_state() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let source_root = dir.path().join("source");
        let restore_root = dir.path().join("restore");
        std::fs::create_dir_all(source_root.join("nested")).unwrap();
        std::fs::create_dir_all(&restore_root).unwrap();

        let alpha = source_root.join("alpha.txt");
        let beta = source_root.join("nested/beta.txt");
        std::fs::write(&alpha, b"alpha from neo").unwrap();
        std::fs::write(&beta, b"beta from neo").unwrap();

        let mut source_state =
            crate::state::StateCache::open(&dir.path().join("source-state.json")).unwrap();
        crate::engine::upload_file_with_device(
            &op,
            &alpha,
            "data",
            &mut source_state,
            None,
            "neo",
            Some("alpha.txt"),
            None,
        )
        .await
        .unwrap();
        crate::engine::upload_file_with_device(
            &op,
            &beta,
            "data",
            &mut source_state,
            None,
            "neo",
            Some("nested/beta.txt"),
            None,
        )
        .await
        .unwrap();

        let mut restore_state =
            crate::state::StateCache::open(&dir.path().join("restore-state.json")).unwrap();
        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(
            &op,
            &restore_root,
            "data",
            &restore_state,
            "honey",
            &blacklist,
            &config,
        )
        .await
        .unwrap();

        assert_eq!(plan.summary.pulls, 2);
        assert!(plan.actions.iter().all(|action| {
            matches!(
                action,
                ReconcileAction::Pull {
                    reason: PullReason::NewRemote,
                    ..
                }
            )
        }));

        let result = execute_plan(
            &plan,
            &op,
            &restore_root,
            "data",
            &mut restore_state,
            "honey",
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.pulled, 2);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            std::fs::read(restore_root.join("alpha.txt")).unwrap(),
            b"alpha from neo"
        );
        assert_eq!(
            std::fs::read(restore_root.join("nested/beta.txt")).unwrap(),
            b"beta from neo"
        );

        let alpha_state = restore_state.get(&restore_root.join("alpha.txt")).unwrap();
        let beta_state = restore_state
            .get(&restore_root.join("nested/beta.txt"))
            .unwrap();
        assert_eq!(alpha_state.status, crate::state::FileSyncStatus::Synced);
        assert_eq!(beta_state.status, crate::state::FileSyncStatus::Synced);
        assert_eq!(alpha_state.device_id, "honey");
        assert_eq!(beta_state.device_id, "honey");
    }

    #[tokio::test]
    async fn execute_plan_restores_remote_empty_directories() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let restore_root = dir.path().join("restore");
        std::fs::create_dir_all(&restore_root).unwrap();

        op.write("data/index/empty/.tcfs_dir", b"type=directory\n".to_vec())
            .await
            .unwrap();
        op.write(
            "data/index/nested/also-empty/.tcfs_dir",
            b"type=directory\n".to_vec(),
        )
        .await
        .unwrap();

        let mut restore_state =
            crate::state::StateCache::open(&dir.path().join("restore-state.json")).unwrap();
        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(
            &op,
            &restore_root,
            "data",
            &restore_state,
            "honey",
            &blacklist,
            &config,
        )
        .await
        .unwrap();

        assert_eq!(plan.summary.directories, 2);
        let result = execute_plan(
            &plan,
            &op,
            &restore_root,
            "data",
            &mut restore_state,
            "honey",
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.directories_created, 2);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(restore_root.join("empty").is_dir());
        assert!(restore_root.join("nested/also-empty").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_plan_new_remote_pull_restores_symlink_state() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let source_root = dir.path().join("source");
        let restore_root = dir.path().join("restore");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&restore_root).unwrap();

        let target = source_root.join("target.txt");
        let link = source_root.join("link.txt");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink("target.txt", &link).unwrap();

        let mut source_state =
            crate::state::StateCache::open(&dir.path().join("source-state.json")).unwrap();
        crate::engine::upload_symlink_with_device(
            &op,
            &link,
            "data",
            &mut source_state,
            "neo",
            "link.txt",
        )
        .await
        .unwrap();

        let mut restore_state =
            crate::state::StateCache::open(&dir.path().join("restore-state.json")).unwrap();
        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(
            &op,
            &restore_root,
            "data",
            &restore_state,
            "honey",
            &blacklist,
            &config,
        )
        .await
        .unwrap();

        assert_eq!(plan.summary.pulls, 1);

        let result = execute_plan(
            &plan,
            &op,
            &restore_root,
            "data",
            &mut restore_state,
            "honey",
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.pulled, 1);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            std::fs::read_link(restore_root.join("link.txt")).unwrap(),
            PathBuf::from("target.txt")
        );

        let link_state = restore_state.get(&restore_root.join("link.txt")).unwrap();
        assert_eq!(link_state.status, crate::state::FileSyncStatus::Synced);
        assert_eq!(link_state.device_id, "honey");
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
