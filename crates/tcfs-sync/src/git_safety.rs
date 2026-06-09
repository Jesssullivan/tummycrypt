//! Git directory sync safety checks.
//!
//! Before syncing .git directories, validates that no git operations
//! are in progress (no lock files, no rebase/merge/cherry-pick).

use std::path::Path;

/// Relative path (under the repo working root) where the TCFS git bundle is
/// written and synced as a normal object. Keeping it inside the working tree
/// (not under `.git/`) means it flows through the regular file collector and
/// is visible to the peer pull as an ordinary path, while the raw `.git/*`
/// internals are skipped by the collector in bundle mode.
pub const GIT_BUNDLE_REL_PATH: &str = ".git-tcfs-bundle";

/// Result of checking whether a .git directory is safe to sync.
#[derive(Debug, Clone, Default)]
pub struct GitSafetyCheck {
    /// Blocking issues that prevent sync (lock files, in-progress operations)
    pub blocking: Vec<String>,
    /// Non-blocking warnings (e.g., stale refs)
    pub warnings: Vec<String>,
}

/// Check if a .git directory is safe to sync.
///
/// Looks for:
/// - Lock files: `index.lock`, `HEAD.lock`, `gc.pid`
/// - In-progress operations: `rebase-merge/`, `rebase-apply/`, `MERGE_HEAD`, `CHERRY_PICK_HEAD`
pub fn git_is_safe(git_dir: &Path) -> GitSafetyCheck {
    let mut check = GitSafetyCheck::default();

    // Lock files that indicate active git operations
    let lock_files = [
        "index.lock",
        "HEAD.lock",
        "gc.pid",
        "refs/heads/*.lock",
        "shallow.lock",
        "packed-refs.lock",
    ];

    for lock in &lock_files {
        let lock_path = git_dir.join(lock);
        if lock_path.exists() {
            check.blocking.push(format!("lock file exists: {}", lock));
        }
    }

    // In-progress operations
    let in_progress = [
        ("rebase-merge", "interactive rebase in progress"),
        ("rebase-apply", "rebase/am in progress"),
        ("MERGE_HEAD", "merge in progress"),
        ("CHERRY_PICK_HEAD", "cherry-pick in progress"),
        ("BISECT_LOG", "bisect in progress"),
        ("REVERT_HEAD", "revert in progress"),
    ];

    for (file, desc) in &in_progress {
        let path = git_dir.join(file);
        if path.exists() {
            check.blocking.push(format!("{desc}: {file} exists"));
        }
    }

    // Warnings (non-blocking)
    let stale_threshold_secs = 3600; // 1 hour
    if let Ok(meta) = std::fs::metadata(git_dir.join("FETCH_HEAD")) {
        if let Ok(modified) = meta.modified() {
            if let Ok(elapsed) = modified.elapsed() {
                if elapsed.as_secs() > stale_threshold_secs {
                    check.warnings.push("FETCH_HEAD is stale (>1h old)".into());
                }
            }
        }
    }

    check
}

/// Create a git bundle for atomic .git snapshot.
///
/// Runs `git bundle create --all` to create a single file containing
/// all refs and objects, suitable for transporting a complete repository.
pub fn snapshot_git_for_sync(repo_root: &Path) -> anyhow::Result<std::path::PathBuf> {
    let bundle_path = repo_root.join(GIT_BUNDLE_REL_PATH);

    let output = std::process::Command::new("git")
        .args(["bundle", "create", &bundle_path.to_string_lossy(), "--all"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| anyhow::anyhow!("running git bundle: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git bundle failed: {stderr}");
    }

    Ok(bundle_path)
}

/// Restore git history from a TCFS bundle into an existing working tree.
///
/// Unlike a fresh `git clone`, this is designed for the rehydrate path where
/// the working-tree files have already been materialized by TCFS as normal
/// objects: only the `.git` metadata (objects, refs, logs) is missing. It:
///
/// 1. `git init`s the repo in place if there is no `.git` directory yet.
/// 2. `git fetch`es every ref from the bundle into the live object store
///    (`+refs/*:refs/*`), which repopulates branches, tags, and history.
/// 3. Restores `HEAD` to the bundle's default branch so `git log` / `git
///    status` / `git fetch` work against a real ref.
///
/// The working-tree files are left untouched: after this runs, `git status`
/// compares the freshly-restored index/HEAD against the already-synced files,
/// so a faithfully-synced tree reports clean.
pub fn restore_git_bundle_into(bundle: &Path, repo_root: &Path) -> anyhow::Result<()> {
    if !bundle.exists() {
        anyhow::bail!("git bundle not found: {}", bundle.display());
    }

    let git_dir = repo_root.join(".git");
    if !git_dir.exists() {
        run_git(repo_root, &["init", "--quiet"]).map_err(|e| anyhow::anyhow!("git init: {e}"))?;
    }

    // Park HEAD on a throwaway unborn branch before fetching. A fresh `git
    // init` puts HEAD on `refs/heads/main` (or `master`); git then refuses
    // `+refs/*:refs/*` because it would fetch into the currently checked-out
    // branch. Pointing HEAD at a ref the bundle does not contain makes every
    // real branch non-current, so the mirror fetch succeeds. We restore HEAD
    // to the real branch immediately after.
    run_git(
        repo_root,
        &["symbolic-ref", "HEAD", "refs/heads/__tcfs_bundle_restore"],
    )
    .map_err(|e| anyhow::anyhow!("parking HEAD before bundle fetch: {e}"))?;

    // Pull every ref from the bundle into the live repo. `+refs/*:refs/*`
    // mirrors heads, tags, and remotes so history is complete.
    let bundle_str = bundle.to_string_lossy().to_string();
    run_git(
        repo_root,
        &["fetch", "--quiet", &bundle_str, "+refs/*:refs/*"],
    )
    .map_err(|e| anyhow::anyhow!("git fetch from bundle: {e}"))?;

    // Restore HEAD to the bundle's default branch. The bundle stores the
    // symbolic HEAD target; resolve it so the peer lands on the same branch.
    if let Some(branch) = bundle_head_branch(bundle) {
        // Point HEAD at the branch without touching the working tree.
        let _ = run_git(repo_root, &["symbolic-ref", "HEAD", &branch]);
        // Make the index match HEAD without overwriting synced files.
        let _ = run_git(repo_root, &["reset", "--mixed", "--quiet"]);
    }

    Ok(())
}

/// Run a git command in `cwd`, returning an error with captured stderr on
/// non-zero exit.
pub(crate) fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| anyhow::anyhow!("running git {args:?}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {args:?} failed: {stderr}");
    }
    Ok(())
}

/// Result of a fast-forward ancestry probe between two commit SHAs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastForward {
    /// `local` is strictly ahead: the remote tip is an ancestor of the local
    /// tip. Pushing local wins without losing remote history.
    LocalAhead,
    /// `remote` is strictly ahead: the local tip is an ancestor of the remote
    /// tip. Pulling remote wins without losing local history.
    RemoteAhead,
    /// Neither is an ancestor of the other (divergent), the tips are equal, or
    /// ancestry could not be determined (e.g. a needed object is not present
    /// locally yet). Callers MUST treat this as "not a clean fast-forward" and
    /// fall back to leaving the conflict unresolved (fail-closed).
    NotFastForward,
}

/// Classify the relationship between a `local` and `remote` commit SHA inside
/// the repo rooted at `repo_root`, using only the local object store.
///
/// Both commit objects must already be present locally for the ancestry probe
/// to succeed — `.git/objects/**` roam as a content-addressed union and are
/// applied before refs, so by the time a ref conflict is reclassified the
/// objects should be present. If `git merge-base --is-ancestor` cannot run
/// (missing object, equal SHAs handled separately, or any git error) this
/// returns `NotFastForward` so the caller defers / stays conflicted rather than
/// half-applying a ref.
pub fn classify_fast_forward(repo_root: &Path, local_sha: &str, remote_sha: &str) -> FastForward {
    // Equal tips are not a fast-forward in either direction; the caller should
    // already have short-circuited identical content, but be explicit.
    if local_sha == remote_sha {
        return FastForward::NotFastForward;
    }

    // remote is ancestor of local => local strictly ahead => push local.
    if is_ancestor(repo_root, remote_sha, local_sha) {
        return FastForward::LocalAhead;
    }
    // local is ancestor of remote => remote strictly ahead => pull remote.
    if is_ancestor(repo_root, local_sha, remote_sha) {
        return FastForward::RemoteAhead;
    }
    FastForward::NotFastForward
}

/// True iff `ancestor` is an ancestor of `descendant` in `repo_root`.
///
/// Uses `git -C <repo> merge-base --is-ancestor <ancestor> <descendant>`, which
/// exits 0 when the ancestry holds, 1 when it does not, and a non-0/1 status
/// (with an error) when an object is missing. Any non-zero/non-1 outcome is
/// treated as "not an ancestor" so a missing object fails closed.
fn is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> bool {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "merge-base",
            "--is-ancestor",
            ancestor,
            descendant,
        ])
        .output();
    match output {
        Ok(out) => out.status.code() == Some(0),
        Err(_) => false,
    }
}

/// Read the commit SHA a packed/loose ref file points at, given the raw bytes of
/// the ref file (the content of `.git/refs/heads/<branch>`).
///
/// A loose ref file is either a 40/64-hex SHA on a single line, or a symbolic
/// `ref: refs/heads/<other>` redirect. Returns `None` for symbolic refs (the
/// caller should resolve those against the concrete branch) or unparseable
/// content.
pub fn parse_ref_sha(ref_bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(ref_bytes).ok()?.trim();
    if text.is_empty() || text.starts_with("ref:") {
        return None;
    }
    let token = text.split_whitespace().next()?;
    if is_hex_sha(token) {
        Some(token.to_string())
    } else {
        None
    }
}

/// True for a 40-char (SHA-1) or 64-char (SHA-256) lowercase/uppercase hex SHA.
fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Find the enclosing git repository root for a path under a `.git` directory.
///
/// `rel_under_git` is a repo-relative path whose first `.git` component marks
/// the repo. Returns the absolute repo root (the directory that contains
/// `.git`) by walking up from `local_root.join(rel)` until a `.git` component is
/// found. Returns `None` if the path is not under a `.git` directory.
pub fn repo_root_for_git_path(local_root: &Path, rel_path: &str) -> Option<std::path::PathBuf> {
    // Split the rel path on the first `.git` component. Everything before it is
    // the repo subdir (possibly empty).
    let mut prefix_components: Vec<&str> = Vec::new();
    let mut found = false;
    for comp in rel_path.split('/') {
        if comp == ".git" {
            found = true;
            break;
        }
        prefix_components.push(comp);
    }
    if !found {
        return None;
    }
    let mut root = local_root.to_path_buf();
    for c in prefix_components {
        if !c.is_empty() {
            root.push(c);
        }
    }
    Some(root)
}

/// Given a repo-relative `.git/...` path, return the branch ref name
/// (`refs/heads/<branch>`) it belongs to, if it is a head ref under
/// `.git/refs/heads/`. Returns `None` for non-head-ref paths (objects, index,
/// logs, packed-refs, HEAD, etc.).
pub fn head_ref_for_git_path(rel_path: &str) -> Option<String> {
    // Locate the `.git/refs/heads/` segment and take the remainder as the
    // branch name (which may itself contain slashes, e.g. `feature/x`).
    let needle = ".git/refs/heads/";
    let idx = rel_path.find(needle)?;
    let branch = &rel_path[idx + needle.len()..];
    if branch.is_empty() || branch.ends_with('/') {
        return None;
    }
    Some(format!("refs/heads/{branch}"))
}

/// Read the local commit SHA for `ref_name` (e.g. `refs/heads/main`) in
/// `repo_root`, consulting loose refs and packed-refs via `git rev-parse`.
/// Returns `None` if the ref cannot be resolved.
pub fn local_ref_sha(repo_root: &Path, ref_name: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-parse",
            "--verify",
            "--quiet",
            ref_name,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if is_hex_sha(&sha) {
        Some(sha)
    } else {
        None
    }
}

/// Resolve the bundle's default HEAD branch ref (e.g. `refs/heads/main`) by
/// listing its refs. Returns `None` if HEAD cannot be determined.
fn bundle_head_branch(bundle: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["bundle", "list-heads", &bundle.to_string_lossy()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    // `git bundle list-heads` prints `<sha> HEAD` for the symbolic head and
    // `<sha> refs/heads/<branch>` lines. Find the SHA that HEAD points at,
    // then map it back to a concrete branch ref.
    let mut head_sha: Option<String> = None;
    for line in listing.lines() {
        let mut parts = line.split_whitespace();
        let sha = parts.next().unwrap_or_default();
        let refname = parts.next().unwrap_or_default();
        if refname == "HEAD" {
            head_sha = Some(sha.to_string());
        }
    }
    let head_sha = head_sha?;
    for line in listing.lines() {
        let mut parts = line.split_whitespace();
        let sha = parts.next().unwrap_or_default();
        let refname = parts.next().unwrap_or_default();
        if sha == head_sha && refname.starts_with("refs/heads/") {
            return Some(refname.to_string());
        }
    }
    None
}

/// RAII guard for the cooperative `.git/tcfs.lock` lock used by the raw `.git`
/// sync path. Holding this guard means no other TCFS sync is collecting or
/// applying this repo's `.git`; dropping it removes the lock file so the next
/// cycle can proceed.
#[derive(Debug)]
pub struct GitLockGuard {
    lock_path: std::path::PathBuf,
    _file: std::fs::File,
}

impl Drop for GitLockGuard {
    fn drop(&mut self) {
        // Best-effort cleanup; a leaked lock is recovered by the caller's
        // staleness handling on the next acquire.
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Acquire a cooperative lock on `.git/tcfs.lock` for raw sync mode.
///
/// Uses `create_new` semantics: if the file already exists, another sync is in
/// progress and this fails. The returned [`GitLockGuard`] removes the lock file
/// when dropped, so callers hold it across the collect/apply window to make the
/// `.git` snapshot atomic against a concurrent commit (TOCTOU guard).
pub fn acquire_git_lock(git_dir: &Path) -> anyhow::Result<GitLockGuard> {
    use std::fs::OpenOptions;

    let lock_path = git_dir.join("tcfs.lock");

    // Fail if lock already exists (another sync in progress)
    if lock_path.exists() {
        anyhow::bail!(
            "could not acquire tcfs.lock in {} (another sync in progress?)",
            git_dir.display()
        );
    }

    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| anyhow::anyhow!("creating tcfs.lock: {e}"))?;

    Ok(GitLockGuard {
        lock_path,
        _file: file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_empty_git() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let check = git_is_safe(&git_dir);
        assert!(check.blocking.is_empty());
    }

    #[test]
    fn test_unsafe_with_lock() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("index.lock"), b"").unwrap();

        let check = git_is_safe(&git_dir);
        assert!(!check.blocking.is_empty());
        assert!(check.blocking[0].contains("index.lock"));
    }

    #[test]
    fn test_unsafe_with_rebase() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::create_dir_all(git_dir.join("rebase-merge")).unwrap();

        let check = git_is_safe(&git_dir);
        assert!(!check.blocking.is_empty());
        assert!(check.blocking[0].contains("rebase"));
    }

    #[test]
    fn test_unsafe_with_merge() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("MERGE_HEAD"), b"abc123").unwrap();

        let check = git_is_safe(&git_dir);
        assert!(!check.blocking.is_empty());
        assert!(check.blocking[0].contains("merge"));
    }
}
