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

fn available_park_ref(repo_root: &Path, park_ref: &str, remote_sha: &str) -> Result<String> {
    if let Some(existing) = git_safety::local_ref_sha(repo_root, park_ref) {
        if existing == remote_sha {
            return Ok(park_ref.to_string());
        }
        let suffix_len = remote_sha.len().min(12);
        let suffixed = format!("{park_ref}-{}", &remote_sha[..suffix_len]);
        ensure_selected_park_ref_available(repo_root, &suffixed, remote_sha)?;
        return Ok(suffixed);
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
pub(crate) fn write_undo_bundle(repo_root: &Path, state_dir: &Path) -> Result<PathBuf> {
    let repo_hex = blake3::hash(repo_root.to_string_lossy().as_bytes()).to_hex();
    let undo_dir = state_dir.join("keep-both-undo").join(&repo_hex[..16]);
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
        let rev_list = std::process::Command::new("git")
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
}
