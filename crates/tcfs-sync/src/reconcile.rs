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
use crate::git_safety;
use crate::index_entry::{
    manifest_key, parse_index_entry_record, resolve_visible_index_entry, RemoteIndexEntry,
};
use crate::manifest::{SymlinkManifest, SyncManifest};
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
    /// Enable `.git`-aware fast-forward conflict resolution. When set (and
    /// `git_sync_mode` is `"raw"`), a post-classification pass reclassifies a
    /// repo's conflicting `.git/*` paths to Push/Pull when the local and remote
    /// branch tips are in a strict fast-forward (ancestor) relationship. Any
    /// `.git` conflict that is NOT a clean fast-forward is left as a `Conflict`
    /// (fail-closed). See `reclassify_git_ff_conflicts`.
    pub git_ff_resolution: bool,
    /// The git sync mode this reconcile is operating under (`"bundle"` or
    /// `"raw"`). The FF reclassifier only engages in `"raw"` mode, where the
    /// raw `.git/*` internals roam as ordinary files.
    pub git_sync_mode: String,
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
            || rel_path.ends_with(DIR_MARKER_SUFFIX)
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
#[allow(clippy::too_many_arguments)]
pub async fn reconcile(
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &StateCache,
    device_id: &str,
    blacklist: &Blacklist,
    config: &ReconcileConfig,
    encryption: OptionalEncryption<'_>,
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

    // 4b. `.git`-aware fast-forward reclassification. In raw git-sync mode, a
    // repo whose `.git/*` paths all conflict purely because each device ticked
    // its own clock can still be a clean fast-forward (one tip is an ancestor of
    // the other). Reclassify those conflicts to Push/Pull atomically per repo;
    // divergent / indeterminate repos are left untouched (fail-closed). The
    // summary is recomputed afterward so counts reflect the reclassification.
    if config.git_ff_resolution && config.git_sync_mode == "raw" {
        reclassify_git_ff_conflicts(
            &mut actions,
            local_root,
            op,
            remote_prefix,
            &remote_index,
            encryption,
        )
        .await;
        summary = recompute_summary(&actions);
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

    // 5. Sort: conflicts first, then pulls, pushes, deletes, up-to-date last.
    //    Within each kind, order `.git` paths so objects/packs apply before
    //    refs/packed-refs/HEAD (a ref must never advance to an object not yet
    //    present locally — that would corrupt the repo). `git_apply_rank` is 0
    //    for objects (and all non-`.git` paths) and 1 for refs, so the stable
    //    sort keeps objects ahead of refs in both the pull and push buckets.
    actions.sort_by(|a, b| {
        kind_rank(a)
            .cmp(&kind_rank(b))
            .then_with(|| git_apply_rank(a).cmp(&git_apply_rank(b)))
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

/// Coarse ordering rank by action kind for the plan sort.
fn kind_rank(a: &ReconcileAction) -> u8 {
    match a {
        ReconcileAction::Conflict { .. } => 0,
        ReconcileAction::CreateDirectory { .. } => 1,
        ReconcileAction::Pull { .. } => 2,
        ReconcileAction::Push { .. } => 3,
        ReconcileAction::DeleteLocal { .. } => 4,
        ReconcileAction::DeleteRemote { .. } => 5,
        ReconcileAction::UpToDate { .. } => 6,
    }
}

/// Intra-kind ordering rank ensuring `.git` objects/packs apply before refs.
///
/// Returns 0 for object/pack paths and every non-`.git` path, and 1 for the
/// ref-class paths (`.git/refs/**`, `packed-refs`, `HEAD`). A stable sort with
/// this as a secondary key keeps objects ahead of refs so a ref never advances
/// to an object that has not yet been written locally.
fn git_apply_rank(a: &ReconcileAction) -> u8 {
    let rel = match a {
        ReconcileAction::Pull { rel_path, .. } => rel_path.as_str(),
        ReconcileAction::Push { rel_path, .. } => rel_path.as_str(),
        _ => return 0,
    };
    if is_git_ref_class_path(rel) {
        1
    } else {
        0
    }
}

/// True for the `.git` paths that publish a ref and therefore must be applied
/// AFTER objects: `.git/refs/**`, `.git/packed-refs`, and `.git/HEAD`.
fn is_git_ref_class_path(rel: &str) -> bool {
    if !is_git_internal_path(rel) {
        return false;
    }
    rel.contains(".git/refs/") || rel.ends_with(".git/packed-refs") || rel.ends_with(".git/HEAD")
}

/// Acquire cooperative `.git/tcfs.lock` guards for every repo that has a
/// `.git/*` Push or Pull action in `plan`. Returns the held guards; dropping the
/// returned vec releases all locks. Repos whose `.git` is mid-operation or whose
/// lock is already held are skipped (logged), so a busy repo does not abort the
/// whole plan — it simply re-reconciles next cycle.
fn acquire_git_locks_for_plan(
    plan: &ReconcilePlan,
    local_root: &Path,
) -> Vec<git_safety::GitLockGuard> {
    use std::collections::BTreeSet;

    let mut repos: BTreeSet<PathBuf> = BTreeSet::new();
    for action in &plan.actions {
        let rel = match action {
            ReconcileAction::Push { rel_path, .. } => rel_path.as_str(),
            ReconcileAction::Pull { rel_path, .. } => rel_path.as_str(),
            _ => continue,
        };
        if !is_git_internal_path(rel) {
            continue;
        }
        if let Some(root) = git_safety::repo_root_for_git_path(local_root, rel) {
            repos.insert(root);
        }
    }

    let mut guards = Vec::new();
    for repo_root in repos {
        let git_dir = repo_root.join(".git");
        if !git_dir.is_dir() {
            continue;
        }
        let safety = git_safety::git_is_safe(&git_dir);
        if !safety.blocking.is_empty() {
            warn!(
                repo = %repo_root.display(),
                blocking = ?safety.blocking,
                "git ff: repo busy, skipping tcfs.lock acquire this cycle"
            );
            continue;
        }
        match git_safety::acquire_git_lock(&git_dir) {
            Ok(guard) => guards.push(guard),
            Err(e) => {
                warn!(
                    repo = %repo_root.display(),
                    error = %format!("{e:#}"),
                    "git ff: could not acquire tcfs.lock; proceeding without lock"
                );
            }
        }
    }
    guards
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
    // Symlinks are first-class entries: they must NOT be dereferenced and hashed
    // like regular files (that would hash the *target's* content and then fail to
    // parse the stored SymlinkManifest as a SyncManifest, re-pushing every cycle).
    // Detect a local symlink with symlink_metadata (does not follow the link) and
    // compare on symlink identity instead. Mirrors the push path
    // (`upload_symlink_with_device`) and `collect_local_set` (preserve_symlinks).
    let local_is_symlink = std::fs::symlink_metadata(local_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if local_is_symlink {
        return compare_both_exist_symlink(
            rel_path,
            local_path,
            remote_entry,
            tracked,
            op,
            remote_prefix,
            device_id,
        )
        .await;
    }

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

    outcome_to_action(outcome, rel_path, local_path, remote_entry)
}

/// Map a `SyncOutcome` to the corresponding `ReconcileAction`.
///
/// Shared tail for `compare_both_exist` and `compare_both_exist_symlink`: both
/// resolve the same vector-clock decision into the same push/pull/conflict
/// action, so the mapping lives here to keep the two sites in lockstep.
fn outcome_to_action(
    outcome: crate::conflict::SyncOutcome,
    rel_path: &str,
    local_path: &Path,
    remote_entry: &RemoteIndexEntry,
) -> ReconcileAction {
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

/// Recompute the plan summary from a finalized action list.
///
/// Used after the `.git` fast-forward reclassification mutates conflict actions
/// into Push/Pull, so the reported counts stay consistent with the actions.
fn recompute_summary(actions: &[ReconcileAction]) -> ReconcileSummary {
    let mut summary = ReconcileSummary::default();
    for action in actions {
        match action {
            ReconcileAction::Push { .. } => summary.pushes += 1,
            ReconcileAction::Pull { .. } => summary.pulls += 1,
            ReconcileAction::DeleteLocal { .. } => summary.local_deletes += 1,
            ReconcileAction::DeleteRemote { .. } => summary.remote_deletes += 1,
            ReconcileAction::Conflict { .. } => summary.conflicts += 1,
            ReconcileAction::CreateDirectory { .. } => summary.directories += 1,
            ReconcileAction::UpToDate { .. } => summary.up_to_date += 1,
        }
    }
    summary
}

/// `.git`-aware fast-forward conflict reclassification (raw git-sync mode only).
///
/// Walks the action list for `Conflict` actions whose path is under a repo's
/// `.git/` directory, groups them by enclosing repo root, and for each repo with
/// a conflicting branch-head ref determines whether the local and remote tips
/// are in a strict fast-forward relationship:
///
/// * remote tip is an ancestor of local tip (local strictly ahead) → the repo's
///   `.git/*` conflicts are reclassified to `Push` (LocalNewer);
/// * local tip is an ancestor of remote tip (remote strictly ahead) → they are
///   reclassified to `Pull` (RemoteNewer).
///
/// Anything else — divergent tips, equal-but-different content, an unresolvable
/// remote ref, or a missing object needed for the ancestry probe — leaves the
/// repo's conflicts untouched (fail-closed: stays `Conflict`). The whole repo is
/// moved toward a single winner; a repo is never split half push / half pull.
#[allow(clippy::too_many_arguments)]
async fn reclassify_git_ff_conflicts(
    actions: &mut [ReconcileAction],
    local_root: &Path,
    op: &Operator,
    remote_prefix: &str,
    remote_index: &HashMap<String, RemoteIndexEntry>,
    encryption: OptionalEncryption<'_>,
) {
    use std::collections::BTreeMap;

    // Group conflicting `.git/*` action indices by enclosing repo root.
    let mut by_repo: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (idx, action) in actions.iter().enumerate() {
        let ReconcileAction::Conflict { rel_path, .. } = action else {
            continue;
        };
        if !is_git_internal_path(rel_path) {
            continue;
        }
        let Some(repo_root) = git_safety::repo_root_for_git_path(local_root, rel_path) else {
            continue;
        };
        by_repo.entry(repo_root).or_default().push(idx);
    }

    for (repo_root, indices) in by_repo {
        let decision = decide_repo_fast_forward(
            &repo_root,
            local_root,
            &indices,
            actions,
            op,
            remote_prefix,
            remote_index,
            encryption,
        )
        .await;
        let Some(direction) = decision else {
            // Indeterminate / divergent: leave every conflict as-is.
            continue;
        };
        // Apply atomically: rewrite ALL of this repo's `.git/*` conflicts toward
        // the single winning direction.
        for &idx in &indices {
            let ReconcileAction::Conflict { rel_path, .. } = &actions[idx] else {
                continue;
            };
            let rel_path = rel_path.clone();
            actions[idx] = match direction {
                git_safety::FastForward::LocalAhead => ReconcileAction::Push {
                    local_path: local_root.join(&rel_path),
                    rel_path: rel_path.clone(),
                    reason: PushReason::LocalNewer,
                },
                git_safety::FastForward::RemoteAhead => {
                    // The remote index entry for this exact path carries the
                    // manifest hash + size needed to pull it.
                    match remote_index.get(&rel_path) {
                        Some(entry) => ReconcileAction::Pull {
                            rel_path: rel_path.clone(),
                            manifest_hash: entry.manifest_hash.clone(),
                            size: entry.size,
                            reason: PullReason::RemoteNewer,
                        },
                        // No remote entry: fail closed, keep the conflict.
                        None => continue,
                    }
                }
                git_safety::FastForward::NotFastForward => continue,
            };
        }
        match direction {
            git_safety::FastForward::LocalAhead => info!(
                repo = %repo_root.display(),
                "git fast-forward: local ahead, pushing .git"
            ),
            git_safety::FastForward::RemoteAhead => info!(
                repo = %repo_root.display(),
                "git fast-forward: remote ahead, pulling .git"
            ),
            git_safety::FastForward::NotFastForward => {}
        }
    }
}

/// Decide the fast-forward direction for one repo's conflicting `.git/*` paths.
///
/// Finds a conflicting branch-head ref (`.git/refs/heads/<branch>`) among the
/// repo's conflicts, resolves the local tip (from the live repo) and the remote
/// tip (from the remote ref blob), and probes ancestry with both objects local.
/// Returns `None` when no branch-head ref is among the conflicts, when either
/// tip is unresolvable, or when the relationship is not a clean fast-forward.
#[allow(clippy::too_many_arguments)]
async fn decide_repo_fast_forward(
    repo_root: &Path,
    local_root: &Path,
    indices: &[usize],
    actions: &[ReconcileAction],
    op: &Operator,
    remote_prefix: &str,
    remote_index: &HashMap<String, RemoteIndexEntry>,
    encryption: OptionalEncryption<'_>,
) -> Option<git_safety::FastForward> {
    // Find the branch-head ref path(s) among this repo's conflicts. There may be
    // several (multiple branches advanced); a clean fast-forward requires EVERY
    // conflicting head ref to agree on the same direction, otherwise fail closed.
    let mut direction: Option<git_safety::FastForward> = None;
    let mut saw_ref = false;

    for &idx in indices {
        let ReconcileAction::Conflict { rel_path, .. } = &actions[idx] else {
            continue;
        };
        let Some(ref_name) = git_safety::head_ref_for_git_path(rel_path) else {
            continue;
        };
        saw_ref = true;

        // Local tip from the live repo (loose refs + packed-refs).
        let Some(local_sha) = git_safety::local_ref_sha(repo_root, &ref_name) else {
            return None; // unresolvable local ref → fail closed
        };

        // Remote tip: read the remote ref blob content for this exact path.
        let Some(remote_sha) = read_remote_ref_sha(
            op,
            remote_prefix,
            rel_path,
            local_root,
            remote_index,
            encryption,
        )
        .await
        else {
            return None; // unresolvable / missing remote ref → fail closed
        };

        let ff = git_safety::classify_fast_forward(repo_root, &local_sha, &remote_sha);
        if ff == git_safety::FastForward::NotFastForward {
            return None; // divergent / indeterminate → fail closed
        }
        match direction {
            None => direction = Some(ff),
            Some(prev) if prev == ff => {}
            Some(_) => return None, // refs disagree on direction → fail closed
        }
    }

    if !saw_ref {
        // Conflicts under `.git` but no branch-head ref among them (e.g. only
        // index/logs). Without a ref tip to compare we cannot prove a clean FF,
        // so stay conflicted.
        return None;
    }
    direction
}

/// Read the remote commit SHA stored at a `.git/refs/heads/<branch>` path by
/// downloading the (tiny) ref blob from the remote and parsing its content.
///
/// Returns `None` if the remote has no entry for this path, the download fails,
/// or the content is not a concrete SHA (e.g. a symbolic ref).
#[allow(clippy::too_many_arguments)]
async fn read_remote_ref_sha(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    local_root: &Path,
    remote_index: &HashMap<String, RemoteIndexEntry>,
    encryption: OptionalEncryption<'_>,
) -> Option<String> {
    let entry = remote_index.get(rel_path)?;
    let manifest_path = format!(
        "{}/manifests/{}",
        remote_prefix.trim_end_matches('/'),
        &entry.manifest_hash
    );
    // Download into a unique temp file, then read + parse.
    let tmp_dir = local_root.join(".git").join("tcfs-ff-tmp");
    if std::fs::create_dir_all(&tmp_dir).is_err() {
        return None;
    }
    let tmp_path = tmp_dir.join(format!(
        "ref-{}",
        rel_path
            .rsplit('/')
            .next()
            .unwrap_or("ref")
            .replace(['/', '\\'], "_")
    ));
    let download = engine::download_file_with_device(
        op,
        &manifest_path,
        &tmp_path,
        remote_prefix,
        None,
        "",
        None,
        encryption,
    )
    .await;
    let sha = match download {
        Ok(_) => std::fs::read(&tmp_path)
            .ok()
            .and_then(|bytes| git_safety::parse_ref_sha(&bytes)),
        Err(e) => {
            warn!(path = rel_path, error = %format!("{e:#}"), "git ff: remote ref download failed");
            None
        }
    };
    let _ = std::fs::remove_file(&tmp_path);
    sha
}

/// True if a repo-relative path lies inside a `.git` directory.
fn is_git_internal_path(rel_path: &str) -> bool {
    rel_path == ".git"
        || rel_path.starts_with(".git/")
        || rel_path.contains("/.git/")
        || rel_path.ends_with("/.git")
}

/// Compare when both sides exist and the local entry is a symbolic link.
///
/// Symlink identity is the link target text, hashed via
/// `engine::symlink_manifest_hash` — the SAME identity the push path
/// (`upload_symlink_with_device`) writes into the manifest hash, the remote
/// index entry, and the local sync-state `blake3`. Comparing those identities
/// through `compare_clocks` reuses the regular-file conflict/pull/push logic:
/// identical targets short-circuit to UpToDate, divergent targets fall through
/// to the vector-clock decision (Push / Pull / Conflict). This fixes the
/// steady-state defect where a tracked symlink was re-pushed every cycle.
async fn compare_both_exist_symlink(
    rel_path: &str,
    local_path: &Path,
    remote_entry: &RemoteIndexEntry,
    tracked: Option<&SyncState>,
    op: &Operator,
    remote_prefix: &str,
    device_id: &str,
) -> ReconcileAction {
    // Read the local link target without following it.
    let local_target = match crate::engine::read_symlink_target_text(local_path) {
        Ok(t) => t,
        Err(e) => {
            warn!(path = %local_path.display(), error = %e, "failed to read local symlink target");
            return ReconcileAction::UpToDate {
                rel_path: rel_path.to_string(),
            };
        }
    };
    let local_hash = crate::engine::symlink_manifest_hash(&local_target);
    let local_vclock = tracked.map(|s| s.vclock.clone()).unwrap_or_default();

    // Fetch and parse the remote manifest as a SymlinkManifest — the exact type
    // the push path serialized. Failing closed: if the remote entry is not a
    // symlink manifest (kind/version mismatch or unreadable), fall back to the
    // conservative re-push so we never silently treat a mismatched remote as
    // up-to-date.
    let manifest_path = format!(
        "{}/manifests/{}",
        remote_prefix.trim_end_matches('/'),
        &remote_entry.manifest_hash
    );
    let remote_manifest = match op.read(&manifest_path).await {
        Ok(data) => match SymlinkManifest::from_bytes(&data.to_vec()) {
            Ok(m) => m,
            Err(e) => {
                warn!(path = manifest_path, error = %e, "failed to parse remote symlink manifest");
                return ReconcileAction::Push {
                    local_path: local_path.to_path_buf(),
                    rel_path: rel_path.to_string(),
                    reason: PushReason::NewLocal,
                };
            }
        },
        Err(e) => {
            warn!(path = manifest_path, error = %e, "failed to read remote symlink manifest");
            return ReconcileAction::Push {
                local_path: local_path.to_path_buf(),
                rel_path: rel_path.to_string(),
                reason: PushReason::NewLocal,
            };
        }
    };

    let remote_hash = crate::engine::symlink_manifest_hash(&remote_manifest.symlink_target);
    let remote_device = remote_manifest.written_by.as_str();

    let outcome = compare_clocks(
        &local_vclock,
        &remote_manifest.vclock,
        &local_hash,
        &remote_hash,
        rel_path,
        device_id,
        remote_device,
    );

    outcome_to_action(outcome, rel_path, local_path, remote_entry)
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

    // TOCTOU guard for raw `.git` sync: hold a cooperative `.git/tcfs.lock` for
    // every repo that has a `.git/*` action in this plan, for the whole execute
    // window. This stops a concurrent commit (which rewrites refs/index mid-run)
    // from tearing the push. Best-effort: if a repo's lock cannot be acquired
    // (another sync in progress) or the `.git` is mid-operation, we still run —
    // the FF reclassifier only ever moves toward a fast-forward winner, and a
    // failed/locked repo simply re-reconciles next cycle. Guards live for the
    // duration of this function and drop (release) on return.
    let _git_locks = acquire_git_locks_for_plan(plan, local_root);

    for action in &plan.actions {
        match action {
            ReconcileAction::Push {
                local_path,
                rel_path,
                ..
            } => {
                // Symlinks (TIN-1620 T13-Z) are published as first-class link
                // manifests, not run through the chunked-file uploader, which
                // would otherwise dereference or fail on them. `symlink_metadata`
                // does not follow the link, so we detect it without touching the
                // target.
                let is_symlink = std::fs::symlink_metadata(local_path)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                let upload = if is_symlink {
                    engine::upload_symlink_with_device(
                        op,
                        local_path,
                        remote_prefix,
                        state,
                        device_id,
                        rel_path.as_str(),
                    )
                    .await
                } else {
                    // The plan classified this path by CONTENT HASH; execute
                    // must not re-derive staleness from the `(size,
                    // mtime-seconds)` stat quick-check, which is blind to a
                    // same-second same-size rewrite (e.g. `git commit`
                    // rewriting a 41-byte branch head ref) and would silently
                    // skip the push.
                    engine::upload_planned_push_with_device(
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
                };
                match upload {
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
                }
            }

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
    let mut result = ExecutionResult::default();

    // Directories first so pulled files have their parents. Ref-class `.git`
    // pulls (`.git/refs/**`, packed-refs, HEAD) are deferred to a second wave so
    // they land only AFTER objects/packs from the first wave are written — a ref
    // must never point at an object not yet present locally (corruption). All
    // other paths (objects included) run in the first wave.
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
        }
    }

    for wave in [false, true] {
        let mut tasks = JoinSet::new();
        for action in &plan.actions {
            let ReconcileAction::Pull {
                rel_path,
                manifest_hash,
                reason: PullReason::NewRemote,
                ..
            } = action
            else {
                continue;
            };

            // Wave 0 = objects + everything non-ref; wave 1 = ref-class paths.
            if is_git_ref_class_path(rel_path) != wave {
                continue;
            }

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
    }

    Ok(result)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Collect local files into a `rel_path → PathBuf` map, applying the blacklist.
///
/// Tracked symlinks (git mode `120000`) are collected as links rather than
/// dropped or dereferenced (TIN-1620 T13-Z): `preserve_symlinks: true` keeps
/// them in their own `CollectResult::symlinks` bucket, which we fold into the
/// same `rel_path → PathBuf` map so reconcile sees them. `follow_symlinks`
/// stays `false` so we never walk *through* a link. The fail-closed deny-set
/// guard in the collector still screens link targets before they reach here,
/// and the push/restore paths below are symlink-aware.
fn collect_local_set(local_root: &Path, blacklist: &Blacklist) -> Result<HashMap<String, PathBuf>> {
    let config = crate::engine::CollectConfig {
        sync_git_dirs: blacklist.allows_git_dirs(),
        git_sync_mode: blacklist.git_sync_mode().to_string(),
        sync_hidden_dirs: blacklist.allows_hidden_dirs(),
        exclude_patterns: blacklist.glob_patterns(),
        follow_symlinks: false,
        preserve_symlinks: true,
        sync_empty_dirs: false, // reconcile only cares about files
    };
    let result = crate::engine::collect_files(local_root, &config)?;

    let mut map = HashMap::new();
    for entry in result.files.into_iter().chain(result.symlinks) {
        if let Ok(rel) = entry.strip_prefix(local_root) {
            let rel_str = crate::engine::normalize_rel_path_text(&rel.to_string_lossy());
            map.insert(rel_str, entry);
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
            mtime: None,
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
            mtime: None,
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
            mtime: None,
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

        let plan = reconcile(
            &op, local_root, "data", &state, "neo", &blacklist, &config, None,
        )
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

        let plan = reconcile(
            &op, local_root, "data", &state, "neo", &blacklist, &config, None,
        )
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

        let plan = reconcile(
            &op, local_root, "data", &state, "neo", &blacklist, &config, None,
        )
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
            None,
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
            None,
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
            None,
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

    /// (c) Symlink round-trips through reconcile's *push* path: a local-only
    /// symlink is collected as a link (not dropped, not dereferenced), pushed as
    /// a first-class symlink manifest, then restored on a fresh peer with its
    /// target intact (TIN-1620 T13-Z). This exercises the new
    /// `preserve_symlinks: true` collection plus the symlink-aware push dispatch.
    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_push_then_restore_round_trips_symlink() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let source_root = dir.path().join("source");
        let restore_root = dir.path().join("restore");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&restore_root).unwrap();

        // Source has a regular file plus a tracked symlink pointing at it.
        std::fs::write(source_root.join("target.txt"), b"target body").unwrap();
        std::os::unix::fs::symlink("target.txt", source_root.join("link.txt")).unwrap();

        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        // 1. The source side reconciles: the symlink must surface as a Push, not
        //    be silently dropped by collection.
        let mut source_state =
            crate::state::StateCache::open(&dir.path().join("source-state.json")).unwrap();
        let push_plan = reconcile(
            &op,
            &source_root,
            "data",
            &source_state,
            "neo",
            &blacklist,
            &config,
            None,
        )
        .await
        .unwrap();
        assert!(
            push_plan.actions.iter().any(|a| matches!(
                a,
                ReconcileAction::Push { rel_path, .. } if rel_path == "link.txt"
            )),
            "symlink must be collected and planned as a push, got {:?}",
            push_plan.summary
        );

        let push_result = execute_plan(
            &push_plan,
            &op,
            &source_root,
            "data",
            &mut source_state,
            "neo",
            None,
            None,
        )
        .await
        .unwrap();
        assert!(push_result.errors.is_empty(), "{:?}", push_result.errors);

        // The published manifest must be a symlink manifest, not a regular-file
        // one that dereferenced the link into target bytes.
        let manifest_bytes = op.read("data/index/link.txt").await.unwrap().to_vec();
        let entry = crate::index_entry::parse_index_entry_record(&manifest_bytes).unwrap();
        let manifest_hash = match entry {
            crate::index_entry::ParsedIndexEntry::Legacy(e) => e.manifest_hash,
            crate::index_entry::ParsedIndexEntry::V2(e) => e.current.unwrap().manifest_hash,
        };
        let published = op
            .read(&format!("data/manifests/{manifest_hash}"))
            .await
            .unwrap()
            .to_vec();
        let sym = crate::manifest::SymlinkManifest::from_bytes(&published)
            .expect("published manifest must be a symlink manifest, not dereferenced content");
        assert_eq!(sym.symlink_target, "target.txt");

        // 2. A fresh peer restores: the link comes back as a link with the same
        //    target, never dereferenced into a copy of the file.
        let mut restore_state =
            crate::state::StateCache::open(&dir.path().join("restore-state.json")).unwrap();
        let pull_plan = reconcile(
            &op,
            &restore_root,
            "data",
            &restore_state,
            "honey",
            &blacklist,
            &config,
            None,
        )
        .await
        .unwrap();
        let pull_result = execute_plan(
            &pull_plan,
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
        assert!(pull_result.errors.is_empty(), "{:?}", pull_result.errors);

        let restored_link = restore_root.join("link.txt");
        assert!(
            std::fs::symlink_metadata(&restored_link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "restored entry must be a symlink, not a dereferenced regular file"
        );
        assert_eq!(
            std::fs::read_link(&restored_link).unwrap(),
            PathBuf::from("target.txt")
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

        let plan = reconcile(
            &op, local_root, "data", &state, "neo", &blacklist, &config, None,
        )
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

        let plan = reconcile(
            &op, local_root, "data", &state, "neo", &blacklist, &config, None,
        )
        .await
        .unwrap();

        assert_eq!(plan.summary.up_to_date, 1);
        assert_eq!(plan.summary.pushes, 0);
        assert_eq!(plan.summary.pulls, 0);
        assert_eq!(plan.summary.conflicts, 0);
    }

    // ── reconcile plan: tracked symlink converges (steady state) ─────────
    //
    // Regression for the symlink steady-state convergence defect: a tracked
    // symlink present on BOTH local and remote must reconcile to UpToDate on
    // every subsequent cycle, not re-Push forever. The remote here is written
    // by `upload_symlink_with_device`, the same primitive `push_tree` uses, so
    // the index entry, manifest, and local sync state match production exactly.
    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_tracked_symlink_converges_up_to_date() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path().join("repo");
        std::fs::create_dir_all(&local_root).unwrap();

        // A real regular file plus a tracked symlink that points at it.
        std::fs::write(local_root.join("target.txt"), b"target").unwrap();
        let link = local_root.join("link.txt");
        std::os::unix::fs::symlink("target.txt", &link).unwrap();

        // First reconcile cycle: push the symlink to the remote, recording the
        // symlink sync state (blake3 = symlink_manifest_hash(target)) keyed on
        // the live local path. This stands in for the prior successful push.
        let state_path = dir.path().join("state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        crate::engine::upload_symlink_with_device(
            &op, &link, "data", &mut state, "neo", "link.txt",
        )
        .await
        .unwrap();

        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        // Second reconcile cycle against the SAME local tree + remote: the
        // symlink exists on both sides and is tracked, so it must converge.
        let plan = reconcile(
            &op,
            &local_root,
            "data",
            &state,
            "neo",
            &blacklist,
            &config,
            None,
        )
        .await
        .unwrap();

        let link_action = plan.actions.iter().find(|a| match a {
            ReconcileAction::UpToDate { rel_path }
            | ReconcileAction::Push { rel_path, .. }
            | ReconcileAction::Pull { rel_path, .. }
            | ReconcileAction::Conflict { rel_path, .. } => rel_path == "link.txt",
            _ => false,
        });
        assert!(
            matches!(link_action, Some(ReconcileAction::UpToDate { .. })),
            "tracked unchanged symlink must converge to UpToDate, got {link_action:?}"
        );
        assert!(
            !plan.actions.iter().any(
                |a| matches!(a, ReconcileAction::Push { rel_path, .. } if rel_path == "link.txt")
            ),
            "tracked unchanged symlink must not re-push on a steady-state cycle"
        );
    }

    // A *changed* symlink target must still be detected as a divergence so the
    // fix is not a vacuous "always UpToDate".
    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_changed_symlink_target_is_not_up_to_date() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path().join("repo");
        std::fs::create_dir_all(&local_root).unwrap();

        std::fs::write(local_root.join("a.txt"), b"a").unwrap();
        std::fs::write(local_root.join("b.txt"), b"b").unwrap();
        let link = local_root.join("link.txt");
        std::os::unix::fs::symlink("a.txt", &link).unwrap();

        // Push the symlink pointing at a.txt, recording its sync state.
        let state_path = dir.path().join("state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        crate::engine::upload_symlink_with_device(
            &op, &link, "data", &mut state, "neo", "link.txt",
        )
        .await
        .unwrap();

        // Repoint the local symlink at b.txt without re-syncing: local diverges
        // from the tracked + remote target.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("b.txt", &link).unwrap();

        let blacklist = Blacklist::default();
        let config = ReconcileConfig::default();

        let plan = reconcile(
            &op,
            &local_root,
            "data",
            &state,
            "neo",
            &blacklist,
            &config,
            None,
        )
        .await
        .unwrap();

        let link_action = plan.actions.iter().find(|a| match a {
            ReconcileAction::UpToDate { rel_path }
            | ReconcileAction::Push { rel_path, .. }
            | ReconcileAction::Pull { rel_path, .. }
            | ReconcileAction::Conflict { rel_path, .. } => rel_path == "link.txt",
            _ => false,
        });
        assert!(
            !matches!(link_action, Some(ReconcileAction::UpToDate { .. })),
            "a changed symlink target must not be classified UpToDate, got {link_action:?}"
        );
        assert!(
            matches!(
                link_action,
                Some(ReconcileAction::Push { .. }) | Some(ReconcileAction::Conflict { .. })
            ),
            "a changed symlink target must surface as Push/Conflict, got {link_action:?}"
        );
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
