//! Directory reconciliation pipeline — plan-then-execute bidirectional sync.
//!
//! `reconcile()` diffs a local directory tree against the remote index and
//! produces a `ReconcilePlan` (pure data, no side effects). `execute_plan()`
//! then performs the actual I/O using existing engine primitives.
//!
//! This separation enables dry-run mode, TUI preview, and deterministic testing.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use opendal::Operator;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::blacklist::Blacklist;
use crate::conflict::{compare_clocks, ConflictInfo, VectorClock};
use crate::conflict_git;
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

/// A branch-head ref pinned by the `.git` fast-forward ancestry proof at plan
/// time. Execution re-reads the live ref file and refuses to dominate (push) or
/// overwrite (pull) unless it still resolves to exactly this SHA — closing the
/// plan-time-proof / execute-time-state race (a mid-cycle commit / reset /
/// amend on the losing device would otherwise publish an unproven rewind, or
/// have its fresh ref + reflog silently clobbered). See BLOCKER-2 on PR #513.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRefPin {
    /// Repo-relative path of the head ref file (e.g. `.git/refs/heads/main`).
    pub rel_path: String,
    /// The local SHA the ancestry proof was computed against at plan time.
    pub sha: String,
}

/// Why a file needs to be pushed.
#[derive(Debug, Clone)]
pub enum PushReason {
    /// Exists locally but not in the remote index.
    NewLocal,
    /// Vector clock indicates local is ahead of remote.
    LocalNewer,
    /// Reclassified from a `.git` conflict by the fast-forward resolver: the
    /// local git tip is a strict descendant of the remote tip, so pushing
    /// local cannot lose remote history even though the vector clocks are
    /// concurrent. Carries the remote manifest hash the ancestry proof was
    /// computed against so execution can verify the remote has not moved
    /// before letting this push dominate the remote clock.
    GitFastForward {
        /// Manifest hash of this path's remote index entry at plan time.
        expected_remote_manifest: String,
        /// Branch-head refs the ancestry proof pinned at plan time. Execution
        /// re-reads each live ref file and refuses to dominate the remote clock
        /// unless every pin still matches — a mid-cycle local ref move
        /// (commit/reset/amend) makes the whole group DEFER, never dominate.
        ref_pins: Vec<GitRefPin>,
    },
}

/// Why a file needs to be pulled.
#[derive(Debug, Clone)]
pub enum PullReason {
    /// Exists in remote index but not locally.
    NewRemote,
    /// Vector clock indicates remote is ahead of local.
    RemoteNewer,
    /// Reclassified from a `.git` conflict by the fast-forward resolver: the
    /// remote git tip is a strict descendant of the local tip. Carries the
    /// branch-head refs the proof pinned at plan time; execution re-reads each
    /// live ref file and refuses to OVERWRITE local state unless every pin
    /// still matches — a mid-cycle local commit must not be silently clobbered
    /// (its ref + reflog would otherwise dangle with no pointer).
    GitFastForward {
        /// Branch-head refs the ancestry proof pinned at plan time.
        ref_pins: Vec<GitRefPin>,
    },
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
    /// Ref-class `.git` actions (`refs/**`, `packed-refs`, `HEAD`) that were
    /// deferred this run rather than applied. Two causes, both fail-closed and
    /// non-error: an object action for the same repo failed
    /// (objects-before-refs barrier), or the repo's `.git/tcfs.lock` is held by
    /// a live foreign holder (keep-both PR-2, S3), or the PR-4 loser-side
    /// no-loss guard could not park a locally divergent head before overwrite.
    /// The next reconcile cycle re-plans them once the objects land / the holder
    /// releases / parking succeeds.
    pub deferred_git_refs: Vec<String>,
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
/// Planning performs **no writes under `local_root` and no writes to the
/// remote** — it reads state and the remote index, and the returned plan can
/// be inspected, displayed, or executed. One planning-only side effect exists:
/// when `.git` fast-forward reclassification is enabled, conflicting
/// branch-head ref blobs are downloaded to an ephemeral temp directory
/// OUTSIDE any sync root (see `read_remote_ref_sha`) to resolve the remote
/// tips; that directory is removed again before planning returns.
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
/// AFTER objects: `.git/refs/**`, `.git/packed-refs`, and `.git/HEAD` — plus the
/// same layout inside a submodule's real gitdir at `.git/modules/<name>/**`
/// (M-4, PR #513) so raw-roamed submodule internals get the ordering + barrier.
pub(crate) fn is_git_ref_class_path(rel: &str) -> bool {
    if !is_git_internal_path(rel) {
        return false;
    }
    rel.contains(".git/refs/")
        || rel.ends_with(".git/packed-refs")
        || rel.ends_with(".git/HEAD")
        || is_git_modules_ref_class(rel)
}

/// True for `.git` paths that carry object data (`.git/objects/**`, loose or
/// pack) — including a submodule's own object store at
/// `.git/modules/<name>/objects/**` (M-4, PR #513). A failed object action for
/// a repo bars that repo's ref-class actions for the rest of the run
/// (objects-before-refs barrier): a ref must never be published or applied when
/// the objects it needs may not have landed.
fn is_git_object_class_path(rel: &str) -> bool {
    if !is_git_internal_path(rel) {
        return false;
    }
    rel.contains(".git/objects/") || is_git_modules_object_class(rel)
}

/// Submodule object-store paths: anything under a `.git/modules/<name>/objects/`
/// tail (the `<name>` may itself contain slashes / nested `modules/`).
fn is_git_modules_object_class(rel: &str) -> bool {
    match rel.find(".git/modules/") {
        Some(pos) => rel[pos..].contains("/objects/"),
        None => false,
    }
}

/// Submodule ref-class paths: `refs/**`, `packed-refs`, or `HEAD` inside a
/// `.git/modules/<name>/` gitdir.
fn is_git_modules_ref_class(rel: &str) -> bool {
    match rel.find(".git/modules/") {
        Some(pos) => {
            let tail = &rel[pos..];
            tail.contains("/refs/") || tail.ends_with("/packed-refs") || tail.ends_with("/HEAD")
        }
        None => false,
    }
}

/// Record a failed `.git/objects/**` action: inserts the enclosing repo root
/// into the per-run barred set consulted by [`git_ref_barrier_hit`].
fn mark_git_object_failure(rel_path: &str, local_root: &Path, barred: &mut BTreeSet<PathBuf>) {
    if !is_git_object_class_path(rel_path) {
        return;
    }
    if let Some(root) = git_safety::repo_root_for_git_path(local_root, rel_path) {
        barred.insert(root);
    }
}

/// True when `rel_path` is a ref-class `.git` path whose enclosing repo had an
/// object-class failure earlier in this run — the action must be deferred
/// (objects-before-refs barrier, both push and pull directions).
fn git_ref_barrier_hit(rel_path: &str, local_root: &Path, barred: &BTreeSet<PathBuf>) -> bool {
    if barred.is_empty() || !is_git_ref_class_path(rel_path) {
        return false;
    }
    git_safety::repo_root_for_git_path(local_root, rel_path)
        .is_some_and(|root| barred.contains(&root))
}

/// True when `rel_path` is a ref-class `.git` path whose enclosing repo is held
/// by a live FOREIGN `.git/tcfs.lock` holder this run — the action must be
/// deferred (keep-both PR-2, S3). Only ref-class paths (`refs/**`,
/// `packed-refs`, `HEAD`, submodule ref-class) gate on the lock; object-class
/// (`.git/objects/**`) and normal-file writes are content-addressed / atomic
/// and proceed as today.
fn git_ref_foreign_lock_hit(
    rel_path: &str,
    local_root: &Path,
    foreign_locked: &BTreeSet<PathBuf>,
) -> bool {
    if foreign_locked.is_empty() || !is_git_ref_class_path(rel_path) {
        return false;
    }
    git_safety::repo_root_for_git_path(local_root, rel_path)
        .is_some_and(|root| foreign_locked.contains(&root))
}

/// Extract the plan-time head-ref pins carried by a `.git` fast-forward action
/// (push or pull). Ordinary actions carry no pins and yield `None`.
fn git_ff_ref_pins(action: &ReconcileAction) -> Option<&[GitRefPin]> {
    match action {
        ReconcileAction::Push {
            reason: PushReason::GitFastForward { ref_pins, .. },
            ..
        } => Some(ref_pins),
        ReconcileAction::Pull {
            reason: PullReason::GitFastForward { ref_pins },
            ..
        } => Some(ref_pins),
        _ => None,
    }
}

/// Re-read every pinned head ref from the live working tree and confirm it
/// still resolves to exactly the SHA the fast-forward proof pinned at plan
/// time. Any mismatch — a mid-cycle commit / reset / amend on this device, or a
/// vanished ref file — means the ancestry proof is stale, so the caller must
/// DEFER, never dominate (push) or overwrite (pull). Fail-closed (BLOCKER-2,
/// PR #513).
fn git_ff_pins_still_valid(local_root: &Path, pins: &[GitRefPin]) -> bool {
    pins.iter().all(|pin| {
        match std::fs::read(local_root.join(&pin.rel_path)) {
            Ok(bytes) => git_safety::parse_ref_sha(&bytes).as_deref() == Some(pin.sha.as_str()),
            Err(_) => false, // ref file gone → stale, defer
        }
    })
}

/// A `.git` ref-class file a pull is about to overwrite that the loser-side
/// no-loss guard (PR-4, S10) must vet.
enum LoserGuardTarget {
    /// Concrete ref resolvable through a gitdir. Top-level heads/stash are
    /// parkable; all other ref names are defer-only.
    Ref {
        /// Git dir the ref lives in (`<repo>/.git` or
        /// `<repo>/.git/modules/<name>`).
        git_dir: PathBuf,
        /// Repo root — where a parkable head is parked (top-level refs only).
        repo_root: PathBuf,
        /// Fully-qualified ref name (`refs/heads/<b>` or `refs/stash`).
        ref_name: String,
        /// True for top-level heads/stash (`refs/tcfs/theirs/<self>/**`).
        /// False for module-gitdir/non-head refs; future work, so defer.
        parkable: bool,
    },
    /// Opaque packed ref table. We do not parse or rewrite individual entries
    /// in PR-4; any byte-level difference is defer-only.
    PackedRefs { local_path: PathBuf },
}

/// Classify a pull's `rel_path` as a loser-guard target, or `None` if the guard
/// does not apply (objects, index, logs, non-`.git` files).
fn loser_guard_ref_target(local_root: &Path, rel_path: &str) -> Option<LoserGuardTarget> {
    let rel = rel_path.replace('\\', "/");
    let repo_root = git_safety::repo_root_for_git_path(local_root, &rel)?;
    let git_root = repo_root.join(".git");
    let local_path = local_root.join(&rel);
    let after_path = local_path.strip_prefix(&git_root).ok()?;
    let after = after_path.to_string_lossy().replace('\\', "/");
    let after = after.trim_start_matches('/');

    if let Some(module_tail) = after.strip_prefix("modules/") {
        if let Some(module_subpath) = module_tail.strip_suffix("/packed-refs") {
            if !module_subpath.is_empty() {
                return Some(LoserGuardTarget::PackedRefs {
                    local_path: git_root
                        .join("modules")
                        .join(module_subpath)
                        .join("packed-refs"),
                });
            }
        }
        // `.git/modules/<name...>/refs/<kind>/<name>`. `<name>` may contain
        // slashes / nested `modules/`, so split on the LAST `/refs/`.
        let marker = "/refs/";
        if let Some(idx) = module_tail.rfind(marker) {
            let ref_suffix = &module_tail[idx + 1..];
            if ref_suffix == "refs/" || ref_suffix.ends_with('/') {
                return None;
            }
            let module_subpath = &module_tail[..idx];
            return Some(LoserGuardTarget::Ref {
                git_dir: git_root.join("modules").join(module_subpath),
                repo_root,
                ref_name: ref_suffix.to_string(),
                parkable: false,
            });
        }
        if let Some(module_subpath) = module_tail_raw_head(module_tail) {
            let git_dir = git_root.join("modules").join(module_subpath);
            if std::fs::read(git_dir.join("HEAD"))
                .ok()
                .and_then(|bytes| git_safety::parse_ref_sha(&bytes))
                .is_some()
            {
                return Some(LoserGuardTarget::Ref {
                    git_dir,
                    repo_root,
                    ref_name: "HEAD".to_string(),
                    parkable: false,
                });
            }
        }
        return None;
    }

    if after == "packed-refs" {
        return Some(LoserGuardTarget::PackedRefs {
            local_path: git_root.join("packed-refs"),
        });
    }
    if after == "refs/stash" {
        return Some(LoserGuardTarget::Ref {
            git_dir: git_root,
            repo_root,
            ref_name: "refs/stash".to_string(),
            parkable: true,
        });
    }
    if let Some(branch) = after.strip_prefix("refs/heads/") {
        if branch.is_empty() || branch.ends_with('/') {
            return None;
        }
        return Some(LoserGuardTarget::Ref {
            git_dir: git_root,
            repo_root,
            ref_name: format!("refs/heads/{branch}"),
            parkable: true,
        });
    }
    if after.starts_with("refs/") && !after.ends_with('/') {
        return Some(LoserGuardTarget::Ref {
            git_dir: git_root,
            repo_root,
            ref_name: after.to_string(),
            parkable: false,
        });
    }
    if after == "HEAD" && git_root.join("HEAD").exists() {
        return Some(LoserGuardTarget::Ref {
            git_dir: git_root,
            repo_root,
            ref_name: "HEAD".to_string(),
            parkable: false,
        });
    }
    None
}

fn module_tail_raw_head(module_tail: &str) -> Option<&str> {
    module_tail
        .strip_suffix("/HEAD")
        .filter(|module_subpath| !module_subpath.is_empty())
}

/// Read the SHA `ref_name` resolves to in an explicit git dir (`--git-dir`),
/// consulting loose refs and packed-refs. `--git-dir` (not `-C`) so a
/// submodule's bare-style module gitdir resolves correctly. `None` if the ref
/// is absent or not a concrete SHA.
fn git_dir_ref_sha(git_dir: &Path, ref_name: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["rev-parse", "--verify", "--quiet", ref_name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    git_safety::parse_ref_sha(&output.stdout)
}

/// True iff `ancestor` is an ancestor of `descendant` in an explicit git dir.
/// Mirrors `git_safety::is_ancestor` but targets a `--git-dir` so submodule
/// module-gitdirs work. A missing object (exit != 0/1) is treated as "not an
/// ancestor" so the guard fails closed toward parking/deferring.
fn git_dir_is_ancestor(git_dir: &Path, ancestor: &str, descendant: &str) -> bool {
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map(|out| out.status.code() == Some(0))
        .unwrap_or(false)
}

fn git_dir_commit_present(git_dir: &Path, sha: &str) -> bool {
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn git_dir_object_present(git_dir: &Path, sha: &str) -> bool {
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["cat-file", "-e", &format!("{sha}^{{object}}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn git_dir_ready_for_ref_guard(git_dir: &Path) -> bool {
    git_dir.join("HEAD").exists() && git_dir.join("objects").is_dir()
}

fn git_dir_head_is_symbolic(git_dir: &Path) -> bool {
    std::fs::read(git_dir.join("HEAD"))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|head| head.trim_start().starts_with("ref:"))
        .unwrap_or(false)
}

fn packed_refs_shas(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let token = trimmed
                .strip_prefix('^')
                .unwrap_or(trimmed)
                .split_whitespace()
                .next()?;
            git_safety::parse_ref_sha(token.as_bytes())
        })
        .collect()
}

fn packed_refs_objects_present(git_dir: &Path, bytes: &[u8]) -> bool {
    let shas = packed_refs_shas(bytes);
    !shas.is_empty() && shas.iter().all(|sha| git_dir_object_present(git_dir, sha))
}

fn write_file_create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir: {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating new file: {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing new file: {}", path.display()))
}

fn zero_oid_like(oid: &str) -> String {
    "0".repeat(oid.len())
}

fn git_dir_update_ref_cas(
    git_dir: &Path,
    ref_name: &str,
    new_sha: &str,
    expected_old: Option<&str>,
) -> Result<()> {
    let expected = expected_old
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| zero_oid_like(new_sha));
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["update-ref", ref_name, new_sha, &expected])
        .output()
        .with_context(|| format!("running git update-ref for {ref_name}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git update-ref CAS failed for {ref_name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn git_dir_delete_ref_cas(git_dir: &Path, ref_name: &str, expected_old: &str) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["update-ref", "-d", ref_name, expected_old])
        .output()
        .with_context(|| format!("running git update-ref -d for {ref_name}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git update-ref delete CAS failed for {ref_name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Outcome of acquiring cooperative `.git/tcfs.lock` guards for a plan.
///
/// keep-both PR-2 (S3): the executor no longer proceeds unconditionally when a
/// repo's lock cannot be taken. A lock held by a FOREIGN live holder — another
/// sync cycle or process, a PID `acquire_git_lock` could neither acquire nor
/// steal as stale — means the cooperative lock fences nothing if we still write
/// ref-class paths. Those repos are surfaced in `foreign_locked_repos` so the
/// executor DEFERS their ref-class `.git` actions this run.
struct GitLockAcquisition {
    /// Held guards; dropping the vec (with the owning struct) releases all locks.
    guards: Vec<git_safety::GitLockGuard>,
    /// Repo roots whose `.git/tcfs.lock` is held by a live foreign holder that
    /// could not be acquired or stolen-as-stale this run. Ref-class writes for
    /// these repos are deferred (recorded, not errored); the next cycle
    /// re-plans them once the holder releases. A STALE (dead-owner, aged) lock
    /// is stolen by `acquire_git_lock` and returns `Ok`, so a leaked lock never
    /// lands a repo here — no deadlock on a leaked lock.
    foreign_locked_repos: BTreeSet<PathBuf>,
}

/// Acquire cooperative `.git/tcfs.lock` guards for every repo that has a
/// `.git/*` write action in `plan`. Returns the held guards plus the set of
/// repos whose lock is held by a live foreign holder (ref-class writes there
/// must defer, keep-both PR-2). Repos whose `.git` is mid-operation are skipped
/// (logged) and keep today's behavior — a busy repo simply re-reconciles next
/// cycle.
fn acquire_git_locks_for_plan(plan: &ReconcilePlan, local_root: &Path) -> GitLockAcquisition {
    let mut repos: BTreeSet<PathBuf> = BTreeSet::new();
    for action in &plan.actions {
        let rel = match action {
            ReconcileAction::Push { rel_path, .. }
            | ReconcileAction::Pull { rel_path, .. }
            | ReconcileAction::DeleteLocal { rel_path, .. }
            | ReconcileAction::DeleteRemote { rel_path } => rel_path.as_str(),
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
    let mut foreign_locked_repos: BTreeSet<PathBuf> = BTreeSet::new();
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
                // A live FOREIGN holder owns this repo's `.git/tcfs.lock`. A
                // stale (dead-owner, aged) lock would have been stolen and
                // returned Ok, so this Err means the lock is genuinely held —
                // writing ref-class paths anyway would race the holder. Record
                // the repo so the executor DEFERS its ref-class actions this
                // run (keep-both PR-2, S3) instead of proceeding unlocked.
                warn!(
                    repo = %repo_root.display(),
                    error = %format!("{e:#}"),
                    "git ff: foreign holder owns tcfs.lock; deferring ref-class writes for this repo"
                );
                foreign_locked_repos.insert(repo_root);
            }
        }
    }
    GitLockAcquisition {
        guards,
        foreign_locked_repos,
    }
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

    outcome_to_action(outcome, rel_path, local_path, remote_entry, &manifest_path)
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
    remote_manifest_key: &str,
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
        crate::conflict::SyncOutcome::Conflict(mut info) => {
            // keep-both PR-2 data-model graft: capture the remote side's
            // manifest storage key (the S3/prefix path this classification
            // already computed to read the remote manifest). This is the only
            // point where the remote manifest hash is in scope — the later
            // record site (`ReconcileAction::Conflict` arm) holds only the
            // local state entry — so populate it here; the value rides the
            // `ConflictInfo` into the recorded conflict. A future PR-3 resolve
            // verb fetches the remote ref SHA from this key.
            info.remote_manifest_key = Some(remote_manifest_key.to_string());
            ReconcileAction::Conflict {
                rel_path: rel_path.to_string(),
                info,
            }
        }
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
            &indices,
            actions,
            op,
            remote_prefix,
            remote_index,
            encryption,
        )
        .await;
        let Some((direction, ref_pins)) = decision else {
            // Indeterminate / divergent: leave every conflict as-is.
            continue;
        };
        // Apply atomically: rewrite ALL of this repo's `.git/*` conflicts toward
        // the single winning direction. Every rewritten action carries the same
        // plan-time head-ref pins (BLOCKER-2): execution re-verifies them before
        // dominating/overwriting, so a mid-cycle local ref move defers the whole
        // group — reflog and index included.
        for &idx in &indices {
            let ReconcileAction::Conflict { rel_path, .. } = &actions[idx] else {
                continue;
            };
            let rel_path = rel_path.clone();
            actions[idx] = match direction {
                git_safety::FastForward::LocalAhead => {
                    // Carry the remote manifest hash the ancestry proof was
                    // computed against: at execute time the upload path only
                    // lets this push dominate a concurrent remote clock if the
                    // remote entry is still exactly this manifest (if the
                    // remote moved since planning, the ordinary conflict veto
                    // applies and the repo re-plans next cycle).
                    match remote_index.get(&rel_path) {
                        Some(entry) => ReconcileAction::Push {
                            local_path: local_root.join(&rel_path),
                            rel_path: rel_path.clone(),
                            reason: PushReason::GitFastForward {
                                expected_remote_manifest: entry.manifest_hash.clone(),
                                ref_pins: ref_pins.clone(),
                            },
                        },
                        // No remote entry: fail closed, keep the conflict.
                        None => continue,
                    }
                }
                git_safety::FastForward::RemoteAhead => {
                    // The remote index entry for this exact path carries the
                    // manifest hash + size needed to pull it.
                    match remote_index.get(&rel_path) {
                        Some(entry) => ReconcileAction::Pull {
                            rel_path: rel_path.clone(),
                            manifest_hash: entry.manifest_hash.clone(),
                            size: entry.size,
                            reason: PullReason::GitFastForward {
                                ref_pins: ref_pins.clone(),
                            },
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
    indices: &[usize],
    actions: &[ReconcileAction],
    op: &Operator,
    remote_prefix: &str,
    remote_index: &HashMap<String, RemoteIndexEntry>,
    encryption: OptionalEncryption<'_>,
) -> Option<(git_safety::FastForward, Vec<GitRefPin>)> {
    // BLOCKER-1 + BLOCKER-3 (fail-closed, PR #513): the ancestry proof below
    // covers ONLY provable top-level branch-head refs (`.git/refs/heads/*`, via
    // `head_ref_for_git_path`). If this conflict group contains ANY OTHER
    // ref-class path — `packed-refs`, tags, stash, remotes, notes, a
    // detached/divergent `.git/HEAD`, OR any submodule ref-class path under
    // `.git/modules/<name>/**` (which `repo_root_for_git_path` groups under this
    // outer repo) — a fast-forward decision would force-sync that pointer state
    // under group dominance with NO ancestry proof, a deterministic silent
    // clobber. Fail closed on every ref-class path that is not a provable
    // top-level head: veto the whole repo, every conflict stays Conflict, zero
    // writes. (`.git/index` and head-following `.git/logs/**` are NOT ref-class,
    // so they keep riding the group decision.)
    for &idx in indices {
        if let ReconcileAction::Conflict { rel_path, .. } = &actions[idx] {
            if is_git_ref_class_path(rel_path)
                && git_safety::head_ref_for_git_path(rel_path).is_none()
            {
                debug!(
                    repo = %repo_root.display(),
                    path = %rel_path,
                    "git ff: ref-class path with no provable top-level head in conflict group; fail-closed (stays Conflict)"
                );
                return None;
            }
        }
    }

    // Find the branch-head ref path(s) among this repo's conflicts. There may be
    // several (multiple branches advanced); a clean fast-forward requires EVERY
    // conflicting head ref to agree on the same direction, otherwise fail closed.
    let mut direction: Option<git_safety::FastForward> = None;
    let mut saw_ref = false;
    // Plan-time head-ref pins for the execute-time re-verify (BLOCKER-2).
    let mut ref_pins: Vec<GitRefPin> = Vec::new();

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
        let Some(remote_sha) =
            read_remote_ref_sha(op, remote_prefix, rel_path, remote_index, encryption).await
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
        // Pin the exact local SHA this ref was proven against.
        ref_pins.push(GitRefPin {
            rel_path: rel_path.clone(),
            sha: local_sha,
        });
    }

    if !saw_ref {
        // Conflicts under `.git` but no branch-head ref among them (e.g. only
        // index/logs). Without a ref tip to compare we cannot prove a clean FF,
        // so stay conflicted.
        return None;
    }
    direction.map(|d| (d, ref_pins))
}

/// Read the remote commit SHA stored at a `.git/refs/heads/<branch>` path by
/// downloading the (tiny) ref blob from the remote and parsing its content.
///
/// The blob is downloaded into an ephemeral, per-call temp directory under
/// [`std::env::temp_dir`] — never under the sync root (planning must not
/// fabricate files a raw-mode collector could then roam) and never inside the
/// repo's live `.git`. The directory is removed again before returning,
/// success or failure.
///
/// Returns `None` if the remote has no entry for this path, the download fails,
/// or the content is not a concrete SHA (e.g. a symbolic ref).
async fn read_remote_ref_sha(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    remote_index: &HashMap<String, RemoteIndexEntry>,
    encryption: OptionalEncryption<'_>,
) -> Option<String> {
    let entry = remote_index.get(rel_path)?;
    download_ref_sha_from_manifest(op, remote_prefix, &entry.manifest_hash, encryption).await
}

/// Download the tiny ref blob addressed by `manifest_hash` into an ephemeral,
/// per-call temp dir (never under a sync root, never inside a live `.git`) and
/// parse it as a concrete SHA. Shared by `read_remote_ref_sha` (plan-time) and
/// the execute-loop loser-side no-loss guard (PR-4), which needs the INCOMING
/// ref SHA a pull is about to write before deciding whether the overwrite would
/// orphan committed local work. Returns `None` on download failure or a
/// non-concrete (symbolic) ref — callers MUST treat `None` as "cannot prove
/// safe" and fail closed.
async fn download_ref_sha_from_manifest(
    op: &Operator,
    remote_prefix: &str,
    manifest_hash: &str,
    encryption: OptionalEncryption<'_>,
) -> Option<String> {
    download_bytes_from_manifest(op, remote_prefix, manifest_hash, encryption)
        .await
        .and_then(|bytes| git_safety::parse_ref_sha(&bytes))
}

/// Download a manifest-addressed blob into an ephemeral temp dir and return its
/// bytes. Used by SHA-ref guards and opaque packed-refs guards.
async fn download_bytes_from_manifest(
    op: &Operator,
    remote_prefix: &str,
    manifest_hash: &str,
    encryption: OptionalEncryption<'_>,
) -> Option<Vec<u8>> {
    let manifest_path = format!(
        "{}/manifests/{}",
        remote_prefix.trim_end_matches('/'),
        manifest_hash
    );
    // Unique per call (pid + process-wide sequence) so concurrent reconciles
    // in one or many processes never collide on the same path.
    static FF_REF_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = FF_REF_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_dir = std::env::temp_dir().join(format!("tcfs-ff-{}-{seq}", std::process::id()));
    if std::fs::create_dir_all(&tmp_dir).is_err() {
        return None;
    }
    let tmp_path = tmp_dir.join("ref");
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
    let bytes = match download {
        Ok(_) => std::fs::read(&tmp_path).ok(),
        Err(e) => {
            warn!(manifest = manifest_hash, error = %format!("{e:#}"), "git ref guard: remote blob download failed");
            None
        }
    };
    let _ = std::fs::remove_dir_all(&tmp_dir);
    bytes
}

async fn read_sync_manifest_for_state(
    op: &Operator,
    remote_prefix: &str,
    manifest_hash: &str,
) -> Option<(String, SyncManifest)> {
    let manifest_path = format!(
        "{}/manifests/{}",
        remote_prefix.trim_end_matches('/'),
        manifest_hash
    );
    let manifest_bytes = op.read(&manifest_path).await.ok()?;
    SyncManifest::from_bytes(&manifest_bytes.to_vec())
        .ok()
        .map(|manifest| (manifest_path, manifest))
}

async fn record_guarded_ref_pull_state(
    op: &Operator,
    remote_prefix: &str,
    manifest_hash: &str,
    local_path: &Path,
    incoming_bytes: &[u8],
    state: &mut StateCache,
    device_id: &str,
) -> Result<()> {
    let (manifest_path, manifest) = read_sync_manifest_for_state(op, remote_prefix, manifest_hash)
        .await
        .context("reading pulled ref manifest for state")?;
    let mut local_vclock = state
        .get(local_path)
        .map(|s| s.vclock.clone())
        .unwrap_or_else(VectorClock::new);
    local_vclock.merge(&manifest.vclock);
    let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(incoming_bytes));
    let sync_state = crate::state::make_sync_state_full(
        local_path,
        hash,
        manifest.chunk_hashes().len(),
        manifest_path,
        local_vclock,
        device_id.to_string(),
    )?;
    state.set(local_path, sync_state);
    Ok(())
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

    outcome_to_action(outcome, rel_path, local_path, remote_entry, &manifest_path)
}

// ── Execution ────────────────────────────────────────────────────────────────

fn new_remote_pull_fast_path_safe(plan: &ReconcilePlan) -> bool {
    plan.actions.iter().all(|action| {
        matches!(
            action,
            ReconcileAction::Pull {
                reason: PullReason::NewRemote,
                ..
            } | ReconcileAction::CreateDirectory { .. }
                | ReconcileAction::UpToDate { .. }
        )
    }) && plan.actions.iter().any(|action| {
        matches!(
            action,
            ReconcileAction::Pull {
                reason: PullReason::NewRemote,
                ..
            }
        )
    }) && plan.actions.iter().all(|action| {
        !matches!(
            action,
            ReconcileAction::Pull { rel_path, .. } if is_git_ref_class_path(rel_path)
        )
    })
}

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
    if encryption.is_none() && progress.is_none() && new_remote_pull_fast_path_safe(plan) {
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
    // from tearing the push. keep-both PR-2 (S3): if a repo's lock is held by a
    // live FOREIGN holder, the cooperative lock fences nothing were we to write
    // ref-class paths anyway — so those repos' ref-class actions DEFER this run
    // (see `git_ref_foreign_lock_hit` below); object-class and normal-file
    // writes still proceed. A STALE (dead-owner, aged) lock is stolen on
    // acquire, so a leaked lock never deadlocks a repo. Repos mid-`.git`
    // operation keep today's behavior (skipped, re-reconciled next cycle).
    // Guards live for the duration of this function and drop (release) on return.
    let git_lock_acq = acquire_git_locks_for_plan(plan, local_root);
    let foreign_locked_repos = &git_lock_acq.foreign_locked_repos;
    let _git_locks = &git_lock_acq.guards;

    // Objects-before-refs BARRIER (per repo, both directions): the plan orders
    // `.git/objects/**` ahead of ref-class paths, but ordering alone is not a
    // barrier — if an object action FAILS, applying or publishing the ref
    // anyway could point it at an object that never landed (repo corruption).
    // Any repo with a failed object-class action this run gets its ref-class
    // actions deferred (recorded, not errored); the next cycle re-plans them.
    let mut git_object_failed_repos: BTreeSet<PathBuf> = BTreeSet::new();

    // keep-both PR-4 (S10): where the loser-side no-loss guard writes its
    // pre-overwrite `.git` undo bundle. The state cache lives in the
    // machine-local state dir, so its parent is that dir — OUTSIDE any sync root
    // (the bundle must never roam and re-conflict). A parent-less db path (bare
    // filename, e.g. in-memory tests) falls back to the temp dir; still
    // out-of-tree.
    let undo_state_dir = state.state_dir();

    for action in &plan.actions {
        let git_write_rel_path = match action {
            ReconcileAction::Push { rel_path, .. }
            | ReconcileAction::Pull { rel_path, .. }
            | ReconcileAction::DeleteLocal { rel_path, .. }
            | ReconcileAction::DeleteRemote { rel_path } => Some(rel_path),
            _ => None,
        };
        if let Some(rel_path) = git_write_rel_path {
            // keep-both PR-2 (S3): a live FOREIGN holder owns this repo's
            // `.git/tcfs.lock`. Writing ref-class paths while another sync holds
            // the lock races it, so DEFER this repo's ref-class actions this run
            // (recorded, not errored); the next cycle re-plans them once the
            // holder releases. Object-class and normal-file writes are
            // unaffected — only ref-class paths gate on the lock.
            if git_ref_foreign_lock_hit(rel_path, local_root, foreign_locked_repos) {
                info!(
                    path = %rel_path,
                    "git lock: foreign holder owns .git/tcfs.lock; deferring ref action"
                );
                result.deferred_git_refs.push(rel_path.clone());
                continue;
            }
            if git_ref_barrier_hit(rel_path, local_root, &git_object_failed_repos) {
                info!(
                    path = %rel_path,
                    "git barrier: object action failed this run; deferring ref action"
                );
                result.deferred_git_refs.push(rel_path.clone());
                continue;
            }
            // BLOCKER-2 (PR #513): a `.git` fast-forward push/pull earned its
            // right to dominate/overwrite from the plan-time head-ref SHA. Right
            // before applying it, re-read the live local ref(s): if any moved
            // since planning (a mid-cycle commit/reset/amend on this device),
            // the proof is stale — DEFER the whole group (reflog + index
            // included) instead of dominating the remote clock (push) or
            // clobbering the fresh local commit (pull).
            if let Some(pins) = git_ff_ref_pins(action) {
                if !git_ff_pins_still_valid(local_root, pins) {
                    warn!(
                        path = %rel_path,
                        "git ff: local ref moved between plan and execute; deferring (no dominate/overwrite)"
                    );
                    result.deferred_git_refs.push(rel_path.clone());
                    continue;
                }
            }
        }
        match action {
            ReconcileAction::Push {
                local_path,
                rel_path,
                reason,
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
                    //
                    // Reclassified `.git` fast-forward pushes additionally
                    // carry the plan-time remote manifest hash: the upload
                    // path uses it to let the ancestry-proven push dominate a
                    // concurrent remote clock (merge + tick) instead of being
                    // veto-skipped forever — see HIGH-2 on PR #513.
                    let git_ff_expected = match reason {
                        PushReason::GitFastForward {
                            expected_remote_manifest,
                            ..
                        } => Some(expected_remote_manifest.as_str()),
                        _ => None,
                    };
                    engine::upload_planned_push_with_device(
                        op,
                        local_path,
                        remote_prefix,
                        state,
                        progress,
                        device_id,
                        Some(rel_path.as_str()),
                        encryption,
                        git_ff_expected,
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
                        mark_git_object_failure(rel_path, local_root, &mut git_object_failed_repos);
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
                        mark_git_object_failure(rel_path, local_root, &mut git_object_failed_repos);
                        result
                            .errors
                            .push((rel_path.clone(), format!("mkdir failed: {e}")));
                        continue;
                    }
                }

                // keep-both PR-4 (S10): loser-side no-loss guard. This pull is
                // about to OVERWRITE a local `.git` ref-class HEAD file. If that
                // head currently holds committed work UNREACHABLE from the
                // incoming SHA (not equal, not an ancestor of it), overwriting
                // would orphan it. Before overwriting, park the current SHA at
                // `refs/tcfs/theirs/<self_device>/**` (keeping the loser's line
                // reachable + fsck-clean) and bundle-snapshot the pre-overwrite
                // `.git` to the state dir. Park OR bundle failure — or a
                // non-parkable module-gitdir head — DEFERS the pull (fail
                // closed: never overwrite without a durable escape hatch). The
                // FF / equal / new-ref fast paths take no parking, no bundle.
                if let Some(target) = loser_guard_ref_target(local_root, rel_path) {
                    match target {
                        LoserGuardTarget::PackedRefs { local_path } => {
                            let git_dir = local_path.parent().unwrap_or(local_root);
                            match (
                                std::fs::read(&local_path),
                                download_bytes_from_manifest(
                                    op,
                                    remote_prefix,
                                    manifest_hash,
                                    encryption,
                                )
                                .await,
                            ) {
                                (Ok(current), Some(incoming)) if current != incoming => {
                                    warn!(
                                        path = %rel_path,
                                        "loser-guard: packed-refs would change opaquely; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                                (Err(e), Some(_)) if e.kind() == std::io::ErrorKind::NotFound => {
                                    if !git_dir_ready_for_ref_guard(git_dir) {
                                        // Bootstrap raw restore of a not-yet
                                        // initialized gitdir. The wave-0 object
                                        // barrier still guards missing object
                                        // pulls before this wave-1 ref table.
                                    } else {
                                        let Some(incoming) = download_bytes_from_manifest(
                                            op,
                                            remote_prefix,
                                            manifest_hash,
                                            encryption,
                                        )
                                        .await
                                        else {
                                            warn!(
                                                path = %rel_path,
                                                "loser-guard: incoming packed-refs unreadable; deferring pull"
                                            );
                                            result.deferred_git_refs.push(rel_path.clone());
                                            continue;
                                        };
                                        if !packed_refs_objects_present(git_dir, &incoming) {
                                            warn!(
                                                path = %rel_path,
                                                "loser-guard: incoming packed-refs objects missing; deferring pull"
                                            );
                                            result.deferred_git_refs.push(rel_path.clone());
                                            continue;
                                        }
                                        if let Err(e) =
                                            write_file_create_new(&local_path, &incoming)
                                        {
                                            warn!(
                                                path = %rel_path,
                                                error = %format!("{e:#}"),
                                                "loser-guard: packed-refs appeared before create-new write; deferring pull"
                                            );
                                            result.deferred_git_refs.push(rel_path.clone());
                                            continue;
                                        }
                                        if let Err(e) = record_guarded_ref_pull_state(
                                            op,
                                            remote_prefix,
                                            manifest_hash,
                                            &local_path,
                                            &incoming,
                                            state,
                                            device_id,
                                        )
                                        .await
                                        {
                                            result.errors.push((
                                                rel_path.clone(),
                                                format!(
                                                    "guarded packed-refs state update failed: {e:#}"
                                                ),
                                            ));
                                            continue;
                                        }
                                        result.pulled += 1;
                                        result.bytes_downloaded += incoming.len() as u64;
                                        continue;
                                    }
                                }
                                (Err(e), Some(_)) => {
                                    warn!(
                                        path = %rel_path,
                                        error = %e,
                                        "loser-guard: cannot inspect local packed-refs; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                                (_, None) => {
                                    warn!(
                                        path = %rel_path,
                                        "loser-guard: incoming packed-refs unreadable; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                                (Ok(current), Some(_)) => {
                                    if let Err(e) = record_guarded_ref_pull_state(
                                        op,
                                        remote_prefix,
                                        manifest_hash,
                                        &local_path,
                                        &current,
                                        state,
                                        device_id,
                                    )
                                    .await
                                    {
                                        result.errors.push((
                                            rel_path.clone(),
                                            format!(
                                                "guarded packed-refs state update failed: {e:#}"
                                            ),
                                        ));
                                        continue;
                                    }
                                    result.pulled += 1;
                                    result.bytes_downloaded += current.len() as u64;
                                    continue;
                                }
                            }
                        }
                        LoserGuardTarget::Ref {
                            git_dir,
                            repo_root,
                            ref_name,
                            parkable,
                        } => {
                            let incoming_bytes = match download_bytes_from_manifest(
                                op,
                                remote_prefix,
                                manifest_hash,
                                encryption,
                            )
                            .await
                            {
                                Some(bytes) => bytes,
                                None => {
                                    warn!(
                                        path = %rel_path,
                                        "loser-guard: incoming ref blob unreadable; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                            };
                            if !git_dir_ready_for_ref_guard(&git_dir) {
                                // Bootstrap raw restore of a not-yet
                                // initialized gitdir. Git commands cannot prove
                                // refs until the repository skeleton exists; the
                                // object-before-ref ordering and wave-0 failure
                                // barrier still apply before ref-class paths.
                            } else {
                                if ref_name == "HEAD"
                                    && git_safety::parse_ref_sha(&incoming_bytes).is_none()
                                {
                                    let local_head = std::fs::read(git_dir.join("HEAD")).ok();
                                    if local_head.as_deref() == Some(incoming_bytes.as_slice()) {
                                        if let Err(e) = record_guarded_ref_pull_state(
                                            op,
                                            remote_prefix,
                                            manifest_hash,
                                            &local_path,
                                            &incoming_bytes,
                                            state,
                                            device_id,
                                        )
                                        .await
                                        {
                                            result.errors.push((
                                                rel_path.clone(),
                                                format!("guarded HEAD state update failed: {e:#}"),
                                            ));
                                            continue;
                                        }
                                        result.pulled += 1;
                                        result.bytes_downloaded += incoming_bytes.len() as u64;
                                        continue;
                                    }
                                    warn!(
                                        path = %rel_path,
                                        "loser-guard: symbolic HEAD change is not CAS-protectable; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                                if ref_name == "HEAD" && git_dir_head_is_symbolic(&git_dir) {
                                    warn!(
                                        path = %rel_path,
                                        "loser-guard: incoming detached HEAD would rewrite symbolic HEAD; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                                let Some(incoming) = git_safety::parse_ref_sha(&incoming_bytes)
                                else {
                                    warn!(
                                        path = %rel_path,
                                        "loser-guard: incoming ref SHA unreadable; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                };
                                if !git_dir_commit_present(&git_dir, &incoming) {
                                    warn!(
                                        path = %rel_path,
                                        incoming = %incoming,
                                        "loser-guard: incoming ref object missing; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }

                                let current = git_dir_ref_sha(&git_dir, &ref_name);
                                let mut parked: Option<(String, String, PathBuf)> = None;
                                if let Some(current_sha) = current.as_deref() {
                                    if current_sha != incoming
                                        && !git_dir_is_ancestor(&git_dir, current_sha, &incoming)
                                    {
                                        // Non-ancestor overwrite: committed work at
                                        // `current_sha` is not reachable from
                                        // `incoming`.
                                        if !parkable {
                                            warn!(
                                                path = %rel_path,
                                                r#ref = %ref_name,
                                                "loser-guard: non-parkable module head would be orphaned; deferring pull"
                                            );
                                            result.deferred_git_refs.push(rel_path.clone());
                                            continue;
                                        }
                                        let Some(park_ref) =
                                            conflict_git::theirs_ref_name(device_id, &ref_name)
                                        else {
                                            warn!(
                                                path = %rel_path,
                                                r#ref = %ref_name,
                                                "loser-guard: no safe park ref namespace; deferring pull"
                                            );
                                            result.deferred_git_refs.push(rel_path.clone());
                                            continue;
                                        };
                                        let bundle = match conflict_git::write_undo_bundle(
                                            &repo_root,
                                            &undo_state_dir,
                                        ) {
                                            Ok(bundle) => bundle,
                                            Err(e) => {
                                                warn!(
                                                    path = %rel_path,
                                                    error = %format!("{e:#}"),
                                                    "loser-guard: pre-overwrite bundle failed; deferring pull"
                                                );
                                                result.deferred_git_refs.push(rel_path.clone());
                                                continue;
                                            }
                                        };
                                        let parked_ref = match conflict_git::park_ref_create_only(
                                            &repo_root,
                                            &park_ref,
                                            current_sha,
                                        ) {
                                            Ok(parked_ref) => parked_ref,
                                            Err(e) => {
                                                warn!(
                                                    path = %rel_path,
                                                    error = %format!("{e:#}"),
                                                    "loser-guard: park failed; deferring pull"
                                                );
                                                result.deferred_git_refs.push(rel_path.clone());
                                                continue;
                                            }
                                        };
                                        parked =
                                            Some((parked_ref, current_sha.to_string(), bundle));
                                    }
                                }

                                if let Err(e) = git_dir_update_ref_cas(
                                    &git_dir,
                                    &ref_name,
                                    &incoming,
                                    current.as_deref(),
                                ) {
                                    warn!(
                                        path = %rel_path,
                                        r#ref = %ref_name,
                                        error = %format!("{e:#}"),
                                        "loser-guard: ref moved before CAS update; deferring pull"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                                if let Err(e) = record_guarded_ref_pull_state(
                                    op,
                                    remote_prefix,
                                    manifest_hash,
                                    &local_path,
                                    &incoming_bytes,
                                    state,
                                    device_id,
                                )
                                .await
                                {
                                    result.errors.push((
                                        rel_path.clone(),
                                        format!("guarded ref pull state update failed: {e:#}"),
                                    ));
                                    continue;
                                }
                                if let Some((parked_ref, orphaned_sha, bundle)) = parked {
                                    info!(
                                        path = %rel_path,
                                        r#ref = %ref_name,
                                        parked = %parked_ref,
                                        orphaned_sha = %orphaned_sha,
                                        bundle = %bundle.display(),
                                        "loser-guard: bundled pre-overwrite .git + parked local head; applied CAS ref pull"
                                    );
                                }
                                result.pulled += 1;
                                result.bytes_downloaded += incoming_bytes.len() as u64;
                                continue;
                            }
                        }
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
                        mark_git_object_failure(rel_path, local_root, &mut git_object_failed_repos);
                        result
                            .errors
                            .push((rel_path.clone(), format!("pull failed: {e:#}")));
                    }
                }
            }

            ReconcileAction::DeleteLocal {
                local_path,
                rel_path,
            } => {
                if is_git_ref_class_path(rel_path) {
                    match loser_guard_ref_target(local_root, rel_path) {
                        Some(LoserGuardTarget::PackedRefs { .. }) => {
                            warn!(
                                path = %rel_path,
                                "loser-guard: packed-refs delete is opaque; deferring local delete"
                            );
                            result.deferred_git_refs.push(rel_path.clone());
                            continue;
                        }
                        Some(LoserGuardTarget::Ref {
                            git_dir,
                            repo_root,
                            ref_name,
                            parkable,
                        }) => {
                            if !parkable {
                                warn!(
                                    path = %rel_path,
                                    r#ref = %ref_name,
                                    "loser-guard: non-parkable ref delete would orphan committed work; deferring local delete"
                                );
                                result.deferred_git_refs.push(rel_path.clone());
                                continue;
                            }
                            let Some(current) = git_dir_ref_sha(&git_dir, &ref_name) else {
                                warn!(
                                    path = %rel_path,
                                    r#ref = %ref_name,
                                    "loser-guard: local ref delete target is unresolved; deferring local delete"
                                );
                                result.deferred_git_refs.push(rel_path.clone());
                                continue;
                            };
                            let Some(park_ref) =
                                conflict_git::theirs_ref_name(device_id, &ref_name)
                            else {
                                warn!(
                                    path = %rel_path,
                                    r#ref = %ref_name,
                                    "loser-guard: no safe park ref namespace; deferring local delete"
                                );
                                result.deferred_git_refs.push(rel_path.clone());
                                continue;
                            };
                            let bundle = match conflict_git::write_undo_bundle(
                                &repo_root,
                                &undo_state_dir,
                            ) {
                                Ok(bundle) => bundle,
                                Err(e) => {
                                    warn!(
                                        path = %rel_path,
                                        error = %format!("{e:#}"),
                                        "loser-guard: pre-delete bundle failed; deferring local delete"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                            };
                            let parked_ref = match conflict_git::park_ref_create_only(
                                &repo_root, &park_ref, &current,
                            ) {
                                Ok(parked_ref) => parked_ref,
                                Err(e) => {
                                    warn!(
                                        path = %rel_path,
                                        error = %format!("{e:#}"),
                                        "loser-guard: pre-delete park failed; deferring local delete"
                                    );
                                    result.deferred_git_refs.push(rel_path.clone());
                                    continue;
                                }
                            };
                            info!(
                                path = %rel_path,
                                r#ref = %ref_name,
                                parked = %parked_ref,
                                orphaned_sha = %current,
                                bundle = %bundle.display(),
                                "loser-guard: bundled pre-delete .git + parked local ref; applying CAS local delete"
                            );
                            if let Err(e) = git_dir_delete_ref_cas(&git_dir, &ref_name, &current) {
                                warn!(
                                    path = %rel_path,
                                    r#ref = %ref_name,
                                    error = %format!("{e:#}"),
                                    "loser-guard: ref moved before CAS delete; deferring local delete"
                                );
                                result.deferred_git_refs.push(rel_path.clone());
                                continue;
                            }
                            state.remove(local_path);
                            result.deleted_local += 1;
                            continue;
                        }
                        None => {
                            warn!(
                                path = %rel_path,
                                "loser-guard: unclassified ref-class delete; deferring local delete"
                            );
                            result.deferred_git_refs.push(rel_path.clone());
                            continue;
                        }
                    }
                }
                match tokio::fs::remove_file(local_path).await {
                    Ok(()) => {
                        state.remove(local_path);
                        result.deleted_local += 1;
                    }
                    Err(e) => {
                        result
                            .errors
                            .push((rel_path.clone(), format!("local delete failed: {e}")));
                    }
                }
            }

            ReconcileAction::DeleteRemote { rel_path } => {
                if is_git_ref_class_path(rel_path) {
                    warn!(
                        path = %rel_path,
                        "git guard: remote ref-class delete cannot be parked locally; deferring remote delete"
                    );
                    result.deferred_git_refs.push(rel_path.clone());
                    continue;
                }
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
                // Record conflict in state cache for later resolution.
                //
                // keep-both PR-1: this arm re-records the same conflict every
                // reconcile cycle (the record-only, never-converges arm for
                // diverged `.git` groups). Bump `times_recorded` and preserve
                // the original `detected_at` across cycles instead of
                // overwriting them, so `tcfs conflicts` can surface how long /
                // how many cycles a conflict has persisted.
                if let Some((key, existing)) = state.get_by_rel_path(rel_path) {
                    let key_owned = key.to_string();
                    let mut updated = existing.clone();
                    let mut info = info.clone();
                    if let Some(prior) = updated.conflict.as_ref() {
                        info.times_recorded = prior.times_recorded.saturating_add(1);
                        info.detected_at = prior.detected_at;
                    } else {
                        info.times_recorded = 1;
                    }
                    // keep-both PR-2: `info.remote_manifest_key` was populated at
                    // classification (`outcome_to_action`), the only site where the
                    // remote manifest storage key is in scope; it rides through here
                    // unchanged so the recorded conflict carries the key a future
                    // PR-3 resolve verb needs to fetch the remote ref SHA.
                    updated.conflict = Some(info);
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
        deferred_git_refs = result.deferred_git_refs.len(),
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

    // Objects-before-refs BARRIER: a repo whose wave-0 `.git` pulls had any
    // failure must not have its wave-1 ref-class pulls applied this run — a
    // ref pointing at an object that never landed corrupts the repo. Deferred
    // refs are recorded (not errored) and re-planned next cycle.
    let mut wave0_failed_git_repos: BTreeSet<PathBuf> = BTreeSet::new();

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

            if wave && !wave0_failed_git_repos.is_empty() {
                if let Some(root) = git_safety::repo_root_for_git_path(local_root, rel_path) {
                    if wave0_failed_git_repos.contains(&root) {
                        info!(
                            path = %rel_path,
                            "git barrier: wave-0 pull failed in this repo; deferring ref pull"
                        );
                        result.deferred_git_refs.push(rel_path.clone());
                        continue;
                    }
                }
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
                    if !wave && is_git_internal_path(&rel_path) {
                        if let Some(root) =
                            git_safety::repo_root_for_git_path(local_root, &rel_path)
                        {
                            wave0_failed_git_repos.insert(root);
                        }
                    }
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

    // ── M-4 (PR #513): submodule gitdir class ordering ───────────────────────

    #[test]
    fn test_submodule_gitdir_paths_classify() {
        // Submodule internals live under `.git/modules/<name>/**`. They must get
        // the same object/ref class treatment as a top-level gitdir so the
        // objects-before-refs ordering + barrier apply to raw-roamed submodules.
        assert!(is_git_ref_class_path(
            "repo/.git/modules/dep/refs/heads/main"
        ));
        assert!(is_git_ref_class_path("repo/.git/modules/dep/refs/tags/v1"));
        assert!(is_git_ref_class_path("repo/.git/modules/dep/packed-refs"));
        assert!(is_git_ref_class_path("repo/.git/modules/dep/HEAD"));
        // Nested submodule (`<name>` contains a further `modules/` segment).
        assert!(is_git_ref_class_path(
            "repo/.git/modules/a/modules/b/refs/heads/main"
        ));

        assert!(is_git_object_class_path(
            "repo/.git/modules/dep/objects/ab/cdef0123456789"
        ));
        assert!(is_git_object_class_path(
            "repo/.git/modules/dep/objects/pack/pack-abc.pack"
        ));

        // A submodule ref is NOT object-class, and a submodule object is NOT
        // ref-class (the two axes stay distinct).
        assert!(!is_git_object_class_path(
            "repo/.git/modules/dep/refs/heads/main"
        ));
        assert!(!is_git_ref_class_path(
            "repo/.git/modules/dep/objects/ab/cdef"
        ));

        // Top-level gitdir classification is unchanged.
        assert!(is_git_ref_class_path("repo/.git/refs/heads/main"));
        assert!(is_git_object_class_path("repo/.git/objects/ab/cdef"));
        // A submodule working-tree source file is neither class.
        assert!(!is_git_ref_class_path("repo/dep/src/main.rs"));
        assert!(!is_git_object_class_path("repo/dep/src/main.rs"));
    }

    // ── BLOCKER-1 + BLOCKER-3 (PR #513): fast-forward veto predicate ──────────

    /// The exact predicate `decide_repo_fast_forward` fails closed on: any
    /// ref-class `.git` path that is NOT a provable top-level branch head. This
    /// mirrors the veto call site (`is_git_ref_class_path(p) &&
    /// head_ref_for_git_path(p).is_none()`), so a group containing any such path
    /// stays Conflict with zero writes.
    fn ff_group_vetoes(rel: &str) -> bool {
        is_git_ref_class_path(rel) && git_safety::head_ref_for_git_path(rel).is_none()
    }

    #[test]
    fn test_ff_veto_ref_class_predicate() {
        // Top-level non-head ref-class paths → veto (no ancestry proof covers
        // them): packed-refs, tags, stash, remotes.
        assert!(ff_group_vetoes("repo/.git/packed-refs"));
        assert!(ff_group_vetoes("repo/.git/refs/tags/v1"));
        assert!(ff_group_vetoes("repo/.git/refs/stash"));
        assert!(ff_group_vetoes("repo/.git/refs/remotes/origin/main"));
        // A detached / divergent top-level HEAD (raw SHA) → veto (closes the
        // detached-HEAD clobber variant).
        assert!(ff_group_vetoes("repo/.git/HEAD"));

        // BLOCKER-3: EVERY submodule ref-class path under `.git/modules/<name>/`
        // → veto. `repo_root_for_git_path` groups these under the outer repo, so
        // without this they would ride the outer head's FF dominance unproven.
        assert!(ff_group_vetoes("repo/.git/modules/dep/refs/heads/main"));
        assert!(ff_group_vetoes("repo/.git/modules/dep/refs/tags/v1"));
        assert!(ff_group_vetoes("repo/.git/modules/dep/packed-refs"));
        assert!(ff_group_vetoes("repo/.git/modules/dep/refs/stash"));
        assert!(ff_group_vetoes("repo/.git/modules/dep/HEAD"));
        // Submodule reflogs are ref-class here too → over-veto, but fail-closed
        // and safe (the divergent submodule pointer already vetoes the group).
        assert!(ff_group_vetoes(
            "repo/.git/modules/a/modules/b/refs/heads/main"
        ));

        // Provable top-level branch heads → do NOT veto (the ancestry proof
        // covers exactly these).
        assert!(!ff_group_vetoes("repo/.git/refs/heads/main"));
        assert!(!ff_group_vetoes("repo/.git/refs/heads/feature/x"));

        // Workdir / index / head-following reflog state → NOT ref-class, so they
        // keep riding the group decision (never veto).
        assert!(!ff_group_vetoes("repo/.git/index"));
        assert!(!ff_group_vetoes("repo/.git/logs/HEAD"));
        assert!(!ff_group_vetoes("repo/.git/logs/refs/heads/main"));
        // Object data and non-git paths → never veto.
        assert!(!ff_group_vetoes("repo/.git/objects/ab/cdef"));
        assert!(!ff_group_vetoes("repo/src/refs.rs"));
    }

    // ── keep-both PR-4 (S10): loser-side no-loss guard ─────────────────────

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git_safety::run_git(path, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(path, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(path, &["config", "user.name", "TCFS Test"]).unwrap();
    }

    fn commit_file(path: &Path, file: &str, body: &str, msg: &str) -> String {
        std::fs::write(path.join(file), body.as_bytes()).unwrap();
        git_safety::run_git(path, &["add", file]).unwrap();
        git_safety::run_git(path, &["commit", "-m", msg, "--quiet"]).unwrap();
        git_safety::local_ref_sha(path, "HEAD").unwrap()
    }

    async fn upload_ref_blob(op: &Operator, root: &Path, sha: &str) -> String {
        upload_blob(
            op,
            root,
            "remote-ref",
            format!("{sha}\n").as_bytes(),
            "repo/.git/refs/heads/main",
        )
        .await
    }

    async fn upload_blob(
        op: &Operator,
        root: &Path,
        file_name: &str,
        bytes: &[u8],
        rel_path: &str,
    ) -> String {
        let ref_blob = root.join(file_name);
        std::fs::write(&ref_blob, bytes).unwrap();
        let mut state = crate::state::StateCache::open(&root.join("upload-state.json")).unwrap();
        engine::upload_file_with_device(
            op,
            &ref_blob,
            "data",
            &mut state,
            None,
            "remote",
            Some(rel_path),
            None,
        )
        .await
        .unwrap();
        // The pull's manifest_hash is the remote index's content-addressed
        // manifest key (NOT UploadResult.hash, which is the file-content hash);
        // the guard downloads the ref blob by exactly this key. Look it up by
        // the uploaded `rel_path` so callers uploading under non-head paths
        // (packed-refs, module refs) get their own entry, not a hardcoded one.
        list_remote_index(op, "data")
            .await
            .unwrap()
            .get(rel_path)
            .expect("uploaded ref index entry")
            .manifest_hash
            .clone()
    }

    #[test]
    fn loser_guard_target_catches_symbolic_and_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let head = commit_file(&repo, "file.txt", "base", "base");

        let target = loser_guard_ref_target(local_root, "repo/.git/HEAD")
            .expect("symbolic HEAD must be guarded so detached overwrites defer");
        match target {
            LoserGuardTarget::Ref {
                ref_name, parkable, ..
            } => {
                assert_eq!(ref_name, "HEAD");
                assert!(!parkable, "HEAD is defer-only");
            }
            LoserGuardTarget::PackedRefs { .. } => panic!("HEAD must classify as a ref target"),
        }

        git_safety::run_git(&repo, &["checkout", "--quiet", "--detach", &head]).unwrap();
        let target = loser_guard_ref_target(local_root, "repo/.git/HEAD")
            .expect("detached HEAD must be guarded");
        match target {
            LoserGuardTarget::Ref {
                ref_name, parkable, ..
            } => {
                assert_eq!(ref_name, "HEAD");
                assert!(!parkable, "detached HEAD is defer-only");
            }
            LoserGuardTarget::PackedRefs { .. } => panic!("HEAD must classify as a ref target"),
        }
    }

    #[test]
    fn loser_guard_uses_exact_git_path_component() {
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("foo.git");
        init_git_repo(&repo);
        let head = commit_file(&repo, "file.txt", "base", "base");
        git_safety::run_git(&repo, &["checkout", "--quiet", "--detach", &head]).unwrap();

        let target = loser_guard_ref_target(local_root, "foo.git/.git/HEAD")
            .expect("repo directory names ending in .git must not confuse guard parsing");
        match target {
            LoserGuardTarget::Ref {
                git_dir, ref_name, ..
            } => {
                assert_eq!(git_dir, repo.join(".git"));
                assert_eq!(ref_name, "HEAD");
            }
            LoserGuardTarget::PackedRefs { .. } => panic!("HEAD must classify as a ref target"),
        }
    }

    #[tokio::test]
    async fn loser_guard_parks_divergent_local_head_before_ref_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        let local_head = commit_file(&repo, "file.txt", "local work", "local");
        assert_ne!(base, local_head);

        let manifest_hash = upload_ref_blob(&op, local_root, &base).await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: "repo/.git/refs/heads/main".to_string(),
            manifest_hash,
            size: 41,
            reason: PullReason::RemoteNewer,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert!(result.deferred_git_refs.is_empty(), "{result:?}");
        assert_eq!(result.pulled, 1);
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/heads/main").as_deref(),
            Some(base.as_str()),
            "remote pull overwrites the live branch only after parking"
        );
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/tcfs/theirs/neo/heads/main").as_deref(),
            Some(local_head.as_str()),
            "loser's previous head remains reachable under its own device namespace"
        );
        assert!(
            state_path.parent().unwrap().join("keep-both-undo").exists(),
            "pre-overwrite undo bundle lives under the state dir"
        );
        git_safety::run_git(&repo, &["fsck", "--full"]).unwrap();
    }

    #[tokio::test]
    async fn loser_guard_defers_divergent_submodule_head_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        let current = commit_file(&repo, "file.txt", "local work", "local");
        git_safety::run_git(&repo, &["checkout", "--quiet", "-b", "incoming", &base]).unwrap();
        let incoming = commit_file(&repo, "file.txt", "incoming work", "incoming");
        git_safety::run_git(&repo, &["checkout", "--quiet", "main"]).unwrap();

        let module_git = repo.join(".git/modules/dep");
        std::fs::create_dir_all(repo.join(".git/modules")).unwrap();
        git_safety::run_git(
            &repo,
            &["clone", "--quiet", "--bare", ".", ".git/modules/dep"],
        )
        .unwrap();

        let manifest_hash = upload_ref_blob(&op, local_root, &incoming).await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let rel_path = "repo/.git/modules/dep/refs/heads/main";
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: 41,
            reason: PullReason::RemoteNewer,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(
            result.deferred_git_refs,
            vec![rel_path.to_string()],
            "module-gitdir heads are not silently overwritten until module parking exists"
        );
        assert_eq!(
            git_dir_ref_sha(&module_git, "refs/heads/main").as_deref(),
            Some(current.as_str())
        );
    }

    #[tokio::test]
    async fn loser_guard_defers_divergent_non_head_ref_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        let current = commit_file(&repo, "file.txt", "tagged work", "tagged");
        git_safety::run_git(&repo, &["tag", "v1", &current]).unwrap();
        git_safety::run_git(&repo, &["checkout", "--quiet", "-b", "incoming", &base]).unwrap();
        let incoming = commit_file(&repo, "file.txt", "incoming work", "incoming");
        git_safety::run_git(&repo, &["checkout", "--quiet", "main"]).unwrap();

        let manifest_hash = upload_ref_blob(&op, local_root, &incoming).await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let rel_path = "repo/.git/refs/tags/v1";
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: 41,
            reason: PullReason::RemoteNewer,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(
            result.deferred_git_refs,
            vec![rel_path.to_string()],
            "non-head refs that point at local-only commits must defer, not overwrite"
        );
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/tags/v1").as_deref(),
            Some(current.as_str())
        );
    }

    #[tokio::test]
    async fn loser_guard_defers_top_level_packed_refs_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        git_safety::run_git(&repo, &["checkout", "--quiet", "-b", "incoming", &base]).unwrap();
        let incoming = commit_file(&repo, "file.txt", "incoming work", "incoming");
        git_safety::run_git(&repo, &["checkout", "--quiet", "main"]).unwrap();

        let packed = repo.join(".git/packed-refs");
        let local_bytes =
            format!("# pack-refs with: peeled fully-peeled sorted\n{base} refs/heads/main\n");
        let remote_bytes =
            format!("# pack-refs with: peeled fully-peeled sorted\n{incoming} refs/heads/main\n");
        std::fs::write(&packed, local_bytes.as_bytes()).unwrap();

        let rel_path = "repo/.git/packed-refs";
        let manifest_hash = upload_blob(
            &op,
            local_root,
            "remote-packed-refs",
            remote_bytes.as_bytes(),
            rel_path,
        )
        .await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: remote_bytes.len() as u64,
            reason: PullReason::RemoteNewer,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(
            result.deferred_git_refs,
            vec![rel_path.to_string()],
            "packed-refs is opaque and must not be overwritten when bytes differ"
        );
        assert_eq!(std::fs::read(&packed).unwrap(), local_bytes.as_bytes());
    }

    #[tokio::test]
    async fn loser_guard_defers_submodule_packed_refs_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        let incoming = commit_file(&repo, "file.txt", "incoming work", "incoming");

        let packed = repo.join(".git/modules/dep/packed-refs");
        std::fs::create_dir_all(packed.parent().unwrap()).unwrap();
        let local_bytes =
            format!("# pack-refs with: peeled fully-peeled sorted\n{base} refs/heads/main\n");
        let remote_bytes =
            format!("# pack-refs with: peeled fully-peeled sorted\n{incoming} refs/heads/main\n");
        std::fs::write(&packed, local_bytes.as_bytes()).unwrap();

        let rel_path = "repo/.git/modules/dep/packed-refs";
        let manifest_hash = upload_blob(
            &op,
            local_root,
            "remote-module-packed-refs",
            remote_bytes.as_bytes(),
            rel_path,
        )
        .await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: remote_bytes.len() as u64,
            reason: PullReason::RemoteNewer,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(
            result.deferred_git_refs,
            vec![rel_path.to_string()],
            "submodule packed-refs is opaque and must defer on byte differences"
        );
        assert_eq!(std::fs::read(&packed).unwrap(), local_bytes.as_bytes());
    }

    #[tokio::test]
    async fn loser_guard_parks_branch_before_local_delete() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let side = commit_file(&repo, "file.txt", "side work", "side");
        git_safety::run_git(&repo, &["branch", "side", &side]).unwrap();

        let rel_path = "repo/.git/refs/heads/side";
        let local_path = local_root.join(rel_path);
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::DeleteLocal {
            local_path: local_path.clone(),
            rel_path: rel_path.to_string(),
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert!(result.deferred_git_refs.is_empty(), "{result:?}");
        assert_eq!(result.deleted_local, 1);
        assert!(!local_path.exists(), "the loose branch ref was deleted");
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/tcfs/theirs/neo/heads/side").as_deref(),
            Some(side.as_str()),
            "local branch tip remains reachable before delete"
        );
        assert!(
            state_path.parent().unwrap().join("keep-both-undo").exists(),
            "pre-delete undo bundle lives under the state dir"
        );
        git_safety::run_git(&repo, &["fsck", "--full"]).unwrap();
    }

    #[tokio::test]
    async fn loser_guard_defers_non_head_ref_local_delete() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let tagged = commit_file(&repo, "file.txt", "tagged work", "tagged");
        git_safety::run_git(&repo, &["tag", "v1", &tagged]).unwrap();

        let rel_path = "repo/.git/refs/tags/v1";
        let local_path = local_root.join(rel_path);
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::DeleteLocal {
            local_path: local_path.clone(),
            rel_path: rel_path.to_string(),
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(result.deferred_git_refs, vec![rel_path.to_string()]);
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/tags/v1").as_deref(),
            Some(tagged.as_str())
        );
        assert!(local_path.exists(), "non-parkable ref delete must defer");
    }

    #[tokio::test]
    async fn loser_guard_defers_packed_refs_local_delete() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let head = commit_file(&repo, "file.txt", "base", "base");
        let rel_path = "repo/.git/packed-refs";
        let local_path = local_root.join(rel_path);
        let packed_bytes =
            format!("# pack-refs with: peeled fully-peeled sorted\n{head} refs/heads/main\n");
        std::fs::write(&local_path, packed_bytes.as_bytes()).unwrap();

        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::DeleteLocal {
            local_path: local_path.clone(),
            rel_path: rel_path.to_string(),
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(result.deferred_git_refs, vec![rel_path.to_string()]);
        assert_eq!(std::fs::read(&local_path).unwrap(), packed_bytes.as_bytes());
    }

    #[tokio::test]
    async fn loser_guard_defers_new_ref_when_incoming_object_missing() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        commit_file(&repo, "file.txt", "base", "base");

        let missing = "0123456789abcdef0123456789abcdef01234567";
        let rel_path = "repo/.git/refs/heads/missing";
        let manifest_hash = upload_blob(
            &op,
            local_root,
            "remote-missing-ref",
            format!("{missing}\n").as_bytes(),
            rel_path,
        )
        .await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: 41,
            reason: PullReason::NewRemote,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(
            result.deferred_git_refs,
            vec![rel_path.to_string()],
            "new local refs still need a present incoming commit object"
        );
        assert_eq!(result.pulled, 0);
        assert!(git_safety::local_ref_sha(&repo, "refs/heads/missing").is_none());
    }

    #[tokio::test]
    async fn loser_guard_defers_refs_tcfs_when_incoming_object_missing() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        commit_file(&repo, "file.txt", "base", "base");

        let missing = "fedcba9876543210fedcba9876543210fedcba98";
        let rel_path = "repo/.git/refs/tcfs/theirs/honey/heads/main";
        let manifest_hash = upload_blob(
            &op,
            local_root,
            "remote-tcfs-ref",
            format!("{missing}\n").as_bytes(),
            rel_path,
        )
        .await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: 41,
            reason: PullReason::NewRemote,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(result.deferred_git_refs, vec![rel_path.to_string()]);
        assert_eq!(result.pulled, 0);
        assert!(
            !local_root.join(rel_path).exists(),
            "refs/tcfs/** must not raw-materialize broken ref content"
        );
    }

    #[tokio::test]
    async fn loser_guard_defers_symbolic_head_to_detached_head_pull() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        git_safety::run_git(&repo, &["checkout", "--quiet", "-b", "incoming", &base]).unwrap();
        let incoming = commit_file(&repo, "file.txt", "incoming", "incoming");
        git_safety::run_git(&repo, &["checkout", "--quiet", "main"]).unwrap();
        assert!(std::fs::read_to_string(repo.join(".git/HEAD"))
            .unwrap()
            .starts_with("ref:"));

        let rel_path = "repo/.git/HEAD";
        let manifest_hash = upload_blob(
            &op,
            local_root,
            "remote-detached-head",
            format!("{incoming}\n").as_bytes(),
            rel_path,
        )
        .await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: 41,
            reason: PullReason::RemoteNewer,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(result.deferred_git_refs, vec![rel_path.to_string()]);
        assert!(
            std::fs::read_to_string(repo.join(".git/HEAD"))
                .unwrap()
                .starts_with("ref:"),
            "symbolic HEAD must not be raw-overwritten into detached HEAD"
        );
    }

    #[tokio::test]
    async fn loser_guard_defers_new_packed_refs_when_objects_missing() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_git_repo(&repo);
        commit_file(&repo, "file.txt", "base", "base");

        let missing = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let rel_path = "repo/.git/packed-refs";
        let packed_bytes =
            format!("# pack-refs with: peeled fully-peeled sorted\n{missing} refs/heads/missing\n");
        let manifest_hash = upload_blob(
            &op,
            local_root,
            "remote-packed-missing",
            packed_bytes.as_bytes(),
            rel_path,
        )
        .await;
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let plan = plan_of(vec![ReconcileAction::Pull {
            rel_path: rel_path.to_string(),
            manifest_hash,
            size: packed_bytes.len() as u64,
            reason: PullReason::NewRemote,
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(result.deferred_git_refs, vec![rel_path.to_string()]);
        assert!(
            !local_root.join(rel_path).exists(),
            "new packed-refs must not materialize before its objects are present"
        );
    }

    #[test]
    fn guarded_ref_pull_cas_rejects_moved_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        git_safety::run_git(&repo, &["checkout", "--quiet", "-b", "incoming", &base]).unwrap();
        let incoming = commit_file(&repo, "file.txt", "incoming", "incoming");
        git_safety::run_git(&repo, &["checkout", "--quiet", "main"]).unwrap();
        let local = commit_file(&repo, "file.txt", "local", "local");

        let err = git_dir_update_ref_cas(
            &repo.join(".git"),
            "refs/heads/main",
            &incoming,
            Some(&base),
        )
        .unwrap_err();
        assert!(err.to_string().contains("CAS failed"));
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/heads/main").as_deref(),
            Some(local.as_str())
        );
    }

    #[test]
    fn guarded_ref_delete_cas_rejects_moved_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo);
        let base = commit_file(&repo, "file.txt", "base", "base");
        git_safety::run_git(&repo, &["branch", "side", &base]).unwrap();
        let local = commit_file(&repo, "file.txt", "local", "local");
        git_safety::run_git(&repo, &["branch", "-f", "side", &local]).unwrap();

        let err = git_dir_delete_ref_cas(&repo.join(".git"), "refs/heads/side", &base).unwrap_err();
        assert!(err.to_string().contains("delete CAS failed"));
        assert_eq!(
            git_safety::local_ref_sha(&repo, "refs/heads/side").as_deref(),
            Some(local.as_str())
        );
    }

    #[tokio::test]
    async fn git_ref_class_remote_delete_defers() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let state_path = dir.path().join("state/state.json");
        let mut state = crate::state::StateCache::open(&state_path).unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let plan = plan_of(vec![ReconcileAction::DeleteRemote {
            rel_path: rel_path.to_string(),
        }]);

        let result = execute_plan(
            &plan, &op, local_root, "data", &mut state, "neo", None, None,
        )
        .await
        .unwrap();

        assert_eq!(result.deferred_git_refs, vec![rel_path.to_string()]);
        assert_eq!(result.deleted_remote, 0);
        assert!(result.errors.is_empty(), "{result:?}");
    }

    // ── keep-both PR-2 (S3): executor hard-respects a foreign `.git/tcfs.lock` ──

    /// A `.git` Push action for `rel` (the class of the path is what matters to
    /// the lock gate; `local_path`/reason are inert here).
    fn git_push(rel: &str) -> ReconcileAction {
        ReconcileAction::Push {
            local_path: PathBuf::from(rel),
            rel_path: rel.to_string(),
            reason: PushReason::LocalNewer,
        }
    }

    fn git_pull_new_remote(rel: &str) -> ReconcileAction {
        ReconcileAction::Pull {
            rel_path: rel.to_string(),
            manifest_hash: "remote-manifest".to_string(),
            size: 41,
            reason: PullReason::NewRemote,
        }
    }

    fn plan_of(actions: Vec<ReconcileAction>) -> ReconcilePlan {
        ReconcilePlan {
            actions,
            summary: ReconcileSummary::default(),
            device_id: "test-device".to_string(),
            generated_at: 0,
        }
    }

    /// Backdate a lock file's mtime past the staleness threshold, so the ONLY
    /// variable between the live-owner and dead-owner tests is owner liveness.
    fn age_lock(lock_path: &Path) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let f = std::fs::File::options()
            .write(true)
            .open(lock_path)
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
    }

    /// (test a) A FOREIGN live holder of `.git/tcfs.lock` forces this repo's
    /// ref-class `.git` writes to DEFER, while object-class and normal-file
    /// writes are unaffected. Fail-before: without the foreign-lock plumbing the
    /// executor took no note of the held lock and wrote refs anyway.
    #[test]
    fn foreign_live_lock_defers_ref_class_writes_only() {
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let git_dir = local_root.join("repo/.git");
        std::fs::create_dir_all(&git_dir).unwrap();

        // Simulate a live FOREIGN holder: lock owned by PID 1 (init/launchd —
        // always alive, genuinely not this process), backdated past staleness so
        // youth is not what keeps it held. A live owner is never stolen.
        let lock_path = git_dir.join("tcfs.lock");
        std::fs::write(&lock_path, "1 0\n").unwrap();
        age_lock(&lock_path);

        let plan = plan_of(vec![
            git_push("repo/.git/refs/heads/main"), // ref-class → defers
            git_push("repo/.git/objects/ab/cdef01234567"), // object-class → runs
            git_push("repo/notes.txt"),            // normal file → runs
        ]);

        let acq = acquire_git_locks_for_plan(&plan, local_root);
        let repo_root = local_root.join("repo");

        assert!(
            acq.foreign_locked_repos.contains(&repo_root),
            "a live foreign tcfs.lock holder must mark the repo foreign-locked"
        );
        assert!(
            acq.guards.is_empty(),
            "no guard is acquired while a foreign holder lives"
        );
        assert!(
            lock_path.exists(),
            "the foreign holder's lock must be left in place"
        );

        // Only ref-class paths in the foreign-locked repo defer.
        assert!(
            git_ref_foreign_lock_hit(
                "repo/.git/refs/heads/main",
                local_root,
                &acq.foreign_locked_repos
            ),
            "ref-class write must defer under a foreign lock"
        );
        assert!(
            !git_ref_foreign_lock_hit(
                "repo/.git/objects/ab/cdef01234567",
                local_root,
                &acq.foreign_locked_repos
            ),
            "object-class write proceeds (content-addressed, never conflicts)"
        );
        assert!(
            !git_ref_foreign_lock_hit("repo/notes.txt", local_root, &acq.foreign_locked_repos),
            "normal-file write proceeds"
        );
    }

    /// (test c) The all-NewRemote concurrent pull fast path returns before
    /// normal execute-plan lock acquisition. It must therefore refuse plans
    /// containing ref-class `.git` pulls and let the main executor path apply
    /// the foreign-lock gate.
    #[test]
    fn new_remote_ref_class_pull_disables_pre_lock_fast_path() {
        let normal_plan = plan_of(vec![
            git_pull_new_remote("repo/.git/objects/ab/cdef01234567"),
            ReconcileAction::CreateDirectory {
                rel_path: "repo/.git/objects/ab".to_string(),
            },
        ]);
        assert!(
            new_remote_pull_fast_path_safe(&normal_plan),
            "object-class new-remote pulls may still use the concurrent fast path"
        );

        let ref_plan = plan_of(vec![git_pull_new_remote("repo/.git/refs/heads/main")]);
        assert!(
            !new_remote_pull_fast_path_safe(&ref_plan),
            "ref-class new-remote pulls must go through the locked executor path"
        );

        let submodule_ref_plan = plan_of(vec![git_pull_new_remote(
            "repo/.git/modules/sub/refs/heads/main",
        )]);
        assert!(
            !new_remote_pull_fast_path_safe(&submodule_ref_plan),
            "submodule ref-class pulls must also go through the locked executor path"
        );
    }

    /// (test d) Ref-class deletes are writes too: a plan containing only a
    /// delete must still discover a live foreign `.git/tcfs.lock` holder so the
    /// main executor can defer it instead of removing the ref through the lock.
    #[test]
    fn foreign_lock_acquisition_covers_ref_class_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let git_dir = local_root.join("repo/.git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let lock_path = git_dir.join("tcfs.lock");
        std::fs::write(&lock_path, "1 0\n").unwrap();
        age_lock(&lock_path);

        let plan = plan_of(vec![
            ReconcileAction::DeleteLocal {
                local_path: local_root.join("repo/.git/refs/heads/main"),
                rel_path: "repo/.git/refs/heads/main".to_string(),
            },
            ReconcileAction::DeleteRemote {
                rel_path: "repo/.git/packed-refs".to_string(),
            },
        ]);
        let acq = acquire_git_locks_for_plan(&plan, local_root);
        let repo_root = local_root.join("repo");

        assert!(
            acq.foreign_locked_repos.contains(&repo_root),
            "delete-only ref-class plans must still mark the repo foreign-locked"
        );
        assert!(
            git_ref_foreign_lock_hit(
                "repo/.git/refs/heads/main",
                local_root,
                &acq.foreign_locked_repos
            ),
            "local ref delete must defer under a foreign lock"
        );
        assert!(
            git_ref_foreign_lock_hit(
                "repo/.git/packed-refs",
                local_root,
                &acq.foreign_locked_repos
            ),
            "remote packed-refs delete must defer under a foreign lock"
        );
    }

    /// (test b) A STALE lock (dead owner, aged past the threshold) is stolen on
    /// acquire and yields a held guard — a leaked lock must NEVER deadlock this
    /// repo's ref-class writes. Same fixture as (a) but a dead PID.
    #[test]
    fn stale_dead_lock_is_stealable_no_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let git_dir = local_root.join("repo/.git");
        std::fs::create_dir_all(&git_dir).unwrap();

        // Leaked lock: dead owner PID (far past any real pid space), aged past
        // the staleness threshold. Must be stolen, not treated as foreign-held.
        let lock_path = git_dir.join("tcfs.lock");
        std::fs::write(&lock_path, "999999999 0\n").unwrap();
        age_lock(&lock_path);

        let plan = plan_of(vec![git_push("repo/.git/refs/heads/main")]);
        let acq = acquire_git_locks_for_plan(&plan, local_root);
        let repo_root = local_root.join("repo");

        assert!(
            !acq.foreign_locked_repos.contains(&repo_root),
            "a stale dead-owner lock must be stolen, not deferred as foreign-held"
        );
        assert_eq!(
            acq.guards.len(),
            1,
            "stealing the stale lock yields exactly one held guard (no deadlock)"
        );
        assert!(
            !git_ref_foreign_lock_hit(
                "repo/.git/refs/heads/main",
                local_root,
                &acq.foreign_locked_repos
            ),
            "ref-class writes proceed once the stale lock is stolen"
        );
    }
}
