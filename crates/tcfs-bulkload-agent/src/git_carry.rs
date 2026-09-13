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
        set_ref(private, &format!("refs/carry-export/{name}"), value)?;
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
/// Repeats are idempotent; differing captures remain separately reachable.
///
/// # Errors
/// Refuses invalid source names, non-export bundle refs, collisions or invalid
/// bundles. A failed fetch may leave unreachable objects, never changed HEAD.
pub fn import_bundle(repo: &Path, bundle: &Path, source: &str) -> Result<usize> {
    use std::io::Read;
    if source.is_empty()
        || source.len() > 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let bundle = fs::canonicalize(bundle)?;
    let mut file = fs::File::open(&bundle)?;
    let mut hasher = blake3::Hasher::new();
    let mut bytes = vec![0_u8; 65_536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hasher.update(
            bytes
                .get(..count)
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?,
        );
    }
    let digest = hasher.finalize().to_hex();
    output(git(repo).args(["bundle", "verify"]).arg(&bundle))?;
    let heads = text(git(repo).args(["bundle", "list-heads"]).arg(&bundle))?;
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
        names.push((
            value.to_owned(),
            name.to_owned(),
            format!("refs/carry/{source}/{digest}/{suffix}"),
        ));
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
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
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
            output(git(repo).args(["add", "."])).unwrap();
            output(git(repo).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])).unwrap();
        }
        for content in [b"stash-one", b"stash-two"] {
            fs::write(source.join("tracked"), content).unwrap();
            output(git(&source).args(["stash", "push"])).unwrap();
        }
        fs::write(source.join("tracked"), b"staged").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"unstaged").unwrap();
        fs::write(source.join("binary"), [0, 255, 128]).unwrap();
        fs::write(source.join(".gitattributes"), b"*.txt text eol=lf\n").unwrap();
        fs::write(source.join("raw.txt"), b"raw\r\nbytes\r\n").unwrap();
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"unique ignored bytes").unwrap();
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
        }
        assert_eq!(fs::read(restored.join("binary")).unwrap(), [0, 255, 128]);
        assert!(restore_bundle(&bundle, &restored, "neo").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
