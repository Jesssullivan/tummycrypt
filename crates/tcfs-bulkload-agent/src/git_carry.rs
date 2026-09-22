//! Git-native archival union. Native refs, HEAD, index and checkout are never written.
//!
//! Bundles preserve staged state separately from the worktree (including ignored
//! files), minus the fixed rebuildable set in [`REBUILDABLE_DIRECTORIES`], which
//! is recorded as custody instead of carried. Submodules must be captured
//! separately; this is not Git administration reconstruction. Capture is
//! optimistic, not an atomic filesystem snapshot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{BulkloadRefusal, Result};

mod batch_objects;
mod raw_tree;
pub mod registered;
mod shallow;
pub mod shared;

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

// Build a raw tree without running attributes/filters or starting Git per file.
fn capture_tree(private: &Path, repo: &Path, _index: &Path) -> Result<String> {
    let rows = filesystem_rows(repo)?;
    let pass = raw_tree::capture(private, repo, &rows, &raw_tree::Reuse::new())?;
    // Verification, not capture: a destination changing under the audit is
    // never tolerated as drift, exactly as before drift tolerance existed.
    if !pass.drift.is_empty() || rows != filesystem_rows(repo)? {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(pass.tree)
}

fn capture_refs(repo: &Path, private: &Path, inventory: &str) -> Result<()> {
    let mut pending = std::collections::BTreeMap::new();
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
        if !oid(value) || exported.as_bytes().contains(&0) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if pending.insert(exported, value.to_owned()).is_some() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let stash = output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]));
    if let Ok(stash) = stash {
        let stash = String::from_utf8(stash).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        for value in stash.lines() {
            if !oid(value) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            let name = format!("refs/carry-export/stashes/{value}");
            pending.entry(name).or_insert_with(|| value.to_owned());
        }
    } else if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // One create-only Git transaction instead of one process per ref. NUL
    // framing preserves legal quote characters without command interpolation.
    let mut commands = Vec::new();
    for (name, value) in pending {
        commands.extend_from_slice(b"create ");
        commands.extend_from_slice(name.as_bytes());
        commands.push(0);
        commands.extend_from_slice(value.as_bytes());
        commands.push(0);
    }
    if !commands.is_empty() {
        input(
            git(private).args(["update-ref", "--stdin", "-z"]),
            &commands,
        )?;
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

/// The typed inputs of a reusable capture key.
///
/// The key is an opaque digest and cannot say *what* moved. These parts can, so
/// a caller re-reading them after a pass can tell drift it may tolerate (the ref
/// inventory and the worktree census: what a sibling agent lane or a build tool
/// produces) from Git authority it must refuse (HEAD, the index, configuration,
/// the shallow boundary, nested worktree custody, the omitted rebuildable roots).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyParts {
    repo: Vec<u8>,
    common: Vec<u8>,
    inventory: String,
    head: String,
    symbolic: Vec<u8>,
    index: Vec<u8>,
    exclude: Vec<u8>,
    stash: Vec<u8>,
    rows: Vec<crate::RowSchema>,
    configuration: Vec<(String, Vec<u8>)>,
    boundary: Vec<u8>,
    nested: Vec<NestedWorktree>,
    omitted: Vec<Vec<u8>>,
    identities: Vec<(u64, u64)>,
}

/// Reusable-key domain for the authority digest of a capture's key parts.
const CAPTURE_AUTHORITY_DOMAIN: &[u8] = b"tcfs-git-capture-authority-v1\0";

fn framed(hash: &mut blake3::Hasher, bytes: &[u8]) -> Result<()> {
    hash.update(
        &u64::try_from(bytes.len())
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?
            .to_le_bytes(),
    );
    hash.update(bytes);
    Ok(())
}

impl KeyParts {
    /// The reusable capture key these parts hash to.
    ///
    /// The domain tag, the input order, the u64-length framing and the
    /// present-only sidecar domains are exactly what they were before drift
    /// tolerance existed, so retained captures of unchanged checkouts keep their
    /// keys bit-for-bit and are never re-done.
    ///
    /// # Errors
    /// Refuses inputs too large to frame or encode.
    pub fn digest(&self) -> Result<[u8; 32]> {
        let rows = postcard::to_allocvec(&self.rows).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let configuration =
            postcard::to_allocvec(&self.configuration).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tcfs-git-reusable-capture-v1\0");
        for bytes in [
            self.repo.as_slice(),
            self.common.as_slice(),
            self.inventory.as_bytes(),
            self.head.as_bytes(),
            &self.symbolic,
            &self.index,
            &self.exclude,
            &self.stash,
            &rows,
            &configuration,
            &self.boundary,
        ] {
            framed(&mut hash, bytes)?;
        }
        // A sidecar, hashed only when present: repos without nested worktrees keep
        // their existing keys, so retained captures are not re-done for a schema.
        if !self.nested.is_empty() {
            let nested =
                postcard::to_allocvec(&self.nested).map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(NESTED_WORKTREES_DOMAIN);
            framed(&mut hash, &nested)?;
        }
        // A sidecar, hashed only when present, and only the omitted roots -- never
        // their sizes. A repository with no omission keeps the key it already had,
        // and a build writing inside an omitted root cannot move this key, which is
        // the whole point: rebuildable churn must not refuse a 40-minute capture.
        if !self.omitted.is_empty() {
            let omitted =
                postcard::to_allocvec(&self.omitted).map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(REBUILDABLE_DOMAIN);
            framed(&mut hash, &omitted)?;
        }
        for (dev, ino) in &self.identities {
            for value in [i128::from(*dev), i128::from(*ino)] {
                hash.update(&value.to_le_bytes());
            }
        }
        Ok(*hash.finalize().as_bytes())
    }

    /// Digest of every key input outside the two drift-tolerant ones.
    ///
    /// Two passes with equal authority differ, if at all, only in the ref
    /// inventory and the worktree census. A later pass with the same authority
    /// as a retained drifted capture extends that capture: it re-reads exactly
    /// the seats whose identity moved and reuses every other blob (R25).
    ///
    /// # Errors
    /// Refuses inputs too large to frame or encode.
    pub fn authority(&self) -> Result<[u8; 32]> {
        let configuration =
            postcard::to_allocvec(&self.configuration).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let nested =
            postcard::to_allocvec(&self.nested).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let omitted =
            postcard::to_allocvec(&self.omitted).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let identities =
            postcard::to_allocvec(&self.identities).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let mut hash = blake3::Hasher::new();
        hash.update(CAPTURE_AUTHORITY_DOMAIN);
        for bytes in [
            self.repo.as_slice(),
            self.common.as_slice(),
            self.head.as_bytes(),
            &self.symbolic,
            &self.index,
            &self.exclude,
            &self.stash,
            &configuration,
            &self.boundary,
            &nested,
            &omitted,
            &identities,
        ] {
            framed(&mut hash, bytes)?;
        }
        Ok(*hash.finalize().as_bytes())
    }

    /// Whether every key input outside the two drift-tolerant ones is unchanged.
    ///
    /// The ref inventory and the worktree census are the concurrency classes a
    /// sibling agent lane or a build tool produces. Everything else moving is
    /// Git authority changing under the capture and stays a refusal (R-N30).
    #[must_use]
    pub fn drift_only(&self, other: &Self) -> bool {
        self.repo == other.repo
            && self.common == other.common
            && self.head == other.head
            && self.symbolic == other.symbolic
            && self.index == other.index
            && self.exclude == other.exclude
            && self.stash == other.stash
            && self.configuration == other.configuration
            && self.boundary == other.boundary
            && self.nested == other.nested
            && self.omitted == other.omitted
            && self.identities == other.identities
    }
}

/// The typed inputs of the reusable capture key for `repo`, default policy.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn capture_key_parts(repo: &Path) -> Result<KeyParts> {
    capture_key_parts_with_policy(repo, CapturePolicy::default())
}

/// [`capture_key_parts`] under an explicit capture policy.
///
/// This reads Git metadata and filesystem metadata, not ordinary file contents.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn capture_key_parts_with_policy(repo: &Path, policy: CapturePolicy) -> Result<KeyParts> {
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
    let census = capture_census(&repo, &common, policy)?;
    let configuration = source_configuration(&repo)?;
    let boundary = shallow::frontier(&repo)?;
    let mut identities = Vec::with_capacity(2);
    for directory in [&repo, &common] {
        let identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(directory)?);
        identities.push((identity.dev, identity.ino));
    }
    Ok(KeyParts {
        repo: repo.as_os_str().as_bytes().to_vec(),
        common: common.as_os_str().as_bytes().to_vec(),
        inventory,
        head,
        symbolic: symbolic.stdout,
        index,
        exclude,
        stash,
        rows: census.rows,
        configuration,
        boundary,
        nested: census.nested_worktrees,
        omitted: census.omitted,
        identities,
    })
}

/// Identity of a reusable capture, including new transit refs and full census.
///
/// This reads Git metadata and filesystem metadata, not ordinary file contents.
/// Callers must compare before/after keys and retain the successful bundle.
/// It is not a filesystem journal, atomic snapshot, or a no-rewalk claim.
///
/// Uses the default [`CapturePolicy`], which omits the fixed rebuildable set.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn reusable_capture_key(repo: &Path) -> Result<[u8; 32]> {
    reusable_capture_key_with_policy(repo, CapturePolicy::default())
}

/// [`reusable_capture_key`] under an explicit capture policy.
///
/// A policy that carries the rebuildable set produces exactly the key the
/// pre-omission agent produced, so retained full-fidelity captures stay reusable.
///
/// # Errors
/// Refuses unsupported source indexes, filesystem seats or Git state.
pub fn reusable_capture_key_with_policy(repo: &Path, policy: CapturePolicy) -> Result<[u8; 32]> {
    capture_key_parts_with_policy(repo, policy)?.digest()
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
    let heads = shallow::headers(&repo, &bundle)?;
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
/// Uses the default [`CapturePolicy`], which omits the fixed rebuildable set
/// and records it as custody. Use [`export_repository_with_policy`] for full
/// fidelity.
///
/// # Errors
/// Refuses unsupported/unmerged indexes, changing refs/index/worktree, and
/// populated submodules (which require their own capture). On refusal the
/// private capture is retained for diagnosis; source state is never changed.
pub fn export_repository(repo: &Path, capture: &Path) -> Result<PathBuf> {
    Ok(refusing_drift(export_repository_inner(
        repo,
        capture,
        &ExportOptions::default(),
    )?)?
    .bundle)
}

/// Export one worktree under an explicit capture policy and prerequisite.
///
/// The returned [`Export`] names every omitted rebuildable root and its
/// measured size, which is the same custody the bundle's metadata ref carries.
/// Drift under the pass is still a refusal here; [`export_repository_with_drift`]
/// is the tolerant entry point.
///
/// # Errors
/// Refuses everything [`export_repository`] refuses, plus invalid prerequisites.
pub fn export_repository_with_policy(
    repo: &Path,
    capture: &Path,
    prerequisite: Option<&Path>,
    policy: CapturePolicy,
) -> Result<Export> {
    refusing_drift(export_repository_inner(
        repo,
        capture,
        &ExportOptions {
            prerequisite,
            policy,
            reuse: None,
        },
    )?)
}

/// Export workspace state without repacking a shared base's commit closure.
///
/// The base must be transported and imported before this prerequisite bundle.
/// Callers must bind both bundles' digests in their durable capture record.
///
/// # Errors
/// Refuses invalid prerequisites or any state refused by standalone capture.
pub fn export_repository_with_prerequisite(
    repo: &Path,
    capture: &Path,
    base: &Path,
) -> Result<PathBuf> {
    Ok(refusing_drift(export_repository_inner(
        repo,
        capture,
        &ExportOptions {
            prerequisite: Some(base),
            policy: CapturePolicy::default(),
            reuse: None,
        },
    )?)?
    .bundle)
}

/// Export one worktree and all repository refs, tolerating concurrent drift.
///
/// Refs appearing, moving or disappearing in the shared ref store, and worktree
/// seats appearing, changing or vanishing under the byte pass, are reported as
/// [`Export::drift`] instead of refusing the capture (R25: the host need not
/// halt agent work or git work). A drifted seat's bytes are carried by neither
/// the worktree tree nor `filesystem-v1`; the drift list is the statement that
/// they are absent, and the next pass re-reads exactly those seats (R-N28).
/// Git authority moving -- HEAD, the index, configuration, the shallow
/// boundary, nested worktree custody, the omitted rebuildable roots -- is still
/// a refusal (R-N30).
///
/// # Errors
/// Refuses changing Git authority, unsupported Git or filesystem state, invalid
/// prerequisites, and drift larger than [`DRIFT_ROW_LIMIT`].
pub fn export_repository_with_drift(
    repo: &Path,
    capture: &Path,
    options: &ExportOptions<'_>,
) -> Result<Export> {
    export_repository_inner(repo, capture, options)
}

// The pre-drift contract for callers that never asked for tolerance.
fn refusing_drift(export: Export) -> Result<Export> {
    if export.drift.is_empty() {
        Ok(export)
    } else {
        Err(BulkloadRefusal::GitAuthorityChanged)
    }
}

fn export_repository_inner(
    repo: &Path,
    capture: &Path,
    options: &ExportOptions<'_>,
) -> Result<Export> {
    use std::os::unix::fs::DirBuilderExt;
    let repo = fs::canonicalize(repo)?;
    fs::DirBuilder::new().mode(0o700).create(capture)?;
    let capture = fs::canonicalize(capture)?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    let before_refs = refs(&repo)?;
    let configuration = source_configuration(&repo)?;
    let boundary = shallow::frontier(&repo)?;
    let common = common_repository(&repo)?;
    let census = capture_census(&repo, &common, options.policy)?;
    let seats = &census.rows;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let (index_path, before_index) = source_index(&repo)?;
    let private = prepare_private(&repo, &capture)?;
    capture_refs(&repo, &private, &before_refs)?;
    set_ref(&private, "refs/carry-export/head", &head)?;
    capture_administration(&repo, &private, &boundary, &configuration)?;
    let index = capture.join("index");
    fs::write(&index, &before_index)?;
    let staged = text(snapshot_command(&private, &repo, &index).arg("write-tree"))?;
    set_ref(
        &private,
        "refs/carry-export/staged",
        &commit_tree(&private, &staged, "bulkload staged tree")?,
    )?;
    #[cfg(test)]
    mid_pass::fire(&repo);
    // Seats a retained capture already holds at this exact identity are emitted
    // by object name. Nothing is opened for them and no source byte is re-read.
    let reuse = match options.reuse {
        Some(previous) => reusable_blobs(&private, previous, seats)?,
        None => raw_tree::Reuse::new(),
    };
    let pass = raw_tree::capture(&private, &repo, seats, &reuse)?;
    let after = capture_census(&repo, &common, options.policy)?;
    // Git authority moving under the capture is never drift. The nested
    // worktree census is compared apart from the seats precisely because it
    // carries each nested HEAD, which must keep its refusal; so is the omitted
    // set, because it is a key input a rebuild can move.
    if configuration != source_configuration(&repo)?
        || boundary != shallow::frontier(&repo)?
        || before_index != fs::read(index_path)?
        || head != text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?
        || census.nested_worktrees != after.nested_worktrees
        || census.omitted != after.omitted
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let drift = observed_drift(pass.drift, &before_refs, &refs(&repo)?, seats, &after.rows)?;
    // A seat whose identity moved only after its bytes were streamed is already
    // in the tree; withdraw it rather than present bytes the pass cannot vouch for.
    let withdrawn = drift.withdrawn(seats);
    let tree = raw_tree::prune(&private, &pass.tree, &withdrawn)?;
    set_ref(
        &private,
        "refs/carry-export/worktree",
        &commit_tree(
            &private,
            &tree,
            "bulkload worktree including untracked and ignored files",
        )?,
    )?;
    // filesystem-v1 must never name a seat whose bytes were not captured: a
    // restore replays it as a mode and shape claim about the restored payload.
    let absent: std::collections::BTreeSet<&[u8]> = withdrawn.iter().map(Vec::as_slice).collect();
    let carried: Vec<&crate::RowSchema> = seats
        .iter()
        .filter(|row| !absent.contains(row.rel_path.as_slice()))
        .collect();
    metadata(
        &private,
        "filesystem-v1",
        &postcard::to_allocvec(&carried).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    // Typed custody for registered worktrees nested inside this checkout. Their
    // bytes are captured as their own estate items; this manifest names them so
    // the receipt and the parity audit can account for the skipped subtrees.
    if !census.nested_worktrees.is_empty() {
        metadata(
            &private,
            NESTED_WORKTREES_METADATA,
            &postcard::to_allocvec(&census.nested_worktrees)
                .map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    // Omission is recorded, never silent. Sizes are measured once, here, and
    // deliberately excluded from both the reusable key and the before/after
    // census comparison: they are custody evidence about bytes this capture
    // chose not to carry, not an assertion that those bytes held still.
    let omitted = measure_omissions(&repo, &census.omitted)?;
    if !omitted.is_empty() {
        metadata(
            &private,
            REBUILDABLE_METADATA,
            &postcard::to_allocvec(&omitted).map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    // A sidecar, emitted only when the pass raced something: a clean capture's
    // ref set, and therefore its bundle, is exactly what it was before drift
    // tolerance existed. No restore path reads this ref back, so old and new
    // bundles stay mutually applicable.
    if !drift.is_empty() {
        metadata(
            &private,
            CAPTURE_DRIFT_METADATA,
            &postcard::to_allocvec(&drift).map_err(|_| BulkloadRefusal::FrameCodec)?,
        )?;
    }
    let bundle = capture.join("capture.bundle");
    shared::write_bundle(&private, &bundle, options.prerequisite)?;
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    Ok(Export {
        bundle,
        omitted,
        drift,
        bytes_read: pass.bytes_read,
    })
}

// HEAD, the symbolic head, the exclude file, the shallow frontier and the
// configuration: administration every capture carries, before the byte pass.
fn capture_administration(
    repo: &Path,
    private: &Path,
    boundary: &[u8],
    configuration: &[(String, Vec<u8>)],
) -> Result<()> {
    let symbolic_head = text(git(repo).args(["symbolic-ref", "-q", "HEAD"])).unwrap_or_default();
    metadata(private, "head-symbolic", symbolic_head.as_bytes())?;
    let exclude_path = PathBuf::from(text(git(repo).args([
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
    metadata(private, "exclude", &exclude)?;
    if !boundary.is_empty() {
        metadata(private, "shallow-frontier-v1", boundary)?;
    }
    metadata(
        private,
        "configuration-v1",
        &postcard::to_allocvec(configuration).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )
}

// Everything one pass observed moving: what the byte pass saw seat by seat,
// what the ref store did, and what the post-pass census says about the seats.
fn observed_drift(
    in_pass: Vec<DriftRow>,
    refs_before: &str,
    refs_after: &str,
    seats_before: &[crate::RowSchema],
    seats_after: &[crate::RowSchema],
) -> Result<CaptureDrift> {
    let mut drift = CaptureDrift::default();
    drift.extend(in_pass);
    drift.extend(reference_drift(refs_before, refs_after)?);
    drift.extend(seat_drift(seats_before, seats_after));
    drift.seal()?;
    Ok(drift)
}

/// Blobs a retained capture of this same checkout already holds.
///
/// Only seats whose `StatIdentity` is unchanged since that capture qualify:
/// exactly the freshness tuple `RowSchema::stat_identity` documents. A seat that
/// drifted in the retained pass is absent from its tree and is therefore read
/// again here, which is the whole point of the incremental pass (R-N28).
fn reusable_blobs(
    private: &Path,
    previous: &Path,
    seats: &[crate::RowSchema],
) -> Result<raw_tree::Reuse> {
    use std::process::Stdio;
    use tcfs_bulkload_proto::FileKind;
    let mut reuse = raw_tree::Reuse::new();
    // An unreadable or unsatisfiable retained bundle costs only the optimization.
    if !git(private)
        .args(["fetch", "--no-tags", "--quiet"])
        .arg(previous)
        .args([
            "+refs/carry-export/worktree:refs/carry-reuse/worktree",
            "+refs/carry-export/filesystem-v1:refs/carry-reuse/filesystem-v1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success()
    {
        return Ok(reuse);
    }
    let held: Vec<crate::RowSchema> = postcard::from_bytes(&output(
        git(private).args(["show", "refs/carry-reuse/filesystem-v1:value"]),
    )?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    let held: std::collections::BTreeMap<&[u8], &crate::RowSchema> = held
        .iter()
        .map(|row| (row.rel_path.as_slice(), row))
        .collect();
    let current: std::collections::BTreeMap<&[u8], &crate::RowSchema> = seats
        .iter()
        .map(|row| (row.rel_path.as_slice(), row))
        .collect();
    #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax, not interpolation.
    let entries =
        output(git(private).args(["ls-tree", "-r", "-z", "refs/carry-reuse/worktree^{tree}"]))?;
    for entry in entries.split(|byte| *byte == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let (header, path) = entry.split_at(tab);
        let path = path
            .get(1..)
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        let header =
            std::str::from_utf8(header).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
        let mut fields = header.split(' ');
        let (Some(mode), Some("blob"), Some(object)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        };
        if !oid(object) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let (Some(row), Some(before)) = (current.get(path), held.get(path)) else {
            continue;
        };
        if row.kind == FileKind::Regular
            && seat_equivalent(before, row)
            && raw_tree::mode_of(row)? == mode
        {
            reuse.insert(path.to_vec(), object.to_owned());
        }
    }
    // The retained capture's refs are an optimization input, never carried on.
    for name in [
        "refs/carry-reuse/worktree",
        "refs/carry-reuse/filesystem-v1",
    ] {
        output(git(private).args(["update-ref", "-d", name]))?;
    }
    Ok(reuse)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod mid_pass {
    //! Test-only injection point: run a closure inside one export, after its
    //! census and ref snapshot and before its byte pass, keyed by the captured
    //! checkout so parallel tests never fire each other's hooks.
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    type Hook = Box<dyn FnOnce() + Send>;
    static ARMED: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

    pub fn arm(repo: &Path, hook: impl FnOnce() + Send + 'static) {
        let repo = std::fs::canonicalize(repo).expect("armed checkout exists");
        ARMED.lock().expect("hooks").push((repo, Box::new(hook)));
    }

    pub fn fire(repo: &Path) {
        let hook = {
            let mut armed = ARMED.lock().expect("hooks");
            armed
                .iter()
                .position(|(path, _)| path == repo)
                .map(|index| armed.remove(index).1)
        };
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// Directory names whose contents a capture omits, at any depth below a root.
///
/// Every entry is a build or tool output directory whose contents a standard
/// command regenerates from bytes the capture *does* carry, so omitting them
/// loses no state that cannot be rebuilt offline from the same checkout:
///
/// - `target` -- `cargo build` / `mvn package` output.
/// - `node_modules` -- `npm|pnpm|yarn install` output, pinned by the lockfile.
/// - `.venv`, `venv` -- Python virtual environments, rebuilt from the lockfile.
/// - `__pycache__` -- interpreter bytecode cache, rewritten on next import.
/// - `.direnv` -- direnv's layout/Nix profile cache, rebuilt by `direnv reload`.
/// - `.pytest_cache`, `.mypy_cache`, `.ruff_cache` -- tool caches, rebuilt on
///   the next run of the tool that wrote them.
/// - `.gradle` -- project-local Gradle build cache.
/// - `.next`, `.turbo`, `.parcel-cache`, `.swc` -- JavaScript bundler output
///   and build caches, rebuilt by the next build.
/// - `.terraform` -- provider plugins and module cache, rebuilt by
///   `terraform init`. Local *state* lives in `terraform.tfstate` beside it,
///   which is an ordinary carried file.
///
/// Deliberately absent: `build` and `dist` (too many repositories track them),
/// `.cargo/registry` (not a name, and a vendored registry may be the only
/// offline copy), and the `bazel-*` convenience symlinks -- the walk records a
/// symlink as one row and never descends it, so they already cost nothing and
/// omitting them would discard real state for no saving.
///
/// A name matches only when the seat is a real directory (not a symlink to
/// one) and Git tracks nothing beneath it; see [`CapturePolicy`].
pub const REBUILDABLE_DIRECTORIES: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".direnv",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".gradle",
    ".next",
    ".turbo",
    ".parcel-cache",
    ".swc",
    ".terraform",
];

/// Metadata ref naming the rebuildable roots a capture omitted.
const REBUILDABLE_METADATA: &str = "rebuildable-omissions-v1";
/// Reusable-key domain for the omission sidecar; only hashed when present.
const REBUILDABLE_DOMAIN: &[u8] = b"tcfs-git-rebuildable-omissions-v1\0";

/// What a capture carries beyond tracked content.
///
/// The default omits [`REBUILDABLE_DIRECTORIES`]; untracked and ignored files
/// everywhere else are still carried, exactly as before. `include_rebuildable`
/// restores full fidelity and reproduces the pre-omission capture key byte for
/// byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CapturePolicy {
    /// Carry the rebuildable set instead of recording it as custody.
    pub include_rebuildable: bool,
}

impl CapturePolicy {
    /// The full-fidelity policy: carry everything, omit nothing.
    #[must_use]
    pub const fn including_rebuildable() -> Self {
        Self {
            include_rebuildable: true,
        }
    }
}

/// One rebuildable root a capture omitted, with the size it did not carry.
///
/// Sizes are a single stat pass taken after the seats census. Entries that
/// vanish during that pass are simply not counted: a build rewriting its own
/// output is the expected condition, and custody evidence must not refuse.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RebuildableOmission {
    /// Omitted root relative to the captured checkout, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// Apparent bytes of regular files below the root, at measurement time.
    pub bytes: u64,
    /// Seats below the root, at measurement time.
    pub entries: u64,
}

/// What a capture may exclude, may reuse and must not carry.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExportOptions<'a> {
    /// A shared base bundle whose commit closure this capture may exclude.
    pub prerequisite: Option<&'a Path>,
    /// Which rebuildable roots become custody instead of seats.
    pub policy: CapturePolicy,
    /// A retained capture of this same checkout whose blobs may be reused.
    ///
    /// Every seat whose `StatIdentity` is unchanged since that capture is
    /// emitted by object name instead of being opened: the R25 never-rewalk
    /// clause, measured by [`Export::bytes_read`].
    pub reuse: Option<&'a Path>,
}

/// A completed export: the bundle, the custody for what it did not carry, what
/// raced it, and what it had to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The written and verified capture bundle.
    pub bundle: PathBuf,
    /// Rebuildable roots omitted from the capture, with measured sizes.
    pub omitted: Vec<RebuildableOmission>,
    /// Refs and seats that changed under the pass. Empty is the ordinary case.
    pub drift: CaptureDrift,
    /// Bytes streamed from source file descriptors during this pass.
    pub bytes_read: u64,
}

/// One metadata census of a checkout: typed seats plus custody for what the
/// capture does not carry (nested worktrees, omitted rebuildable roots).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Census {
    rows: Vec<crate::RowSchema>,
    /// Registered worktrees of the same repository nested below the root;
    /// their HEADs are part of the census, their bytes are their own item's.
    nested_worktrees: Vec<NestedWorktree>,
    /// Omitted roots, relative to the census root. Sizes are not part of the
    /// census: a rebuildable root's contents are exactly what must not be able
    /// to invalidate a capture in flight.
    omitted: Vec<Vec<u8>>,
}

/// Rebuildable roots a capture of `repo` would omit, with their measured sizes.
///
/// This is the custody the capture records instead of their bytes, available to
/// receipt and parity-audit tooling without performing a capture.
///
/// # Errors
/// Refuses any Git or filesystem state capture itself refuses.
pub fn rebuildable_omissions(repo: &Path) -> Result<Vec<RebuildableOmission>> {
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    let census = capture_census(&repo, &common, CapturePolicy::default())?;
    measure_omissions(&repo, &census.omitted)
}

// Apparent size and seat count below one omitted root. A vanished entry is
// rebuildable churn, not a fault: skip it rather than refuse the capture.
fn measure_omissions(root: &Path, omitted: &[Vec<u8>]) -> Result<Vec<RebuildableOmission>> {
    use std::os::unix::ffi::OsStrExt;
    let mut measured = Vec::with_capacity(omitted.len());
    for rel_path in omitted {
        let start = safe_destination(root, Path::new(std::ffi::OsStr::from_bytes(rel_path)))?;
        let (mut bytes, mut entries) = (0u64, 0u64);
        let mut pending = vec![start];
        while let Some(directory) = pending.pop() {
            let listing = match fs::read_dir(&directory) {
                Ok(listing) => listing,
                Err(error) if vanished(&error) => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in listing {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error.into()),
                };
                let path = entry.path();
                let meta = match fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error.into()),
                };
                entries = entries.saturating_add(1);
                if meta.is_dir() {
                    pending.push(path);
                } else if meta.is_file() {
                    bytes = bytes.saturating_add(meta.len());
                }
            }
        }
        measured.push(RebuildableOmission {
            rel_path: rel_path.clone(),
            bytes,
            entries,
        });
    }
    Ok(measured)
}

fn vanished(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}

// A capture-side census: rebuildable roots below `root` become custody instead
// of seats unless the policy asks for full fidelity. Attach and comparison
// paths keep the strict full census; they verify payload that is already here.
fn capture_census(root: &Path, common: &Path, policy: CapturePolicy) -> Result<Census> {
    filesystem_census(root, Some(common), policy)
}

// A name on the fixed list is only rebuildable if Git tracks nothing beneath
// it. A repository that really does track `target/...` keeps its bytes; the
// omission claim stays provable per repository instead of merely asserted.
fn rebuildable_root(root: &Path, relative: &Path, name: &std::ffi::OsStr) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    if !REBUILDABLE_DIRECTORIES
        .iter()
        .any(|candidate| name.as_bytes() == candidate.as_bytes())
    {
        return Ok(false);
    }
    let mut pathspec = std::ffi::OsString::from(":(top,literal)");
    pathspec.push(relative.as_os_str());
    Ok(output(git(root).args(["ls-files", "-z", "--"]).arg(pathspec))?.is_empty())
}

/// Metadata ref naming registered worktrees nested inside a captured checkout.
const NESTED_WORKTREES_METADATA: &str = "nested-worktrees-v1";
/// Reusable-key domain for the nested-worktree sidecar; only hashed when present.
const NESTED_WORKTREES_DOMAIN: &[u8] = b"tcfs-git-nested-worktrees-v1\0";
/// Largest `.git` gitdir-pointer file this census will read.
const GITDIR_POINTER_LIMIT: u64 = 64 * 1024;

/// Metadata ref naming the refs and seats that drifted under one capture pass.
const CAPTURE_DRIFT_METADATA: &str = "capture-drift-v1";
/// Largest drift list a single capture may report.
///
/// Unbounded drift is indistinguishable from a rebuild of the checkout and must
/// never be silently truncated into a capture that claims to be complete.
pub const DRIFT_ROW_LIMIT: usize = 65_536;

/// What changed under a capture pass that did not have to refuse.
///
/// Ref and seat drift are the two concurrency classes an agent lane produces:
/// a sibling worktree creating branches in the shared ref store, and a build
/// tool rewriting a file between the census and its byte pass. Neither is
/// corruption and neither is Git authority moving under the capture.
#[non_exhaustive]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum DriftKind {
    /// A ref absent from the pre-pass inventory appeared.
    RefAdded,
    /// A ref in the pre-pass inventory moved to another object.
    RefChanged,
    /// A ref in the pre-pass inventory was deleted.
    RefRemoved,
    /// A seat absent from the pre-pass census appeared; it is not carried.
    SeatAdded,
    /// A seat's identity changed under the pass; its bytes are not carried.
    SeatChanged,
    /// A seat in the pre-pass census was removed; its bytes are not carried.
    SeatRemoved,
}

impl DriftKind {
    /// Whether this row names a filesystem seat rather than a ref.
    #[must_use]
    pub const fn is_seat(self) -> bool {
        matches!(
            self,
            Self::SeatAdded | Self::SeatChanged | Self::SeatRemoved
        )
    }

    // Whether the named seat was in the census yet its bytes are absent.
    const fn withdraws_payload(self) -> bool {
        matches!(self, Self::SeatChanged | Self::SeatRemoved)
    }
}

/// One drifted ref or seat.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct DriftRow {
    /// What kind of drift this is.
    pub kind: DriftKind,
    /// Refname for ref kinds, checkout-relative path for seat kinds, raw bytes.
    pub name: Vec<u8>,
}

impl DriftRow {
    pub(crate) fn seat(kind: DriftKind, name: &[u8]) -> Self {
        Self {
            kind,
            name: name.to_vec(),
        }
    }

    fn reference(kind: DriftKind, name: &str) -> Self {
        Self {
            kind,
            name: name.as_bytes().to_vec(),
        }
    }

    /// One receipt line. Debug-escaped: receipts must not carry raw newlines.
    #[must_use]
    #[allow(clippy::unnecessary_debug_formatting)] // Escaping is the point.
    pub fn display(&self) -> String {
        use std::os::unix::ffi::OsStrExt;
        format!(
            "{:?} {:?}",
            self.kind,
            Path::new(std::ffi::OsStr::from_bytes(&self.name))
        )
    }
}

/// Everything one capture pass observed changing under it.
///
/// An empty list is the ordinary case and is never carried, hashed or written:
/// a checkout nothing raced against produces exactly the bundle it produced
/// before drift tolerance existed. Drift is never part of the reusable key; it
/// describes a pass, not a repository state.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaptureDrift {
    /// Drifted refs and seats, sorted by (kind, name), deduplicated.
    pub rows: Vec<DriftRow>,
}

impl CaptureDrift {
    /// Whether this pass raced against nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many refs and seats drifted.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.len()
    }

    /// One receipt line per drifted ref and seat.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.rows.iter().map(DriftRow::display).collect()
    }

    /// Seats whose bytes this capture deliberately does not carry.
    ///
    /// Directories hold no bytes and are never withdrawn from the restored
    /// shape; a seat that appeared mid-pass was never in the census at all.
    #[must_use]
    pub fn withdrawn(&self, seats: &[crate::RowSchema]) -> Vec<Vec<u8>> {
        use tcfs_bulkload_proto::FileKind;
        let directories: std::collections::BTreeSet<&[u8]> = seats
            .iter()
            .filter(|row| row.kind == FileKind::Directory)
            .map(|row| row.rel_path.as_slice())
            .collect();
        self.rows
            .iter()
            .filter(|row| {
                row.kind.withdraws_payload() && !directories.contains(row.name.as_slice())
            })
            .map(|row| row.name.clone())
            .collect()
    }

    fn extend(&mut self, rows: impl IntoIterator<Item = DriftRow>) {
        self.rows.extend(rows);
    }

    // Deterministic order, exactly as the census sorts its seats, and a hard
    // budget: drift larger than the cap is a rebuild, not a captured pass.
    fn seal(&mut self) -> Result<()> {
        self.rows.sort_unstable();
        self.rows.dedup();
        if self.rows.len() > DRIFT_ROW_LIMIT {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        Ok(())
    }
}

// Keyed on refname: a lane creating branches in a shared ref store is drift,
// never a refusal. HEAD is not in `for-each-ref` and keeps its own comparison.
fn reference_drift(before: &str, after: &str) -> Result<Vec<DriftRow>> {
    let index = |inventory: &str| -> Result<std::collections::BTreeMap<String, String>> {
        inventory
            .lines()
            .map(|line| {
                line.split_once(' ')
                    .map(|(value, name)| (name.to_owned(), value.to_owned()))
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)
            })
            .collect()
    };
    let (before, after) = (index(before)?, index(after)?);
    let mut rows = Vec::new();
    for (name, value) in &before {
        match after.get(name) {
            None => rows.push(DriftRow::reference(DriftKind::RefRemoved, name)),
            Some(current) if current != value => {
                rows.push(DriftRow::reference(DriftKind::RefChanged, name));
            }
            Some(_) => (),
        }
    }
    for name in after.keys().filter(|name| !before.contains_key(*name)) {
        rows.push(DriftRow::reference(DriftKind::RefAdded, name));
    }
    Ok(rows)
}

// Directories carry no bytes and their timestamps move whenever any child is
// created or removed, so they drift only on kind, mode or link target. Regular
// files and symlinks drift on the full StatIdentity the freshness cache keys on.
fn seat_equivalent(a: &crate::RowSchema, b: &crate::RowSchema) -> bool {
    use tcfs_bulkload_proto::FileKind;
    a.kind == b.kind
        && a.mode == b.mode
        && a.link_target == b.link_target
        && (a.kind == FileKind::Directory
            || (a.stat_identity() == b.stat_identity() && a.nlink == b.nlink))
}

fn seat_drift(before: &[crate::RowSchema], after: &[crate::RowSchema]) -> Vec<DriftRow> {
    fn index(rows: &[crate::RowSchema]) -> std::collections::BTreeMap<&[u8], &crate::RowSchema> {
        rows.iter()
            .map(|row| (row.rel_path.as_slice(), row))
            .collect()
    }
    let (before, after) = (index(before), index(after));
    let mut rows = Vec::new();
    for (path, row) in &before {
        match after.get(path) {
            None => rows.push(DriftRow::seat(DriftKind::SeatRemoved, path)),
            Some(current) if !seat_equivalent(row, current) => {
                rows.push(DriftRow::seat(DriftKind::SeatChanged, path));
            }
            Some(_) => (),
        }
    }
    for path in after.keys().filter(|path| !before.contains_key(*path)) {
        rows.push(DriftRow::seat(DriftKind::SeatAdded, path));
    }
    rows
}

/// A registered linked worktree of the same repository nested inside the checkout.
///
/// Its subtree is not walked or carried by the enclosing capture: it is its own
/// estate item. This row is the enclosing capture's custody statement for it.
/// Claude Code's `<repo>/.claude/worktrees/<name>` convention is the common case.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NestedWorktree {
    /// Worktree root relative to the captured checkout, as raw OS bytes.
    pub rel_path: Vec<u8>,
    /// Name under the common `worktrees/` administration directory.
    pub worktree_name: String,
    /// HEAD of the nested worktree at census time.
    pub head_oid: String,
}

/// Registered worktrees of the same repository nested inside `repo`.
///
/// This is the custody the enclosing capture records instead of their bytes.
///
/// # Errors
/// Refuses any other nested Git administration, exactly as capture does.
pub fn nested_worktrees(repo: &Path) -> Result<Vec<NestedWorktree>> {
    let repo = fs::canonicalize(repo)?;
    let common = common_repository(&repo)?;
    Ok(repository_census(&repo, &common)?.nested_worktrees)
}

// Reuse the transport's typed filesystem seats instead of treating Git's
// executable-bit-only tree modes as complete filesystem metadata. .git is
// administration owned by Git-native capture; nested repositories refuse.
fn filesystem_rows(root: &Path) -> Result<Vec<crate::RowSchema>> {
    Ok(filesystem_census(root, None, CapturePolicy::including_rebuildable())?.rows)
}

// A checkout census: registered worktrees of `common` nested below `root` are
// recorded as custody and not descended; every other nested .git still refuses.
// Full fidelity: nothing rebuildable is omitted.
fn repository_census(root: &Path, common: &Path) -> Result<Census> {
    filesystem_census(root, Some(common), CapturePolicy::including_rebuildable())
}

// Classify a directory below the root that contains an entry named .git.
// Only a regular gitdir-pointer file resolving to a worktree administration
// directory of `common`, registered back to this checkout, is custody; a nested
// independent repository, a foreign pointer or a symlink keeps the refusal.
fn nested_worktree(root: &Path, directory: &Path, common: &Path) -> Result<Option<NestedWorktree>> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let pointer = directory.join(".git");
    let meta = match fs::symlink_metadata(&pointer) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !meta.is_file() || meta.len() > GITDIR_POINTER_LIMIT {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut contents = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&pointer)?
        .take(GITDIR_POINTER_LIMIT)
        .read_to_string(&mut contents)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let target = contents
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("gitdir: "))
        .map(str::trim_end)
        .filter(|value| !value.is_empty())
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    // Any unresolvable pointer is malformed inventory, never a transient IO fault.
    let admin = fs::canonicalize(directory.join(target))
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let worktrees = common.join("worktrees");
    if !admin.is_dir() || admin.parent() != Some(worktrees.as_path()) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let worktree_name = admin
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?
        .to_owned();
    // Registration is the administration's back-pointer to this very checkout.
    let registered = fs::read_to_string(admin.join("gitdir"))
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    if fs::canonicalize(registered.trim_end()).ok() != Some(fs::canonicalize(&pointer)?) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // Git itself must agree that this is a linked worktree of the same repository.
    let seen =
        text(git(directory).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))?;
    if fs::canonicalize(seen).ok().as_deref() != Some(common) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let head_oid = text(git(directory).args(["rev-parse", "--verify", "HEAD"]))?;
    if !oid(&head_oid) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(Some(NestedWorktree {
        rel_path: directory
            .strip_prefix(root)
            .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
            .as_os_str()
            .as_bytes()
            .to_vec(),
        worktree_name,
        head_oid,
    }))
}

fn filesystem_census(root: &Path, common: Option<&Path>, policy: CapturePolicy) -> Result<Census> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use tcfs_bulkload_proto::FileKind;
    let mut pending = vec![root.to_path_buf()];
    let mut rows = Vec::new();
    let mut nested_worktrees = Vec::new();
    let mut omitted = Vec::new();
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
            let relative = path
                .strip_prefix(root)
                .map_err(|_| BulkloadRefusal::PathEscapesRoot)?;
            if kind == FileKind::Directory {
                // A registered nested worktree is custody, not seats: no row for
                // its root, no descent, no contents. Its own item carries them.
                if let Some(custody) = common
                    .map(|common| nested_worktree(root, &path, common))
                    .transpose()?
                    .flatten()
                {
                    nested_worktrees.push(custody);
                    continue;
                }
                // A rebuildable root is custody, not seats: no row for the root
                // itself, no descent, no contents. `cargo build` rebuilds it.
                if !policy.include_rebuildable
                    && rebuildable_root(root, relative, &entry.file_name())?
                {
                    omitted.push(relative.as_os_str().as_bytes().to_vec());
                    continue;
                }
                pending.push(path.clone());
            }
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
    nested_worktrees.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    omitted.sort();
    Ok(Census {
        rows,
        nested_worktrees,
        omitted,
    })
}

fn restore_filesystem_rows(destination: &Path, revision: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
                // Open before applying a potentially unreadable captured mode.
                let file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)?;
                if !file.metadata()?.is_file() {
                    return Err(BulkloadRefusal::GitInventoryMalformed);
                }
                file.set_permissions(fs::Permissions::from_mode(row.mode & 0o777))?;
                file.sync_all()?;
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
        // Flush descendants before their parent, retaining an open descriptor
        // across modes such as 000 so durability does not require reopening it.
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?;
        directory.set_permissions(fs::Permissions::from_mode(row.mode & 0o777))?;
        directory.sync_all()?;
    }
    // These flush payload entry creation, not the separate Git administration
    // or receipt transactions, which retain their own durability boundaries.
    for path in [
        destination,
        destination
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    ] {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?
            .sync_all()?;
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
    let boundary = shallow::frontier(repo)?;
    if !boundary.is_empty() {
        fs::write(private.join("shallow"), boundary)?;
    }
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
    let unpacked = shallow::unpack(repo, &bundle, &heads)?;
    let heads = unpacked.as_ref().unwrap_or(&heads);
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
    if unpacked.is_none() {
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
    }
    let inventory = text(git(repo).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        "refs/carry/v1/",
    ]))?;
    let mut existing = std::collections::BTreeMap::new();
    for line in inventory.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) || existing.insert(name, value).is_some() {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let mut desired = std::collections::BTreeMap::new();
    for (value, _, target) in &names {
        if target.contains('\0')
            || desired
                .insert(target.as_str(), value.as_str())
                .is_some_and(|prior| prior != value.as_str())
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    let mut transaction = Vec::new();
    for (target, value) in desired {
        let operation = match existing.get(target) {
            Some(prior) if *prior == value => "verify",
            Some(_) => return Err(BulkloadRefusal::GitDestinationOccupied),
            None => "create",
        };
        transaction.extend_from_slice(format!("{operation} {target}\0{value}\0").as_bytes());
    }
    // One atomic compare/create transaction, including unchanged refs: a
    // concurrent update or deletion refuses instead of publishing a partial set.
    if !transaction.is_empty() {
        input(
            git(repo).args(["update-ref", "--no-deref", "--stdin", "-z"]),
            &transaction,
        )?;
    }
    Ok(names.len())
}

fn capture_revision(heads: &str, suffix: &str) -> Result<String> {
    heads
        .lines()
        .find_map(|line| {
            line.split_once(' ')
                .filter(|(_, name)| *name == format!("refs/carry-export/{suffix}"))
        })
        .map(|(value, _)| value.to_owned())
        .filter(|value| oid(value))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)
}

fn bundle_object_format(heads: &str) -> Result<&'static str> {
    let value = heads
        .lines()
        .next()
        .and_then(|line| line.split_once(' '))
        .map(|(value, _)| value)
        .filter(|value| oid(value))
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    Ok(if value.len() == 40 { "sha1" } else { "sha256" })
}

fn payload_shape_equal(a: &[crate::RowSchema], b: &[crate::RowSchema]) -> bool {
    use tcfs_bulkload_proto::FileKind;
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.rel_path == b.rel_path
                && a.kind == b.kind
                && a.link_target == b.link_target
                && (a.kind != FileKind::Regular || a.size == b.size)
                && (a.kind == FileKind::Symlink || a.mode & 0o777 == b.mode & 0o777)
        })
}

fn sync_private_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_private_tree(&entry.path())?;
        } else if entry.file_type()?.is_file() {
            fs::File::open(entry.path())?.sync_all()?;
        } else {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
    }
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn prepare_attachment(
    bundle: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
) -> Result<(PathBuf, String)> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(receipt)?;
    // Retain the actual input, not merely a pathname that may later disappear.
    let retained = receipt.join("capture.bundle");
    fs::copy(bundle, &retained)?;
    let heads = text(git(receipt).args(["bundle", "list-heads"]).arg(&retained))?;
    let format = bundle_object_format(&heads)?;
    let private = receipt.join("repository.git");
    output(
        git(receipt)
            .args([
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .arg(&private),
    )?;
    import_bundle(&private, &retained, source)?;
    let heads = shallow::headers(&private, &retained)?;
    let head = capture_revision(&heads, "head")?;
    let symbolic = text(git(&private).args([
        "show",
        &format!("{}:value", capture_revision(&heads, "head-symbolic")?),
    ]))?;
    if symbolic.is_empty() {
        output(git(&private).args(["update-ref", "--no-deref", "HEAD", &head]))?;
    } else {
        if !symbolic.starts_with("refs/heads/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        output(git(&private).args(["check-ref-format", &symbolic]))?;
        set_ref(&private, &symbolic, &head)?;
        output(git(&private).args(["symbolic-ref", "HEAD", &symbolic]))?;
    }
    let exclude = output(git(&private).args([
        "show",
        &format!("{}:value", capture_revision(&heads, "exclude")?),
    ]))?;
    fs::create_dir_all(private.join("info"))?;
    fs::write(private.join("info/exclude"), exclude)?;
    output(
        snapshot_command(&private, destination, &private.join("index")).args([
            "read-tree",
            &format!("{}^{{tree}}", capture_revision(&heads, "staged")?),
        ]),
    )?;
    output(git(&private).args(["config", "core.bare", "false"]))?;
    output(
        git(&private)
            .args(["config", "core.worktree"])
            .arg(destination),
    )?;
    Ok((private, heads))
}

fn source_configuration(repo: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for name in ["config", "config.worktree"] {
        let path =
            text(git(repo).args(["rev-parse", "--path-format=absolute", "--git-path", name]))?;
        match fs::read(path) {
            Ok(bytes) => files.push((name.to_owned(), bytes)),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && name == "config.worktree" => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(files)
}

fn safe_configuration_value(private: &Path, key: &str, value: &str) -> bool {
    if value.contains(['\0', '\n', '\r']) {
        return false;
    }
    if matches!(key, "core.filemode" | "core.ignorecase" | "core.symlinks") {
        return matches!(value, "true" | "false");
    }
    if matches!(key, "user.name" | "user.email") {
        return true;
    }
    if key == "push.default" {
        return matches!(
            value,
            "nothing" | "current" | "upstream" | "simple" | "matching"
        );
    }
    if key == "pull.ff" {
        return matches!(value, "true" | "false" | "only");
    }
    if key == "pull.rebase" {
        return matches!(value, "true" | "false" | "merges");
    }
    if let Some((prefix, field)) = key.rsplit_once('.') {
        if prefix.starts_with("branch.") {
            return match field {
                "remote" | "pushremote" => matches!(value, "origin" | "."),
                "merge" => {
                    value.starts_with("refs/heads/")
                        && git(private)
                            .args(["check-ref-format", value])
                            .output()
                            .is_ok_and(|out| out.status.success())
                }
                "rebase" => matches!(value, "true" | "false" | "merges"),
                _ => false,
            };
        }
        if prefix == "remote.origin" && field == "fetch" {
            return value == "+refs/heads/*:refs/remotes/origin/*";
        }
    }
    false
}

fn activate_standalone_configuration(
    private: &Path,
    heads: &str,
    receipt: &Path,
    mapping: Option<(&Path, &Path)>,
) -> Result<()> {
    let mapping = mapping
        .map(|(from, to)| {
            if !from.is_absolute() || !to.is_absolute() {
                return Err(BulkloadRefusal::PathNotAbsolute);
            }
            common_repository(to)?;
            Ok((
                from.to_str()
                    .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
                to.to_str().ok_or(BulkloadRefusal::GitInventoryMalformed)?,
            ))
        })
        .transpose()?;
    let files: Vec<(String, Vec<u8>)> = postcard::from_bytes(&output(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "configuration-v1")?),
    ]))?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    let mut activated = Vec::new();
    let mut preserved_only = Vec::new();
    let mut origin_seen = false;
    for (name, bytes) in files {
        if !matches!(name.as_str(), "config" | "config.worktree") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let retained = receipt.join(format!("source-{name}"));
        fs::write(&retained, bytes)?;
        // Worktree-local configuration may be dormant unless the source enables
        // its extension. Preserve it, but do not silently flatten its authority.
        if name == "config.worktree" {
            preserved_only.push("config.worktree (not activated)".to_owned());
            continue;
        }
        let parsed = output(
            git(private)
                .args(["config", "--no-includes", "--null", "--list", "--file"])
                .arg(&retained),
        )?;
        for entry in parsed.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
            let entry =
                std::str::from_utf8(entry).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
            let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
            let value = if key == "remote.origin.url" {
                origin_seen = true;
                match mapping {
                    Some((from, to)) if value == from => to,
                    None if safe_https_origin(value) => value,
                    _ => return Err(BulkloadRefusal::GitAuthorityChanged),
                }
            } else if safe_configuration_value(private, key, value) {
                value
            } else {
                preserved_only.push(key.to_owned());
                continue;
            };
            output(git(private).args(["config", "--local", "--add", key, value]))?;
            activated.push(key.to_owned());
        }
    }
    if mapping.is_some() && !origin_seen {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    // Keys, not potentially sensitive values, explain the deliberate policy boundary.
    fs::write(
        receipt.join("configuration-activation.postcard"),
        postcard::to_allocvec(&(activated, preserved_only, mapping))
            .map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    Ok(())
}

fn safe_https_origin(value: &str) -> bool {
    let Some(address) = value.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = address.split_once('/') else {
        return false;
    };
    !host.is_empty()
        && !path.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-._~%".contains(&b))
}

fn prepare_linked_attachment(
    repository: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
    private: &Path,
    heads: &str,
) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let retained = receipt.join("capture.bundle");
    import_bundle(repository, &retained, source)?;
    let head = capture_revision(heads, "head")?;
    let symbolic = text(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "head-symbolic")?),
    ]))?;
    let captured_exclude = output(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "exclude")?),
    ]))?;
    let exclude_path = PathBuf::from(text(git(repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?);
    let exclude = match fs::read(&exclude_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if exclude != captured_exclude {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    let attached = text(git(repository).args(["worktree", "list", "--porcelain"]))?;
    let reuse = symbolic.starts_with("refs/heads/")
        && text(git(repository).args(["rev-parse", "--verify", &symbolic]))
            .is_ok_and(|tip| tip == head)
        && !attached
            .lines()
            .any(|line| line == format!("branch {symbolic}"));
    let branch = if reuse {
        symbolic
            .strip_prefix("refs/heads/")
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?
            .to_owned()
    } else {
        format!(
            "carry/{source}/{}",
            blake3::hash(destination.as_os_str().as_bytes()).to_hex()
        )
    };
    let prepared = receipt.join("prepared-worktree");
    let mut command = git(repository);
    command.args(["worktree", "add", "--no-checkout"]);
    if !reuse {
        command.args(["-b", &branch]);
    }
    output(
        command
            .arg("--")
            .arg(&prepared)
            .arg(if reuse { &branch } else { &head }),
    )?;
    if text(git(&prepared).args(["rev-parse", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    output(git(&prepared).args([
        "read-tree",
        &format!("{}^{{tree}}", capture_revision(heads, "staged")?),
    ]))?;
    let admin = PathBuf::from(text(
        git(&prepared).args(["rev-parse", "--absolute-git-dir"]),
    )?);
    let mut reverse = destination.join(".git").as_os_str().as_bytes().to_vec();
    if reverse.contains(&b'\n') || reverse.contains(&b'\r') {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    reverse.push(b'\n');
    fs::write(admin.join("gitdir"), reverse)?;
    // Native administrative locking prevents prune before pointer publication.
    fs::write(admin.join("locked"), b"bulkload attachment preparation\n")?;
    sync_private_tree(&admin)?;
    fs::File::open(admin.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?.sync_all()?;
    Ok(admin)
}

fn attachment_policy_matches(repository: &Path, private: &Path, heads: &str) -> Result<bool> {
    let expected = output(git(private).args([
        "show",
        &format!("{}:value", capture_revision(heads, "exclude")?),
    ]))?;
    let path = text(git(repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ]))?;
    let actual = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if expected != actual {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    Ok(true)
}

/// Attach captured source administration to an exactly matching payload only.
///
/// Existing payload is never rewritten. New linked administration inherits the
/// explicit common repository's configuration and restores captured source
/// staging, not unknown old staging. Comparisons are
/// optimistic full censuses, not an atomic snapshot of uncooperative writers.
///
/// # Errors
/// Refuses existing .git, differing bytes/modes/empty directories, concurrent
/// changes, unsupported seats, or a receipt inside the payload/on another device.
/// Failed private preparations remain available for inspection.
pub fn attach_matching_payload(
    bundle: &Path,
    repository: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    attach_payload(bundle, Some(repository), None, destination, source, receipt)
}

/// Attach standalone administration with an explicit local-origin path mapping.
///
/// Complete source local configuration remains immutable capture data. Only
/// safe declarative keys activate; hooks, helpers, includes, extensions and
/// source worktree paths never activate. The activation receipt names omissions.
/// The receipt directory becomes live Git administration and must be retained.
///
/// # Errors
/// Refuses payload divergence, occupied .git, invalid mapping or unknown origin.
pub fn attach_standalone_payload(
    bundle: &Path,
    destination: &Path,
    source: &str,
    receipt: &Path,
    origin_from: &Path,
    origin_to: &Path,
) -> Result<()> {
    attach_payload(
        bundle,
        None,
        Some((origin_from, origin_to)),
        destination,
        source,
        receipt,
    )
}

fn attach_payload(
    bundle: &Path,
    repository: Option<&Path>,
    mapping: Option<(&Path, &Path)>,
    destination: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let destination = fs::canonicalize(destination)?;
    let repository = repository.map(fs::canonicalize).transpose()?;
    let common = repository.as_deref().map(common_repository).transpose()?;
    let receipt_parent =
        fs::canonicalize(receipt.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?;
    let receipt = receipt_parent.join(
        receipt
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    if receipt.starts_with(&destination) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    require_missing(&destination.join(".git"))?;
    let root = fs::metadata(&destination)?;
    if !root.is_dir() || root.dev() != fs::metadata(&receipt_parent)?.dev() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let before = filesystem_rows(&destination)?;
    let (private, heads) = prepare_attachment(bundle, &destination, source, &receipt)?;
    let expected: Vec<crate::RowSchema> = postcard::from_bytes(&output(git(&private).args([
        "show",
        &format!("{}:value", capture_revision(&heads, "filesystem-v1")?),
    ]))?)
    .map_err(|_| BulkloadRefusal::FrameCodec)?;
    if !payload_shape_equal(&before, &expected) {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let comparison = receipt.join("comparison.index");
    output(snapshot_command(&private, &destination, &comparison).args(["read-tree", "--empty"]))?;
    let expected_tree = text(git(&private).args([
        "rev-parse",
        &format!("{}^{{tree}}", capture_revision(&heads, "worktree")?),
    ]))?;
    // The raw reader checks each file's identity before and after its one byte
    // pass. The outer census also rejects namespace or metadata changes; a
    // second full byte pass does not make this an atomic snapshot.
    if capture_tree(&private, &destination, &comparison)? != expected_tree
        || filesystem_rows(&destination)? != before
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    fs::write(
        receipt.join("original-payload-index-absent.postcard"),
        postcard::to_allocvec(&before).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    sync_private_tree(&receipt)?;
    fs::File::open(&receipt_parent)?.sync_all()?;
    let admin = if let Some(repository) = &repository {
        prepare_linked_attachment(repository, &destination, source, &receipt, &private, &heads)?
    } else {
        let (from, to) = mapping.ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        activate_standalone_configuration(&private, &heads, &receipt, Some((from, to)))?;
        private.clone()
    };
    let pointer = write_git_pointer(&receipt, &admin)?;
    sync_private_tree(&receipt)?;
    fs::File::open(&receipt_parent)?.sync_all()?;
    let current_root = fs::metadata(&destination)?;
    if root.dev() != current_root.dev()
        || root.ino() != current_root.ino()
        || filesystem_rows(&destination)? != before
        || repository
            .as_deref()
            .map(|repo| attachment_policy_matches(repo, &private, &heads))
            .transpose()?
            .is_some_and(|matches| !matches)
        || text(
            snapshot_command(&admin, &destination, &admin.join("index"))
                .args(["rev-parse", "HEAD"]),
        )? != capture_revision(&heads, "head")?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    // Atomic create-only publication cannot overwrite another writer's .git.
    fs::hard_link(pointer, destination.join(".git"))?;
    fs::File::open(&destination)?.sync_all()?;
    if filesystem_rows(&destination)? != before
        || &common_repository(&destination)? != common.as_ref().unwrap_or(&private)
        || repository
            .as_deref()
            .map(|repo| attachment_policy_matches(repo, &private, &heads))
            .transpose()?
            .is_some_and(|matches| !matches)
        || text(git(&destination).args(["rev-parse", "HEAD"]))? != capture_revision(&heads, "head")?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    if repository.is_some() {
        fs::remove_file(admin.join("locked"))?;
    }
    fs::File::open(admin)?.sync_all()?;
    Ok(())
}

fn write_git_pointer(receipt: &Path, admin: &Path) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let pointer = receipt.join("git-pointer");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pointer)?;
    let path = admin.as_os_str().as_bytes();
    if path.contains(&b'\n') || path.contains(&b'\r') {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    file.write_all(b"gitdir: ")?;
    file.write_all(path)?;
    file.write_all(b"\n")?;
    Ok(pointer)
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
    restore_bundle_configured(bundle, destination, source, None)
}

/// Restore an absent standalone checkout with explicit local-origin mapping.
///
/// Without a mapping, a captured HTTPS origin is retained unchanged; no origin
/// is invented when the source has none. Other origin schemes require a reviewed
/// mapping. Configuration omissions remain named in .git/carry-config receipts.
/// No network operation or captured executable configuration is activated.
///
/// # Errors
/// Refuses old captures lacking configuration, unsafe/unmapped origins, occupied
/// destinations, or any malformed filesystem/capture state.
pub fn restore_bundle_configured(
    bundle: &Path,
    destination: &Path,
    source: &str,
    mapping: Option<(&Path, &Path)>,
) -> Result<()> {
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
    if !shallow::is_custody(&heads) {
        capture_revision(&heads, "configuration-v1")?;
    }
    let format = bundle_object_format(&heads)?;
    output(git(&destination).args(["init", "--template=", &format!("--object-format={format}")]))?;
    import_bundle(&destination, &bundle, source)?;
    let heads = shallow::headers(&destination, &bundle)?;
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
    find("configuration-v1")?;
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
    restore_entries(&destination, &entries)?;
    let staged = find("staged")?;
    output(git(&destination).args(["read-tree", &format!("{staged}^{{tree}}")]))?;
    restore_filesystem_rows(&destination, &find("filesystem-v1")?)?;
    let config_receipt = destination.join(".git/carry-config");
    fs::DirBuilder::new().mode(0o700).create(&config_receipt)?;
    activate_standalone_configuration(&destination, &heads, &config_receipt, mapping)?;
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
    let heads = shallow::headers(&repository, &bundle)?;
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
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
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
    restore_entries(&destination, &entries)?;
    output(git(&destination).args(["read-tree", &format!("{}^{{tree}}", find("staged")?)]))?;
    restore_filesystem_rows(&destination, &find("filesystem-v1")?)?;
    let final_exclude = match fs::read(exclude_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if final_exclude != exclude {
        return Err(BulkloadRefusal::GitIgnorePolicyConflict);
    }
    if text(git(&destination).args(["rev-parse", "--verify", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(())
}

fn restore_entries(destination: &Path, entries: &[u8]) -> Result<()> {
    let mut objects = batch_objects::BatchObjects::new(destination)?;
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        restore_entry(destination, entry, &mut objects)?;
    }
    objects.finish()
}

fn restore_entry(
    destination: &Path,
    entry: &[u8],
    objects: &mut batch_objects::BatchObjects,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Component;
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
    if fields.next().is_some() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
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
            let mut target = Vec::new();
            objects.copy_into(value, &mut target, Some(65_536))?;
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), path)?;
        }
        "100644" | "100755" => {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            objects.copy_into(value, &mut file, None)?;
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
    fn standalone_attachment_maps_origin_and_retains_tracking_without_executable_config() {
        let root = std::env::temp_dir().join(format!("bulkload-config-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let upstream = root.join("upstream");
        for repo in [&source, &upstream] {
            fs::create_dir(repo).unwrap();
            output(git(repo).args(["init", "--template="])).unwrap();
        }
        output(git(&source).args(["config", "user.name", "Test"])).unwrap();
        output(git(&source).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(source.join("tracked"), b"base").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        let symbolic = text(git(&source).args(["symbolic-ref", "HEAD"])).unwrap();
        let branch = symbolic.strip_prefix("refs/heads/").unwrap();
        let from = Path::new("/Users/jess/git/legalab");
        output(git(&source).args(["config", "remote.origin.url"]).arg(from)).unwrap();
        output(git(&source).args([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]))
        .unwrap();
        output(git(&source).args(["config", &format!("branch.{branch}.remote"), "origin"]))
            .unwrap();
        output(git(&source).args(["config", &format!("branch.{branch}.merge"), &symbolic]))
            .unwrap();
        output(git(&source).args(["config", "credential.helper", "!unsafe-helper"])).unwrap();
        output(git(&source).args(["config", "alias.unsafe", "!unsafe-command"])).unwrap();
        fs::write(source.join("tracked"), b"staged").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"dirty").unwrap();
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let payload = root.join("payload");
        restore_bundle_configured(&bundle, &payload, "neo", Some((from, &upstream))).unwrap();
        fs::rename(payload.join(".git"), root.join("original-git")).unwrap();
        let before = filesystem_rows(&payload).unwrap();
        let receipt = root.join("receipt");
        attach_standalone_payload(&bundle, &payload, "neo", &receipt, from, &upstream).unwrap();
        assert_eq!(before, filesystem_rows(&payload).unwrap());
        assert_eq!(
            text(git(&payload).args(["config", "remote.origin.url"])).unwrap(),
            upstream.to_str().unwrap()
        );
        assert_eq!(
            text(git(&payload).args(["config", &format!("branch.{branch}.remote")])).unwrap(),
            "origin"
        );
        assert_eq!(
            text(git(&payload).args(["config", &format!("branch.{branch}.merge")])).unwrap(),
            symbolic
        );
        assert!(text(git(&payload).args(["config", "credential.helper"])).is_err());
        assert!(text(git(&payload).args(["config", "alias.unsafe"])).is_err());
        assert_eq!(
            fs::read(source.join(".git/config")).unwrap(),
            fs::read(receipt.join("source-config")).unwrap()
        );
        assert_eq!(
            output(git(&source).args(["diff", "--cached", "--binary"])).unwrap(),
            output(git(&payload).args(["diff", "--cached", "--binary"])).unwrap()
        );
        assert_eq!(
            output(git(&source).args(["diff", "--binary"])).unwrap(),
            output(git(&payload).args(["diff", "--binary"])).unwrap()
        );
        assert_https_restore(&root, &source);
        fs::remove_dir_all(root).unwrap();
    }

    fn assert_https_restore(root: &Path, source: &Path) {
        let origin =
            "https://github.com/Medical-Massage-Specialists/medical-massage-specialists-infra.git";
        output(git(source).args(["config", "remote.origin.url", origin])).unwrap();
        let capture = root.join("https-capture");
        let bundle = export_repository(source, &capture).unwrap();
        let destination = root.join("https-restored");
        restore_bundle(&bundle, &destination, "neo").unwrap();
        assert_eq!(
            text(git(&destination).args(["config", "remote.origin.url"])).unwrap(),
            origin
        );
        assert_eq!(
            text(git(&destination).args(["config", "remote.origin.fetch"])).unwrap(),
            "+refs/heads/*:refs/remotes/origin/*"
        );
        let symbolic = text(git(source).args(["symbolic-ref", "HEAD"])).unwrap();
        let branch = symbolic.strip_prefix("refs/heads/").unwrap();
        assert_eq!(
            text(git(&destination).args(["config", &format!("branch.{branch}.remote")])).unwrap(),
            "origin"
        );
        assert_eq!(
            text(git(&destination).args(["config", &format!("branch.{branch}.merge")])).unwrap(),
            symbolic
        );
        assert!(text(git(&destination).args(["config", "credential.helper"])).is_err());
        assert_eq!(
            output(git(source).args(["status", "--porcelain"])).unwrap(),
            output(git(&destination).args(["status", "--porcelain"])).unwrap()
        );
        for unsafe_origin in [
            "ext::bad",
            "https://user:secret@host/repo",
            "https://host/repo?token=secret",
            "/Users/jess/git/legalab",
        ] {
            assert!(!safe_https_origin(unsafe_origin));
        }
        let private = capture.join("repository.git");
        output(git(&private).args(["update-ref", "-d", "refs/carry-export/configuration-v1"]))
            .unwrap();
        let old_bundle = capture.join("old-format.bundle");
        output(
            git(&private)
                .args(["bundle", "create"])
                .arg(&old_bundle)
                .arg("--all"),
        )
        .unwrap();
        let old_destination = root.join("old-format-refused");
        assert!(restore_bundle(&old_bundle, &old_destination, "neo").is_err());
        assert!(!old_destination.join(".git").exists());
    }

    #[test]
    fn exact_payload_attachment_preserves_bytes_inodes_and_captured_staging() {
        use std::os::unix::fs::MetadataExt;
        let root = std::env::temp_dir().join(format!("bulkload-attach-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        output(git(&source).args(["init", "--template="])).unwrap();
        output(git(&source).args(["config", "user.name", "Test"])).unwrap();
        output(git(&source).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(source.join("tracked"), b"base\0binary").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        fs::write(source.join("tracked"), b"staged\0binary").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"unstaged\0binary").unwrap();
        fs::write(source.join("untracked"), b"kept").unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        let bundle = export_repository(&source, &root.join("capture")).unwrap();
        let payload = root.join("payload");
        restore_bundle(&bundle, &payload, "neo").unwrap();
        fs::rename(payload.join(".git"), root.join("old-private-git")).unwrap();
        let before = filesystem_rows(&payload).unwrap();
        let inode = fs::metadata(payload.join("tracked")).unwrap().ino();
        output(git(&source).args([
            "config",
            "remote.origin.url",
            "ssh://example.test/estate.git",
        ]))
        .unwrap();
        attach_matching_payload(&bundle, &source, &payload, "neo", &root.join("receipt")).unwrap();
        assert_eq!(
            common_repository(&source).unwrap(),
            common_repository(&payload).unwrap()
        );
        assert_eq!(
            text(git(&payload).args(["config", "remote.origin.url"])).unwrap(),
            "ssh://example.test/estate.git"
        );
        assert!(text(git(&source).args(["worktree", "list", "--porcelain"]))
            .unwrap()
            .contains(payload.to_str().unwrap()));
        assert_eq!(before, filesystem_rows(&payload).unwrap());
        assert_eq!(inode, fs::metadata(payload.join("tracked")).unwrap().ino());
        for args in [
            vec!["status", "--porcelain"],
            vec!["diff", "--cached", "--binary"],
            vec!["diff", "--binary"],
        ] {
            assert_eq!(
                output(git(&source).args(&args)).unwrap(),
                output(git(&payload).args(&args)).unwrap()
            );
        }
        assert!(attach_matching_payload(
            &bundle,
            &source,
            &payload,
            "neo",
            &root.join("occupied-receipt")
        )
        .is_err());
        let different = root.join("different");
        restore_bundle(&bundle, &different, "neo").unwrap();
        fs::rename(different.join(".git"), root.join("other-private-git")).unwrap();
        fs::write(different.join("extra"), b"destination-only").unwrap();
        assert!(attach_matching_payload(
            &bundle,
            &source,
            &different,
            "neo",
            &root.join("different-receipt")
        )
        .is_err());
        assert!(!different.join(".git").exists());
        fs::remove_file(different.join("extra")).unwrap();
        fs::write(different.join("tracked"), b"different\0bytes").unwrap();
        assert!(attach_matching_payload(
            &bundle,
            &source,
            &different,
            "neo",
            &root.join("bytes-receipt")
        )
        .is_err());
        assert!(!different.join(".git").exists());
        fs::remove_dir_all(root).unwrap();
    }

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
        output(git(&source).args(["branch", "quote\"branch"])).unwrap();
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
            // Restoration must flush final modes even when the result is readonly.
            fs::set_permissions(source.join("ignored"), fs::Permissions::from_mode(0o400)).unwrap();
            fs::create_dir_all(source.join("empty/nested")).unwrap();
            fs::set_permissions(source.join("empty"), fs::Permissions::from_mode(0o500)).unwrap();
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
        assert!(
            text(git(&source).args(["bundle", "list-heads"]).arg(&bundle))
                .unwrap()
                .contains("refs/carry-export/refs/heads/quote\"branch")
        );
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
        let staged_oid = text(git(&dest).args(["rev-parse", staged])).unwrap();
        let conflicting_oid = text(git(&dest).args(["rev-parse", "HEAD"])).unwrap();
        output(git(&dest).args(["update-ref", staged, &conflicting_oid, &staged_oid])).unwrap();
        let before_refusal = refs(&dest).unwrap();
        assert_eq!(
            import_bundle(&dest, &bundle, "neo"),
            Err(BulkloadRefusal::GitDestinationOccupied)
        );
        assert_eq!(refs(&dest).unwrap(), before_refusal);
        output(git(&dest).args(["update-ref", staged, &staged_oid, &conflicting_oid])).unwrap();
        output(git(&dest).args(["update-ref", "-d", staged, &staged_oid])).unwrap();
        let native_missing = "refs/heads/must-stay-absent";
        output(git(&dest).args(["symbolic-ref", staged, native_missing])).unwrap();
        assert!(import_bundle(&dest, &bundle, "neo").is_err());
        assert!(text(git(&dest).args(["rev-parse", "--verify", native_missing])).is_err());
        assert_eq!(
            text(git(&dest).args(["symbolic-ref", staged])).unwrap(),
            native_missing
        );
        output(git(&dest).args(["update-ref", "--no-deref", "-d", staged])).unwrap();
        set_ref(&dest, staged, &staged_oid).unwrap();
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
        let exclude_path = dest.join(".git/info/exclude");
        let original_exclude = fs::read(&exclude_path).unwrap_or_default();
        fs::create_dir_all(dest.join(".git/info")).unwrap();
        fs::write(&exclude_path, b"operator-local-policy\n").unwrap();
        assert_eq!(
            restore_linked(&bundle, &dest, &linked, "neo"),
            Err(BulkloadRefusal::GitIgnorePolicyConflict)
        );
        assert!(!linked.exists());
        assert_eq!(fs::read(&exclude_path).unwrap(), b"operator-local-policy\n");
        assert_eq!(dest_index, fs::read(dest.join(".git/index")).unwrap());
        assert_eq!(dest_head, fs::read(dest.join(".git/HEAD")).unwrap());
        fs::write(&exclude_path, original_exclude).unwrap();
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
                    0o400
                );
                assert_eq!(
                    fs::metadata(target.join("empty"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o500
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
        let recorded_gitdir = PathBuf::from(
            fs::read_to_string(Path::new(&admin).join("gitdir"))
                .unwrap()
                .trim(),
        );
        assert_eq!(
            fs::canonicalize(&recorded_gitdir).unwrap(),
            fs::canonicalize(linked.join(".git")).unwrap()
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
        // Only the test-owned trees regain write permission for fixture cleanup.
        for target in [&source, &restored, &linked] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target.join("empty"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn committed_repository(repo: &Path, content: &[u8]) {
        fs::create_dir_all(repo).unwrap();
        output(git(repo).args(["init", "--template="])).unwrap();
        output(git(repo).args(["config", "user.name", "Test"])).unwrap();
        output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(repo.join("tracked"), content).unwrap();
        output(git(repo).args(["add", "."])).unwrap();
        output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
    }

    // Regression for the lab capture refusal: `.claude/worktrees/<name>/.git`
    // is a gitdir pointer for a registered linked worktree of the same
    // repository (Claude Code's EnterWorktree convention). It is custody, not
    // malformed inventory, and its bytes belong to its own estate item.
    #[test]
    fn registered_worktree_nested_inside_the_checkout_is_typed_custody() {
        let root =
            std::env::temp_dir().join(format!("bulkload-nested-worktree-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("lab");
        committed_repository(&source, b"lab");
        let nested = source.join(".claude/worktrees/agent-x");
        fs::create_dir_all(source.join(".claude/worktrees")).unwrap();
        fs::write(source.join(".claude/settings.json"), b"{}").unwrap();
        output(
            git(&source)
                .args(["worktree", "add", "-b", "agent-x"])
                .arg(&nested),
        )
        .unwrap();
        assert!(nested.join(".git").is_file());
        fs::write(nested.join("agent-only"), b"belongs to the nested item").unwrap();
        let nested_head = text(git(&nested).args(["rev-parse", "--verify", "HEAD"])).unwrap();

        let custody = nested_worktrees(&source).unwrap();
        assert_eq!(
            custody,
            vec![NestedWorktree {
                rel_path: b".claude/worktrees/agent-x".to_vec(),
                worktree_name: "agent-x".to_owned(),
                head_oid: nested_head,
            }]
        );
        let key = reusable_capture_key(&source).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        // Nested payload is not this item's census; its own HEAD is.
        fs::write(nested.join("agent-only"), b"changed nested bytes").unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        output(git(&nested).args(["add", "."])).unwrap();
        output(git(&nested).args(["-c", "commit.gpgsign=false", "commit", "-m", "agent"])).unwrap();
        assert_ne!(key, reusable_capture_key(&source).unwrap());
        let key = reusable_capture_key(&source).unwrap();

        let capture = root.join("capture");
        let bundle = export_repository(&source, &capture).unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        let private = capture.join("repository.git");
        let manifest: Vec<NestedWorktree> = postcard::from_bytes(
            &output(git(&private).args([
                "show",
                &format!("refs/carry-export/{NESTED_WORKTREES_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest,
            vec![NestedWorktree {
                rel_path: b".claude/worktrees/agent-x".to_vec(),
                worktree_name: "agent-x".to_owned(),
                head_oid: text(git(&nested).args(["rev-parse", "--verify", "HEAD"])).unwrap(),
            }]
        );
        let seats: Vec<crate::RowSchema> = postcard::from_bytes(
            &output(git(&private).args(["show", "refs/carry-export/filesystem-v1:value"])).unwrap(),
        )
        .unwrap();
        let paths: Vec<&[u8]> = seats.iter().map(|row| row.rel_path.as_slice()).collect();
        assert!(paths.contains(&b".claude".as_slice()));
        assert!(paths.contains(&b".claude/settings.json".as_slice()));
        assert!(paths.contains(&b".claude/worktrees".as_slice()));
        assert!(!paths
            .iter()
            .any(|path| path.starts_with(b".claude/worktrees/agent-x")));
        let carried = output(git(&private).args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/carry-export/worktree",
        ]))
        .unwrap();
        assert!(!String::from_utf8(carried).unwrap().contains("agent-x"));

        // A restore of the enclosing item leaves the nested root absent for the
        // nested item's own restore; nothing of the worktree was invented.
        let restored = root.join("restored");
        restore_bundle(&bundle, &restored, "neo").unwrap();
        assert!(restored.join(".claude/worktrees").is_dir());
        assert!(!restored.join(".claude/worktrees/agent-x").exists());
        assert_eq!(fs::read(restored.join("tracked")).unwrap(), b"lab");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_independent_repository_still_refuses() {
        let root = std::env::temp_dir().join(format!(
            "bulkload-nested-independent-{}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("outer");
        committed_repository(&source, b"outer");
        committed_repository(&source.join("vendor/inner"), b"inner");
        assert!(source.join("vendor/inner/.git").is_dir());
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            export_repository(&source, &root.join("capture")),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            nested_worktrees(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pointer_to_a_worktree_of_a_different_repository_still_refuses() {
        let root =
            std::env::temp_dir().join(format!("bulkload-nested-foreign-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("outer");
        let other = root.join("other");
        committed_repository(&source, b"outer");
        committed_repository(&other, b"other");
        // A real registered worktree, but of `other`, placed inside `source`.
        let foreign = source.join(".claude/worktrees/foreign");
        fs::create_dir_all(source.join(".claude/worktrees")).unwrap();
        output(
            git(&other)
                .args(["worktree", "add", "-b", "foreign"])
                .arg(&foreign),
        )
        .unwrap();
        assert!(foreign.join(".git").is_file());
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        assert_eq!(
            export_repository(&source, &root.join("capture-foreign")),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        // A pointer file that is not a registered worktree of anything.
        let stray = source.join("stray");
        fs::create_dir(&stray).unwrap();
        fs::write(
            stray.join(".git"),
            format!(
                "gitdir: {}\n",
                source.join(".git/worktrees/missing").display()
            ),
        )
        .unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(&stray).unwrap();
        // A symlink named .git is never custody.
        let linked = source.join("linked");
        fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(source.join(".git"), linked.join(".git")).unwrap();
        assert_eq!(
            reusable_capture_key(&source),
            Err(BulkloadRefusal::GitInventoryMalformed)
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn rebuildable_fixture(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        output(git(repo).args(["init", "--template="])).unwrap();
        output(git(repo).args(["config", "user.name", "Test"])).unwrap();
        output(git(repo).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(repo.join("tracked"), b"source of truth").unwrap();
        fs::write(repo.join(".gitignore"), b"/target\n/node_modules\n").unwrap();
        output(git(repo).args(["add", "."])).unwrap();
        output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        // An ignored file outside the rebuildable set is still carried: the
        // ruling keeps untracked AND ignored carry, minus a fixed list.
        fs::write(repo.join("ignored-but-carried"), b"operator state").unwrap();
    }

    fn rebuildable_repository(repo: &Path) {
        rebuildable_fixture(repo);
        fs::create_dir_all(repo.join("target/debug/incremental")).unwrap();
        fs::write(
            repo.join("target/debug/incremental/artifact"),
            vec![7u8; 4096],
        )
        .unwrap();
        fs::write(repo.join("target/.rustc_info.json"), b"{\"rustc\":0}").unwrap();
        fs::create_dir_all(repo.join("crates/inner/node_modules/left-pad")).unwrap();
        fs::write(
            repo.join("crates/inner/node_modules/left-pad/index.js"),
            vec![b'x'; 512],
        )
        .unwrap();
        fs::write(repo.join("crates/inner/kept.rs"), b"carried").unwrap();
    }

    fn sidecar_present(private: &Path) -> bool {
        git(private)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/carry-export/{REBUILDABLE_METADATA}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    fn carried_paths(private: &Path) -> String {
        String::from_utf8(
            output(git(private).args([
                "ls-tree",
                "-r",
                "--name-only",
                "refs/carry-export/worktree",
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn manifest_paths(private: &Path) -> Vec<Vec<u8>> {
        let seats: Vec<crate::RowSchema> = postcard::from_bytes(
            &output(git(private).args(["show", "refs/carry-export/filesystem-v1:value"])).unwrap(),
        )
        .unwrap();
        seats.into_iter().map(|row| row.rel_path).collect()
    }

    // The measured failure this change exists for: a capture of a repository
    // holding target/ and node_modules/ carried 34 GB of rebuildable bytes and
    // was then refused outright because cargo rewrote one of them mid-pass.
    #[test]
    fn rebuildable_roots_are_omitted_recorded_and_never_carried() {
        let root =
            std::env::temp_dir().join(format!("bulkload-rebuildable-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_repository(&source);

        // (b) Custody names each omitted root and its size, before any capture.
        let custody = rebuildable_omissions(&source).unwrap();
        assert_eq!(
            custody
                .iter()
                .map(|row| row.rel_path.clone())
                .collect::<Vec<_>>(),
            vec![b"crates/inner/node_modules".to_vec(), b"target".to_vec()]
        );
        let sizes: Vec<(u64, u64)> = custody.iter().map(|row| (row.bytes, row.entries)).collect();
        assert_eq!(sizes.first(), Some(&(512, 2)));
        assert_eq!(sizes.get(1).map(|row| row.0), Some(4096 + 11));
        assert!(sizes.get(1).is_some_and(|row| row.1 >= 4));

        // Rebuildable churn no longer moves the key. This is exactly the write
        // that refused tonight's capture with GIT_AUTHORITY_CHANGED.
        let key = reusable_capture_key(&source).unwrap();
        fs::write(
            source.join("target/.rustc_info.json"),
            b"{\"rustc\":1,\"x\":2}",
        )
        .unwrap();
        fs::write(source.join("target/debug/incremental/fresh"), b"mid-pass").unwrap();
        assert_eq!(key, reusable_capture_key(&source).unwrap());

        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        let private = capture.join("repository.git");

        // (a) None of those bytes are carried, and no seat names them.
        let carried = carried_paths(&private);
        assert!(carried.contains("tracked"));
        assert!(carried.contains("ignored-but-carried"));
        assert!(carried.contains("crates/inner/kept.rs"));
        assert!(!carried.contains("target"));
        assert!(!carried.contains("node_modules"));
        let paths = manifest_paths(&private);
        assert!(paths.contains(&b"crates/inner".to_vec()));
        assert!(paths.contains(&b"ignored-but-carried".to_vec()));
        assert!(!paths
            .iter()
            .any(|path| path.starts_with(b"target")
                || path.windows(12).any(|w| w == b"node_modules")));

        // (b) The bundle's own sidecar carries the same custody.
        let manifest: Vec<RebuildableOmission> = postcard::from_bytes(
            &output(git(&private).args([
                "show",
                &format!("refs/carry-export/{REBUILDABLE_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest, export.omitted);
        assert_eq!(
            manifest
                .iter()
                .map(|row| row.rel_path.clone())
                .collect::<Vec<_>>(),
            vec![b"crates/inner/node_modules".to_vec(), b"target".to_vec()]
        );
        assert!(manifest.iter().all(|row| row.bytes > 0));

        // A restore leaves the rebuildable roots absent; `cargo build` remakes them.
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert_eq!(
            fs::read(restored.join("tracked")).unwrap(),
            b"source of truth"
        );
        assert_eq!(
            fs::read(restored.join("ignored-but-carried")).unwrap(),
            b"operator state"
        );
        assert!(!restored.join("target").exists());
        assert!(!restored.join("crates/inner/node_modules").exists());
        assert!(restored.join("crates/inner/kept.rs").is_file());

        fs::remove_dir_all(root).unwrap();
    }

    // (d) --include-rebuildable opts back in to full fidelity: every omitted
    // byte is carried again and no custody sidecar is written.
    #[test]
    fn include_rebuildable_carries_the_whole_rebuildable_set() {
        let root = std::env::temp_dir().join(format!("bulkload-full-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_repository(&source);
        let capture = root.join("capture");
        let export = export_repository_with_policy(
            &source,
            &capture,
            None,
            CapturePolicy::including_rebuildable(),
        )
        .unwrap();
        assert!(export.omitted.is_empty());
        let private = capture.join("repository.git");
        let carried = carried_paths(&private);
        assert!(carried.contains("target/.rustc_info.json"));
        assert!(carried.contains("target/debug/incremental/artifact"));
        assert!(carried.contains("crates/inner/node_modules/left-pad/index.js"));
        assert!(!sidecar_present(&private));
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert_eq!(
            fs::read(restored.join("crates/inner/node_modules/left-pad/index.js")).unwrap(),
            vec![b'x'; 512]
        );
        fs::remove_dir_all(root).unwrap();
    }

    // A name on the list is only rebuildable when Git tracks nothing beneath
    // it. A repository that really does track `target/...` keeps every byte.
    #[test]
    fn a_tracked_rebuildable_name_is_still_carried_in_full() {
        let root =
            std::env::temp_dir().join(format!("bulkload-tracked-target-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("target")).unwrap();
        fs::create_dir_all(source.join("venv")).unwrap();
        rebuildable_fixture(&source);
        fs::write(
            source.join("target/committed.txt"),
            b"this repo tracks target",
        )
        .unwrap();
        fs::write(source.join("venv/scratch"), b"nothing tracked here").unwrap();
        output(git(&source).args(["add", "-f", "target/committed.txt"])).unwrap();
        output(git(&source).args(["-c", "commit.gpgsign=false", "commit", "-m", "target"]))
            .unwrap();

        let custody = rebuildable_omissions(&source).unwrap();
        assert_eq!(
            custody
                .iter()
                .map(|row| row.rel_path.clone())
                .collect::<Vec<_>>(),
            vec![b"venv".to_vec()]
        );
        let capture = root.join("capture");
        let export =
            export_repository_with_policy(&source, &capture, None, CapturePolicy::default())
                .unwrap();
        let carried = carried_paths(&capture.join("repository.git"));
        assert!(carried.contains("target/committed.txt"));
        assert!(!carried.contains("venv/scratch"));
        assert_eq!(export.omitted.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    // (c) A repository with nothing rebuildable in it hashes and encodes
    // exactly as it did before omission existed: the omission sidecar is only
    // hashed and only written when it is non-empty, and the full-fidelity
    // policy is byte-for-byte the pre-change code path. No RowSchema field or
    // variant was added, so every retained capture key stays valid.
    #[test]
    fn a_repository_without_rebuildable_roots_keeps_its_exact_capture_key() {
        let root = std::env::temp_dir().join(format!("bulkload-unchanged-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        rebuildable_fixture(&source);
        fs::create_dir_all(source.join("src/build")).unwrap();
        fs::write(
            source.join("src/build/kept"),
            b"build and dist are not on the list",
        )
        .unwrap();
        fs::create_dir_all(source.join("dist")).unwrap();
        fs::write(source.join("dist/kept"), b"carried").unwrap();
        // A bazel convenience symlink is one row and is never descended.
        std::os::unix::fs::symlink("/nonexistent/output-base/out", source.join("bazel-out"))
            .unwrap();

        assert!(rebuildable_omissions(&source).unwrap().is_empty());
        assert_eq!(
            reusable_capture_key(&source).unwrap(),
            reusable_capture_key_with_policy(&source, CapturePolicy::including_rebuildable())
                .unwrap()
        );
        let common = common_repository(&source).unwrap();
        let census = capture_census(&source, &common, CapturePolicy::default()).unwrap();
        assert!(census.nested_worktrees.is_empty());
        assert!(census.omitted.is_empty());
        assert_eq!(census.rows, filesystem_rows(&source).unwrap());
        assert_eq!(
            postcard::to_allocvec(&census.rows).unwrap(),
            postcard::to_allocvec(&filesystem_rows(&source).unwrap()).unwrap()
        );

        let export = export_repository_with_policy(
            &source,
            &root.join("capture"),
            None,
            CapturePolicy::default(),
        )
        .unwrap();
        assert!(export.omitted.is_empty());
        let private = root.join("capture/repository.git");
        // No sidecar ref exists at all, so the bundle is the one it always was.
        assert!(!sidecar_present(&private));
        let carried = carried_paths(&private);
        assert!(carried.contains("src/build/kept"));
        assert!(carried.contains("dist/kept"));
        assert!(carried.contains("bazel-out"));
        fs::remove_dir_all(root).unwrap();
    }

    // ---- drift tolerance (R25, bulkload #34; rulings R-N28/R-N29/R-N30) ----

    fn drift_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("bulkload-drift-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        committed_repository(&source, b"tracked bytes at census");
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"ignored but carried").unwrap();
        fs::create_dir(source.join("dir")).unwrap();
        fs::write(source.join("dir/untracked"), b"untracked payload").unwrap();
        (root, source)
    }

    fn drift_ref_present(private: &Path) -> bool {
        git(private)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/carry-export/{CAPTURE_DRIFT_METADATA}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    fn drift_sidecar(private: &Path) -> CaptureDrift {
        postcard::from_bytes(
            &output(git(private).args([
                "show",
                &format!("refs/carry-export/{CAPTURE_DRIFT_METADATA}:value"),
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn row(kind: DriftKind, name: &[u8]) -> DriftRow {
        DriftRow {
            kind,
            name: name.to_vec(),
        }
    }

    // Refusal #2 of 9/21-22: four sibling-worktree lanes created branches in
    // the shared ref store while a capture was in flight.
    #[test]
    fn refs_created_elsewhere_during_capture_are_drift_not_refusal() {
        let (root, source) = drift_fixture("refs-elsewhere");
        let head = text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap();
        let index_before = fs::read(source.join(".git/index")).unwrap();
        let (lane, tip) = (source.clone(), head.clone());
        mid_pass::arm(&source, move || {
            for number in 0..4 {
                output(git(&lane).args(["update-ref", &format!("refs/heads/lane-{number}"), &tip]))
                    .unwrap();
            }
        });
        let capture = root.join("capture");
        let export =
            export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
        assert_eq!(
            export.drift.rows,
            (0..4)
                .map(|number| row(
                    DriftKind::RefAdded,
                    format!("refs/heads/lane-{number}").as_bytes()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            head,
            text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap()
        );
        assert_eq!(index_before, fs::read(source.join(".git/index")).unwrap());
        output(git(&source).args(["bundle", "verify"]).arg(&export.bundle)).unwrap();
        let private = capture.join("repository.git");
        assert_eq!(drift_sidecar(&private), export.drift);
        // The lanes' refs were not in the captured inventory and are not in the bundle.
        let heads = text(
            git(&source)
                .args(["bundle", "list-heads"])
                .arg(&export.bundle),
        )
        .unwrap();
        assert!(!heads.contains("lane-"));
        // The pre-drift contract is untouched for callers that never asked for tolerance.
        let again = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&again).args(["update-ref", "refs/heads/lane-4", "HEAD"])).unwrap();
        });
        assert!(matches!(
            export_repository(&source, &root.join("strict")),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // Refusal #1 of 9/21-22: cargo rewrote .rustc_info.json between the census
    // and that seat's byte pass.
    #[test]
    fn a_file_rewritten_during_the_raw_pass_is_drift_not_refusal() {
        let (root, source) = drift_fixture("file-rewritten");
        let rewritten = source.join("tracked");
        mid_pass::arm(&source, move || {
            fs::write(&rewritten, b"{\"rustc\":1,\"rewritten\":true}").unwrap();
        });
        let capture = root.join("capture");
        let export =
            export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
        assert_eq!(
            export.drift.rows,
            vec![row(DriftKind::SeatChanged, b"tracked")]
        );
        let private = capture.join("repository.git");
        // Absent from the tree and from filesystem-v1: the drift list is the
        // statement that the seat's bytes are not here (R-N28).
        assert!(!carried_paths(&private)
            .lines()
            .any(|path| path == "tracked"));
        assert!(!manifest_paths(&private).contains(&b"tracked".to_vec()));
        assert!(carried_paths(&private).contains("dir/untracked"));
        assert!(manifest_paths(&private).contains(&b"ignored".to_vec()));
        output(git(&private).args(["fsck", "--strict", "--no-dangling"])).unwrap();
        output(git(&source).args(["bundle", "verify"]).arg(&export.bundle)).unwrap();
        assert_eq!(drift_sidecar(&private), export.drift);
        fs::remove_dir_all(root).unwrap();
    }

    // The operator-requested test: a build truncating a tracked file, an agent
    // creating a new file, and a lane creating a branch, all under one pass.
    #[test]
    fn concurrent_ref_creation_and_file_mutation_during_one_capture_succeed_with_drift() {
        let (root, source) = drift_fixture("concurrent");
        let inside = source.clone();
        mid_pass::arm(&source, move || {
            fs::write(inside.join("tracked"), b"").unwrap();
            fs::write(inside.join("dir/appeared"), b"created mid-pass").unwrap();
            output(git(&inside).args(["update-ref", "refs/heads/concurrent-lane", "HEAD"]))
                .unwrap();
        });
        let capture = root.join("capture");
        let export =
            export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
        assert_eq!(
            export.drift.rows,
            vec![
                row(DriftKind::RefAdded, b"refs/heads/concurrent-lane"),
                row(DriftKind::SeatAdded, b"dir/appeared"),
                row(DriftKind::SeatChanged, b"tracked"),
            ]
        );
        assert_eq!(export.drift.len(), 3);
        output(git(&source).args(["bundle", "verify"]).arg(&export.bundle)).unwrap();
        // Capture never writes the source: it is exactly its post-mutation state.
        assert_eq!(fs::read(source.join("tracked")).unwrap(), b"");
        assert_eq!(
            fs::read(source.join("dir/appeared")).unwrap(),
            b"created mid-pass"
        );
        assert_eq!(
            text(git(&source).args(["rev-parse", "--verify", "refs/heads/concurrent-lane"]))
                .unwrap(),
            text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap()
        );
        let private = capture.join("repository.git");
        let carried = carried_paths(&private);
        assert!(!carried.lines().any(|path| path == "tracked"));
        assert!(!carried.contains("appeared"));
        assert!(carried.contains("dir/untracked"));
        // A restore of this bundle materializes exactly what was captured and
        // nothing the pass could not vouch for.
        let restored = root.join("restored");
        restore_bundle(&export.bundle, &restored, "neo").unwrap();
        assert!(!restored.join("tracked").exists());
        assert!(!restored.join("dir/appeared").exists());
        assert_eq!(
            fs::read(restored.join("dir/untracked")).unwrap(),
            b"untracked payload"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // (a)/(b) tolerance must not leak into HEAD (R-N30).
    #[test]
    fn head_moving_during_capture_still_refuses() {
        let (root, source) = drift_fixture("head-moves");
        let inside = source.clone();
        mid_pass::arm(&source, move || {
            output(git(&inside).args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "committed mid-pass",
            ]))
            .unwrap();
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // Separability from the index (R-N30): HEAD pinned, index rewritten.
    #[test]
    fn index_rewritten_during_capture_still_refuses() {
        let (root, source) = drift_fixture("index-rewritten");
        let head = text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap();
        let inside = source.clone();
        mid_pass::arm(&source, move || {
            fs::write(inside.join("staged-mid-pass"), b"added to the index").unwrap();
            output(git(&inside).args(["add", "staged-mid-pass"])).unwrap();
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        assert_eq!(
            head,
            text(git(&source).args(["rev-parse", "--verify", "HEAD"])).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    // The Census split holds: a nested worktree's HEAD is custody, not seats,
    // and it moving under the pass is still a refusal.
    #[test]
    fn nested_worktree_head_moving_during_capture_still_refuses() {
        let (root, source) = drift_fixture("nested-head");
        let nested = source.join(".claude/worktrees/agent-x");
        fs::create_dir_all(source.join(".claude/worktrees")).unwrap();
        output(
            git(&source)
                .args(["worktree", "add", "-b", "agent-x"])
                .arg(&nested),
        )
        .unwrap();
        assert_eq!(nested_worktrees(&source).unwrap().len(), 1);
        let inside = nested;
        mid_pass::arm(&source, move || {
            output(git(&inside).args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "agent committed mid-pass",
            ]))
            .unwrap();
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // The no-invalidation proof for every retained bundle: a clean capture has
    // exactly the ref set it had before drift tolerance existed, no drift ref,
    // and a deterministic shape across passes.
    #[test]
    fn a_clean_capture_emits_no_drift_ref_and_keeps_the_pre_change_bundle_shape() {
        let (root, source) = drift_fixture("clean");
        let symbolic = text(git(&source).args(["symbolic-ref", "HEAD"])).unwrap();
        let mut expected: Vec<String> = [
            "configuration-v1",
            "exclude",
            "filesystem-v1",
            "head",
            "head-symbolic",
            "staged",
            "worktree",
        ]
        .iter()
        .map(|name| format!("refs/carry-export/{name}"))
        .chain(std::iter::once(format!("refs/carry-export/{symbolic}")))
        .collect();
        expected.sort();
        let key = reusable_capture_key(&source).unwrap();
        let mut shapes = Vec::new();
        for pass in ["first", "second"] {
            let capture = root.join(pass);
            let export =
                export_repository_with_drift(&source, &capture, &ExportOptions::default()).unwrap();
            assert!(export.drift.is_empty());
            let private = capture.join("repository.git");
            assert!(!drift_ref_present(&private));
            let names: Vec<String> = refs(&private)
                .unwrap()
                .lines()
                .map(|line| line.split_once(' ').unwrap().1.to_owned())
                .collect();
            assert_eq!(names, expected);
            let heads: Vec<String> = text(
                git(&source)
                    .args(["bundle", "list-heads"])
                    .arg(&export.bundle),
            )
            .unwrap()
            .lines()
            .map(|line| line.split_once(' ').unwrap().1.to_owned())
            .collect();
            assert_eq!(heads, expected);
            shapes.push((
                text(git(&private).args(["rev-parse", "refs/carry-export/worktree^{tree}"]))
                    .unwrap(),
                output(git(&private).args(["show", "refs/carry-export/filesystem-v1:value"]))
                    .unwrap(),
                export.bytes_read,
            ));
        }
        assert_eq!(shapes.first(), shapes.last());
        assert_eq!(key, reusable_capture_key(&source).unwrap());
        assert_eq!(
            key,
            legacy::reusable_capture_key_with_policy(&source, CapturePolicy::default()).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Unbounded drift is a rebuild, never a silently truncated drift list.
    #[test]
    fn drift_beyond_the_row_budget_refuses() {
        let (root, source) = drift_fixture("budget");
        fs::create_dir(source.join("burst")).unwrap();
        let burst = source.join("burst");
        mid_pass::arm(&source, move || {
            for number in 0..=DRIFT_ROW_LIMIT {
                fs::File::create(burst.join(format!("{number}"))).unwrap();
            }
        });
        assert!(matches!(
            export_repository_with_drift(&source, &root.join("capture"), &ExportOptions::default()),
            Err(BulkloadRefusal::BudgetExceeded)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    // ENOENT is drift; EACCES is a real IO fault that tolerance must not swallow.
    #[test]
    fn a_seat_deleted_mid_pass_is_drift_but_an_unreadable_seat_still_refuses() {
        use std::os::unix::fs::PermissionsExt;
        let (root, source) = drift_fixture("deleted-vs-unreadable");
        fs::write(source.join("doomed"), b"gone before its byte pass").unwrap();
        fs::write(source.join("locked"), b"unreadable before its byte pass").unwrap();
        let doomed = source.join("doomed");
        mid_pass::arm(&source, move || fs::remove_file(&doomed).unwrap());
        let export =
            export_repository_with_drift(&source, &root.join("deleted"), &ExportOptions::default())
                .unwrap();
        assert_eq!(
            export.drift.rows,
            vec![row(DriftKind::SeatRemoved, b"doomed")]
        );
        assert!(!manifest_paths(&root.join("deleted/repository.git")).contains(&b"doomed".to_vec()));
        let locked = source.join("locked");
        let lock = locked.clone();
        mid_pass::arm(&source, move || {
            fs::set_permissions(&lock, fs::Permissions::from_mode(0o000)).unwrap();
        });
        let refused = export_repository_with_drift(
            &source,
            &root.join("unreadable"),
            &ExportOptions::default(),
        );
        assert!(matches!(refused, Err(BulkloadRefusal::Io(Some(_)))));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod legacy {
    //! The reusable-key derivation exactly as it was before drift tolerance
    //! (PR #597 head, 7b017af), kept verbatim so tests can prove that
    //! `KeyParts::digest` is bit-identical and retained captures stay reused.
    use super::{
        capture_census, common_repository, git, output, refs, shallow, source_configuration,
        source_index, text, CapturePolicy, NESTED_WORKTREES_DOMAIN, REBUILDABLE_DOMAIN,
    };
    use crate::{BulkloadRefusal, Result};
    use std::fs;
    use std::path::Path;

    pub fn reusable_capture_key_with_policy(
        repo: &Path,
        policy: CapturePolicy,
    ) -> Result<[u8; 32]> {
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
        let census = capture_census(&repo, &common, policy)?;
        let rows = postcard::to_allocvec(&census.rows).map_err(|_| BulkloadRefusal::FrameCodec)?;
        let configuration = postcard::to_allocvec(&source_configuration(&repo)?)
            .map_err(|_| BulkloadRefusal::FrameCodec)?;
        let boundary = shallow::frontier(&repo)?;
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
            &configuration,
            &boundary,
        ] {
            hash.update(
                &u64::try_from(bytes.len())
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                    .to_le_bytes(),
            );
            hash.update(bytes);
        }
        if !census.nested_worktrees.is_empty() {
            let nested = postcard::to_allocvec(&census.nested_worktrees)
                .map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(NESTED_WORKTREES_DOMAIN);
            hash.update(
                &u64::try_from(nested.len())
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                    .to_le_bytes(),
            );
            hash.update(&nested);
        }
        if !census.omitted.is_empty() {
            let omitted =
                postcard::to_allocvec(&census.omitted).map_err(|_| BulkloadRefusal::FrameCodec)?;
            hash.update(REBUILDABLE_DOMAIN);
            hash.update(
                &u64::try_from(omitted.len())
                    .map_err(|_| BulkloadRefusal::BudgetExceeded)?
                    .to_le_bytes(),
            );
            hash.update(&omitted);
        }
        for directory in [&repo, &common] {
            let identity = crate::freshness::StatIdentity::from_metadata(&fs::metadata(directory)?);
            for value in [i128::from(identity.dev), i128::from(identity.ino)] {
                hash.update(&value.to_le_bytes());
            }
        }
        Ok(*hash.finalize().as_bytes())
    }
}
