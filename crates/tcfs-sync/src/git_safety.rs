//! Git directory sync safety checks.
//!
//! Before syncing .git directories, validates that no git operations
//! are in progress (no lock files, no rebase/merge/cherry-pick).

use std::path::Path;

const GIT_ROUTING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_QUARANTINE_PATH",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_REPLACE_REF_BASE",
    "GIT_GRAFT_FILE",
];

/// Build a Git command that cannot inherit repository-routing overrides from
/// the daemon/service environment. Local repository configuration still loads;
/// callers must validate its reported topology before mutation.
pub(crate) fn sanitized_git_command() -> std::process::Command {
    let mut command = std::process::Command::new("git");
    for variable in GIT_ROUTING_ENV {
        command.env_remove(variable);
    }
    // `git -c` values can also be injected through a numbered environment
    // protocol. Remove every inherited member, not just COUNT.
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("GIT_CONFIG_KEY_") || key_text.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(key);
        }
    }
    // `update-ref` can invoke reference-transaction hooks, and `status` can
    // invoke a configured fsmonitor hook. The resolver must never execute code
    // carried inside the roamed repository while handling a conflict.
    #[cfg(windows)]
    let null_device = "NUL";
    #[cfg(not(windows))]
    let null_device = "/dev/null";
    command
        .arg("-c")
        .arg(format!("core.hooksPath={null_device}"))
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.logAllRefUpdates=false");
    command
}

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
        "shallow.lock",
        "packed-refs.lock",
    ];

    for lock in &lock_files {
        let lock_path = git_dir.join(lock);
        if lock_path.exists() {
            check.blocking.push(format!("lock file exists: {}", lock));
        }
    }
    collect_ref_head_locks(&git_dir.join("refs/heads"), "refs/heads", &mut check);

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

fn collect_ref_head_locks(dir: &Path, rel: &str, check: &mut GitSafetyCheck) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let child_rel = format!("{rel}/{name}");
        if path.is_dir() {
            collect_ref_head_locks(&path, &child_rel, check);
        } else if name.ends_with(".lock") {
            check
                .blocking
                .push(format!("lock file exists: {child_rel}"));
        }
    }
}

/// Create a git bundle for atomic .git snapshot.
///
/// Runs `git bundle create --all` to create a single file containing
/// all refs and objects, suitable for transporting a complete repository.
pub fn snapshot_git_for_sync(repo_root: &Path) -> anyhow::Result<std::path::PathBuf> {
    let bundle_path = repo_root.join(GIT_BUNDLE_REL_PATH);

    let output = sanitized_git_command()
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
    let output = sanitized_git_command()
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
/// This runs at PLAN time (see `reclassify_git_ff_conflicts`), before any of
/// the current cycle's pulls have executed, so the remote tip's commit may not
/// be present in the local object store yet. In that case `git merge-base
/// --is-ancestor` fails and this returns `NotFastForward` — an implicit DEFER:
/// the repo stays conflicted this cycle while its `.git/objects/**` (which are
/// content-addressed and non-conflicting) still roam, and a later cycle
/// re-probes with the objects present. Equal SHAs and any other git error also
/// return `NotFastForward`, so the caller never half-applies a ref
/// (fail-closed).
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
    let output = sanitized_git_command()
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
    let rel_path = rel_path.replace('\\', "/");
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
    let rel_path = rel_path.replace('\\', "/");
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
    let output = sanitized_git_command()
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
    let output = sanitized_git_command()
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
        // Best-effort cleanup. If this never runs (SIGKILL, power loss), the
        // leaked lock is recovered by `acquire_git_lock`'s staleness check: a
        // lock older than `GIT_LOCK_STALE_SECS` whose recorded owner PID is no
        // longer alive is removed and the acquire retried once.
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Age past which a `tcfs.lock` becomes eligible for the dead-owner staleness
/// check in [`acquire_git_lock`].
const GIT_LOCK_STALE_SECS: u64 = 600; // 10 minutes

/// Acquire a cooperative lock on `.git/tcfs.lock` for raw sync mode.
///
/// Uses `create_new` semantics: if the file already exists, another sync is in
/// progress and this fails — unless the existing lock is STALE (older than
/// [`GIT_LOCK_STALE_SECS`] AND its recorded owner PID is no longer alive, e.g.
/// the holder was SIGKILLed before its Drop ran), in which case the stale file
/// is removed and the acquire retried once. The lock file records
/// `<pid> <unix-secs>` on acquire to make that check possible. The returned
/// [`GitLockGuard`] removes the lock file when dropped, so callers hold it
/// across the collect/apply window to make the `.git` snapshot atomic against
/// a concurrent commit (TOCTOU guard).
pub fn acquire_git_lock(git_dir: &Path) -> anyhow::Result<GitLockGuard> {
    let lock_path = git_dir.join("tcfs.lock");

    match try_create_git_lock(&lock_path) {
        Ok(guard) => Ok(guard),
        Err(first_err) => {
            if remove_stale_git_lock(&lock_path) {
                // Retry once after clearing a stale lock.
                try_create_git_lock(&lock_path)
            } else {
                Err(first_err)
            }
        }
    }
}

/// One `create_new` attempt on the lock file, recording owner PID + acquire
/// time so a leaked lock can later be detected as stale.
fn try_create_git_lock(lock_path: &Path) -> anyhow::Result<GitLockGuard> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| {
            anyhow::anyhow!(
                "could not acquire tcfs.lock at {} (another sync in progress?): {e}",
                lock_path.display()
            )
        })?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = writeln!(file, "{} {now}", std::process::id());

    Ok(GitLockGuard {
        lock_path: lock_path.to_path_buf(),
        _file: file,
    })
}

/// Remove `lock_path` iff it is stale: older than [`GIT_LOCK_STALE_SECS`] AND
/// its recorded owner PID is no longer alive. Returns `true` only when the
/// stale file was actually removed. Any ambiguity (unreadable, unparseable,
/// young, owner alive or unprobeable) leaves the lock in place (fail-closed:
/// never steal a lock that might be held).
fn remove_stale_git_lock(lock_path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(lock_path) else {
        return false;
    };
    let old_enough = meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .map(|age| age.as_secs() > GIT_LOCK_STALE_SECS)
        .unwrap_or(false);
    if !old_enough {
        return false;
    }
    let Some(owner_pid) = std::fs::read_to_string(lock_path)
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .and_then(|t| t.parse::<i32>().ok())
    else {
        return false;
    };
    if pid_alive(owner_pid) {
        return false;
    }
    if std::fs::remove_file(lock_path).is_ok() {
        tracing::warn!(
            lock = %lock_path.display(),
            owner_pid,
            "removed stale tcfs.lock (owner dead, older than 10min)"
        );
        true
    } else {
        false
    }
}

/// True if a process with `pid` exists. Probes with `kill(pid, 0)`, which
/// signals nothing: success or `EPERM` (exists but not ours) both mean alive.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Non-unix fallback: report every PID as alive so a lock is never stolen on
/// platforms where liveness cannot be probed (fail-closed).
#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_git_command_removes_repository_routing_environment() {
        let command = sanitized_git_command();
        let removed = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().to_string())
            .collect::<std::collections::BTreeSet<_>>();

        for variable in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_IMPLICIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_QUARANTINE_PATH",
            "GIT_NO_REPLACE_OBJECTS",
            "GIT_CONFIG_COUNT",
        ] {
            assert!(removed.contains(variable), "{variable} was not removed");
        }
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg.starts_with("core.hooksPath=")));
        assert!(args.iter().any(|arg| arg == "core.fsmonitor=false"));
        assert!(args.iter().any(|arg| arg == "core.logAllRefUpdates=false"));
    }

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
    fn test_unsafe_with_nested_branch_lock() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        let branch_dir = git_dir.join("refs/heads/feature");
        std::fs::create_dir_all(&branch_dir).unwrap();
        std::fs::write(branch_dir.join("x.lock"), b"").unwrap();

        let check = git_is_safe(&git_dir);
        assert!(
            check
                .blocking
                .iter()
                .any(|entry| entry.contains("refs/heads/feature/x.lock")),
            "nested branch lock must block: {:?}",
            check.blocking
        );
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
    fn test_git_lock_stale_dead_owner_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().to_path_buf();
        let lock_path = git_dir.join("tcfs.lock");

        // Leaked lock: dead owner PID (way past any real pid space), mtime
        // pinned older than the staleness threshold.
        std::fs::write(&lock_path, "999999999 0\n").unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(GIT_LOCK_STALE_SECS + 60);
        let f = std::fs::File::options()
            .write(true)
            .open(&lock_path)
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        drop(f);

        let guard = acquire_git_lock(&git_dir).expect("stale dead-owner lock must be recovered");
        drop(guard);
        assert!(!lock_path.exists(), "guard drop must remove the lock");
    }

    #[test]
    fn test_git_lock_live_owner_still_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().to_path_buf();
        let lock_path = git_dir.join("tcfs.lock");

        // Lock held by THIS (alive) process, but with an old mtime: age alone
        // must never be grounds to steal a lock whose owner is alive.
        std::fs::write(&lock_path, format!("{} 0\n", std::process::id())).unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(GIT_LOCK_STALE_SECS + 60);
        let f = std::fs::File::options()
            .write(true)
            .open(&lock_path)
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        drop(f);

        assert!(
            acquire_git_lock(&git_dir).is_err(),
            "live-owner lock must not be stolen"
        );
        assert!(lock_path.exists());
    }

    #[test]
    fn test_git_lock_fresh_lock_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().to_path_buf();

        let _guard = acquire_git_lock(&git_dir).expect("first acquire");
        assert!(
            acquire_git_lock(&git_dir).is_err(),
            "second acquire must fail while the lock is held"
        );
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
