//! Git-native archival union. Native refs, HEAD, index and checkout are never written.
//!
//! Bundles preserve staged state separately from the worktree (including ignored
//! files). Submodules must be captured separately; this is not Git administration
//! reconstruction. Capture is optimistic, not an atomic filesystem snapshot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{BulkloadRefusal, Result};

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(key);
    }
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "pack.threads=2",
            "-c",
            "pack.windowMemory=64m",
            "-C",
        ])
        .arg(repo);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let Output { status, stdout, .. } = command.output()?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(stdout)
}

fn text(command: &mut Command) -> Result<String> {
    String::from_utf8(output(command)?)
        .map(|s| s.trim_end().to_owned())
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
}

fn input(command: &mut Command, bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or(BulkloadRefusal::Io(None))?
        .write_all(bytes)?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(result.stdout)
}

fn metadata(private: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let value = input(git(private).args(["hash-object", "-w", "--stdin"]), bytes)?;
    let value = std::str::from_utf8(&value)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    let tree = input(
        git(private).args(["mktree", "-z"]),
        format!("100644 blob {value}\tvalue\0").as_bytes(),
    )?;
    let tree = std::str::from_utf8(&tree)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    set_ref(
        private,
        &format!("refs/carry-export/{name}"),
        &commit_tree(private, tree, name)?,
    )
}

fn refs(repo: &Path) -> Result<String> {
    text(git(repo).args(["for-each-ref", "--format=%(objectname) %(refname)"]))
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn set_ref(repo: &Path, name: &str, value: &str) -> Result<()> {
    if !oid(value) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    output(git(repo).args(["update-ref", name, value, ""]))?;
    Ok(())
}

fn snapshot_command(private: &Path, worktree: &Path, index: &Path) -> Command {
    let mut command = git(worktree);
    command
        .env("GIT_DIR", private)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_INDEX_FILE", index);
    command.args(["-c", "core.bare=false"]);
    command
}

fn commit_tree(private: &Path, tree: &str, label: &str) -> Result<String> {
    text(
        git(private)
            .env("GIT_AUTHOR_NAME", "Bulkload archival capture")
            .env("GIT_AUTHOR_EMAIL", "bulkload@localhost")
            .env("GIT_COMMITTER_NAME", "Bulkload archival capture")
            .env("GIT_COMMITTER_EMAIL", "bulkload@localhost")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .args(["commit-tree", tree, "-m", label]),
    )
}

// Git's normal add applies attributes (including EOL normalization). Archive
// raw worktree bytes instead; the separate staged tree retains index semantics.
fn capture_tree(private: &Path, repo: &Path, index: &Path) -> Result<String> {
    use std::os::unix::ffi::OsStrExt;
    output(snapshot_command(private, repo, index).args(["add", "--all", "--force", "--", "."]))?;
    let entries =
        output(snapshot_command(private, repo, index).args(["ls-files", "--stage", "-z"]))?;
    for entry in entries.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header = std::str::from_utf8(
            entry
                .get(..tab)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        )
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mode = header
            .split_whitespace()
            .next()
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let path = std::ffi::OsStr::from_bytes(
            entry
                .get(tab + 1..)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        );
        if mode == "160000" {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if matches!(mode, "100644" | "100755") {
            let value = text(
                snapshot_command(private, repo, index)
                    .args(["hash-object", "-w", "--no-filters", "--"])
                    .arg(path),
            )?;
            output(
                snapshot_command(private, repo, index)
                    .args(["update-index", "--add", "--cacheinfo", mode, &value])
                    .arg(path),
            )?;
        }
    }
    text(snapshot_command(private, repo, index).arg("write-tree"))
}

fn capture_refs(repo: &Path, private: &Path, inventory: &str) -> Result<()> {
    for line in inventory.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        // Older carry spellings remain fully recoverable, once. They are
        // never discarded merely because provenance predates this schema.
        let exported = name
            .strip_prefix("refs/carry/v1/")
            .filter(|tail| canonical_tail(tail))
            .map_or_else(
                || format!("refs/carry-export/{name}"),
                |tail| format!("refs/carry-export/union/v1/{tail}"),
            );
        set_ref(private, &exported, value)?;
    }
    let stash = output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]));
    if let Ok(stash) = stash {
        let stash = String::from_utf8(stash).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        for value in stash.lines() {
            let name = format!("refs/carry-export/stashes/{value}");
            if !git(private)
                .args(["show-ref", "--verify", "--quiet", &name])
                .status()?
                .success()
            {
                set_ref(private, &name, value)?;
            }
        }
    } else if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(())
}

fn source_slug(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 64
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn canonical_tail(tail: &str) -> bool {
    let mut parts = tail.splitn(3, '/');
    let source = parts.next().unwrap_or_default();
    let snapshot = parts.next().unwrap_or_default();
    let suffix = parts.next().unwrap_or_default();
    source_slug(source) && snapshot.len() == 64 && oid(snapshot) && !suffix.is_empty()
}

/// Canonical common repository used to serialize applies sharing Git storage.
///
/// # Errors
/// Refuses unavailable or malformed Git administration.
pub fn common_repository(repo: &Path) -> Result<PathBuf> {
    let common = text(git(repo).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))?;
    Ok(fs::canonicalize(common)?)
}

/// Identity of a reusable capture, including new transit refs and full census.
///
/// This reads Git metadata and filesystem metadata, not ordinary file contents.
/// Callers must compare before/after keys and retain the successful bundle.
/// It is not a filesystem journal, atomic snapshot, or a no-rewalk claim.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn reusable_capture_key(repo: &Path) -> Result<[u8; 32]> {
    use std::os::unix::ffi::OsStrExt;
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    let inventory = refs(&repo)?;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let symbolic = git(&repo).args(["symbolic-ref", "-q", "HEAD"]).output()?;
    if !symbolic.status.success() && symbolic.status.code() != Some(1) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let (_, index) = source_index(&repo)?;
    let exclude_path = text(git(&repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?;
    let exclude = match fs::read(exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let stash = if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        output(git(&repo).args(["reflog", "show", "--format=%H", "refs/stash"]))?
    } else {
        Vec::new()
    };
    let rows =
        postcard::to_allocvec(&filesystem_rows(&repo)?).map_err(|_| BulkloadRefusal::FrameCodec)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"tcfs-git-reusable-capture-v1\0");
    for bytes in [
        repo.as_os_str().as_bytes(),
        common.as_os_str().as_bytes(),
        inventory.as_bytes(),
        head.as_bytes(),
        &symbolic.stdout,
        &index,
        &exclude,
        &stash,
        &rows,
    ] {
        hash.update(
            &u64::try_from(bytes.len())
                .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                .to_le_bytes(),
        );
        hash.update(bytes);
    }
    for directory in [&repo, &common] {
        let identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(directory)?);
        for value in [i128::from(identity.dev), i128::from(identity.ino)] {
            hash.update(&value.to_le_bytes());
        }
    }
    Ok(*hash.finalize().as_bytes())
}

/// Reconstruct a missing index from a same-HEAD capture without touching payload.
///
/// Receipt must be new, outside the worktree, and on the index filesystem for
/// atomic create-only publication. Existing indexes always refuse. This restores
/// captured staging, not a claim that the destination's lost staging was known.
///
/// # Errors
/// Refuses HEAD/admin changes, active Git operations, occupied index/receipt,
/// unsupported capture/index state, or cross-filesystem atomic publication.
pub fn repair_missing_index(
    bundle: &Path,
    repo: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    repair_missing_index_inner(bundle, repo, source, receipt, |_| Ok(()))
}

fn repair_missing_index_inner(
    bundle: &Path,
    repo: &Path,
    source: &str,
    receipt: &Path,
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let repo = fs::canonicalize(repo)?;
    let bundle = fs::canonicalize(bundle)?;
    let admin = PathBuf::from(text(git(&repo).args(["rev-parse", "--absolute-git-dir"]))?);
    let index = PathBuf::from(text(git(&repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ]))?);
    require_missing(&index)?;
    for name in [
        "index.lock",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        require_missing(&admin.join(name))?;
    }
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let heads = text(git(&repo).args(["bundle", "list-heads"]).arg(&bundle))?;
    let find = |suffix: &str| -> Result<String> {
        heads
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
            })
            .map(|(value, _)| value.to_owned())
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    };
    if find("head")? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let controls = admin_controls(&admin)?;
    let admin_identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(&admin)?);
    let receipt_parent =
        fs::canonicalize(receipt.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?;
    let receipt = receipt_parent.join(
        receipt
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    if receipt.starts_with(&repo) || receipt.starts_with(&admin) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    fs::DirBuilder::new().mode(0o700).create(&receipt)?;
    fs::write(
        receipt.join("original-administration.postcard"),
        postcard::to_allocvec(&(head.clone(), true, &controls))
            .map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    fs::File::open(receipt.join("original-administration.postcard"))?.sync_all()?;
    fs::File::open(&receipt)?.sync_all()?;
    fs::File::open(&receipt_parent)?.sync_all()?;
    import_bundle(&repo, &bundle, source)?;
    let staged_entries = output(git(&repo).args(["ls-tree", "-r", "-z", &find("staged")?]))?;
    if staged_entries
        .split(|b| *b == 0)
        .any(|entry| entry.starts_with(b"160000 "))
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let private_index = receipt.join("captured.index");
    output(
        git(&repo)
            .env("GIT_INDEX_FILE", &private_index)
            .args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]),
    )?;
    fs::set_permissions(&private_index, fs::Permissions::from_mode(0o600))?;
    fs::File::open(&private_index)?.sync_all()?;
    fs::File::open(&receipt)?.sync_all()?;
    let reservation = IndexReservation::acquire(admin.join("index.lock"))?;
    let current_identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(&admin)?);
    if (admin_identity.dev, admin_identity.ino) != (current_identity.dev, current_identity.ino)
        || controls != admin_controls(&admin)?
        || text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))? != head
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    before_publish(&index)?;
    if controls != admin_controls(&admin)?
        || text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))? != head
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    fs::hard_link(&private_index, &index)?;
    fs::File::open(&admin)?.sync_all()?;
    if text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    reservation.release()
}

/// Cooperates with native Git's index lock. The held descriptor identifies the
/// exact inode owned by this operation; a replaced/pre-existing lock is never
/// removed. Error paths release only this reservation, never another writer's.
struct IndexReservation {
    path: PathBuf,
    file: fs::File,
    released: bool,
}

impl IndexReservation {
    fn acquire(path: PathBuf) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            path,
            file,
            released: false,
        })
    }

    fn remove_owned(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let original = self.file.metadata()?;
        let current = fs::symlink_metadata(&self.path)?;
        if !current.is_file() || (original.dev(), original.ino()) != (current.dev(), current.ino())
        {
            return Err(BulkloadRefusal::GitAuthorityChanged);
        }
        fs::remove_file(&self.path)?;
        fs::File::open(self.path.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?)?.sync_all()?;
        Ok(())
    }

    fn release(mut self) -> Result<()> {
        self.remove_owned()?;
        self.released = true;
        Ok(())
    }
}

impl Drop for IndexReservation {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.remove_owned();
        }
    }
}

fn require_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(BulkloadRefusal::GitDestinationOccupied),
    }
}

fn admin_controls(admin: &Path) -> Result<Vec<(String, Option<Vec<u8>>)>> {
    ["HEAD", "commondir", "gitdir", "config", "config.worktree"]
        .into_iter()
        .map(|name| {
            let bytes = match fs::read(admin.join(name)) {
                Ok(value) => Some(value),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            Ok((name.to_owned(), bytes))
        })
        .collect()
}

/// Export one worktree and all repository refs into a new, private directory.
///
/// # Errors
/// Refuses unsupported/unmerged indexes, changing refs/index/worktree, and
/// populated submodules (which require their own capture). On refusal the
/// private capture is retained for diagnosis; source state is never changed.
pub fn export_repository(repo: &Path, capture: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let repo = fs::canonicalize(repo)?;
    fs::DirBuilder::new().mode(0o700).create(capture)?;
    let capture = fs::canonicalize(capture)?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    let before_refs = refs(&repo)?;
    let seats = filesystem_rows(&repo)?;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let (index_path, before_index) = source_index(&repo)?;
    let private = prepare_private(&repo, &capture)?;
    capture_refs(&repo, &private, &before_refs)?;
    set_ref(&private, "refs/carry-export/head", &head)?;
    let symbolic_head = text(git(&repo).args(["symbolic-ref", "-q", "HEAD"])).unwrap_or_default();
    metadata(&private, "head-symbolic", symbolic_head.as_bytes())?;
    let exclude_path = PathBuf::from(text(git(&repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?);
    let exclude = match fs::read(exclude_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    metadata(&private, "exclude", &exclude)?;
    let index = capture.join("index");
    fs::write(&index, &before_index)?;
    let staged = text(snapshot_command(&private, &repo, &index).arg("write-tree"))?;
    set_ref(
        &private,
        "refs/carry-export/staged",
        &commit_tree(&private, &staged, "bulkload staged tree")?,
    )?;
    let tree = capture_tree(&private, &repo, &index)?;
    if tree != capture_tree(&private, &repo, &index)?
        || before_refs != refs(&repo)?
        || before_index != fs::read(index_path)?
        || head != text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?
        || seats != filesystem_rows(&repo)?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    set_ref(
        &private,
        "refs/carry-export/worktree",
        &commit_tree(
            &private,
            &tree,
            "bulkload worktree including untracked and ignored files",
        )?,
    )?;
    metadata(
        &private,
        "filesystem-v1",
        &postcard::to_allocvec(&seats).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    let bundle = capture.join("capture.bundle");
    output(
        git(&private)
            .args(["bundle", "create"])
            .arg(&bundle)
            .arg("--all"),
    )?;
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    Ok(bundle)
}

// Reuse the transport's typed filesystem seats instead of treating Git's
// executable-bit-only tree modes as complete filesystem metadata. .git is
// administration owned by Git-native capture; nested repositories refuse.
fn filesystem_rows(root: &Path) -> Result<Vec<crate::RowSchema>> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use tcfs_bulkload_proto::FileKind;
    let mut pending = vec![root.to_path_buf()];
    let mut rows = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                if directory == root {
                    continue;
                }
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            let identity = crate::freshness::StatIdentity::from_metadata(&meta);
            let kind = if meta.is_dir() {
                FileKind::Directory
            } else if meta.is_file() {
                FileKind::Regular
            } else if meta.is_symlink() {
                FileKind::Symlink
            } else {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            };
            if kind == FileKind::Directory {
                pending.push(path.clone());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| BulkloadRefusal::PathEscapesRoot)?;
            rows.push(crate::RowSchema {
                rel_path: relative.as_os_str().as_bytes().to_vec(),
                kind,
                dev: identity.dev,
                ino: identity.ino,
                size: identity.size,
                mtime_ns: identity.mtime_ns,
                ctime_ns: identity.ctime_ns,
                mode: meta.mode(),
                nlink: meta.nlink(),
                link_target: if kind == FileKind::Symlink {
                    Some(fs::read_link(&path)?.as_os_str().as_bytes().to_vec())
                } else {
                    None
                },
                blake3: None,
            });
        }
    }
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(rows)
}

fn restore_filesystem_rows(destination: &Path, revision: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use tcfs_bulkload_proto::FileKind;
    let bytes = output(git(destination).args(["show", &format!("{revision}:value")]))?;
    let rows: Vec<crate::RowSchema> =
        postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)?;
    for row in &rows {
        let path = safe_destination(
            destination,
            Path::new(std::ffi::OsStr::from_bytes(&row.rel_path)),
        )?;
        match row.kind {
            FileKind::Directory => match fs::create_dir(&path) {
                Ok(()) => (),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && fs::symlink_metadata(&path)?.is_dir() => {}
                Err(error) => return Err(error.into()),
            },
            FileKind::Regular if fs::symlink_metadata(&path)?.is_file() => {
                fs::set_permissions(&path, fs::Permissions::from_mode(row.mode & 0o777))?;
            }
            FileKind::Symlink if fs::symlink_metadata(&path)?.is_symlink() => {
                if row.link_target.as_deref() != Some(fs::read_link(&path)?.as_os_str().as_bytes())
                {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
            }
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        }
    }
    // Parents become readonly only after all descendants have materialized.
    for row in rows
        .iter()
        .rev()
        .filter(|row| row.kind == FileKind::Directory)
    {
        let path = safe_destination(
            destination,
            Path::new(std::ffi::OsStr::from_bytes(&row.rel_path)),
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(row.mode & 0o777))?;
    }
    Ok(())
}

fn safe_destination(root: &Path, relative: &Path) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    if relative.as_os_str().is_empty() || relative.components().any(|part| !matches!(part, Component::Normal(name) if !name.as_bytes().eq_ignore_ascii_case(b".git"))) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(part) = components.next() {
        path.push(part);
        if components.peek().is_some() && !fs::symlink_metadata(&path)?.is_dir() {
            return Err(BulkloadRefusal::PathEscapesRoot);
        }
    }
    Ok(path)
}

fn source_index(repo: &Path) -> Result<(PathBuf, Vec<u8>)> {
    let index_path = PathBuf::from(text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "index",
    ]))?);
    let before_index = fs::read(&index_path)?;
    let flags = text(git(repo).args(["ls-files", "--debug"]))?;
    if flags
        .lines()
        .filter_map(|line| line.split_once("\tflags: "))
        .any(|(_, flags)| flags != "0")
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    if !text(git(repo).args(["rev-parse", "--shared-index-path"]))?.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let entries = output(git(repo).args(["ls-files", "--stage", "-z"]))?;
    if entries
        .split(|b| *b == 0)
        .any(|entry| entry.starts_with(b"160000 "))
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok((index_path, before_index))
}

fn prepare_private(repo: &Path, capture: &Path) -> Result<PathBuf> {
    let format = text(git(repo).args(["rev-parse", "--show-object-format"]))?;
    if !matches!(format.as_str(), "sha1" | "sha256") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let private = capture.join("repository.git");
    output(
        git(capture)
            .args([
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .arg(&private),
    )?;
    let objects = text(git(repo).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ]))?;
    if objects.contains(['\n', '\r']) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    fs::write(
        private.join("objects/info/alternates"),
        format!("{objects}\n"),
    )?;
    Ok(private)
}

/// Import into a content-addressed source namespace, never native branches.
///
/// Snapshot identity includes native refs and capture metadata, not carried
/// transit refs. Already canonical refs retain their original source and
/// identity across bidirectional rounds, bounding growth by distinct captures.
///
/// # Errors
/// Refuses invalid source names, non-export bundle refs, collisions or invalid
/// bundles. A failed fetch may leave unreachable objects, never changed HEAD.
pub fn import_bundle(repo: &Path, bundle: &Path, source: &str) -> Result<usize> {
    if !source_slug(source) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let bundle = fs::canonicalize(bundle)?;
    output(git(repo).args(["bundle", "verify"]).arg(&bundle))?;
    let heads = text(git(repo).args(["bundle", "list-heads"]).arg(&bundle))?;
    let mut native: Vec<_> = heads
        .lines()
        .filter(|line| {
            !line
                .split_once(' ')
                .is_some_and(|(_, name)| name.starts_with("refs/carry-export/union/v1/"))
        })
        .collect();
    native.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tcfs-git-native-snapshot-v1\0");
    for line in native {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize().to_hex();
    let mut names = Vec::new();
    for line in heads.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let suffix = name
            .strip_prefix("refs/carry-export/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let target = if let Some(tail) = suffix.strip_prefix("union/v1/") {
            if !canonical_tail(tail) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            format!("refs/carry/v1/{tail}")
        } else {
            format!("refs/carry/v1/{source}/{digest}/{suffix}")
        };
        names.push((value.to_owned(), name.to_owned(), target));
    }
    // Fetch objects only. Compare-and-create below cannot clobber a native ref.
    output(
        git(repo)
            .args([
                "fetch",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
                "--no-tags",
                "--no-recurse-submodules",
            ])
            .arg(&bundle)
            .args(names.iter().map(|(_, name, _)| name)),
    )?;
    for (value, _, target) in &names {
        if let Ok(existing) = text(git(repo).args(["rev-parse", "--verify", target])) {
            if existing != *value {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
        } else {
            set_ref(repo, target, value)?;
        }
    }
    Ok(names.len())
}

/// Restore staged and unstaged state into a newly created, standalone repository.
///
/// Existing destinations are always refused, including empty directories.
/// The source bundle remains the recovery carrier if restoration is interrupted.
///
/// # Errors
/// Refuses malformed paths/modes, missing capture metadata, or an occupied target.
/// Partial new destinations are retained, never cleaned by recursive deletion.
pub fn restore_bundle(bundle: &Path, destination: &Path, source: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let bundle = fs::canonicalize(bundle)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let destination = fs::canonicalize(destination)?;
    // Read bundle headers without assuming the destination's object format.
    let heads = text(
        git(&destination)
            .args(["bundle", "list-heads"])
            .arg(&bundle),
    )?;
    let find = |suffix: &str| -> Result<String> {
        heads
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
            })
            .map(|(value, _)| value.to_owned())
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    };
    let head = find("head")?;
    let format = if head.len() == 40 { "sha1" } else { "sha256" };
    output(git(&destination).args(["init", "--template=", &format!("--object-format={format}")]))?;
    import_bundle(&destination, &bundle, source)?;
    let symbolic =
        text(git(&destination).args(["show", &format!("{}:value", find("head-symbolic")?)]))?;
    if symbolic.is_empty() {
        output(git(&destination).args(["update-ref", "--no-deref", "HEAD", &head]))?;
    } else {
        if !symbolic.starts_with("refs/heads/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        output(git(&destination).args(["check-ref-format", &symbolic]))?;
        set_ref(&destination, &symbolic, &head)?;
        output(git(&destination).args(["symbolic-ref", "HEAD", &symbolic]))?;
    }
    let exclude = output(git(&destination).args(["show", &format!("{}:value", find("exclude")?)]))?;
    fs::create_dir_all(destination.join(".git/info"))?;
    fs::write(destination.join(".git/info/exclude"), exclude)?;
    let worktree = find("worktree")?;
    let entries = output(git(&destination).args(["ls-tree", "-r", "-z", &worktree]))?;
    for entry in entries.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        restore_entry(&destination, entry)?;
    }
    let staged = find("staged")?;
    output(git(&destination).args(["read-tree", &format!("{staged}^{{tree}}")]))?;
    restore_filesystem_rows(&destination, &find("filesystem-v1")?)?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Restore into a new linked worktree without changing any existing checkout.
///
/// The source branch is reused only when its tip matches and Git permits a new
/// attachment; otherwise a private carry branch is created. Existing common
/// ignore policy must match the capture: it is never overwritten.
///
/// # Errors
/// Refuses occupied destinations, differing common excludes, and invalid capture.
/// Partially created worktrees are retained on failure for explicit recovery.
pub fn restore_linked(
    bundle: &Path,
    repository: &Path,
    destination: &Path,
    source: &str,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    if destination.symlink_metadata().is_ok() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let parent = fs::canonicalize(
        destination
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    )?;
    let destination = parent.join(
        destination
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    let bundle = fs::canonicalize(bundle)?;
    let repository = fs::canonicalize(repository)?;
    import_bundle(&repository, &bundle, source)?;
    let heads = text(git(&repository).args(["bundle", "list-heads"]).arg(&bundle))?;
    let find = |suffix: &str| -> Result<String> {
        heads
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
            })
            .map(|(value, _)| value.to_owned())
            .filter(|value| oid(value))
            .ok_or(BulkloadRefusal::GitInventoryMalformed)
    };
    let head = find("head")?;
    let symbolic =
        text(git(&repository).args(["show", &format!("{}:value", find("head-symbolic")?)]))?;
    let exclude = output(git(&repository).args(["show", &format!("{}:value", find("exclude")?)]))?;
    let exclude_path = text(git(&repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?;
    let existing_exclude = match fs::read(&exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if exclude != existing_exclude {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let attached = text(git(&repository).args(["worktree", "list", "--porcelain"]))?;
    let source_tip = text(git(&repository).args(["rev-parse", "--verify", &symbolic]));
    let reuse = symbolic.starts_with("refs/heads/")
        && source_tip.as_ref().is_ok_and(|value| value == &head)
        && !attached
            .lines()
            .any(|line| line == format!("branch {symbolic}"));
    let branch = if reuse {
        symbolic
            .strip_prefix("refs/heads/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?
            .to_owned()
    } else {
        let digest = blake3::hash(destination.as_os_str().as_bytes()).to_hex();
        format!("carry/{source}/{digest}")
    };
    let mut command = git(&repository);
    command.args(["worktree", "add", "--no-checkout"]);
    if !reuse {
        command.args(["-b", &branch]);
    }
    command
        .arg("--")
        .arg(&destination)
        .arg(if reuse { &branch } else { &head });
    output(&mut command)?;
    // --no-checkout has created administration only, not captured payload.
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
    if text(git(&destination).args(["rev-parse", "--verify", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let entries = output(git(&destination).args(["ls-tree", "-r", "-z", &find("worktree")?]))?;
    for entry in entries.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        restore_entry(&destination, entry)?;
    }
    output(git(&destination).args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]))?;
    restore_filesystem_rows(&destination, &find("filesystem-v1")?)?;
    let final_exclude = match fs::read(exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if final_exclude != exclude
        || text(git(&destination).args(["rev-parse", "--verify", "HEAD"]))? != head
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(())
}

fn restore_entry(destination: &Path, entry: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Component;
    use std::process::Stdio;
    let tab = entry
        .iter()
        .position(|b| *b == b'\t')
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let header = std::str::from_utf8(
        entry
            .get(..tab)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
    )
    .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut fields = header.split_whitespace();
    let mode = fields
        .next()
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    if fields.next() != Some("blob") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let value = fields
        .next()
        .filter(|value| oid(value))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    let relative = Path::new(std::ffi::OsStr::from_bytes(
        entry
            .get(tab + 1..)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
    ));
    if relative.components().any(|part| !matches!(part, Component::Normal(name) if !name.as_bytes().eq_ignore_ascii_case(b".git"))) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    let path = destination.join(relative);
    let parent = path.parent().ok_or(BulkloadRefusal::PathEscapesRoot)?;
    let mut current = destination.to_path_buf();
    for part in parent
        .strip_prefix(destination)
        .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
        .components()
    {
        current.push(part);
        match fs::create_dir(&current) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&current)?.is_dir() {
                    return Err(BulkloadRefusal::PathEscapesRoot);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    match mode {
        "120000" => {
            let target = output(git(destination).args(["cat-file", "blob", value]))?;
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), path)?;
        }
        "100644" | "100755" => {
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            let status = git(destination)
                .args(["cat-file", "blob", value])
                .stdout(Stdio::from(file.try_clone()?))
                .status()?;
            if !status.success() {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            file.set_permissions(fs::Permissions::from_mode(if mode == "100755" {
                0o755
            } else {
                0o644
            }))?;
            file.sync_all()?;
        }
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn bidirectional_union_reaches_fixed_point_without_provenance_wrapping() {
        let root = std::env::temp_dir().join(format!("bulkload-git-union-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let a = root.join("a");
        let b = root.join("b");
        for (repo, content) in [(&a, b"source-a"), (&b, b"source-b")] {
            fs::create_dir(repo).unwrap();
            output(git(repo).args(["init", "--template="])).unwrap();
            output(git(repo).args(["config", "user.name", "Test"])).unwrap();
            output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
            fs::write(repo.join("tracked"), content).unwrap();
            output(git(repo).args(["add", "."])).unwrap();
            output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        }
        let original_a = text(git(&a).args(["rev-parse", "HEAD"])).unwrap();
        let original_b = text(git(&b).args(["rev-parse", "HEAD"])).unwrap();
        assert_eq!(
            common_repository(&a).unwrap(),
            fs::canonicalize(a.join(".git")).unwrap()
        );
        let reusable = reusable_capture_key(&a).unwrap();
        assert_eq!(reusable, reusable_capture_key(&a).unwrap());
        fs::write(a.join("tracked"), b"dirty-a!").unwrap();
        assert_ne!(reusable, reusable_capture_key(&a).unwrap());
        fs::write(a.join("tracked"), b"source-a").unwrap();
        let mut stashes = Vec::new();
        for repo in [&a, &b] {
            fs::write(repo.join("tracked"), b"unique stash state").unwrap();
            output(git(repo).args(["stash", "push"])).unwrap();
            stashes.push(text(git(repo).args(["rev-parse", "refs/stash"])).unwrap());
        }
        // A legacy nested provenance name is kept, not silently discarded.
        let legacy = "refs/carry/sting/old/registry/carry-export/refs/carry/neo/old/heads/topic";
        let before_ref = reusable_capture_key(&a).unwrap();
        set_ref(&a, legacy, &original_a).unwrap();
        assert_ne!(before_ref, reusable_capture_key(&a).unwrap());
        let mut fixed = None;
        for round in 0..5 {
            let ab = export_repository(&a, &root.join(format!("a-{round}"))).unwrap();
            import_bundle(&b, &ab, "neo").unwrap();
            let ba = export_repository(&b, &root.join(format!("b-{round}"))).unwrap();
            import_bundle(&a, &ba, "sting").unwrap();
            let current = (refs(&a).unwrap(), refs(&b).unwrap());
            if round == 1 {
                fixed = Some(current.clone());
            }
            if round > 1 {
                assert_eq!(fixed.as_ref(), Some(&current));
            }
        }
        for repo in [&a, &b] {
            let inventory = refs(repo).unwrap();
            assert!(inventory
                .lines()
                .any(|line| line.starts_with(&original_a) && line.contains(legacy)));
            assert!(inventory.lines().any(|line| line.starts_with(&original_b)));
            for stash in &stashes {
                assert!(inventory.lines().any(|line| line.starts_with(stash)));
            }
            assert!(!inventory.contains("/refs/carry/v1/"));
        }
        assert_eq!(
            text(git(&a).args(["rev-parse", "HEAD"])).unwrap(),
            original_a
        );
        assert_eq!(
            text(git(&b).args(["rev-parse", "HEAD"])).unwrap(),
            original_b
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One end-to-end source/union/restore invariant.
    fn union_preserves_native_head_index_binary_and_stash_history() {
        let root = std::env::temp_dir().join(format!("bulkload-git-carry-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let dest = root.join("dest");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&dest).unwrap();
        for repo in [&source, &dest] {
            output(git(repo).args(["init", "--template="])).unwrap();
            output(git(repo).args(["config", "user.name", "Test"])).unwrap();
            output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
            fs::write(repo.join("tracked"), b"base").unwrap();
            fs::write(repo.join("deleted"), b"staged deletion").unwrap();
            output(git(repo).args(["add", "."])).unwrap();
            output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        }
        for content in [b"stash-one", b"stash-two"] {
            fs::write(source.join("tracked"), content).unwrap();
            output(git(&source).args(["stash", "push"])).unwrap();
        }
        fs::write(source.join("tracked"), b"staged").unwrap();
        fs::remove_file(source.join("deleted")).unwrap();
        fs::write(source.join("binary"), [0, 1, 128]).unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"unstaged").unwrap();
        fs::write(source.join("binary"), [0, 255, 128]).unwrap();
        fs::write(source.join(".gitattributes"), b"*.txt text eol=lf\n").unwrap();
        fs::write(source.join("raw.txt"), b"raw\r\nbytes\r\n").unwrap();
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"unique ignored bytes").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(source.join("ignored"), fs::Permissions::from_mode(0o600)).unwrap();
            fs::create_dir_all(source.join("empty/nested")).unwrap();
            fs::set_permissions(source.join("empty"), fs::Permissions::from_mode(0o750)).unwrap();
            fs::write(source.join("executable"), b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(source.join("executable"), fs::Permissions::from_mode(0o700))
                .unwrap();
            std::os::unix::fs::symlink("ignored", source.join("symlink")).unwrap();
        }
        fs::write(dest.join("tracked"), b"destination divergence").unwrap();
        output(git(&dest).args(["add", "."])).unwrap();
        output(git(&dest).args(["-c", "commit.gpgsign=false", "commit", "-m", "destination"]))
            .unwrap();
        let index = fs::read(source.join(".git/index")).unwrap();
        let dest_index = fs::read(dest.join(".git/index")).unwrap();
        let dest_head = fs::read(dest.join(".git/HEAD")).unwrap();
        let native = refs(&dest).unwrap();
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let count = import_bundle(&dest, &bundle, "neo").unwrap();
        assert!(count >= 6);
        assert_eq!(count, import_bundle(&dest, &bundle, "neo").unwrap());
        assert_eq!(index, fs::read(source.join(".git/index")).unwrap());
        assert_eq!(dest_index, fs::read(dest.join(".git/index")).unwrap());
        assert_eq!(dest_head, fs::read(dest.join(".git/HEAD")).unwrap());
        assert_eq!(
            fs::read(dest.join("tracked")).unwrap(),
            b"destination divergence"
        );
        for line in native.lines() {
            assert!(refs(&dest).unwrap().lines().any(|now| now == line));
        }
        let carried =
            text(git(&dest).args(["for-each-ref", "--format=%(refname)", "refs/carry/"])).unwrap();
        let worktree = carried
            .lines()
            .find(|line| line.ends_with("/worktree"))
            .unwrap();
        assert_eq!(
            output(git(&dest).args(["show", &format!("{worktree}:binary")])).unwrap(),
            [0, 255, 128]
        );
        assert_eq!(
            output(git(&dest).args(["show", &format!("{worktree}:raw.txt")])).unwrap(),
            b"raw\r\nbytes\r\n"
        );
        assert_eq!(
            output(git(&dest).args(["show", &format!("{worktree}:ignored")])).unwrap(),
            b"unique ignored bytes"
        );
        assert_eq!(
            import_bundle(&dest, &bundle, "../native"),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        let staged = carried
            .lines()
            .find(|line| line.ends_with("/staged"))
            .unwrap();
        assert_eq!(
            output(git(&dest).args(["show", &format!("{staged}:tracked")])).unwrap(),
            b"staged"
        );
        assert_eq!(
            carried
                .lines()
                .filter(|line| line.contains("/stashes/"))
                .count(),
            2
        );
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        let linked = root.join("linked");
        restore_linked(&bundle, &dest, &linked, "neo").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            for target in [&restored, &linked] {
                assert_eq!(
                    fs::metadata(target.join("ignored"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::metadata(target.join("empty"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o750
                );
                assert_eq!(
                    fs::metadata(target.join("executable"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert!(target.join("empty/nested").is_dir());
                assert_eq!(
                    fs::read_link(target.join("symlink")).unwrap(),
                    PathBuf::from("ignored")
                );
            }
            assert_eq!(
                fs::metadata(&linked).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for args in [
            vec![
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
            vec!["diff", "--binary"],
            vec!["diff", "--cached", "--binary"],
            vec!["symbolic-ref", "HEAD"],
        ] {
            assert_eq!(
                output(git(&source).args(&args)).unwrap(),
                output(git(&restored).args(&args)).unwrap(),
                "{args:?}"
            );
            if args.first() != Some(&"symbolic-ref") {
                assert_eq!(
                    output(git(&source).args(&args)).unwrap(),
                    output(git(&linked).args(&args)).unwrap(),
                    "linked {args:?}"
                );
            }
        }
        assert!(linked.join(".git").is_file());
        let common =
            text(git(&linked).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))
                .unwrap();
        assert_eq!(
            fs::canonicalize(common).unwrap(),
            fs::canonicalize(dest.join(".git")).unwrap()
        );
        let admin = text(git(&linked).args(["rev-parse", "--absolute-git-dir"])).unwrap();
        assert_eq!(
            fs::read_to_string(Path::new(&admin).join("gitdir"))
                .unwrap()
                .trim(),
            linked.join(".git").to_str().unwrap()
        );
        assert_eq!(dest_index, fs::read(dest.join(".git/index")).unwrap());
        assert_eq!(dest_head, fs::read(dest.join(".git/HEAD")).unwrap());
        assert!(restore_linked(&bundle, &dest, &linked, "neo").is_err());
        assert_eq!(fs::read(restored.join("binary")).unwrap(), [0, 255, 128]);
        assert!(restore_bundle(&bundle, &restored, "neo").is_err());
        fs::remove_file(restored.join(".git/index")).unwrap();
        fs::write(restored.join("tracked"), b"destination-only pending change").unwrap();
        repair_missing_index(&bundle, &restored, "neo", &root.join("repair")).unwrap();
        assert_eq!(
            fs::read(restored.join("tracked")).unwrap(),
            b"destination-only pending change"
        );
        assert_eq!(
            output(git(&source).args(["diff", "--cached", "--binary"])).unwrap(),
            output(git(&restored).args(["diff", "--cached", "--binary"])).unwrap()
        );
        assert!(root
            .join("repair/original-administration.postcard")
            .is_file());
        assert!(
            repair_missing_index(&bundle, &restored, "neo", &root.join("repair-again")).is_err()
        );
        fs::remove_file(restored.join(".git/index")).unwrap();
        fs::write(restored.join(".git/index.lock"), b"another Git writer").unwrap();
        assert!(
            repair_missing_index(&bundle, &restored, "neo", &root.join("repair-locked")).is_err()
        );
        assert_eq!(
            fs::read(restored.join(".git/index.lock")).unwrap(),
            b"another Git writer"
        );
        fs::remove_file(restored.join(".git/index.lock")).unwrap();
        assert!(repair_missing_index_inner(
            &bundle,
            &restored,
            "neo",
            &root.join("repair-race"),
            |index| {
                assert!(restored.join(".git/index.lock").is_file());
                assert!(output(git(&restored).args(["read-tree", "HEAD"])).is_err());
                fs::write(index, b"concurrent index publication")?;
                Ok(())
            }
        )
        .is_err());
        assert_eq!(
            fs::read(restored.join(".git/index")).unwrap(),
            b"concurrent index publication"
        );
        assert!(!restored.join(".git/index.lock").exists());
        let lock_path = root.join("reservation.lock");
        let reservation = IndexReservation::acquire(lock_path.clone()).unwrap();
        fs::rename(&lock_path, root.join("reservation-original")).unwrap();
        fs::write(&lock_path, b"replacement writer").unwrap();
        drop(reservation);
        assert_eq!(fs::read(lock_path).unwrap(), b"replacement writer");
        fs::remove_file(dest.join(".git/index")).unwrap();
        assert_eq!(
            repair_missing_index(&bundle, &dest, "neo", &root.join("repair-wrong-head")),
            Err(BulkloadRefusal::GitAuthorityChanged)
        );
        assert!(!dest.join(".git/index").exists());
        assert_eq!(
            fs::read(dest.join("tracked")).unwrap(),
            b"destination divergence"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
