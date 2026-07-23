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
use crate::index_entry::RemoteEntryKind;
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

fn repo_git_command(
    repo_root: &Path,
    anchor: Option<&GitRepoAnchor>,
) -> Result<std::process::Command> {
    if let Some(anchor) = anchor {
        anchor.root_git_command()
    } else {
        let mut command = git_safety::sanitized_git_command();
        command.arg("-c").arg("core.fsync=reference");
        command.current_dir(repo_root);
        Ok(command)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn descriptor_rooted_sanitized_git_command(
    directory: &std::fs::File,
) -> Result<std::process::Command> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let directory = directory
        .try_clone()
        .context("cloning descriptor-rooted Git cwd")?;
    let mut command = git_safety::sanitized_git_command();
    // SAFETY: `fchdir(2)` is async-signal-safe. The callback owns the cloned
    // descriptor, so delayed spawn cannot observe a closed/reused fd number.
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(directory.as_raw_fd()) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    Ok(command)
}

fn git_output_at(
    repo_root: &Path,
    anchor: Option<&GitRepoAnchor>,
    args: &[&str],
) -> Result<String> {
    let output = repo_git_command(repo_root, anchor)?
        .args(args)
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn require_same_principal_git_entry(
    path: &Path,
    metadata: &std::fs::Metadata,
    description: &str,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` has no preconditions and only reads process identity.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        bail!(
            "{description} must be owned by tcfsd effective uid {effective_uid}, got uid {}: {}",
            metadata.uid(),
            path.display()
        );
    }
    if metadata.mode() & 0o022 != 0 {
        bail!(
            "{description} must not be group/world-writable for registered-root Git resolution: {}",
            path.display()
        );
    }
    crate::path_acl::reject_write_grant_acl(path)
        .with_context(|| format!("validating registered-root Git ACL: {}", path.display()))?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn require_same_principal_git_entry(
    _path: &Path,
    _metadata: &std::fs::Metadata,
    _description: &str,
) -> Result<()> {
    bail!("registered-root Git trust validation is supported only on Linux and macOS")
}

/// Validate that every canonical ancestor is owned by the daemon effective
/// user or root and cannot be renamed by another principal. Root-owned sticky
/// boundaries (for example `/tmp`) are accepted only when the next child is
/// itself owned by the daemon user or root.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn validate_trusted_ancestor_chain(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("canonicalizing trusted path: {}", path.display()))?;
    // SAFETY: `geteuid` has no preconditions and only reads process identity.
    let effective_uid = unsafe { libc::geteuid() };
    let chain = canonical_path.ancestors().collect::<Vec<_>>();
    for index in (1..chain.len()).rev() {
        let ancestor = chain[index];
        let next_child = chain[index - 1];
        let metadata = std::fs::symlink_metadata(ancestor)
            .with_context(|| format!("inspecting trusted path ancestor: {}", ancestor.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "trusted path ancestor must be a real directory: {}",
                ancestor.display()
            );
        }
        if metadata.uid() != effective_uid && metadata.uid() != 0 {
            bail!(
                "trusted path ancestor must be owned by effective uid {effective_uid} or root, got uid {}: {}",
                metadata.uid(),
                ancestor.display()
            );
        }
        crate::path_acl::reject_write_grant_acl(ancestor).with_context(|| {
            format!(
                "validating trusted path ancestor ACL: {}",
                ancestor.display()
            )
        })?;
        if metadata.mode() & 0o022 == 0 {
            continue;
        }

        let sticky_root_directory = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        let child_metadata = std::fs::symlink_metadata(next_child).with_context(|| {
            format!(
                "inspecting child below sticky trusted path ancestor: {}",
                next_child.display()
            )
        })?;
        let protected_child = !child_metadata.file_type().is_symlink()
            && (child_metadata.uid() == effective_uid || child_metadata.uid() == 0);
        if !sticky_root_directory || !protected_child {
            bail!(
                "trusted path ancestor is writable by another principal without a protected root-owned sticky boundary: {}",
                ancestor.display()
            );
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn validate_trusted_ancestor_chain(_path: &Path) -> Result<()> {
    bail!("registered-root trusted-ancestor validation is supported only on Linux and macOS")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn configured_component_requires_acl(file_type: &std::fs::FileType) -> bool {
    file_type.is_dir() || (cfg!(target_os = "macos") && file_type.is_symlink())
}

/// Validate both the original configured spelling of a directory path and its
/// canonical destination.
///
/// Canonical-only validation loses the security properties of an alias such
/// as `/writable/route -> /safe/repo`: another principal can replace `route`
/// even when the destination itself is pristine. This walk lstat(2)s every
/// lexical component before canonicalization, accepts only root/euid-owned
/// directories and symlinks, and permits a writable parent only when it is a
/// root-owned sticky boundary protecting the next root/euid-owned entry. The
/// final canonical target and its ancestor chain are then validated too.
/// Root-owned platform aliases such as macOS `/var -> /private/var` remain
/// valid because the symlink and its parent are not mutable by another user.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn validate_trusted_configured_path(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;

    let absolute = std::path::absolute(path)
        .with_context(|| format!("making configured path absolute: {}", path.display()))?;
    let mut current = PathBuf::new();
    let mut prefixes = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) => {
                bail!(
                    "configured path contains an unsupported platform prefix: {}",
                    absolute.display()
                );
            }
            Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
                prefixes.push(current.clone());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                bail!(
                    "configured path must not contain parent traversal: {}",
                    absolute.display()
                );
            }
        }
    }
    if prefixes.is_empty() {
        bail!("configured path is empty");
    }

    // SAFETY: `geteuid` has no preconditions and only reads process identity.
    let effective_uid = unsafe { libc::geteuid() };
    for (index, prefix) in prefixes.iter().enumerate() {
        let metadata = std::fs::symlink_metadata(prefix).with_context(|| {
            format!(
                "inspecting original configured path component: {}",
                prefix.display()
            )
        })?;
        let file_type = metadata.file_type();
        if !metadata.is_dir() && !file_type.is_symlink() {
            bail!(
                "configured path component must be a real directory or protected symlink: {}",
                prefix.display()
            );
        }
        if metadata.uid() != effective_uid && metadata.uid() != 0 {
            bail!(
                "configured path component must be owned by effective uid {effective_uid} or root, got uid {}: {}",
                metadata.uid(),
                prefix.display()
            );
        }
        // macOS extended ACLs can be attached to the lexical symlink itself.
        // Inspect them with acl_get_link_np before canonicalization so an
        // allow-write entry on a root/euid-owned route cannot hide behind an
        // otherwise pristine destination. Linux lgetxattr behavior for
        // symlink ACL namespaces varies by filesystem, so Linux retains the
        // directory-only check and relies on owner/sticky-boundary protection
        // for lexical symlinks.
        if configured_component_requires_acl(&file_type) {
            crate::path_acl::reject_write_grant_acl(prefix).with_context(|| {
                format!(
                    "validating configured path component ACL: {}",
                    prefix.display()
                )
            })?;
        }
        if !metadata.is_dir() || metadata.mode() & 0o022 == 0 {
            continue;
        }

        let Some(next_prefix) = prefixes.get(index + 1) else {
            bail!(
                "configured directory is group/world-writable: {}",
                prefix.display()
            );
        };
        let next_metadata = std::fs::symlink_metadata(next_prefix).with_context(|| {
            format!(
                "inspecting entry below sticky configured path boundary: {}",
                next_prefix.display()
            )
        })?;
        let sticky_root_directory = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        let protected_child = next_metadata.uid() == effective_uid || next_metadata.uid() == 0;
        if !sticky_root_directory || !protected_child {
            bail!(
                "configured path component is writable by another principal without a protected root-owned sticky boundary: {}",
                prefix.display()
            );
        }
    }

    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("canonicalizing configured path: {}", absolute.display()))?;
    let final_metadata = std::fs::symlink_metadata(&canonical)
        .with_context(|| format!("inspecting configured path target: {}", canonical.display()))?;
    if final_metadata.file_type().is_symlink() || !final_metadata.is_dir() {
        bail!(
            "configured path target must be a real directory: {}",
            canonical.display()
        );
    }
    require_same_principal_git_entry(&canonical, &final_metadata, "configured path target")?;
    validate_trusted_ancestor_chain(&canonical)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn validate_trusted_configured_path(_path: &Path) -> Result<()> {
    bail!("configured-path trust validation is supported only on Linux and macOS")
}

fn require_real_git_directory(path: &Path, description: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting {description}: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{description} must be a real directory: {}", path.display());
    }
    require_same_principal_git_entry(path, &metadata, description)?;
    Ok(())
}

fn validate_real_git_directory_if_present(path: &Path, description: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("{description} must be a real directory: {}", path.display());
            }
            require_same_principal_git_entry(path, &metadata, description)?;
            reject_redirects_under_git_dir(path, description)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting {description}: {}", path.display()));
        }
    }
    Ok(())
}

fn require_real_git_file_if_present(path: &Path, description: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("{description} must be a real file: {}", path.display());
            }
            require_same_principal_git_entry(path, &metadata, description)?;
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
        require_same_principal_git_entry(&entry_path, &metadata, description)?;
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
    validate_standalone_repo_topology_at(repo_root, None)
}

fn validate_standalone_repo_topology_at(
    repo_root: &Path,
    anchor: Option<&GitRepoAnchor>,
) -> Result<()> {
    let repo_root = if let Some(anchor) = anchor {
        anchor.revalidate()?;
        anchor.canonical_root.clone()
    } else {
        repo_root
            .canonicalize()
            .with_context(|| format!("canonicalizing git repo root: {}", repo_root.display()))?
    };
    if !repo_root.is_dir() {
        bail!("git repo root is not a directory: {}", repo_root.display());
    }
    validate_trusted_ancestor_chain(&repo_root)?;
    let repo_metadata = std::fs::symlink_metadata(&repo_root)
        .with_context(|| format!("inspecting git repo root: {}", repo_root.display()))?;
    require_same_principal_git_entry(&repo_root, &repo_metadata, "Git repo root")?;

    let git_dir_path = repo_root.join(".git");
    let metadata = std::fs::symlink_metadata(&git_dir_path)
        .with_context(|| format!("inspecting git directory: {}", git_dir_path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "linked or redirected .git metadata is outside the standalone resolver seam: {}",
            git_dir_path.display()
        );
    }
    require_same_principal_git_entry(&git_dir_path, &metadata, "Git metadata directory")?;
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
            require_same_principal_git_entry(&alternates, &metadata, "Git alternates file")?;
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

    let effective_config = repo_git_command(&repo_root, anchor)?
        .args(["config", "--null", "--name-only", "--list"])
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
        let key = String::from_utf8_lossy(key).trim().to_ascii_lowercase();
        let remote_promisor = key.starts_with("remote.")
            && (key.ends_with(".promisor") || key.ends_with(".partialclonefilter"));
        if key == "extensions.partialclone"
            || key == "extensions.refstorage"
            || remote_promisor
            || key == "protocol.ext.allow"
            || key == "core.alternaterefscommand"
            || key.starts_with("fsck.")
        {
            bail!("effective Git config key {key} is outside the registered-root resolver seam");
        }
    }

    // The sanitized builder appends `core.sharedRepository=0` as a final
    // defense, so this query always includes one safe command-scope value.
    // Reject every additional non-false value from repository/global/system
    // config: shared-repository mode can make Git itself recreate refs with
    // group/world write permission after our topology preflight.
    let shared_repository = repo_git_command(&repo_root, anchor)?
        .args(["config", "--null", "--get-all", "core.sharedRepository"])
        .output()
        .context("inspecting effective Git shared-repository mode")?;
    match shared_repository.status.code() {
        Some(0) => {
            for value in shared_repository
                .stdout
                .split(|byte| *byte == 0)
                .filter(|value| !value.is_empty())
            {
                let value = String::from_utf8_lossy(value).trim().to_ascii_lowercase();
                if !matches!(value.as_str(), "0" | "false" | "no" | "off") {
                    bail!(
                        "effective core.sharedRepository={value} is outside the same-principal registered-root resolver seam"
                    );
                }
            }
        }
        Some(1) => {}
        _ => bail!(
            "effective Git shared-repository inspection failed: {}",
            String::from_utf8_lossy(&shared_repository.stderr)
        ),
    }

    let reported_top = git_output_at(&repo_root, anchor, &["rev-parse", "--show-toplevel"])?;
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

    let reported_git_dir = git_output_at(&repo_root, anchor, &["rev-parse", "--absolute-git-dir"])?;
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

    let reported_common_dir = git_output_at(
        &repo_root,
        anchor,
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

    let is_bare = git_output_at(&repo_root, anchor, &["rev-parse", "--is-bare-repository"])?;
    if is_bare != "false" {
        bail!("bare Git repositories are outside the standalone resolver seam");
    }
    let inside_worktree =
        git_output_at(&repo_root, anchor, &["rev-parse", "--is-inside-work-tree"])?;
    if inside_worktree != "true" {
        bail!("Git does not report the enrolled root as a working tree");
    }
    if let Some(anchor) = anchor {
        anchor.revalidate()?;
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
    /// Exact value written by this transaction. Rollback uses it as Git's old
    /// value CAS so it cannot erase a concurrent operator update.
    written_sha: String,
}

/// Stable filesystem capability captured by the daemon immediately after it
/// authorizes a canonical repository root.
///
/// Every Git child in the registered-root resolver enters this descriptor via
/// child-side `fchdir(2)`. The canonical pathname is retained only for
/// operator messages, conflict-cache matching, and read-only identity checks;
/// it is never used as a Git mutation cwd after authorization.
pub struct GitRepoAnchor {
    canonical_root: PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    directory: std::fs::File,
    git_directory: std::fs::File,
    #[cfg(test)]
    before_update_ref: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_conflict_state_flush: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for GitRepoAnchor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitRepoAnchor")
            .field("canonical_root", &self.canonical_root)
            .finish_non_exhaustive()
    }
}

impl GitRepoAnchor {
    pub fn capture(repo_root: &Path) -> Result<Self> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = repo_root;
            bail!(
                "registered-root Git resolution requires descriptor-anchored commands on Linux or macOS"
            );
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::unix::fs::OpenOptionsExt;

            let canonical_root = repo_root
                .canonicalize()
                .with_context(|| format!("canonicalizing repo root: {}", repo_root.display()))?;
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let directory = options.open(&canonical_root).with_context(|| {
                format!(
                    "opening authorized repo root capability: {}",
                    canonical_root.display()
                )
            })?;
            Self::capture_from_authorized_root(&canonical_root, directory)
        }
    }

    /// Build an anchor from a root descriptor that an earlier authority check
    /// already opened and retained.
    ///
    /// The supplied descriptor, not a reopened pathname, becomes the worktree
    /// capability. `.git` is opened relative to it with `O_NOFOLLOW`; the
    /// canonical spelling is retained for identity revalidation and operator
    /// diagnostics only.
    pub(crate) fn capture_from_authorized_root(
        canonical_root: &Path,
        directory: std::fs::File,
    ) -> Result<Self> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (canonical_root, directory);
            bail!(
                "registered-root Git resolution requires descriptor-anchored commands on Linux or macOS"
            );
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let metadata = directory
                .metadata()
                .context("inspecting authorized repo root descriptor")?;
            if !metadata.is_dir() {
                bail!("authorized repo root descriptor is not a directory");
            }
            if !canonical_root.is_absolute() {
                bail!(
                    "authorized repo root spelling is not absolute: {}",
                    canonical_root.display()
                );
            }

            let git_directory = git_safety::open_git_directory_at(&directory)?;
            let anchor = Self {
                canonical_root: canonical_root.to_owned(),
                directory,
                git_directory,
                #[cfg(test)]
                before_update_ref: std::sync::Mutex::new(None),
                #[cfg(test)]
                after_conflict_state_flush: std::sync::Mutex::new(None),
            };
            anchor.revalidate()?;
            Ok(anchor)
        }
    }

    pub(crate) fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            bail!(
                "registered-root Git resolution requires descriptor identity checks on Linux or macOS"
            );
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

            let current = self.canonical_root.canonicalize().with_context(|| {
                format!(
                    "revalidating authorized repo root: {}",
                    self.canonical_root.display()
                )
            })?;
            if current != self.canonical_root {
                bail!(
                    "registered repo root changed after authorization: {} now resolves to {}",
                    self.canonical_root.display(),
                    current.display()
                );
            }
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let current_directory = options.open(&current).with_context(|| {
                format!(
                    "reopening authorized repo root identity: {}",
                    current.display()
                )
            })?;
            let authorized_metadata = self
                .directory
                .metadata()
                .context("inspecting authorized repo root descriptor")?;
            let current_metadata = current_directory
                .metadata()
                .context("inspecting current repo root descriptor")?;
            if authorized_metadata.dev() != current_metadata.dev()
                || authorized_metadata.ino() != current_metadata.ino()
            {
                bail!(
                    "registered repo root was replaced after authorization: {}",
                    self.canonical_root.display()
                );
            }
            let current_git_directory = git_safety::open_git_directory_at(&current_directory)?;
            let authorized_git_metadata = self
                .git_directory
                .metadata()
                .context("inspecting authorized Git metadata descriptor")?;
            let current_git_metadata = current_git_directory
                .metadata()
                .context("inspecting current Git metadata descriptor")?;
            if authorized_git_metadata.dev() != current_git_metadata.dev()
                || authorized_git_metadata.ino() != current_git_metadata.ino()
            {
                bail!(
                    "registered repo Git metadata was replaced after authorization: {}",
                    self.canonical_root.join(".git").display()
                );
            }
            Ok(())
        }
    }

    /// Git command rooted in the captured worktree for legacy clean-worktree
    /// inspection and the separately fenced mutation path.
    fn root_git_command(&self) -> Result<std::process::Command> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            bail!("registered-root Git resolution requires child-side fchdir on Linux or macOS");
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            descriptor_rooted_sanitized_git_command(&self.directory)
        }
    }

    fn local_ref_sha(&self, ref_name: &str) -> Option<String> {
        let output = self
            .metadata_git_command()
            .ok()?
            .args(["rev-parse", "--verify", "--quiet", ref_name])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        git_safety::parse_ref_sha(&output.stdout)
    }

    fn run_git(&self, args: &[&str]) -> Result<()> {
        let output = self
            .metadata_git_command()?
            .args(args)
            .output()
            .with_context(|| format!("running descriptor-anchored git {args:?}"))?;
        if !output.status.success() {
            bail!(
                "descriptor-anchored git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    /// Git command rooted directly in the captured metadata directory.
    /// `GIT_DIR=.` and `GIT_COMMON_DIR=.` prevent a swapped `.git` entry or a
    /// late `commondir` file from redirecting ref/object operations elsewhere.
    fn metadata_git_command(&self) -> Result<std::process::Command> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            bail!("registered-root Git resolution requires metadata fchdir on Linux or macOS");
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut command = descriptor_rooted_sanitized_git_command(&self.git_directory)?;
            command
                .arg("-c")
                .arg("core.fsync=reference")
                .env("GIT_DIR", ".")
                .env("GIT_COMMON_DIR", ".");
            Ok(command)
        }
    }

    #[cfg(test)]
    fn set_before_update_ref_hook(&mut self, hook: impl Fn() + Send + Sync + 'static) {
        *self.before_update_ref.lock().expect("test hook lock") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_before_update_ref_hook(&self) {
        let hook = self
            .before_update_ref
            .lock()
            .expect("test hook lock")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn run_before_update_ref_hook(&self) {}

    #[cfg(test)]
    fn set_after_conflict_state_flush_hook(&mut self, hook: impl Fn() + Send + Sync + 'static) {
        *self
            .after_conflict_state_flush
            .lock()
            .expect("test hook lock") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_after_conflict_state_flush_hook(&self) {
        let hook = self
            .after_conflict_state_flush
            .lock()
            .expect("test hook lock")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn run_after_conflict_state_flush_hook(&self) {}

    fn acquire_git_lock(&self) -> Result<git_safety::GitLockGuard> {
        git_safety::acquire_git_lock_at(&self.git_directory)
    }
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
    authorized_anchor: Option<&GitRepoAnchor>,
    remote_prefix: &str,
    device_id: &str,
    undo_state_dir: &Path,
    mode: GitKeepBothMode,
    encryption: OptionalEncryption<'_>,
) -> Result<GitKeepBothResult> {
    let owned_anchor = authorized_anchor
        .is_none()
        .then(|| GitRepoAnchor::capture(repo_root))
        .transpose()?;
    let anchor = authorized_anchor
        .or(owned_anchor.as_ref())
        .expect("an authorized or locally captured repository anchor is always present");
    anchor.revalidate()?;
    let repo_root = anchor.canonical_root.clone();
    let git_dir = repo_root.join(".git");
    validate_standalone_repo_topology_at(&repo_root, Some(anchor))?;

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
    ensure_clean_worktree(&repo_root, Some(anchor))?;
    anchor
        .run_git(&["fsck", "--full"])
        .context("pre-resolution git fsck")?;

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
            &candidate.rel_path,
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
        anchor.revalidate()?;
        ensure_commit_present_at(anchor, &remote_sha)?;
        let local_sha = anchor
            .local_ref_sha(head_ref)
            .ok_or_else(|| anyhow!("local ref is missing: {}", head_ref))?;
        if local_sha == remote_sha {
            bail!(
                "{} is no longer divergent; rerun reconcile before keep-both",
                head_ref
            );
        }
        let park_ref = park_ref_for_available_at(anchor, remote_device, head_ref, &remote_sha)?;
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
        anchor.revalidate()?;
        return Ok(GitKeepBothResult {
            repo_root,
            mode,
            parked_refs,
            undo_bundle: None,
        });
    }

    anchor.revalidate()?;
    let _guard = anchor
        .acquire_git_lock()
        .with_context(|| format!("acquiring {}", git_dir.join("tcfs.lock").display()))?;

    // Re-check after acquiring the cooperative lock. The lock protects TCFS
    // peers; a local git command may still have started immediately before the
    // lock landed.
    anchor.revalidate()?;
    validate_standalone_repo_topology_at(&repo_root, Some(anchor))
        .context("git topology changed before locked execute")?;
    let safety = git_safety::git_is_safe(&git_dir);
    if !safety.blocking.is_empty() {
        bail!("git repository became busy: {}", safety.blocking.join("; "));
    }
    ensure_clean_worktree(&repo_root, Some(anchor))?;
    anchor
        .run_git(&["fsck", "--full"])
        .context("locked pre-resolution git fsck")?;

    for parked in &parked_refs {
        ensure_local_ref_still_pinned_at(anchor, parked)?;
        ensure_selected_park_ref_available_at(anchor, &parked.park_ref, &parked.remote_sha)?;
    }

    // The undo bundle captures `--all` refs before we park anything. It is only
    // meaningful when refs actually change, so skip it for a pure keep-local
    // group (no write-only cost). It is written under the machine-local state
    // dir, never in-tree (BLOCKING 2 / design S6).
    anchor.revalidate()?;
    let undo_bundle = if parked_refs.is_empty() {
        None
    } else {
        Some(write_undo_bundle_at(anchor, undo_state_dir)?)
    };
    let mut applied = Vec::new();
    for parked in &parked_refs {
        anchor.revalidate()?;
        if anchor.local_ref_sha(&parked.park_ref).as_deref() == Some(parked.remote_sha.as_str()) {
            continue;
        }
        let zero = zero_oid_like(&parked.remote_sha);
        let args = [
            "update-ref",
            parked.park_ref.as_str(),
            parked.remote_sha.as_str(),
            zero.as_str(),
        ];
        anchor.run_before_update_ref_hook();
        if let Err(err) = anchor.run_git(&args) {
            rollback_refs_at(anchor, &applied).context("rolling back previously parked refs")?;
            return Err(err).with_context(|| format!("parking {}", parked.head_ref));
        }
        applied.push(AppliedParkRef {
            ref_name: parked.park_ref.clone(),
            previous_sha: None,
            written_sha: parked.remote_sha.clone(),
        });
        if let Err(error) = anchor.revalidate() {
            rollback_refs_at(anchor, &applied)
                .context("rolling back refs after authorized root identity changed")?;
            return Err(error)
                .context("registered repo root changed immediately after parking a ref");
        }
        if let Err(error) = durably_confirm_parked_ref(anchor, &parked.park_ref, &parked.remote_sha)
        {
            rollback_refs_at(anchor, &applied)
                .context("rolling back refs after durability barrier failed")?;
            return Err(error).context("durably committing parked ref before clearing state");
        }
    }

    if let Err(error) = anchor.revalidate() {
        rollback_refs_at(anchor, &applied)
            .context("rolling back refs after authorized root identity changed")?;
        return Err(error).context("registered repo root changed before post-resolution git fsck");
    }
    if let Err(err) = anchor.run_git(&["fsck", "--full"]) {
        rollback_refs_at(anchor, &applied).context("rolling back refs after git fsck failed")?;
        return Err(err).context("post-resolution git fsck");
    }
    if let Err(error) = anchor.revalidate() {
        rollback_refs_at(anchor, &applied)
            .context("rolling back refs after authorized root identity changed")?;
        return Err(error).context("registered repo root changed after post-resolution git fsck");
    }

    let state_snapshot = state.snapshot_cache_keys(
        candidates
            .iter()
            .map(|candidate| candidate.cache_key.as_str()),
    );
    let original_conflict_entries = candidates
        .iter()
        .map(|candidate| {
            state
                .conflicts()
                .into_iter()
                .find(|(cache_key, _)| *cache_key == candidate.cache_key)
                .map(|(cache_key, entry)| (PathBuf::from(cache_key), entry.clone()))
                .ok_or_else(|| {
                    anyhow!(
                        "conflict state vanished before mutation: {}",
                        candidate.cache_key
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    for candidate in &candidates {
        let mut resolved_vclock = candidate.resolved_vclock.clone();
        resolved_vclock.tick(device_id);
        if !state.resolve_conflict_by_cache_key(
            &candidate.cache_key,
            resolved_vclock,
            device_id.to_string(),
        ) {
            let error = anyhow!(
                "state entry vanished while clearing conflict: {}",
                candidate.cache_key
            );
            return Err(recover_after_state_mutation(
                anchor,
                &applied,
                state,
                &state_snapshot,
                &original_conflict_entries,
                error,
                "rolling back refs after conflict state disappeared",
                "flushing restored conflict state after resolver failure",
            ));
        }
    }
    if let Err(err) = anchor.revalidate() {
        return Err(recover_after_state_mutation(
            anchor,
            &applied,
            state,
            &state_snapshot,
            &original_conflict_entries,
            err.context("registered repo root changed before conflict-state flush"),
            "rolling back refs after authorized root identity changed",
            "restoring conflict state after authorized root identity changed",
        ));
    }
    if let Err(err) = state.flush() {
        return Err(recover_after_state_mutation(
            anchor,
            &applied,
            state,
            &state_snapshot,
            &original_conflict_entries,
            err.context("flushing resolved git conflicts"),
            "rolling back refs after conflict-state flush failed",
            "restoring conflict state after failed resolution flush",
        ));
    }
    anchor.run_after_conflict_state_flush_hook();
    if let Err(err) = anchor.revalidate() {
        return Err(recover_after_state_mutation(
            anchor,
            &applied,
            state,
            &state_snapshot,
            &original_conflict_entries,
            err.context("registered repo root changed after conflict-state flush"),
            "rolling back refs after authorized root identity changed",
            "restoring conflict state after authorized root identity changed",
        ));
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
            // and submodule refs — parking those safely is out of scope for PR-3
            // and dropping them would lose refs. Non-ref-class workdir/reflog
            // state (`.git/index`, `.git/logs/**`, `.git/COMMIT_EDITMSG`) is
            // design-intended kept-local (steps 7/9), which is what makes the
            // verb usable on real divergent repos where those paths almost
            // always differ.
            //
            // `.git/HEAD` is deliberately in the KEEP-LOCAL class, NOT ref-class
            // (TIN-2658 live finding): HEAD is per-checkout WORKING STATE — on
            // the resolving side it is (almost always) a symbolic ref like
            // `ref: refs/heads/main`, not a publishable ref value. Keep-both
            // semantics preserve the remote's line of work through the parked
            // `refs/tcfs/theirs/<device>/heads/**` BRANCH refs; the loser's old
            // head is a parked branch ref, never the HEAD file itself (matches
            // the G5-git-13 criteria). Treating HEAD as ref-class made every
            // real divergent repo unresolvable, because two checkouts' HEAD
            // files conflict whenever the repo diverges at all. Submodule
            // gitdir HEADs (`.git/modules/<name>/HEAD`) stay veto'd with the
            // other submodule refs — parking inside submodule gitdirs is
            // unhandled, and this fix must not silently widen parking.
            None => {
                if is_checkout_head_path(&rel_path) {
                    GitConflictKind::KeepLocal
                } else if crate::reconcile::is_git_ref_class_path(&rel_path) {
                    bail!(
                        "unparkable ref-class .git conflict {}; only branch-head refs \
                         (.git/refs/heads/**) are parkable in PR-3",
                        rel_path
                    );
                } else {
                    GitConflictKind::KeepLocal
                }
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

/// True for the PRIMARY checkout's `.git/HEAD` file (already `/`-normalized
/// rel path). Deliberately does NOT match a submodule gitdir's HEAD
/// (`.git/modules/<name>/HEAD`) — those stay in the unparkable ref-class veto.
fn is_checkout_head_path(rel_path: &str) -> bool {
    rel_path == ".git/HEAD" || rel_path.ends_with("/.git/HEAD")
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
    expected_rel_path: &str,
    device_id: &str,
    encryption: OptionalEncryption<'_>,
) -> Result<String> {
    let temp_dir = tempfile::tempdir().context("creating private remote-ref download directory")?;
    let temp_path = temp_dir.path().join("payload");
    let snapshot =
        engine::resolve_exact_indexed_manifest_snapshot(op, expected_rel_path, remote_prefix)
            .await
            .context("resolving exact remote-ref index")?
            .context("remote-ref index entry disappeared")?;
    anyhow::ensure!(
        snapshot.manifest_path() == remote_manifest_key,
        "remote-ref index moved since conflict classification"
    );
    anyhow::ensure!(
        snapshot.kind() == RemoteEntryKind::RegularFile,
        "remote-ref index no longer selects a regular file"
    );
    engine::hydrate_indexed_snapshot_with_device(
        op,
        &snapshot,
        &temp_path,
        None,
        device_id,
        None,
        encryption,
        &engine::ExpectedLocalFingerprint::Absent,
    )
    .await
    .context("downloading checked remote ref")?;
    let bytes = std::fs::read(&temp_path)
        .with_context(|| format!("reading downloaded ref {}", temp_path.display()))?;
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

fn park_ref_for_available_at(
    anchor: &GitRepoAnchor,
    remote_device: &str,
    head_ref: &str,
    sha: &str,
) -> Result<String> {
    let base = park_ref_for(remote_device, head_ref)?;
    available_park_ref_at(anchor, &base, sha)
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
#[cfg(test)]
fn park_ref_create_only(repo_root: &Path, park_ref: &str, sha: &str) -> Result<String> {
    let park_ref = available_park_ref(repo_root, park_ref, sha)?;
    if git_safety::local_ref_sha(repo_root, &park_ref).as_deref() == Some(sha) {
        return Ok(park_ref);
    }
    let zero = zero_oid_like(sha);
    run_git(repo_root, &["update-ref", &park_ref, sha, zero.as_str()])
        .with_context(|| format!("parking {sha} at {park_ref}"))?;
    Ok(park_ref)
}

fn ensure_clean_worktree(repo_root: &Path, anchor: Option<&GitRepoAnchor>) -> Result<()> {
    use std::io::Write;

    let source_git_dir = repo_root.join(".git");
    let replace_refs = repo_git_command(repo_root, anchor)?
        .args(["for-each-ref", "--format=%(refname)", "refs/replace/"])
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
    let head_sha = match anchor {
        Some(anchor) => anchor.local_ref_sha("HEAD"),
        None => git_safety::local_ref_sha(repo_root, "HEAD"),
    }
    .ok_or_else(|| anyhow!("Git HEAD is missing or invalid"))?;
    std::fs::write(shadow_git_dir.path().join("HEAD"), format!("{head_sha}\n"))
        .context("writing isolated Git HEAD")?;

    let object_format = git_output_at(repo_root, anchor, &["rev-parse", "--show-object-format"])?;
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
    let isolated_git = || -> Result<std::process::Command> {
        #[cfg(windows)]
        let null_device = "NUL";
        #[cfg(not(windows))]
        let null_device = "/dev/null";

        let mut command = repo_git_command(repo_root, anchor)?;
        command
            .env("GIT_DIR", shadow_git_dir.path())
            .env("GIT_COMMON_DIR", shadow_git_dir.path())
            .env(
                "GIT_WORK_TREE",
                if anchor.is_some() {
                    Path::new(".")
                } else {
                    repo_root
                },
            )
            .env("GIT_OBJECT_DIRECTORY", &source_objects)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", null_device)
            .env("GIT_CONFIG_GLOBAL", null_device)
            .env("GIT_OPTIONAL_LOCKS", "0");
        Ok(command)
    };

    let index_flags = isolated_git()?
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

    let staged = isolated_git()?
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
                require_same_principal_git_entry(&config_path, &metadata, "Git config file")?;
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
    let effective_attributes_file = repo_git_command(repo_root, anchor)?
        .args(["config", "--path", "--get-all", "core.attributesFile"])
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
    let tracked = isolated_git()?
        .args(["ls-files", "-z"])
        .output()
        .context("listing tracked paths for isolated Git attribute inspection")?;
    if !tracked.status.success() {
        bail!(
            "isolated Git tracked-path inspection failed: {}",
            String::from_utf8_lossy(&tracked.stderr)
        );
    }
    let mut attribute_command = isolated_git()?;
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
            require_same_principal_git_entry(
                &info_attributes,
                &metadata,
                "Git info attributes file",
            )?;
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

    let out = isolated_git()?
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

#[cfg(test)]
fn ensure_commit_present(repo_root: &Path, sha: &str) -> Result<()> {
    run_git(repo_root, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .with_context(|| format!("remote commit object is missing locally: {sha}"))
}

fn ensure_commit_present_at(anchor: &GitRepoAnchor, sha: &str) -> Result<()> {
    anchor
        .run_git(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .with_context(|| format!("remote commit object is missing locally: {sha}"))
}

#[cfg(test)]
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

fn ensure_local_ref_still_pinned_at(
    anchor: &GitRepoAnchor,
    parked: &GitKeepBothParkedRef,
) -> Result<()> {
    let Some(current) = anchor.local_ref_sha(&parked.head_ref) else {
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

#[cfg(test)]
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

fn available_park_ref_at(
    anchor: &GitRepoAnchor,
    park_ref: &str,
    remote_sha: &str,
) -> Result<String> {
    if let Some(existing) = anchor.local_ref_sha(park_ref) {
        if existing == remote_sha {
            return Ok(park_ref.to_string());
        }
        let min_suffix_len = remote_sha.len().min(12);
        for suffix_len in min_suffix_len..=remote_sha.len() {
            let suffixed = format!("{park_ref}-{}", &remote_sha[..suffix_len]);
            match anchor.local_ref_sha(&suffixed) {
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

fn ensure_selected_park_ref_available_at(
    anchor: &GitRepoAnchor,
    park_ref: &str,
    remote_sha: &str,
) -> Result<()> {
    if let Some(existing) = anchor.local_ref_sha(park_ref) {
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

fn sync_directory_chain(mut directory: &Path, stop_at: &Path) -> Result<()> {
    loop {
        std::fs::File::open(directory)
            .with_context(|| format!("opening durability directory: {}", directory.display()))?
            .sync_all()
            .with_context(|| format!("syncing durability directory: {}", directory.display()))?;
        if directory == stop_at {
            return Ok(());
        }
        directory = directory.parent().ok_or_else(|| {
            anyhow!(
                "durability path escaped stop directory: {}",
                stop_at.display()
            )
        })?;
    }
}

fn durably_confirm_parked_ref(
    anchor: &GitRepoAnchor,
    ref_name: &str,
    expected_sha: &str,
) -> Result<()> {
    if anchor.local_ref_sha(ref_name).as_deref() != Some(expected_sha) {
        bail!("parked ref readback mismatch after update-ref: {ref_name}");
    }
    let git_dir = anchor.canonical_root.join(".git");
    let ref_path = git_dir.join(ref_name);
    std::fs::File::open(&ref_path)
        .with_context(|| format!("opening parked ref for durability: {}", ref_path.display()))?
        .sync_all()
        .with_context(|| format!("syncing parked ref: {}", ref_path.display()))?;
    sync_directory_chain(
        ref_path
            .parent()
            .context("parked ref has no parent directory")?,
        &git_dir,
    )
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

fn write_undo_bundle_at(anchor: &GitRepoAnchor, state_dir: &Path) -> Result<PathBuf> {
    write_undo_bundle_inner(&anchor.canonical_root, Some(anchor), state_dir)
}

fn write_undo_bundle_inner(
    repo_root: &Path,
    anchor: Option<&GitRepoAnchor>,
    state_dir: &Path,
) -> Result<PathBuf> {
    let repo_hex = blake3::hash(repo_root.to_string_lossy().as_bytes()).to_hex();
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    crate::path_acl::reject_write_grant_acl(state_dir)
        .with_context(|| format!("validating undo state-dir ACL: {}", state_dir.display()))?;
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
    let mut bundle_command = match anchor {
        Some(anchor) => anchor.metadata_git_command()?,
        None => repo_git_command(repo_root, None)?,
    };
    let output = bundle_command
        .args(["bundle", "create", "-", "--all"])
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
    sync_directory_chain(&undo_dir, state_dir)
        .context("syncing undo-bundle directory chain before ref mutation")?;
    let verification = match anchor {
        Some(anchor) => anchor.run_git(&["bundle", "verify", &bundle_str]),
        None => run_git(repo_root, &["bundle", "verify", &bundle_str]),
    };
    if let Err(error) = verification {
        let _ = std::fs::remove_file(&bundle);
        return Err(error).context("verifying undo bundle");
    }
    Ok(bundle)
}

#[cfg(test)]
fn rollback_refs(repo_root: &Path, refs: &[AppliedParkRef]) {
    for r in refs.iter().rev() {
        match r.previous_sha.as_deref() {
            Some(previous) => {
                let _ = run_git(
                    repo_root,
                    &[
                        "update-ref",
                        r.ref_name.as_str(),
                        previous,
                        r.written_sha.as_str(),
                    ],
                );
            }
            None => {
                let _ = run_git(
                    repo_root,
                    &[
                        "update-ref",
                        "-d",
                        r.ref_name.as_str(),
                        r.written_sha.as_str(),
                    ],
                );
            }
        }
    }
}

fn rollback_refs_at(anchor: &GitRepoAnchor, refs: &[AppliedParkRef]) -> Result<()> {
    let mut failures = Vec::new();
    for applied_ref in refs.iter().rev() {
        let result = match applied_ref.previous_sha.as_deref() {
            Some(previous) => anchor.run_git(&[
                "update-ref",
                applied_ref.ref_name.as_str(),
                previous,
                applied_ref.written_sha.as_str(),
            ]),
            None => anchor.run_git(&[
                "update-ref",
                "-d",
                applied_ref.ref_name.as_str(),
                applied_ref.written_sha.as_str(),
            ]),
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error:#}", applied_ref.ref_name));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("Git ref rollback failed: {}", failures.join("; "))
    }
}

fn restore_and_flush_state(
    state: &mut StateCache,
    snapshot: &crate::state::StateCacheKeySnapshot,
    original_entries: &[(PathBuf, crate::state::SyncState)],
) -> Result<()> {
    state.restore_cache_key_snapshot(snapshot);
    // `restore_cache_key_snapshot` deliberately restores the old dirty bit.
    // Re-set each original candidate so a recovery after an already-successful
    // resolved-state flush is forced back to disk rather than becoming a
    // dirty=false no-op.
    for (cache_key, entry) in original_entries {
        state.set(cache_key, entry.clone());
    }
    state.flush().context("flushing restored conflict state")
}

#[allow(clippy::too_many_arguments)]
fn recover_after_state_mutation(
    anchor: &GitRepoAnchor,
    refs: &[AppliedParkRef],
    state: &mut StateCache,
    snapshot: &crate::state::StateCacheKeySnapshot,
    original_entries: &[(PathBuf, crate::state::SyncState)],
    operation_error: anyhow::Error,
    rollback_context: &str,
    restore_context: &str,
) -> anyhow::Error {
    let rollback_error = rollback_refs_at(anchor, refs)
        .with_context(|| rollback_context.to_string())
        .err();
    // State is independent of Git ref rollback. Always restore and durably
    // flush it even when a compare-and-swap rollback rejects a concurrent ref
    // update, otherwise a failed resolver can leave its conflict cleared.
    let restore_error = restore_and_flush_state(state, snapshot, original_entries)
        .with_context(|| restore_context.to_string())
        .err();

    let mut recovery_failures = Vec::new();
    if let Some(error) = rollback_error {
        recovery_failures.push(format!("{error:#}"));
    }
    if let Some(error) = restore_error {
        recovery_failures.push(format!("{error:#}"));
    }

    if recovery_failures.is_empty() {
        operation_error
    } else {
        anyhow!(
            "{operation_error:#}; recovery failures: {}",
            recovery_failures.join("; ")
        )
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

    fn memory_op() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        crate::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn configured_lexical_symlink_requires_acl_inspection() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let route = dir.path().join("route");
        symlink(&target, &route).unwrap();
        let file_type = std::fs::symlink_metadata(&route).unwrap().file_type();

        assert!(file_type.is_symlink());
        assert!(configured_component_requires_acl(&file_type));
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn authorized_descriptor_anchor_revalidates_the_captured_route() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        init_repo(&repo);
        commit(&repo, "tracked.txt", "base\n", "base");
        let canonical_root = repo.canonicalize().unwrap();
        let directory = std::fs::File::open(&canonical_root).unwrap();

        let anchor =
            GitRepoAnchor::capture_from_authorized_root(&canonical_root, directory).unwrap();
        assert_eq!(anchor.canonical_root(), canonical_root);
        validate_standalone_repo_topology(&canonical_root).unwrap();

        anchor.revalidate().unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn authorized_descriptor_anchor_rejects_a_different_canonical_route() {
        let temporary = tempfile::tempdir().unwrap();
        let authorized = temporary.path().join("authorized");
        let different = temporary.path().join("different");
        init_repo(&authorized);
        init_repo(&different);

        let authorized_descriptor = std::fs::File::open(&authorized).unwrap();
        let error = GitRepoAnchor::capture_from_authorized_root(
            &different.canonicalize().unwrap(),
            authorized_descriptor,
        )
        .expect_err("the held descriptor, not the supplied route, is authoritative");
        assert!(
            error
                .to_string()
                .contains("repo root was replaced after authorization"),
            "{error:#}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn standalone_topology_rejects_shared_writable_root_and_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let root_case = tempfile::tempdir().unwrap();
        let root_repo = root_case.path().join("repo");
        init_repo(&root_repo);
        std::fs::set_permissions(&root_repo, std::fs::Permissions::from_mode(0o775)).unwrap();
        let root_error = validate_standalone_repo_topology(&root_repo)
            .expect_err("group-writable root must fail closed");
        assert!(
            root_error.to_string().contains("group/world-writable"),
            "{root_error:#}"
        );

        let metadata_case = tempfile::tempdir().unwrap();
        let metadata_repo = metadata_case.path().join("repo");
        init_repo(&metadata_repo);
        std::fs::set_permissions(
            metadata_repo.join(".git/refs"),
            std::fs::Permissions::from_mode(0o775),
        )
        .unwrap();
        let metadata_error = validate_standalone_repo_topology(&metadata_repo)
            .expect_err("group-writable refs directory must fail closed");
        assert!(
            metadata_error.to_string().contains("group/world-writable"),
            "{metadata_error:#}"
        );

        let ancestor_case = tempfile::tempdir().unwrap();
        let shared_parent = ancestor_case.path().join("shared-parent");
        let ancestor_repo = shared_parent.join("repo");
        init_repo(&ancestor_repo);
        std::fs::set_permissions(&shared_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let ancestor_error = validate_standalone_repo_topology(&ancestor_repo)
            .expect_err("untrusted writable ancestor must fail closed");
        assert!(
            ancestor_error.to_string().contains("trusted path ancestor"),
            "{ancestor_error:#}"
        );

        let shared_repository_case = tempfile::tempdir().unwrap();
        let shared_repository_repo = shared_repository_case.path().join("repo");
        init_repo(&shared_repository_repo);
        git_safety::run_git(
            &shared_repository_repo,
            &["config", "core.sharedRepository", "group"],
        )
        .unwrap();
        let shared_repository_error = validate_standalone_repo_topology(&shared_repository_repo)
            .expect_err("Git shared-repository mode must fail closed");
        assert!(
            shared_repository_error
                .to_string()
                .contains("core.sharedRepository"),
            "{shared_repository_error:#}"
        );

        let ref_backend_case = tempfile::tempdir().unwrap();
        let ref_backend_repo = ref_backend_case.path().join("repo");
        init_repo(&ref_backend_repo);
        git_safety::run_git(
            &ref_backend_repo,
            &["config", "core.repositoryFormatVersion", "1"],
        )
        .unwrap();
        git_safety::run_git(
            &ref_backend_repo,
            &["config", "extensions.refStorage", "reftable"],
        )
        .unwrap();
        let ref_backend_error = validate_standalone_repo_topology(&ref_backend_repo)
            .expect_err("non-files reference backend must fail closed");
        assert!(
            ref_backend_error
                .to_string()
                .contains("extensions.refstorage"),
            "{ref_backend_error:#}"
        );
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
        let op = memory_op();
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
            None,
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

        let err = ensure_clean_worktree(&repo, None).unwrap_err();
        assert!(err.to_string().contains("dirty"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn dry_run_rejects_repo_replaced_after_authorization() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let moved_repo = dir.path().join("authorized-repo-moved-aside");
        init_repo(&repo);
        commit(&repo, "tracked.txt", "authorized repository\n", "base");

        let anchor = GitRepoAnchor::capture(&repo).expect("capture authorized repo identity");
        std::fs::rename(&repo, &moved_repo).expect("move authorized repo aside");
        init_repo(&repo);
        commit(
            &repo,
            "tracked.txt",
            "replacement repository\n",
            "replacement",
        );

        let mut state = StateCache::open(&dir.path().join("state.json")).unwrap();
        let error = resolve_repo_keep_both(
            &op,
            &mut state,
            &repo,
            Some(&anchor),
            "data",
            "neo",
            dir.path(),
            GitKeepBothMode::DryRun,
            None,
        )
        .await
        .expect_err("same-path replacement must invalidate the authorized identity");

        assert!(
            error.to_string().contains("replaced after authorization"),
            "{error:#}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn execute_race_fixture() -> (
        Operator,
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        PathBuf,
        StateCache,
        GitRepoAnchor,
        String,
        String,
    ) {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state-dir");
        std::fs::create_dir_all(&state_dir).unwrap();

        let base = dir.path().join("base");
        init_repo(&base);
        commit(&base, "file.txt", "base", "base");
        let winner = dir.path().join("winner");
        let loser = dir.path().join("loser");
        for destination in [&winner, &loser] {
            git_safety::run_git(
                dir.path(),
                &[
                    "clone",
                    "--quiet",
                    &base.to_string_lossy(),
                    &destination.to_string_lossy(),
                ],
            )
            .unwrap();
            init_repo(destination);
        }
        let winner_head = commit(&winner, "file.txt", "winner work", "winner");
        let loser_head = commit(&loser, "file.txt", "loser work", "loser");
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

        let ref_blob = dir.path().join("loser-ref-blob");
        std::fs::write(&ref_blob, format!("{loser_head}\n")).unwrap();
        let mut upload_state = StateCache::open(&dir.path().join("upload-state.json")).unwrap();
        let upload = engine::upload_file_with_device(
            &op,
            &ref_blob,
            "data",
            &mut upload_state,
            None,
            "loser",
            Some("winner/.git/refs/heads/main"),
            None,
        )
        .await
        .unwrap();

        let state_path = dir.path().join("state.json");
        let ref_key = winner.join(".git/refs/heads/main");
        let mut local = crate::conflict::VectorClock::new();
        local.tick("winner");
        let mut remote = crate::conflict::VectorClock::new();
        remote.tick("loser");
        let mut state = StateCache::open(&state_path).unwrap();
        state.set(
            &ref_key,
            conflict_state(
                "winner/.git/refs/heads/main",
                Some(upload.remote_path),
                &local,
                &remote,
            ),
        );
        state.flush().unwrap();
        let anchor = GitRepoAnchor::capture(&winner).unwrap();

        (
            op,
            dir,
            state_dir,
            state_path,
            winner,
            state,
            anchor,
            winner_head,
            loser_head,
        )
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn execute_root_swap_before_update_ref_is_anchored_and_rolled_back() {
        let (op, dir, state_dir, state_path, winner, mut state, mut anchor, winner_head, _) =
            execute_race_fixture().await;
        let authorized_repo = dir.path().join("authorized-repo-moved-aside");
        let replacement = winner.clone();
        let moved_for_hook = authorized_repo.clone();
        anchor.set_before_update_ref_hook(move || {
            std::fs::rename(&replacement, &moved_for_hook).unwrap();
            init_repo(&replacement);
            commit(
                &replacement,
                "file.txt",
                "replacement repository",
                "replacement",
            );
        });

        let error = resolve_repo_keep_both(
            &op,
            &mut state,
            &winner,
            Some(&anchor),
            "data",
            "winner",
            &state_dir,
            GitKeepBothMode::Execute,
            None,
        )
        .await
        .expect_err("same-path replacement must invalidate execute");
        assert!(format!("{error:#}").contains("replaced"), "{error:#}");

        let park_ref = "refs/tcfs/theirs/loser/heads/main";
        assert_eq!(
            git_safety::local_ref_sha(&authorized_repo, "refs/heads/main").as_deref(),
            Some(winner_head.as_str()),
            "authorized head must be preserved"
        );
        assert_eq!(
            git_safety::local_ref_sha(&authorized_repo, park_ref),
            None,
            "authorized repo's temporary park ref must be rolled back"
        );
        assert_eq!(
            git_safety::local_ref_sha(&winner, park_ref),
            None,
            "replacement repo must never be mutated"
        );
        assert_eq!(state.conflicts().len(), 1, "in-memory conflict retained");
        assert_eq!(
            StateCache::open(&state_path).unwrap().conflicts().len(),
            1,
            "on-disk conflict retained"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn rollback_failure_after_state_mutation_still_restores_conflict_durably() {
        let (
            op,
            dir,
            state_dir,
            state_path,
            winner,
            mut state,
            mut anchor,
            winner_head,
            loser_head,
        ) = execute_race_fixture().await;
        let moved_repo = dir.path().join("repo-moved-after-state-mutation");
        let winner_for_hook = winner.clone();
        let moved_for_hook = moved_repo.clone();
        let winner_head_for_hook = winner_head.clone();
        let loser_head_for_hook = loser_head.clone();
        anchor.set_after_conflict_state_flush_hook(move || {
            // Simulate a concurrent operator moving the newly parked ref. The
            // resolver's rollback CAS must reject deleting this newer value.
            git_safety::run_git(
                &winner_for_hook,
                &[
                    "update-ref",
                    "refs/tcfs/theirs/loser/heads/main",
                    winner_head_for_hook.as_str(),
                    loser_head_for_hook.as_str(),
                ],
            )
            .unwrap();
            // Force the real post-flush identity check to fail after the
            // cleared conflict has already been written to disk.
            std::fs::rename(&winner_for_hook, &moved_for_hook).unwrap();
        });

        let error = resolve_repo_keep_both(
            &op,
            &mut state,
            &winner,
            Some(&anchor),
            "data",
            "winner",
            &state_dir,
            GitKeepBothMode::Execute,
            None,
        )
        .await
        .expect_err("post-state identity failure must abort resolution");
        let error = format!("{error:#}");
        assert!(
            error.contains("registered repo root changed after conflict-state flush"),
            "primary failure must be reported: {error}"
        );
        assert!(
            error.contains("Git ref rollback failed"),
            "rollback CAS failure must be reported: {error}"
        );
        assert_eq!(
            git_safety::local_ref_sha(&moved_repo, "refs/tcfs/theirs/loser/heads/main").as_deref(),
            Some(winner_head.as_str()),
            "rollback must preserve the concurrent ref update"
        );
        assert_eq!(state.conflicts().len(), 1, "in-memory conflict restored");
        assert_eq!(
            StateCache::open(&state_path).unwrap().conflicts().len(),
            1,
            "on-disk conflict restored despite ref rollback failure"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn execute_git_dir_commondir_swap_cannot_redirect_update_ref() {
        let (op, dir, state_dir, state_path, winner, mut state, mut anchor, _, _) =
            execute_race_fixture().await;
        let external = dir.path().join("loser");
        let authorized_git = dir.path().join("authorized-git-moved-aside");
        let winner_for_hook = winner.clone();
        let moved_for_hook = authorized_git.clone();
        let external_git = external.join(".git");
        anchor.set_before_update_ref_hook(move || {
            std::fs::rename(winner_for_hook.join(".git"), &moved_for_hook).unwrap();
            std::fs::create_dir(winner_for_hook.join(".git")).unwrap();
            std::fs::write(winner_for_hook.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
            std::fs::write(
                winner_for_hook.join(".git/commondir"),
                format!("{}\n", external_git.display()),
            )
            .unwrap();
        });

        let error = resolve_repo_keep_both(
            &op,
            &mut state,
            &winner,
            Some(&anchor),
            "data",
            "winner",
            &state_dir,
            GitKeepBothMode::Execute,
            None,
        )
        .await
        .expect_err("same-path .git replacement must invalidate execute");
        assert!(
            format!("{error:#}").contains("Git metadata was replaced"),
            "{error:#}"
        );

        let park_ref = "refs/tcfs/theirs/loser/heads/main";
        let old_ref = authorized_git.join("refs/tcfs/theirs/loser/heads/main");
        assert!(
            !old_ref.exists(),
            "authorized metadata's temporary park ref must be rolled back"
        );
        assert_eq!(
            git_safety::local_ref_sha(&external, park_ref),
            None,
            "commondir target repo must never be mutated"
        );
        assert_eq!(state.conflicts().len(), 1, "in-memory conflict retained");
        assert_eq!(
            StateCache::open(&state_path).unwrap().conflicts().len(),
            1,
            "on-disk conflict retained"
        );
    }

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
        std::fs::write(repo.join(".gitattributes"), b"tracked.txt filter=probe\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), b"initial raw bytes\n").unwrap();
        git_safety::run_git(&repo, &["add", ".gitattributes", "tracked.txt"]).unwrap();
        git_safety::run_git(&repo, &["commit", "-m", "base", "--quiet"]).unwrap();
        assert!(marker.exists(), "git add must establish the filtered blob");
        std::fs::remove_file(&marker).unwrap();

        // Keep the raw file size equal to the index's cached worktree size so
        // Git cannot short-circuit on size alone, then force an unmistakably
        // stale mtime. Ordinary status must inspect the bytes and run the
        // repository-carried filter; the constant filter output matches the
        // blob stored by `git add`, producing a deterministic false-clean.
        let tracked = repo.join("tracked.txt");
        let cached_worktree_size = std::fs::metadata(&tracked).unwrap().len();
        std::fs::write(&tracked, b"different raw txt\n").unwrap();
        assert_eq!(
            std::fs::metadata(&tracked).unwrap().len(),
            cached_worktree_size
        );
        std::fs::File::options()
            .write(true)
            .open(&tracked)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();

        // Positive control: repository-aware status executes the filter while
        // inspecting the changed path. The production path must reject the
        // active attribute before any such repository-carried code can run.
        let unsafe_status = git_safety::sanitized_git_command()
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(unsafe_status.status.success());
        assert!(
            unsafe_status.stdout.is_empty(),
            "repository-carried clean filter must make the changed raw bytes appear falsely clean"
        );
        assert!(marker.exists(), "fixture must exercise the unsafe Git path");
        std::fs::remove_file(&marker).unwrap();

        let error = ensure_clean_worktree(&repo, None).expect_err("filtered tree must fail closed");
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

        let error = ensure_clean_worktree(&repo, None).expect_err("replace refs must fail closed");
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

        let error = ensure_clean_worktree(&repo, None)
            .expect_err("assume-unchanged entries must fail closed");
        assert!(error.to_string().contains("assume-unchanged"), "{error:#}");
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
                written_sha: second,
            }],
        );

        assert_eq!(git_safety::local_ref_sha(&repo, park_ref), Some(first));
    }

    #[test]
    fn rollback_does_not_delete_concurrently_changed_park_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let first = commit(&repo, "file.txt", "one", "one");
        let second = commit(&repo, "file.txt", "two", "two");
        let park_ref = "refs/tcfs/theirs/honey/heads/main";
        git_safety::run_git(&repo, &["update-ref", park_ref, &second]).unwrap();

        // Another actor changes the ref after TCFS wrote it but before TCFS
        // attempts rollback. The old-value CAS must preserve that update.
        git_safety::run_git(&repo, &["update-ref", park_ref, &first, &second]).unwrap();
        rollback_refs(
            &repo,
            &[AppliedParkRef {
                ref_name: park_ref.into(),
                previous_sha: None,
                written_sha: second,
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

    #[tokio::test]
    async fn remote_ref_read_binds_manifest_identity_kind_and_path() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let ref_blob = dir.path().join("remote-ref");
        std::fs::write(&ref_blob, format!("{sha}\n")).unwrap();
        let mut upload_state = StateCache::open(&dir.path().join("upload-state.json")).unwrap();
        let upload = engine::upload_file_with_device(
            &op,
            &ref_blob,
            "data",
            &mut upload_state,
            None,
            "remote",
            Some(rel_path),
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            read_remote_ref_sha(&op, &upload.remote_path, "data", rel_path, "local", None)
                .await
                .unwrap(),
            sha
        );
        assert!(
            read_remote_ref_sha(
                &op,
                &upload.remote_path,
                "data",
                "repo/.git/refs/heads/other",
                "local",
                None,
            )
            .await
            .is_err(),
            "a manifest for another ref path must fail closed"
        );

        let manifest_bytes = op.read(&upload.remote_path).await.unwrap().to_vec();
        let forged_key = "data/manifests/forged-manifest-object";
        op.write(forged_key, manifest_bytes).await.unwrap();
        assert!(
            read_remote_ref_sha(&op, forged_key, "data", rel_path, "local", None)
                .await
                .is_err(),
            "manifest bytes stored under the wrong object id must fail closed"
        );

        let symlink = crate::manifest::SymlinkManifest::new(
            "target",
            crate::conflict::VectorClock::new(),
            "remote".into(),
            1,
            Some(rel_path.into()),
        );
        let symlink_bytes = symlink.to_bytes().unwrap();
        let symlink_key = format!(
            "data/manifests/{}",
            crate::index_entry::manifest_object_id(&symlink_bytes)
        );
        op.write(&symlink_key, symlink_bytes).await.unwrap();
        assert!(
            read_remote_ref_sha(&op, &symlink_key, "data", rel_path, "local", None)
                .await
                .is_err(),
            "a symlink manifest must not enter the regular Git ref read lane"
        );
    }

    /// End-to-end Execute over two genuinely divergent `.git` repos (winner +
    /// loser, sharing a base, neither a fast-forward of the other). Exercises
    /// the real resolver — a no-op/stub resolver would leave the theirs-ref
    /// absent and fail invariant (i). Also drives the veto reconciliation
    /// (CHANGES-NEEDED 4) by including a divergent `.git/index` that must ride
    /// kept-local, and asserts idempotency (CHANGES-NEEDED 3).
    #[tokio::test]
    async fn execute_parks_theirs_keeps_local_and_is_idempotent() {
        let op = memory_op();
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
            Some("winner/.git/refs/heads/main"),
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
            None,
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
            None,
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

    /// TIN-2658 live-ceremony regression: a REAL divergent repo's conflict group
    /// always carries a `.git/HEAD` conflict (two checkouts' HEAD files differ
    /// whenever the repo diverges), and on the pre-fix classifier that single
    /// path veto'd the whole group with "unparkable ref-class .git conflict
    /// .git/HEAD" — making the resolve verb unusable exactly where it was
    /// needed. The G5 fixture never hit this because its HEADs agreed.
    ///
    /// Group under test: conflicted `.git/HEAD` + parkable
    /// `.git/refs/heads/main` + git-internals (`.git/index`,
    /// `.git/logs/HEAD`). Asserts: resolution succeeds; HEAD resolves
    /// keep-local (local symbolic-ref content byte-untouched); the branch ref
    /// parks at the loser's SHA; the undo bundle is written; every conflict in
    /// the group clears.
    #[tokio::test]
    async fn head_conflict_resolves_keep_local_alongside_parked_branch() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state-dir");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Shared base, two divergent clones (same shape as the execute test).
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
        init_repo(&winner);
        init_repo(&loser);
        let head_w = commit(&winner, "file.txt", "winner work", "winner");
        let head_l = commit(&loser, "file.txt", "loser work", "loser");
        assert_ne!(head_w, head_l);
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

        // Loser's branch-head ref content published at the winner's logical
        // path, matching the conflict entry that the resolver indexes below.
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
            Some("winner/.git/refs/heads/main"),
            None,
        )
        .await
        .unwrap();
        let manifest_key = up.remote_path.clone();

        // The live group shape: HEAD + branch head + workdir/reflog internals.
        let mut local = crate::conflict::VectorClock::new();
        local.tick("winner");
        let mut remote = crate::conflict::VectorClock::new();
        remote.tick("loser");
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();
        state.set(
            &winner.join(".git/refs/heads/main"),
            conflict_state(
                "winner/.git/refs/heads/main",
                Some(manifest_key),
                &local,
                &remote,
            ),
        );
        state.set(
            &winner.join(".git/HEAD"),
            conflict_state("winner/.git/HEAD", None, &local, &remote),
        );
        state.set(
            &winner.join(".git/logs/HEAD"),
            conflict_state("winner/.git/logs/HEAD", None, &local, &remote),
        );
        state.set(
            &winner.join(".git/index"),
            conflict_state("winner/.git/index", None, &local, &remote),
        );

        let head_file = winner.join(".git/HEAD");
        let head_before = std::fs::read(&head_file).unwrap();
        assert!(
            String::from_utf8_lossy(&head_before).starts_with("ref:"),
            "fixture sanity: local HEAD is a symbolic ref"
        );
        // TCFS Git commands deliberately set core.logAllRefUpdates=false, so
        // build the conflicted reflog fixture explicitly instead of inheriting
        // a user/global Git policy. Keep the line syntactically valid so fsck
        // can traverse it during the resolver proof.
        let logs_head = winner.join(".git/logs/HEAD");
        std::fs::create_dir_all(logs_head.parent().unwrap()).unwrap();
        std::fs::write(
            &logs_head,
            format!("{head_w} {head_w} TCFS Test <tcfs@example.invalid> 0 +0000\tfixture\n"),
        )
        .unwrap();
        let logs_head_before = std::fs::read(&logs_head).unwrap();

        let result = resolve_repo_keep_both(
            &op,
            &mut state,
            &winner,
            None,
            "data",
            "winner",
            &state_dir,
            GitKeepBothMode::Execute,
            None,
        )
        .await
        .expect(
            "a group containing .git/HEAD must resolve (pre-fix: \
             'unparkable ref-class .git conflict .git/HEAD')",
        );

        // Branch ref parked at the loser's SHA; winner's line untouched.
        assert_eq!(result.parked_refs.len(), 1, "only the branch head parks");
        assert_eq!(
            git_safety::local_ref_sha(&winner, "refs/tcfs/theirs/loser/heads/main").as_deref(),
            Some(head_l.as_str())
        );
        assert_eq!(
            git_safety::local_ref_sha(&winner, "refs/heads/main").as_deref(),
            Some(head_w.as_str())
        );
        // HEAD keep-local: the local symbolic-ref file is byte-untouched.
        assert_eq!(
            std::fs::read(&head_file).unwrap(),
            head_before,
            "keep-local HEAD must not be rewritten"
        );
        // .git/logs/HEAD keep-local too (was already non-ref-class; pin it).
        assert_eq!(
            std::fs::read(winner.join(".git/logs/HEAD")).unwrap(),
            logs_head_before,
            "keep-local logs/HEAD must not be rewritten"
        );
        // Undo bundle written (refs changed), under the state dir.
        let bundle = result.undo_bundle.expect("undo bundle written");
        assert!(bundle.starts_with(&state_dir));
        assert!(bundle.is_file(), "undo bundle exists on disk");
        // The whole group clears — HEAD included — so the reconcile loop stops.
        assert!(
            state.conflicts().is_empty(),
            "HEAD + branch + internals all cleared"
        );
        run_git(&winner, &["fsck", "--full"]).expect("fsck clean after execute");
    }

    /// Guard against silently WIDENING parking (the other half of the live
    /// fix): genuinely unhandled ref classes still veto. `.git/refs/tags/**`
    /// keeps the "unparkable ref-class" error, and a submodule gitdir's HEAD
    /// (`.git/modules/<name>/HEAD`) stays veto'd — only the primary checkout's
    /// `.git/HEAD` moved to keep-local.
    #[tokio::test]
    async fn non_head_ref_classes_still_veto() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        commit(&repo, "file.txt", "base", "base");
        let mut local = crate::conflict::VectorClock::new();
        local.tick("winner");
        let mut remote = crate::conflict::VectorClock::new();
        remote.tick("loser");

        for rel in ["repo/.git/refs/tags/v1", "repo/.git/modules/sub/HEAD"] {
            let mut state =
                StateCache::open(&dir.path().join(format!("state-{}.json", rel.len()))).unwrap();
            let abs = dir.path().join(rel);
            state.set(&abs, conflict_state(rel, None, &local, &remote));
            let err = resolve_repo_keep_both(
                &op,
                &mut state,
                &repo,
                None,
                "data",
                "winner",
                dir.path(),
                GitKeepBothMode::DryRun,
                None,
            )
            .await
            .unwrap_err();
            assert!(
                err.to_string().contains("unparkable ref-class"),
                "{rel} must still veto: {err:#}"
            );
        }
    }

    /// The classification carve-out is exactly the primary checkout's HEAD.
    #[test]
    fn checkout_head_path_matches_primary_head_only() {
        assert!(is_checkout_head_path(".git/HEAD"));
        assert!(is_checkout_head_path("repo/.git/HEAD"));
        assert!(is_checkout_head_path("nested/dir/repo/.git/HEAD"));
        // Submodule gitdir HEAD: NOT the carve-out (stays ref-class veto).
        assert!(!is_checkout_head_path("repo/.git/modules/sub/HEAD"));
        // Reflog and branch-head paths are classified elsewhere.
        assert!(!is_checkout_head_path("repo/.git/logs/HEAD"));
        assert!(!is_checkout_head_path("repo/.git/refs/heads/HEAD"));
        assert!(!is_checkout_head_path("repo/.git/HEADS"));
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

        let op = memory_op();
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

        let mut stale = state.get(&ref_path).cloned().expect("recorded conflict");
        stale.remote_path = "old-prefix/manifests/head".into();
        state.set(&ref_path, stale);
        let error = collect_repo_conflicts(&state, &repo_root, "data")
            .expect_err("stale-prefix Git conflicts must fail closed");
        assert!(error.to_string().contains("selected storage prefix"));
    }
}
