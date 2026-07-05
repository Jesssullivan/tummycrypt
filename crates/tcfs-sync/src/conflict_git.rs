//! Repo-group `.git` conflict resolution helpers.
//!
//! Per-file conflict resolution is intentionally fenced away from `.git`
//! internals. A divergent raw-git repo has to be resolved as one group: inspect
//! the recorded conflicts, park the remote branch heads under a TCFS namespace,
//! verify the repository before/after, then clear the group atomically.

use std::path::{Path, PathBuf};
use std::process::Command;

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
struct GitConflictCandidate {
    cache_key: String,
    rel_path: String,
    head_ref: String,
    remote_manifest_key: String,
    remote_device: String,
    resolved_vclock: crate::conflict::VectorClock,
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
pub async fn resolve_repo_keep_both(
    op: &Operator,
    state: &mut StateCache,
    repo_root: &Path,
    remote_prefix: &str,
    device_id: &str,
    mode: GitKeepBothMode,
    encryption: OptionalEncryption<'_>,
) -> Result<GitKeepBothResult> {
    let repo_root = repo_root
        .canonicalize()
        .with_context(|| format!("canonicalizing repo root: {}", repo_root.display()))?;
    let git_dir = repo_root.join(".git");
    if !git_dir.is_dir() {
        bail!("{} is not a git repository root", repo_root.display());
    }

    let candidates = collect_repo_conflicts(state, &repo_root)?;
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

    let mut parked_refs = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let remote_sha = read_remote_ref_sha(
            op,
            &candidate.remote_manifest_key,
            remote_prefix,
            device_id,
            encryption,
        )
        .await
        .with_context(|| {
            format!(
                "reading remote ref for {} from {}",
                candidate.rel_path, candidate.remote_manifest_key
            )
        })?;
        ensure_commit_present(&repo_root, &remote_sha)?;
        let local_sha = git_safety::local_ref_sha(&repo_root, &candidate.head_ref)
            .ok_or_else(|| anyhow!("local ref is missing: {}", candidate.head_ref))?;
        if local_sha == remote_sha {
            bail!(
                "{} is no longer divergent; rerun reconcile before keep-both",
                candidate.head_ref
            );
        }
        let park_ref = park_ref_for(&candidate.remote_device, &candidate.head_ref)?;
        ensure_park_ref_available(&repo_root, &park_ref, &remote_sha)?;
        parked_refs.push(GitKeepBothParkedRef {
            conflict_rel_path: candidate.rel_path.clone(),
            head_ref: candidate.head_ref.clone(),
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
    let safety = git_safety::git_is_safe(&git_dir);
    if !safety.blocking.is_empty() {
        bail!("git repository became busy: {}", safety.blocking.join("; "));
    }
    ensure_clean_worktree(&repo_root)?;
    run_git(&repo_root, &["fsck", "--full"]).context("locked pre-resolution git fsck")?;

    for parked in &parked_refs {
        ensure_local_ref_still_pinned(&repo_root, parked)?;
        ensure_park_ref_available(&repo_root, &parked.park_ref, &parked.remote_sha)?;
    }

    let undo_bundle = write_undo_bundle(&repo_root)?;
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
        undo_bundle: Some(undo_bundle),
    })
}

fn collect_repo_conflicts(
    state: &StateCache,
    repo_root: &Path,
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
        let Some(head_ref) = git_safety::head_ref_for_git_path(&rel_path) else {
            bail!(
                "unparkable .git conflict {}; only branch-head refs are supported in PR-3",
                rel_path
            );
        };
        let Some(remote_manifest_key) = conflict.remote_manifest_key.clone() else {
            bail!(
                "{} has no remote_manifest_key; rerun reconcile with a PR-2 binary first",
                rel_path
            );
        };
        let mut resolved_vclock = conflict.local_vclock.clone();
        resolved_vclock.merge(&conflict.remote_vclock);
        out.push(GitConflictCandidate {
            cache_key: cache_key.to_string(),
            rel_path,
            head_ref,
            remote_manifest_key,
            remote_device: conflict.remote_device.clone(),
            resolved_vclock,
        });
    }
    out.sort_by(|a, b| a.head_ref.cmp(&b.head_ref));
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

fn ensure_clean_worktree(repo_root: &Path) -> Result<()> {
    let out = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .current_dir(repo_root)
        .output()
        .context("running git status")?;
    if !out.status.success() {
        bail!(
            "git status failed: {}",
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

fn ensure_park_ref_available(repo_root: &Path, park_ref: &str, remote_sha: &str) -> Result<()> {
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

fn write_undo_bundle(repo_root: &Path) -> Result<PathBuf> {
    let undo_dir = repo_root.join(".git/tcfs-undo");
    std::fs::create_dir_all(&undo_dir)
        .with_context(|| format!("creating undo dir {}", undo_dir.display()))?;
    let bundle = undo_dir.join(format!("keep-both-{}.bundle", Uuid::new_v4()));
    let bundle_str = bundle.to_string_lossy().to_string();
    run_git(repo_root, &["bundle", "create", &bundle_str, "--all"])
        .context("creating undo bundle")?;
    run_git(repo_root, &["bundle", "verify", &bundle_str]).context("verifying undo bundle")?;
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
    let out = Command::new("git")
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
    fn execute_refuses_to_overwrite_different_parked_ref() {
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

        let err = ensure_park_ref_available(&repo, park_ref, &second).unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
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
}
