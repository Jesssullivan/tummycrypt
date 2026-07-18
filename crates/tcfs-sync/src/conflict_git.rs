//! Repo-group `.git` conflict resolution helpers.
//!
//! Per-file conflict resolution is intentionally fenced away from `.git`
//! internals. A divergent raw-git repo has to be resolved as one group: inspect
//! the recorded conflicts, park the remote branch heads under a TCFS namespace,
//! verify the repository before/after, then clear the group atomically.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use opendal::Operator;
use uuid::Uuid;

use crate::engine::{self, OptionalEncryption};
use crate::git_safety;
use crate::state::{FileSyncStatus, StateCache};

/// User-visible mode for repo-group keep-both resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitKeepBothMode {
    /// Validate and print the planned operation. No refs or state entries are
    /// mutated.
    DryRun,
    /// Apply the plan after all preconditions pass.
    Execute,
}

impl GitKeepBothMode {
    pub fn is_execute(self) -> bool {
        self == Self::Execute
    }
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = git_safety::sanitized_git_command()
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running sanitized git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "sanitized git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn require_real_git_directory(path: &Path, description: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting {description}: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{description} must be a real directory: {}", path.display());
    }
    Ok(())
}

fn validate_real_git_directory_if_present(path: &Path, description: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("{description} must be a real directory: {}", path.display());
            }
            reject_redirects_under_git_dir(path, description)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting {description}: {}", path.display()));
        }

        let mut stale = state.get(&ref_path).cloned().expect("recorded conflict");
        stale.remote_path = "old-prefix/manifests/head".into();
        state.set(&ref_path, stale);
        let error = collect_repo_conflicts(&state, &repo_root, "data")
            .expect_err("stale-prefix Git conflicts must fail closed");
        assert!(error.to_string().contains("selected storage prefix"));
    }
    Ok(())
}

fn require_real_git_file_if_present(path: &Path, description: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("{description} must be a real file: {}", path.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting {description}: {}", path.display()));
        }
    }
    Ok(())
}

fn reject_redirects_under_git_dir(path: &Path, description: &str) -> Result<()> {
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("reading {description}: {}", path.display()))?
    {
        let entry = entry.with_context(|| format!("reading {description} entry"))?;
        let entry_path = entry.path();
        let metadata = std::fs::symlink_metadata(&entry_path)
            .with_context(|| format!("inspecting {description}: {}", entry_path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "{description} contains a symlink redirect outside the standalone resolver seam: {}",
                entry_path.display()
            );
        }
        if metadata.is_dir() {
            reject_redirects_under_git_dir(&entry_path, description)?;
        } else if !metadata.is_file() {
            bail!(
                "{description} contains a non-regular entry: {}",
                entry_path.display()
            );
        }
    }
    Ok(())
}

/// Prove that Git itself resolves this path as one ordinary standalone
/// worktree with all routing anchored at `<root>/.git`.
///
/// Directory shape alone is insufficient: `commondir`, `core.worktree`,
/// attached worktree administration, alternates, and inherited `GIT_*`
/// variables can redirect otherwise normal-looking commands. Every probe and
/// every resolver command uses the same sanitized command builder.
pub fn validate_standalone_repo_topology(repo_root: &Path) -> Result<()> {
    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("canonicalizing git repo root: {}", repo_root.display()))?;
    if !repo_root.is_dir() {
        bail!("git repo root is not a directory: {}", repo_root.display());
    }

    let git_dir_path = repo_root.join(".git");
    let metadata = std::fs::symlink_metadata(&git_dir_path)
        .with_context(|| format!("inspecting git directory: {}", git_dir_path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "linked or redirected .git metadata is outside the standalone resolver seam: {}",
            git_dir_path.display()
        );
    }
    let git_dir = git_dir_path
        .canonicalize()
        .with_context(|| format!("canonicalizing git directory: {}", git_dir_path.display()))?;

    let refs_dir = git_dir.join("refs");
    require_real_git_directory(&refs_dir, "Git refs directory")?;
    reject_redirects_under_git_dir(&refs_dir, "Git refs directory")?;
    let objects_dir = git_dir.join("objects");
    require_real_git_directory(&objects_dir, "Git objects directory")?;
    reject_redirects_under_git_dir(&objects_dir, "Git objects directory")?;
    validate_real_git_directory_if_present(&git_dir.join("logs"), "Git logs directory")?;
    for (relative, description) in [
        ("HEAD", "Git HEAD"),
        ("config", "Git config"),
        ("config.worktree", "Git worktree config"),
        ("index", "Git index"),
        ("packed-refs", "Git packed refs"),
    ] {
        require_real_git_file_if_present(&git_dir.join(relative), description)?;
    }

    for (path, reason) in [
        (
            git_dir.join("commondir"),
            "git commondir redirects metadata outside a standalone repository",
        ),
        (
            git_dir.join("worktrees"),
            "attached linked-worktree administration is outside this resolver seam",
        ),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => bail!("{reason}: {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspecting {}", path.display()));
            }
        }
    }

    let alternates = git_dir.join("objects/info/alternates");
    match std::fs::symlink_metadata(&alternates) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "git alternates path is redirected or non-regular: {}",
                    alternates.display()
                );
            }
            let contents = std::fs::read_to_string(&alternates)
                .with_context(|| format!("reading git alternates: {}", alternates.display()))?;
            if contents.lines().any(|line| !line.trim().is_empty()) {
                bail!(
                    "external Git object alternates are outside the standalone resolver seam: {}",
                    alternates.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting {}", alternates.display()));
        }
    }

    let effective_config = git_safety::sanitized_git_command()
        .args(["config", "--null", "--name-only", "--list"])
        .current_dir(&repo_root)
        .output()
        .context("inspecting effective Git config for resolver safety")?;
    if !effective_config.status.success() {
        bail!(
            "effective Git config inspection failed: {}",
            String::from_utf8_lossy(&effective_config.stderr)
        );
    }
    for key in effective_config
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = String::from_utf8_lossy(key).to_ascii_lowercase();
        let remote_promisor = key.starts_with("remote.")
            && (key.ends_with(".promisor") || key.ends_with(".partialclonefilter"));
        if key == "extensions.partialclone"
            || remote_promisor
            || key == "protocol.ext.allow"
            || key == "core.alternaterefscommand"
            || key.starts_with("fsck.")
        {
            bail!("effective Git config key {key} is outside the registered-root resolver seam");
        }
    }

    let reported_top = git_output(&repo_root, &["rev-parse", "--show-toplevel"])?;
    let reported_top = PathBuf::from(reported_top)
        .canonicalize()
        .context("canonicalizing Git-reported worktree root")?;
    if reported_top != repo_root {
        bail!(
            "Git reports worktree root {} instead of enrolled root {} (external core.worktree or routing override)",
            reported_top.display(),
            repo_root.display()
        );
    }

    let reported_git_dir = git_output(&repo_root, &["rev-parse", "--absolute-git-dir"])?;
    let reported_git_dir = PathBuf::from(reported_git_dir)
        .canonicalize()
        .context("canonicalizing Git-reported git dir")?;
    if reported_git_dir != git_dir {
        bail!(
            "Git reports git dir {} instead of {}",
            reported_git_dir.display(),
            git_dir.display()
        );
    }

    let reported_common_dir = git_output(
        &repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let reported_common_dir = PathBuf::from(reported_common_dir)
        .canonicalize()
        .context("canonicalizing Git-reported common dir")?;
    if reported_common_dir != git_dir {
        bail!(
            "Git reports common dir {} instead of {}",
            reported_common_dir.display(),
            git_dir.display()
        );
    }

    let is_bare = git_output(&repo_root, &["rev-parse", "--is-bare-repository"])?;
    if is_bare != "false" {
        bail!("bare Git repositories are outside the standalone resolver seam");
    }
    let inside_worktree = git_output(&repo_root, &["rev-parse", "--is-inside-work-tree"])?;
    if inside_worktree != "true" {
        bail!("Git does not report the enrolled root as a working tree");
    }
    Ok(())
}

/// One branch head that would be, or was, parked under `refs/tcfs/theirs/**`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitKeepBothParkedRef {
    pub conflict_rel_path: String,
    pub head_ref: String,
    pub park_ref: String,
    pub local_sha: String,
    pub remote_sha: String,
    pub cache_key: String,
}

/// Outcome from repo-group keep-both resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitKeepBothResult {
    pub repo_root: PathBuf,
    pub mode: GitKeepBothMode,
    pub parked_refs: Vec<GitKeepBothParkedRef>,
    pub undo_bundle: Option<PathBuf>,
}

impl GitKeepBothResult {
    pub fn summary(&self) -> String {
        let action = match self.mode {
            GitKeepBothMode::DryRun => "dry-run",
            GitKeepBothMode::Execute => "executed",
        };
        format!(
            "git keep-both {action}: {} ref(s) for {}",
            self.parked_refs.len(),
            self.repo_root.display()
        )
    }
}

#[derive(Debug, Clone)]
enum GitConflictKind {
    /// A branch-head ref (`.git/refs/heads/**`). Park the peer's head under
    /// `refs/tcfs/theirs/<device>/heads/**` and keep the local head.
    Park {
        head_ref: String,
        remote_manifest_key: String,
        remote_device: String,
    },
    /// A non-ref-class `.git` workdir/reflog path (`.git/index`,
    /// `.git/logs/**`, `.git/COMMIT_EDITMSG`, ...). Design steps 7/9 keep the
    /// winner's local version and clear the conflict; the loser's commits are
    /// preserved by the parked theirs-ref, so the loser's index/reflog need not
    /// be materialized. No ref is touched for these.
    KeepLocal,
}

#[derive(Debug, Clone)]
struct GitConflictCandidate {
    cache_key: String,
    rel_path: String,
    resolved_vclock: crate::conflict::VectorClock,
    kind: GitConflictKind,
}

#[derive(Debug, Clone)]
struct AppliedParkRef {
    ref_name: String,
    previous_sha: Option<String>,
}

/// Resolve a raw `.git` divergent conflict group by parking the remote branch
/// heads under `refs/tcfs/theirs/<device>/heads/**`.
///
/// This PR-3 helper handles the winner-side operation only. It does not create
/// merge commits, push to remote storage, or claim broad daily-driver safety.
/// A later reconcile/upload cycle publishes the updated refs through the normal
/// object-before-ref path.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_repo_keep_both(
    op: &Operator,
    state: &mut StateCache,
    repo_root: &Path,
    remote_prefix: &str,
    device_id: &str,
    undo_state_dir: &Path,
    mode: GitKeepBothMode,
    encryption: OptionalEncryption<'_>,
) -> Result<GitKeepBothResult> {
    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("canonicalizing repo root: {}", repo_root.display()))?;
    let git_dir = repo_root.join(".git");
    validate_standalone_repo_topology(&repo_root)?;

    let candidates = collect_repo_conflicts(state, &repo_root, remote_prefix)?;
    if candidates.is_empty() {
        return Ok(GitKeepBothResult {
            repo_root,
            mode,
            parked_refs: Vec::new(),
            undo_bundle: None,
        });
    }

    let safety = git_safety::git_is_safe(&git_dir);
    if !safety.blocking.is_empty() {
        bail!("git repository is busy: {}", safety.blocking.join("; "));
    }
    ensure_clean_worktree(&repo_root)?;
    run_git(&repo_root, &["fsck", "--full"]).context("pre-resolution git fsck")?;

    let mut parked_refs = Vec::new();
    for candidate in &candidates {
        // KeepLocal candidates (index/logs/COMMIT_EDITMSG) park nothing; the
        // winner's version stays and the conflict clears in the state loop
        // below.
        let GitConflictKind::Park {
            head_ref,
            remote_manifest_key,
            remote_device,
        } = &candidate.kind
        else {
            continue;
        };
        let remote_sha = read_remote_ref_sha(
            op,
            remote_manifest_key,
            remote_prefix,
            device_id,
            encryption,
        )
        .await
        .with_context(|| {
            format!(
                "reading remote ref for {} from {}",
                candidate.rel_path, remote_manifest_key
            )
        })?;
        ensure_commit_present(&repo_root, &remote_sha)?;
        let local_sha = git_safety::local_ref_sha(&repo_root, head_ref)
            .ok_or_else(|| anyhow!("local ref is missing: {}", head_ref))?;
        if local_sha == remote_sha {
            bail!(
                "{} is no longer divergent; rerun reconcile before keep-both",
                head_ref
            );
        }
        let park_ref = park_ref_for_available(&repo_root, remote_device, head_ref, &remote_sha)?;
        parked_refs.push(GitKeepBothParkedRef {
            conflict_rel_path: candidate.rel_path.clone(),
            head_ref: head_ref.clone(),
            park_ref,
            local_sha,
            remote_sha,
            cache_key: candidate.cache_key.clone(),
        });
    }

    if mode == GitKeepBothMode::DryRun {
        return Ok(GitKeepBothResult {
            repo_root,
            mode,
            parked_refs,
            undo_bundle: None,
        });
    }

    let _guard = git_safety::acquire_git_lock(&git_dir)
        .with_context(|| format!("acquiring {}", git_dir.join("tcfs.lock").display()))?;

    // Re-check after acquiring the cooperative lock. The lock protects TCFS
    // peers; a local git command may still have started immediately before the
    // lock landed.
    validate_standalone_repo_topology(&repo_root)
        .context("git topology changed before locked execute")?;
    let safety = git_safety::git_is_safe(&git_dir);
    if !safety.blocking.is_empty() {
        bail!("git repository became busy: {}", safety.blocking.join("; "));
    }
    ensure_clean_worktree(&repo_root)?;
    run_git(&repo_root, &["fsck", "--full"]).context("locked pre-resolution git fsck")?;

    for parked in &parked_refs {
        ensure_local_ref_still_pinned(&repo_root, parked)?;
        ensure_selected_park_ref_available(&repo_root, &parked.park_ref, &parked.remote_sha)?;
    }

    // The undo bundle captures `--all` refs before we park anything. It is only
    // meaningful when refs actually change, so skip it for a pure keep-local
    // group (no write-only cost). It is written under the machine-local state
    // dir, never in-tree (BLOCKING 2 / design S6).
    let undo_bundle = if parked_refs.is_empty() {
        None
    } else {
        Some(write_undo_bundle(&repo_root, undo_state_dir)?)
    };
    let mut applied = Vec::new();
    for parked in &parked_refs {
        if git_safety::local_ref_sha(&repo_root, &parked.park_ref).as_deref()
            == Some(parked.remote_sha.as_str())
        {
            continue;
        }
        let zero = zero_oid_like(&parked.remote_sha);
        let args = [
            "update-ref",
            parked.park_ref.as_str(),
            parked.remote_sha.as_str(),
            zero.as_str(),
        ];
        if let Err(err) = run_git(&repo_root, &args) {
            rollback_refs(&repo_root, &applied);
            return Err(err).with_context(|| format!("parking {}", parked.head_ref));
        }
        applied.push(AppliedParkRef {
            ref_name: parked.park_ref.clone(),
            previous_sha: None,
        });
    }

    if let Err(err) = run_git(&repo_root, &["fsck", "--full"]) {
        rollback_refs(&repo_root, &applied);
        return Err(err).context("post-resolution git fsck");
    }

    let state_snapshot = state.snapshot_cache_keys(
        candidates
            .iter()
            .map(|candidate| candidate.cache_key.as_str()),
    );
    for candidate in &candidates {
        let mut resolved_vclock = candidate.resolved_vclock.clone();
        resolved_vclock.tick(device_id);
        if !state.resolve_conflict_by_cache_key(
            &candidate.cache_key,
            resolved_vclock,
            device_id.to_string(),
        ) {
            rollback_refs(&repo_root, &applied);
            state.restore_cache_key_snapshot(&state_snapshot);
            bail!(
                "state entry vanished while clearing conflict: {}",
                candidate.cache_key
            );
        }
    }
    if let Err(err) = state.flush() {
        rollback_refs(&repo_root, &applied);
        state.restore_cache_key_snapshot(&state_snapshot);
        return Err(err).context("flushing resolved git conflicts");
    }

    Ok(GitKeepBothResult {
        repo_root,
        mode,
        parked_refs,
        undo_bundle,
    })
}

fn object_key_is_within_prefix(key: &str, prefix: &str) -> bool {
    let Some(suffix) = key
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('/'))
    else {
        return false;
    };
    !suffix.is_empty()
        && !suffix.contains('\\')
        && suffix
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn collect_repo_conflicts(
    state: &StateCache,
    repo_root: &Path,
    remote_prefix: &str,
) -> Result<Vec<GitConflictCandidate>> {
    let mut out = Vec::new();
    for (cache_key, entry) in state.conflicts() {
        if entry.status != FileSyncStatus::Conflict {
            continue;
        }
        let Some(conflict) = entry.conflict.as_ref() else {
            continue;
        };
        let rel_path = conflict.rel_path.replace('\\', "/");
        let local_root = local_root_from_cache_key(cache_key, &rel_path);
        let Some(root) = git_safety::repo_root_for_git_path(&local_root, &rel_path) else {
            continue;
        };
        if root.canonicalize().ok().as_deref() != Some(repo_root) {
            continue;
        }
        if !object_key_is_within_prefix(&entry.remote_path, remote_prefix) {
            bail!(
                "git conflict {} has a remote_path outside the selected storage prefix",
                rel_path
            );
        }
        if let Some(remote_manifest_key) = conflict.remote_manifest_key.as_deref() {
            if !object_key_is_within_prefix(remote_manifest_key, remote_prefix) {
                bail!(
                    "git conflict {} has a remote_manifest_key outside the selected storage prefix",
                    rel_path
                );
            }
        }
        let mut resolved_vclock = conflict.local_vclock.clone();
        resolved_vclock.merge(&conflict.remote_vclock);

        let kind = match git_safety::head_ref_for_git_path(&rel_path) {
            // Branch-head ref under `.git/refs/heads/**`: parkable. It needs a
            // PR-2 remote manifest key so we can read the peer's head SHA.
            Some(head_ref) => {
                let Some(remote_manifest_key) = conflict.remote_manifest_key.clone() else {
                    bail!(
                        "{} has no remote_manifest_key; rerun reconcile with a PR-2 binary first",
                        rel_path
                    );
                };
                GitConflictKind::Park {
                    head_ref,
                    remote_manifest_key,
                    remote_device: conflict.remote_device.clone(),
                }
            }
            // Not a branch head. Only GENUINELY UNPARKABLE ref-valued paths veto
            // the whole group: `packed-refs`, `refs/**` outside `refs/heads/`,
            // detached/symbolic `HEAD`, and submodule refs — parking those
            // safely is out of scope for PR-3 and dropping them would lose refs.
            // Non-ref-class workdir/reflog state (`.git/index`, `.git/logs/**`,
            // `.git/COMMIT_EDITMSG`) is design-intended kept-local (steps 7/9),
            // which is what makes the verb usable on real divergent repos where
            // those paths almost always differ.
            None => {
                if crate::reconcile::is_git_ref_class_path(&rel_path) {
                    bail!(
                        "unparkable ref-class .git conflict {}; only branch-head refs \
                         (.git/refs/heads/**) are parkable in PR-3",
                        rel_path
                    );
                }
                GitConflictKind::KeepLocal
            }
        };

        out.push(GitConflictCandidate {
            cache_key: cache_key.to_string(),
            rel_path,
            resolved_vclock,
            kind,
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

fn local_root_from_cache_key(cache_key: &str, rel_path: &str) -> PathBuf {
    let cache_key = cache_key.replace('\\', "/");
    let suffix = rel_path.trim_start_matches('/');
    let root = cache_key
        .strip_suffix(suffix)
        .map(|s| s.trim_end_matches('/'))
        .unwrap_or("");
    PathBuf::from(root)
}

async fn read_remote_ref_sha(
    op: &Operator,
    remote_manifest_key: &str,
    remote_prefix: &str,
    device_id: &str,
    encryption: OptionalEncryption<'_>,
) -> Result<String> {
    let temp_path = std::env::temp_dir().join(format!("tcfs-remote-ref-{}", Uuid::new_v4()));
    let download = engine::download_file_with_device(
        op,
        remote_manifest_key,
        &temp_path,
        remote_prefix,
        None,
        device_id,
        None,
        encryption,
    )
    .await;
    let bytes = match download {
        Ok(_) => std::fs::read(&temp_path)
            .with_context(|| format!("reading downloaded ref {}", temp_path.display()))?,
        Err(err) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err);
        }
    };
    let _ = std::fs::remove_file(&temp_path);
    git_safety::parse_ref_sha(&bytes).ok_or_else(|| anyhow!("remote ref content is not a SHA"))
}

fn park_ref_for(remote_device: &str, head_ref: &str) -> Result<String> {
    let branch = head_ref
        .strip_prefix("refs/heads/")
        .ok_or_else(|| anyhow!("not a branch head ref: {head_ref}"))?;
    let safe_device = remote_device
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    Ok(format!("refs/tcfs/theirs/{safe_device}/heads/{branch}"))
}

fn park_ref_for_available(
    repo_root: &Path,
    remote_device: &str,
    head_ref: &str,
    sha: &str,
) -> Result<String> {
    let base = park_ref_for(remote_device, head_ref)?;
    available_park_ref(repo_root, &base, sha)
}

/// Namespace a ref under `refs/tcfs/theirs/<device>/**` so its target objects
/// become fsck-reachable on this machine and the ref never collides across
/// devices. Reuses `park_ref_for` for branch heads (`refs/heads/<b>` →
/// `.../heads/<b>`) and adds `refs/stash` (`→ .../stash`). Returns `None` for
/// any other ref name (callers must not silently park it).
///
/// Used by the winner-side resolver's park_ref_for path AND by the reconcile
/// loser-side no-loss guard (PR-4, S10), which parks the LOCAL head under its
/// OWN device id before a divergent pull overwrites it.
pub(crate) fn theirs_ref_name(device: &str, ref_name: &str) -> Option<String> {
    if ref_name == "refs/stash" {
        let safe_device = device
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        return Some(format!("refs/tcfs/theirs/{safe_device}/stash"));
    }
    park_ref_for(device, ref_name).ok()
}

/// Create-only park of `sha` at `park_ref` in `repo_root` using the same
/// zero-OID compare-and-swap the winner-side resolver uses: if the base ref
/// already exists at a different sha, choose the documented `-<sha12>` suffix;
/// no-op if the selected ref already points at `sha` (idempotent re-run),
/// otherwise `update-ref <selected_ref> <sha> <zero-oid>`.
pub(crate) fn park_ref_create_only(repo_root: &Path, park_ref: &str, sha: &str) -> Result<String> {
    let park_ref = available_park_ref(repo_root, park_ref, sha)?;
    if git_safety::local_ref_sha(repo_root, &park_ref).as_deref() == Some(sha) {
        return Ok(park_ref);
    }
    let zero = zero_oid_like(sha);
    run_git(repo_root, &["update-ref", &park_ref, sha, zero.as_str()])
        .with_context(|| format!("parking {sha} at {park_ref}"))?;
    Ok(park_ref)
}

fn ensure_clean_worktree(repo_root: &Path) -> Result<()> {
    use std::io::Write;

    let source_git_dir = repo_root.join(".git");
    let replace_refs = git_safety::sanitized_git_command()
        .args(["for-each-ref", "--format=%(refname)", "refs/replace/"])
        .current_dir(repo_root)
        .output()
        .context("inspecting Git replace refs")?;
    if !replace_refs.status.success() {
        bail!(
            "Git replace-ref inspection failed: {}",
            String::from_utf8_lossy(&replace_refs.stderr)
        );
    }
    if !replace_refs.stdout.is_empty() {
        bail!("Git replace refs are outside the registered-root resolver seam");
    }

    let shadow_git_dir = tempfile::Builder::new()
        .prefix("tcfs-git-status-")
        .tempdir()
        .context("creating isolated Git status metadata")?;
    std::fs::create_dir(shadow_git_dir.path().join("objects"))
        .context("creating isolated Git objects directory")?;
    std::fs::create_dir(shadow_git_dir.path().join("refs"))
        .context("creating isolated Git refs directory")?;

    std::fs::copy(
        source_git_dir.join("index"),
        shadow_git_dir.path().join("index"),
    )
    .context("copying Git index into isolated status metadata")?;
    let head_sha = git_safety::local_ref_sha(repo_root, "HEAD")
        .ok_or_else(|| anyhow!("Git HEAD is missing or invalid"))?;
    std::fs::write(shadow_git_dir.path().join("HEAD"), format!("{head_sha}\n"))
        .context("writing isolated Git HEAD")?;

    let object_format = git_output(repo_root, &["rev-parse", "--show-object-format"])?;
    let shadow_config = match object_format.as_str() {
        "sha1" => "[core]\nrepositoryformatversion = 0\nbare = false\nlogallrefupdates = false\n"
            .to_string(),
        "sha256" => "[core]\nrepositoryformatversion = 1\nbare = false\nlogallrefupdates = false\n[extensions]\nobjectFormat = sha256\n"
            .to_string(),
        other => bail!("unsupported Git object format for clean-worktree check: {other}"),
    };
    std::fs::write(shadow_git_dir.path().join("config"), shadow_config)
        .context("writing isolated Git status config")?;

    let source_objects = source_git_dir
        .join("objects")
        .canonicalize()
        .context("canonicalizing Git object directory for isolated status")?;
    let isolated_git = || {
        #[cfg(windows)]
        let null_device = "NUL";
        #[cfg(not(windows))]
        let null_device = "/dev/null";

        let mut command = git_safety::sanitized_git_command();
        command
            .env("GIT_DIR", shadow_git_dir.path())
            .env("GIT_WORK_TREE", repo_root)
            .env("GIT_OBJECT_DIRECTORY", &source_objects)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", null_device)
            .env("GIT_CONFIG_GLOBAL", null_device)
            .env("GIT_OPTIONAL_LOCKS", "0");
        command
    };

    let index_flags = isolated_git()
        .args(["ls-files", "-v", "-z"])
        .output()
        .context("inspecting isolated Git index flags")?;
    if !index_flags.status.success() {
        bail!(
            "isolated Git index inspection failed: {}",
            String::from_utf8_lossy(&index_flags.stderr)
        );
    }
    if index_flags.stdout.split(|byte| *byte == 0).any(|record| {
        record
            .first()
            .is_some_and(|tag| *tag == b'S' || tag.is_ascii_lowercase())
    }) {
        bail!("Git index contains skip-worktree or assume-unchanged entries");
    }

    let staged = isolated_git()
        .args(["ls-files", "--stage", "-z"])
        .output()
        .context("inspecting isolated Git index modes")?;
    if !staged.status.success() {
        bail!(
            "isolated Git mode inspection failed: {}",
            String::from_utf8_lossy(&staged.stderr)
        );
    }
    if staged
        .stdout
        .split(|byte| *byte == 0)
        .any(|record| record.starts_with(b"160000 "))
    {
        bail!("Git submodules are outside the registered-root resolver seam");
    }

    // The isolated metadata intentionally ignores local config. Reject local
    // config that can route attributes elsewhere (directly or through an
    // include), rather than silently computing status under different
    // attribute rules. Filter commands themselves are handled by the active
    // attribute check below.
    for config_path in [
        source_git_dir.join("config"),
        source_git_dir.join("config.worktree"),
    ] {
        match std::fs::symlink_metadata(&config_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    bail!(
                        "Git config must be a regular non-symlink file: {}",
                        config_path.display()
                    );
                }
                let config_keys = git_safety::sanitized_git_command()
                    .args(["config", "--file"])
                    .arg(&config_path)
                    .args(["--null", "--name-only", "--list"])
                    .output()
                    .with_context(|| format!("inspecting Git config: {}", config_path.display()))?;
                if !config_keys.status.success() {
                    bail!(
                        "Git config inspection failed for {}: {}",
                        config_path.display(),
                        String::from_utf8_lossy(&config_keys.stderr)
                    );
                }
                for key in config_keys
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|key| !key.is_empty())
                {
                    let key = String::from_utf8_lossy(key).to_ascii_lowercase();
                    if key == "core.attributesfile"
                        || key == "include.path"
                        || (key.starts_with("includeif.") && key.ends_with(".path"))
                    {
                        bail!(
                            "Git config key {key} can change attribute routing and is outside the registered-root resolver seam"
                        );
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspecting {}", config_path.display()));
            }
        }
    }

    // Global and system config can also route attributes through
    // core.attributesFile. Query only that inert config value under the real
    // repository context; do not load or execute any configured filter.
    let effective_attributes_file = git_safety::sanitized_git_command()
        .args(["config", "--path", "--get-all", "core.attributesFile"])
        .current_dir(repo_root)
        .output()
        .context("inspecting effective Git attribute-file routing")?;
    match effective_attributes_file.status.code() {
        Some(0) => {
            if !effective_attributes_file.stdout.is_empty() {
                bail!("effective core.attributesFile is outside the registered-root resolver seam");
            }
        }
        Some(1) => {}
        _ => bail!(
            "effective Git attribute-file inspection failed: {}",
            String::from_utf8_lossy(&effective_attributes_file.stderr)
        ),
    }

    // A repository-defined clean/process filter can make raw worktree bytes
    // appear clean only after executing a command from `.git/config`. The
    // isolated status intentionally cannot execute that command, so accepting
    // such a tree could produce a false-clean result. Fail closed on active
    // filter or working-tree-encoding attributes before running status.
    let tracked = isolated_git()
        .args(["ls-files", "-z"])
        .output()
        .context("listing tracked paths for isolated Git attribute inspection")?;
    if !tracked.status.success() {
        bail!(
            "isolated Git tracked-path inspection failed: {}",
            String::from_utf8_lossy(&tracked.stderr)
        );
    }
    let mut attribute_command = isolated_git();
    attribute_command
        .args([
            "check-attr",
            "-z",
            "--stdin",
            "filter",
            "working-tree-encoding",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut attribute_child = attribute_command
        .spawn()
        .context("starting isolated Git attribute inspection")?;
    attribute_child
        .stdin
        .take()
        .context("opening isolated Git attribute stdin")?
        .write_all(&tracked.stdout)
        .context("sending tracked paths to isolated Git attribute inspection")?;
    let attributes = attribute_child
        .wait_with_output()
        .context("waiting for isolated Git attribute inspection")?;
    if !attributes.status.success() {
        bail!(
            "isolated Git attribute inspection failed: {}",
            String::from_utf8_lossy(&attributes.stderr)
        );
    }
    let fields = attributes
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut triples = fields.chunks_exact(3);
    for triple in &mut triples {
        let [path, attribute, value] = triple else {
            unreachable!("chunks_exact(3) always yields triples")
        };
        let sensitive = *attribute == b"filter" || *attribute == b"working-tree-encoding";
        let specified = *value != b"unspecified" && *value != b"unset";
        if sensitive && specified {
            bail!(
                "tracked path {} uses Git attribute {}={}; custom clean filters and working-tree encodings are outside the registered-root resolver seam",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(attribute),
                String::from_utf8_lossy(value)
            );
        }
    }
    if !triples.remainder().is_empty() {
        bail!("isolated Git attribute inspection returned malformed output");
    }

    // The shadow metadata does not load the roamed `.git/info/attributes`.
    // Refuse any active entries there so that omission cannot become another
    // false-clean path.
    let info_attributes = source_git_dir.join("info/attributes");
    match std::fs::symlink_metadata(&info_attributes) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "Git info attributes must be a regular non-symlink file: {}",
                    info_attributes.display()
                );
            }
            let contents = std::fs::read_to_string(&info_attributes).with_context(|| {
                format!("reading Git info attributes: {}", info_attributes.display())
            })?;
            if contents
                .lines()
                .map(str::trim)
                .any(|line| !line.is_empty() && !line.starts_with('#'))
            {
                bail!(
                    "Git info attributes are outside the registered-root resolver seam: {}",
                    info_attributes.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting {}", info_attributes.display()));
        }
    }

    let out = isolated_git()
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .arg("--ignore-submodules=all")
        .output()
        .context("running isolated git status")?;
    if !out.status.success() {
        bail!(
            "isolated git status failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    if !out.stdout.is_empty() {
        bail!("git worktree is dirty; refusing repo-group keep-both");
    }
    Ok(())
}

fn ensure_commit_present(repo_root: &Path, sha: &str) -> Result<()> {
    run_git(repo_root, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .with_context(|| format!("remote commit object is missing locally: {sha}"))
}

fn ensure_local_ref_still_pinned(repo_root: &Path, parked: &GitKeepBothParkedRef) -> Result<()> {
    let Some(current) = git_safety::local_ref_sha(repo_root, &parked.head_ref) else {
        bail!("local ref vanished before execute: {}", parked.head_ref);
    };
    if current != parked.local_sha {
        bail!(
            "local ref moved before execute: {} was {}, now {}; rerun reconcile",
            parked.head_ref,
            parked.local_sha,
            current
        );
    }
    Ok(())
}

fn available_park_ref(repo_root: &Path, park_ref: &str, remote_sha: &str) -> Result<String> {
    if let Some(existing) = git_safety::local_ref_sha(repo_root, park_ref) {
        if existing == remote_sha {
            return Ok(park_ref.to_string());
        }
        let min_suffix_len = remote_sha.len().min(12);
        for suffix_len in min_suffix_len..=remote_sha.len() {
            let suffixed = format!("{park_ref}-{}", &remote_sha[..suffix_len]);
            match git_safety::local_ref_sha(repo_root, &suffixed) {
                Some(existing) if existing == remote_sha => return Ok(suffixed),
                Some(_) => continue,
                None => return Ok(suffixed),
            }
        }
        bail!(
            "no available park ref for {remote_sha}: base {park_ref} and every SHA-prefixed suffix are occupied"
        );
    }
    Ok(park_ref.to_string())
}

fn ensure_selected_park_ref_available(
    repo_root: &Path,
    park_ref: &str,
    remote_sha: &str,
) -> Result<()> {
    if let Some(existing) = git_safety::local_ref_sha(repo_root, park_ref) {
        if existing != remote_sha {
            bail!(
                "park ref {park_ref} already exists at {existing}; refusing to overwrite with {remote_sha}"
            );
        }
    }
    Ok(())
}

fn zero_oid_like(oid: &str) -> String {
    "0".repeat(oid.len())
}

/// Write the pre-resolution `git bundle --all` escape hatch under the
/// machine-local state dir — NEVER in-tree.
///
/// The bundle used to live at `repo_root/.git/tcfs-undo/**`, but that is a
/// plain `.git/**` file that raw-mode `.git` collection roams: it grew a
/// full-history bundle (fresh uuid, never GC'd) on every execute across the
/// fleet. Anchoring it to the state dir (outside any sync root), keyed by a
/// hash of the repo root, keeps it a genuine local-only rollback aid
/// (BLOCKING 2 / design S6). `blacklist.rs` also fail-closed denies
/// `.git/tcfs-undo/**` so any pre-existing in-tree bundle can never roam.
fn ensure_private_undo_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("undo path must be a real directory: {}", path.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(path)
                .with_context(|| format!("creating undo dir {}", path.display()))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting undo dir {}", path.display()));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing undo dir {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn write_undo_bundle(repo_root: &Path, state_dir: &Path) -> Result<PathBuf> {
    let repo_hex = blake3::hash(repo_root.to_string_lossy().as_bytes()).to_hex();
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    let undo_base = state_dir.join("keep-both-undo");
    ensure_private_undo_dir(&undo_base)?;
    let undo_dir = undo_base.join(&repo_hex[..16]);
    ensure_private_undo_dir(&undo_dir)?;
    let bundle = undo_dir.join(format!("keep-both-{}.bundle", Uuid::new_v4()));
    let bundle_str = bundle.to_string_lossy().to_string();

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&bundle)
        .with_context(|| format!("creating private undo bundle {}", bundle.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing undo bundle {}", bundle.display()))?;
    }

    // Stream Git's bundle output into the already-private inode. Letting Git
    // create the pathname would expose full repository history at the process
    // umask until a later chmod.
    let output = git_safety::sanitized_git_command()
        .args(["bundle", "create", "-", "--all"])
        .current_dir(repo_root)
        .stdout(Stdio::from(file))
        .output()
        .context("creating undo bundle")?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&bundle);
        bail!(
            "git bundle create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let sync_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&bundle)
        .with_context(|| format!("reopening undo bundle {}", bundle.display()))?;
    sync_file
        .sync_all()
        .with_context(|| format!("syncing undo bundle {}", bundle.display()))?;
    if let Err(error) = run_git(repo_root, &["bundle", "verify", &bundle_str]) {
        let _ = std::fs::remove_file(&bundle);
        return Err(error).context("verifying undo bundle");
    }
    Ok(bundle)
}

fn rollback_refs(repo_root: &Path, refs: &[AppliedParkRef]) {
    for r in refs.iter().rev() {
        match r.previous_sha.as_deref() {
            Some(previous) => {
                let _ = run_git(repo_root, &["update-ref", r.ref_name.as_str(), previous]);
            }
            None => {
                let _ = run_git(repo_root, &["update-ref", "-d", r.ref_name.as_str()]);
            }
        }
    }
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<()> {
    let out = git_safety::sanitized_git_command()
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;

    #[cfg(unix)]
    #[test]
    fn clean_worktree_check_rejects_repo_carried_filter_without_execution() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join(".gitattributes"), b"tracked.txt filter=probe\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), b"base\n").unwrap();
        git_safety::run_git(&repo, &["add", ".gitattributes", "tracked.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();

        let marker = dir.path().join("filter-executed");
        let filter = dir.path().join("filter-probe.sh");
        std::fs::write(
            &filter,
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf 'filtered\\n'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o700)).unwrap();
        git_safety::run_git(
            &repo,
            &["config", "filter.probe.clean", &filter.to_string_lossy()],
        )
        .unwrap();

        // Positive control: raw bytes still equal the committed blob, but the
        // repository-aware status executes the filter and reports the tree
        // dirty. An isolated status that merely drops the filter definition
        // would incorrectly report this fixture clean.
        let unsafe_status = git_safety::sanitized_git_command()
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(unsafe_status.status.success());
        assert!(
            !unsafe_status.stdout.is_empty(),
            "fixture must be false-clean without its filter"
        );
        assert!(marker.exists(), "fixture must exercise the unsafe Git path");
        std::fs::remove_file(&marker).unwrap();

        let error = ensure_clean_worktree(&repo).expect_err("filtered tree must fail closed");
        assert!(error.to_string().contains("filter"), "{error:#}");
        assert!(
            !marker.exists(),
            "isolated status must not execute a filter from roamed .git/config"
        );
    }

    #[test]
    fn clean_worktree_check_rejects_replace_ref_false_clean() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), b"base\n").unwrap();
        git_safety::run_git(&repo, &["add", "tracked.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();
        let base = git_safety::local_ref_sha(&repo, "HEAD").unwrap();
        std::fs::write(repo.join("tracked.txt"), b"replacement\n").unwrap();
        git_safety::run_git(&repo, &["add", "tracked.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "replacement", "--quiet"]).unwrap();
        let replacement = git_safety::local_ref_sha(&repo, "HEAD").unwrap();
        git_safety::run_git(&repo, &["reset", "--hard", "--quiet", &base]).unwrap();
        git_safety::run_git(&repo, &["replace", &base, &replacement]).unwrap();

        let ordinary = std::process::Command::new("git")
            .env_remove("GIT_NO_REPLACE_OBJECTS")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "status",
                "--porcelain=v1",
            ])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(ordinary.status.success());
        assert!(
            !ordinary.stdout.is_empty(),
            "replace-ref fixture must make ordinary status dirty"
        );

        let error = ensure_clean_worktree(&repo).expect_err("replace refs must fail closed");
        assert!(error.to_string().contains("replace refs"), "{error:#}");
    }

    #[test]
    fn clean_worktree_check_rejects_hidden_index_entries() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), b"base\n").unwrap();
        git_safety::run_git(&repo, &["add", "tracked.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();
        git_safety::run_git(
            &repo,
            &["update-index", "--assume-unchanged", "tracked.txt"],
        )
        .unwrap();

        let error =
            ensure_clean_worktree(&repo).expect_err("assume-unchanged entries must fail closed");
        assert!(error.to_string().contains("assume-unchanged"), "{error:#}");
    }

    #[test]
    fn park_ref_sanitizes_device_and_preserves_branch() {
        assert_eq!(
            park_ref_for("honey/local", "refs/heads/feature/x").unwrap(),
            "refs/tcfs/theirs/honey-local/heads/feature/x"
        );
    }

    #[test]
    fn local_root_is_derived_from_cache_key_and_rel_path() {
        assert_eq!(
            local_root_from_cache_key(
                "/tmp/root/repo/.git/refs/heads/main",
                "repo/.git/refs/heads/main"
            ),
            PathBuf::from("/tmp/root")
        );
    }

    #[test]
    fn standalone_topology_accepts_ordinary_repo_and_rejects_redirect_files() {
        let ordinary = tempfile::tempdir().unwrap();
        let ordinary_repo = ordinary.path().join("repo");
        init_repo(&ordinary_repo);
        validate_standalone_repo_topology(&ordinary_repo).expect("ordinary repo topology");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let hooks = ordinary_repo.join(".git/hooks");
            std::fs::remove_dir_all(&hooks).unwrap();
            let external_hooks = ordinary.path().join("external-hooks");
            std::fs::create_dir_all(&external_hooks).unwrap();
            symlink(&external_hooks, &hooks).unwrap();
        }
        validate_standalone_repo_topology(&ordinary_repo)
            .expect("hooks topology is irrelevant because hooks are disabled");

        for (relative, expected) in [
            ("commondir", "commondir"),
            ("worktrees", "linked-worktree"),
            ("objects/info/alternates", "alternates"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            init_repo(&repo);
            let path = repo.join(".git").join(relative);
            if relative == "worktrees" {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, b"../external\n").unwrap();
            }
            let error = validate_standalone_repo_topology(&repo)
                .expect_err("redirected/shared Git topology must fail closed");
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn standalone_topology_rejects_external_core_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let external = temp.path().join("external-worktree");
        init_repo(&repo);
        std::fs::create_dir_all(&external).unwrap();
        let external_text = external.to_string_lossy().to_string();
        git_safety::run_git(&repo, &["config", "core.worktree", external_text.as_str()]).unwrap();

        let error = validate_standalone_repo_topology(&repo)
            .expect_err("external core.worktree must fail closed");
        assert!(
            error.to_string().contains("worktree") || error.to_string().contains("show-toplevel"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn object_probe_disables_promisor_remote_helpers() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let marker = temp.path().join("remote-helper-ran");
        let remote = format!("ext::touch {}", marker.display());
        for (key, value) in [
            ("core.repositoryformatversion", "1"),
            ("extensions.partialClone", "origin"),
            ("remote.origin.promisor", "true"),
            ("remote.origin.partialclonefilter", "blob:none"),
            ("protocol.ext.allow", "always"),
            ("remote.origin.url", remote.as_str()),
        ] {
            git_safety::run_git(&repo, &["config", key, value]).unwrap();
        }
        let missing = "1111111111111111111111111111111111111111";

        let ordinary = std::process::Command::new("git")
            .env_remove("GIT_NO_LAZY_FETCH")
            .args(["cat-file", "-e", &format!("{missing}^{{commit}}")])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(!ordinary.status.success());
        assert!(
            marker.exists(),
            "fixture must prove the ordinary object probe invokes the helper"
        );
        std::fs::remove_file(&marker).unwrap();

        let error = ensure_commit_present(&repo, missing).expect_err("missing object must fail");
        assert!(error.to_string().contains("missing locally"), "{error:#}");
        assert!(
            !marker.exists(),
            "sanitized object probe must disable promisor lazy fetch"
        );

        let error = validate_standalone_repo_topology(&repo)
            .expect_err("partial-clone resolver config must fail closed");
        assert!(error.to_string().contains("partialclone"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn standalone_topology_rejects_critical_git_path_symlinks() {
        use std::os::unix::fs::symlink;

        for relative in ["refs", "objects", "logs", "index", "config"] {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            init_repo(&repo);
            let path = repo.join(".git").join(relative);
            let external = temp.path().join(format!("external-{relative}"));
            if matches!(relative, "refs" | "objects" | "logs") {
                if path.exists() {
                    std::fs::remove_dir_all(&path).unwrap();
                }
                std::fs::create_dir_all(&external).unwrap();
            } else {
                if path.exists() {
                    std::fs::remove_file(&path).unwrap();
                }
                std::fs::write(&external, b"external\n").unwrap();
            }
            symlink(&external, &path).unwrap();

            let error = validate_standalone_repo_topology(&repo)
                .expect_err("critical Git symlink must fail closed");
            assert!(
                error.to_string().contains("real")
                    || error.to_string().contains("symlink redirect"),
                "relative={relative}: {error:#}"
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let nested = repo.join(".git/refs/tcfs");
        let external = temp.path().join("external-refs");
        std::fs::create_dir_all(&external).unwrap();
        symlink(&external, &nested).unwrap();
        let error = validate_standalone_repo_topology(&repo)
            .expect_err("park-ref ancestry symlink must fail closed");
        assert!(error.to_string().contains("symlink redirect"), "{error:#}");
    }

    #[tokio::test]
    async fn dry_run_refuses_missing_remote_manifest_key() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("file.txt"), b"base").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();
        let head_path = repo.join(".git/refs/heads/main");

        let mut state = StateCache::open(&dir.path().join("state.json")).unwrap();
        let mut local = crate::conflict::VectorClock::new();
        local.tick("neo");
        let mut remote = crate::conflict::VectorClock::new();
        remote.tick("honey");
        state.set(
            &head_path,
            crate::state::SyncState {
                blake3: "local".into(),
                size: 41,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/manifests/local".into(),
                last_synced: 0,
                vclock: local.clone(),
                device_id: "neo".into(),
                conflict: Some(crate::conflict::ConflictInfo {
                    rel_path: "repo/.git/refs/heads/main".into(),
                    local_vclock: local,
                    remote_vclock: remote,
                    local_blake3: "local".into(),
                    remote_blake3: "remote".into(),
                    local_device: "neo".into(),
                    remote_device: "honey".into(),
                    detected_at: 1,
                    times_recorded: 1,
                    remote_manifest_key: None,
                }),
                status: FileSyncStatus::Conflict,
            },
        );

        let err = resolve_repo_keep_both(
            &op,
            &mut state,
            &repo,
            "data",
            "neo",
            dir.path(),
            GitKeepBothMode::DryRun,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("remote_manifest_key"));
    }

    #[test]
    fn execute_refuses_moved_local_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("file.txt"), b"base").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();
        let base = git_safety::local_ref_sha(&repo, "HEAD").unwrap();

        let parked = GitKeepBothParkedRef {
            conflict_rel_path: "repo/.git/refs/heads/main".into(),
            head_ref: "refs/heads/main".into(),
            park_ref: "refs/tcfs/theirs/honey/heads/main".into(),
            local_sha: base,
            remote_sha: "0123456789012345678901234567890123456789".into(),
            cache_key: "repo/.git/refs/heads/main".into(),
        };

        std::fs::write(repo.join("file.txt"), b"moved").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "moved", "--quiet"]).unwrap();

        let err = ensure_local_ref_still_pinned(&repo, &parked).unwrap_err();
        assert!(err.to_string().contains("local ref moved"));
    }

    #[test]
    fn clean_worktree_check_refuses_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), b"base").unwrap();
        git_safety::run_git(&repo, &["add", "tracked.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();

        std::fs::write(repo.join("untracked.txt"), b"wip").unwrap();

        let err = ensure_clean_worktree(&repo).unwrap_err();
        assert!(err.to_string().contains("dirty"));
    }

    #[test]
    fn occupied_park_ref_gets_sha_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("file.txt"), b"one").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "one", "--quiet"]).unwrap();
        let first = git_safety::local_ref_sha(&repo, "HEAD").unwrap();
        std::fs::write(repo.join("file.txt"), b"two").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "two", "--quiet"]).unwrap();
        let second = git_safety::local_ref_sha(&repo, "HEAD").unwrap();

        let park_ref = "refs/tcfs/theirs/honey/heads/main";
        git_safety::run_git(&repo, &["update-ref", park_ref, &first]).unwrap();

        let selected = available_park_ref(&repo, park_ref, &second).unwrap();
        assert_eq!(
            selected,
            format!("refs/tcfs/theirs/honey/heads/main-{}", &second[..12])
        );
        assert_eq!(
            park_ref_create_only(&repo, park_ref, &second).unwrap(),
            selected
        );
        assert_eq!(git_safety::local_ref_sha(&repo, park_ref), Some(first));
        assert_eq!(git_safety::local_ref_sha(&repo, &selected), Some(second));
    }

    #[test]
    fn occupied_sha12_suffix_widens_until_available() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("file.txt"), b"one").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "one", "--quiet"]).unwrap();
        let first = git_safety::local_ref_sha(&repo, "HEAD").unwrap();
        std::fs::write(repo.join("file.txt"), b"two").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "two", "--quiet"]).unwrap();
        let second = git_safety::local_ref_sha(&repo, "HEAD").unwrap();
        std::fs::write(repo.join("file.txt"), b"three").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "three", "--quiet"]).unwrap();
        let third = git_safety::local_ref_sha(&repo, "HEAD").unwrap();

        let park_ref = "refs/tcfs/theirs/honey/heads/main";
        let sha12_ref = format!("{park_ref}-{}", &second[..12]);
        git_safety::run_git(&repo, &["update-ref", park_ref, &first]).unwrap();
        git_safety::run_git(&repo, &["update-ref", &sha12_ref, &third]).unwrap();

        let selected = available_park_ref(&repo, park_ref, &second).unwrap();
        assert_eq!(
            selected,
            format!("refs/tcfs/theirs/honey/heads/main-{}", &second[..13])
        );
        assert_eq!(
            park_ref_create_only(&repo, park_ref, &second).unwrap(),
            selected
        );
        assert_eq!(git_safety::local_ref_sha(&repo, park_ref), Some(first));
        assert_eq!(git_safety::local_ref_sha(&repo, &sha12_ref), Some(third));
        assert_eq!(git_safety::local_ref_sha(&repo, &selected), Some(second));
    }

    #[test]
    fn rollback_restores_previous_parked_ref_value() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_safety::run_git(&repo, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(&repo, &["config", "user.name", "TCFS Test"]).unwrap();
        std::fs::write(repo.join("file.txt"), b"one").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "one", "--quiet"]).unwrap();
        let first = git_safety::local_ref_sha(&repo, "HEAD").unwrap();
        std::fs::write(repo.join("file.txt"), b"two").unwrap();
        git_safety::run_git(&repo, &["add", "file.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "two", "--quiet"]).unwrap();
        let second = git_safety::local_ref_sha(&repo, "HEAD").unwrap();

        let park_ref = "refs/tcfs/theirs/honey/heads/main";
        git_safety::run_git(&repo, &["update-ref", park_ref, &second]).unwrap();
        rollback_refs(
            &repo,
            &[AppliedParkRef {
                ref_name: park_ref.into(),
                previous_sha: Some(first.clone()),
            }],
        );

        assert_eq!(git_safety::local_ref_sha(&repo, park_ref), Some(first));
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git_safety::run_git(path, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        git_safety::run_git(path, &["config", "user.email", "tcfs@example.invalid"]).unwrap();
        git_safety::run_git(path, &["config", "user.name", "TCFS Test"]).unwrap();
    }

    fn commit(path: &Path, file: &str, body: &str, msg: &str) -> String {
        std::fs::write(path.join(file), body.as_bytes()).unwrap();
        git_safety::run_git(path, &["add", file]).unwrap();
        git_safety::run_git(path, &["commit", "-m", msg, "--quiet"]).unwrap();
        git_safety::local_ref_sha(path, "HEAD").unwrap()
    }

    fn conflict_state(
        rel_path: &str,
        remote_manifest_key: Option<String>,
        local: &crate::conflict::VectorClock,
        remote: &crate::conflict::VectorClock,
    ) -> crate::state::SyncState {
        crate::state::SyncState {
            blake3: "local".into(),
            size: 41,
            mtime: 0,
            chunk_count: 1,
            remote_path: "data/manifests/head".into(),
            last_synced: 0,
            vclock: local.clone(),
            device_id: "winner".into(),
            conflict: Some(crate::conflict::ConflictInfo {
                rel_path: rel_path.into(),
                local_vclock: local.clone(),
                remote_vclock: remote.clone(),
                local_blake3: "local".into(),
                remote_blake3: "remote".into(),
                local_device: "winner".into(),
                remote_device: "loser".into(),
                detected_at: 1,
                times_recorded: 1,
                remote_manifest_key,
            }),
            status: FileSyncStatus::Conflict,
        }
    }

    /// End-to-end Execute over two genuinely divergent `.git` repos (winner +
    /// loser, sharing a base, neither a fast-forward of the other). Exercises
    /// the real resolver — a no-op/stub resolver would leave the theirs-ref
    /// absent and fail invariant (i). Also drives the veto reconciliation
    /// (CHANGES-NEEDED 4) by including a divergent `.git/index` that must ride
    /// kept-local, and asserts idempotency (CHANGES-NEEDED 3).
    #[tokio::test]
    async fn execute_parks_theirs_keeps_local_and_is_idempotent() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state-dir");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Shared base, then two divergent commits (no FF direction).
        let base = dir.path().join("base");
        init_repo(&base);
        commit(&base, "file.txt", "base", "base");
        let winner = dir.path().join("winner");
        let loser = dir.path().join("loser");
        git_safety::run_git(
            dir.path(),
            &[
                "clone",
                "--quiet",
                &base.to_string_lossy(),
                &winner.to_string_lossy(),
            ],
        )
        .unwrap();
        git_safety::run_git(
            dir.path(),
            &[
                "clone",
                "--quiet",
                &base.to_string_lossy(),
                &loser.to_string_lossy(),
            ],
        )
        .unwrap();
        init_repo(&winner); // re-assert identity (clone copies base config, but be explicit)
        init_repo(&loser);
        let head_w = commit(&winner, "file.txt", "winner work", "winner");
        let head_l = commit(&loser, "file.txt", "loser work", "loser");
        assert_ne!(head_w, head_l);

        // Objects-before-refs: the loser's commit is present in the winner's
        // object store (a prior reconcile would have roamed the objects). Fetch
        // brings the objects in without moving any winner ref.
        git_safety::run_git(
            &winner,
            &[
                "fetch",
                "--quiet",
                &loser.to_string_lossy(),
                "refs/heads/main",
            ],
        )
        .unwrap();
        ensure_commit_present(&winner, &head_l).unwrap();

        // Publish the loser's branch-head ref content as a PR-2 remote manifest
        // so read_remote_ref_sha can recover the peer head SHA.
        let ref_blob = dir.path().join("loser-ref-blob");
        std::fs::write(&ref_blob, format!("{head_l}\n")).unwrap();
        let mut up_state = StateCache::open(&dir.path().join("upload-state.json")).unwrap();
        let up = engine::upload_file_with_device(
            &op,
            &ref_blob,
            "data",
            &mut up_state,
            None,
            "loser",
            Some("loser/.git/refs/heads/main"),
            None,
        )
        .await
        .unwrap();
        let manifest_key = up.remote_path.clone();

        let state_path = dir.path().join("state.json");
        let ref_key = winner.join(".git/refs/heads/main");
        let index_key = winner.join(".git/index");
        let mut local = crate::conflict::VectorClock::new();
        local.tick("winner");
        let mut remote = crate::conflict::VectorClock::new();
        remote.tick("loser");

        let inject = |state: &mut StateCache| {
            state.set(
                &ref_key,
                conflict_state(
                    "winner/.git/refs/heads/main",
                    Some(manifest_key.clone()),
                    &local,
                    &remote,
                ),
            );
            // Divergent non-ref-class path: must ride kept-local, not veto.
            state.set(
                &index_key,
                conflict_state("winner/.git/index", None, &local, &remote),
            );
        };

        let mut state = StateCache::open(&state_path).unwrap();
        inject(&mut state);

        let result = resolve_repo_keep_both(
            &op,
            &mut state,
            &winner,
            "data",
            "winner",
            &state_dir,
            GitKeepBothMode::Execute,
            None,
        )
        .await
        .expect("execute should succeed on genuinely divergent repos");

        let park_ref = "refs/tcfs/theirs/loser/heads/main";
        // (i) loser head parked at the CORRECT remote SHA.
        assert_eq!(result.parked_refs.len(), 1, "one branch head parked");
        assert_eq!(
            git_safety::local_ref_sha(&winner, park_ref).as_deref(),
            Some(head_l.as_str()),
            "theirs-ref must point at the loser's head SHA"
        );
        // (ii) winner's local head + HEAD untouched, winner's commit intact.
        assert_eq!(
            git_safety::local_ref_sha(&winner, "refs/heads/main").as_deref(),
            Some(head_w.as_str()),
            "winner head must be untouched"
        );
        assert_eq!(
            git_safety::local_ref_sha(&winner, "HEAD").as_deref(),
            Some(head_w.as_str())
        );
        ensure_commit_present(&winner, &head_w).unwrap();
        // (iii) repository verifies clean.
        run_git(&winner, &["fsck", "--full"]).expect("fsck clean after execute");
        // (iv) BOTH heads reachable, no lost commits.
        let rev_list = git_safety::sanitized_git_command()
            .args(["rev-list", "--all"])
            .current_dir(&winner)
            .output()
            .unwrap();
        let reachable = String::from_utf8_lossy(&rev_list.stdout);
        assert!(reachable.contains(&head_w), "winner head reachable");
        assert!(reachable.contains(&head_l), "parked loser head reachable");
        // KeepLocal + parked conflicts both cleared.
        assert!(
            state.conflicts().is_empty(),
            "all conflicts (ref + index) cleared after execute"
        );
        // Undo bundle lives under the state dir, NEVER in-tree.
        let bundle = result.undo_bundle.expect("undo bundle written");
        assert!(
            bundle.starts_with(&state_dir),
            "undo bundle under state dir"
        );
        assert!(
            !bundle.starts_with(winner.join(".git")),
            "undo bundle must NOT be under the repo .git"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&bundle).unwrap().permissions().mode() & 0o777,
                0o600,
                "undo bundle must be private before it contains repository history"
            );
            assert_eq!(
                std::fs::metadata(bundle.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "per-repo undo directory must be private"
            );
            assert_eq!(
                std::fs::metadata(bundle.parent().unwrap().parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "undo root directory must be private"
            );
        }

        // (v) a second Execute is idempotent: re-inject the (re-detected)
        // conflict and re-run. No error, no double-park, refs unchanged.
        inject(&mut state);
        let again = resolve_repo_keep_both(
            &op,
            &mut state,
            &winner,
            "data",
            "winner",
            &state_dir,
            GitKeepBothMode::Execute,
            None,
        )
        .await
        .expect("second execute idempotent");
        assert_eq!(again.parked_refs.len(), 1);
        assert_eq!(
            git_safety::local_ref_sha(&winner, park_ref).as_deref(),
            Some(head_l.as_str()),
            "theirs-ref still at loser head (no double-park)"
        );
        assert_eq!(
            git_safety::local_ref_sha(&winner, "refs/heads/main").as_deref(),
            Some(head_w.as_str()),
            "winner head still untouched on re-run"
        );
        run_git(&winner, &["fsck", "--full"]).expect("fsck clean after idempotent re-run");
    }

    /// TIN-2652 seam regression: the test that was missing and would have caught
    /// the live no-op. Record a divergent head-ref conflict through the REAL plan
    /// path (`reconcile::execute_plan`'s `Conflict` arm), then assert
    /// `collect_repo_conflicts` — the keep-both resolver's candidate scan — yields
    /// exactly one `Park` candidate for `refs/heads/main`. Before the status flip,
    /// the plan-recorded entry stayed `Synced`; `collect_repo_conflicts` skipped
    /// it (`status != Conflict`) and keep-both silently resolved nothing.
    #[tokio::test]
    async fn tin2652_plan_recorded_conflict_is_a_keep_both_park_candidate() {
        use crate::conflict::{ConflictInfo, VectorClock};
        use crate::reconcile::{execute_plan, ReconcileAction, ReconcilePlan, ReconcileSummary};

        let op = Operator::new(Memory::default()).unwrap().finish();
        let dir = tempfile::tempdir().unwrap();
        let local_root = dir.path();
        let repo = local_root.join("repo");
        init_repo(&repo);
        commit(&repo, "file.txt", "base", "base");
        let ref_path = repo.join(".git/refs/heads/main");
        assert!(
            ref_path.exists(),
            "base commit must create the loose head ref"
        );

        // Tracked head ref starts fully Synced — the live canary shape.
        let rel = "repo/.git/refs/heads/main";
        let mut local = VectorClock::new();
        local.tick("honey");
        let mut remote = VectorClock::new();
        remote.tick("neo");
        remote.tick("neo");
        let mut state = StateCache::open(&local_root.join("state.json")).unwrap();
        state.set(
            &ref_path,
            crate::state::SyncState {
                blake3: "baselinehash".into(),
                size: 41,
                mtime: 0,
                chunk_count: 1,
                remote_path: "data/manifests/head".into(),
                last_synced: 0,
                vclock: local.clone(),
                device_id: "honey".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );

        // Record the conflict through the real plan-execution path.
        let plan = ReconcilePlan {
            actions: vec![ReconcileAction::Conflict {
                rel_path: rel.into(),
                info: ConflictInfo {
                    rel_path: rel.into(),
                    local_vclock: local.clone(),
                    remote_vclock: remote,
                    local_blake3: "baselinehash".into(),
                    remote_blake3: "remotehash".into(),
                    local_device: "honey".into(),
                    remote_device: "neo".into(),
                    detected_at: 1,
                    times_recorded: 0,
                    // Park candidates REQUIRE the remote manifest key.
                    remote_manifest_key: Some("data/manifests/remotehead".into()),
                },
            }],
            summary: ReconcileSummary::default(),
            device_id: "honey".into(),
            generated_at: 0,
        };
        let exec = execute_plan(
            &plan, &op, local_root, "data", &mut state, "honey", None, None,
        )
        .await
        .expect("execute plan");
        assert_eq!(exec.conflicts_recorded, 1, "one conflict must be recorded");

        // The seam: the keep-both resolver's candidate scan must see it.
        let repo_root = repo.canonicalize().unwrap();
        let candidates =
            collect_repo_conflicts(&state, &repo_root, "data").expect("collect candidates");
        assert_eq!(
            candidates.len(),
            1,
            "TIN-2652: exactly one keep-both candidate for the plan-recorded head-ref conflict, \
             got {candidates:?}"
        );
        match &candidates[0].kind {
            GitConflictKind::Park { head_ref, .. } => {
                assert_eq!(head_ref, "refs/heads/main");
            }
            other => panic!("head ref must be a Park candidate, got {other:?}"),
        }
    }
}
