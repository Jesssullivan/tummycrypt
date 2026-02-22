//! Git directory sync safety checks.
//!
//! Before syncing .git directories, validates that no git operations
//! are in progress (no lock files, no rebase/merge/cherry-pick).

use std::path::Path;

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
            check
                .blocking
                .push(format!("lock file exists: {}", lock));
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
                    check
                        .warnings
                        .push("FETCH_HEAD is stale (>1h old)".into());
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
    let bundle_path = repo_root.join(".git-tcfs-bundle");

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

/// Restore a git repository from a bundle.
pub fn restore_git_from_bundle(bundle: &Path, target: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .args([
            "clone",
            &bundle.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("running git clone from bundle: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git clone from bundle failed: {stderr}");
    }

    Ok(())
}

/// Acquire a cooperative lock on .git/tcfs.lock for raw sync mode.
///
/// Uses `create_new` semantics: if the file already exists, another sync
/// is in progress. The lock file is removed on drop via the caller.
pub fn acquire_git_lock(git_dir: &Path) -> anyhow::Result<std::fs::File> {
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

    Ok(file)
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
