//! One common Git closure plus workspace-specific prerequisite bundles.
//!
//! Shared bases are transport dependencies, not a replacement for each
//! workspace's staged, dirty, ignored and filesystem metadata capture.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{capture_refs, git, input, oid, output, prepare_private, refs, set_ref, text};
use crate::{BulkloadRefusal, Result};

fn stash_history(repo: &Path, inventory: &str) -> Result<Vec<u8>> {
    if inventory.lines().any(|line| line.ends_with(" refs/stash")) {
        output(git(repo).args(["reflog", "show", "--format=%H", "refs/stash"]))
    } else {
        Ok(Vec::new())
    }
}

/// Capture refs, stash history and HEAD once without walking workspace payloads.
///
/// This optimistic snapshot does not freeze writers. Subsequent workspace
/// captures may include new commits absent from this base; those travel in
/// their delta. The caller owns digest binding, retention and import ordering.
///
/// # Errors
/// Refuses changing refs/stashes/HEAD, invalid Git state or occupied capture paths.
pub fn export_base(repo: &Path, capture: &Path) -> Result<PathBuf> {
    let repo = fs::canonicalize(repo)?;
    fs::DirBuilder::new().mode(0o700).create(capture)?;
    let capture = fs::canonicalize(capture)?;
    if capture.starts_with(&repo) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
    }
    let inventory = refs(&repo)?;
    let stashes = stash_history(&repo, &inventory)?;
    let head = text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?;
    let private = prepare_private(&repo, &capture)?;
    capture_refs(&repo, &private, &inventory)?;
    set_ref(&private, "refs/carry-export/shared-base-head", &head)?;
    let bundle = capture.join("base.bundle");
    write_bundle(&private, &bundle, None)?;
    if inventory != refs(&repo)?
        || stashes != stash_history(&repo, &inventory)?
        || head != text(git(&repo).args(["rev-parse", "--verify", "HEAD"]))?
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    output(git(&private).args(["bundle", "verify"]).arg(&bundle))?;
    Ok(bundle)
}

fn prerequisite_commits(private: &Path, base: &Path) -> Result<BTreeSet<String>> {
    output(git(private).args(["bundle", "verify"]).arg(base))?;
    let heads = text(git(private).args(["bundle", "list-heads"]).arg(base))?;
    let mut commits = BTreeSet::new();
    for line in heads.lines() {
        let (value, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(value) || !name.starts_with("refs/carry-export/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        // A ref may legally point to a tree/blob. Only commits can be bundle
        // prerequisites; non-commit objects remain in the workspace pack.
        output(git(private).args(["cat-file", "-e", value]))?;
        if let Ok(commit) =
            text(git(private).args(["rev-parse", "--verify", &format!("{value}^{{commit}}")]))
        {
            if !oid(&commit) {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
            commits.insert(commit);
        }
    }
    if commits.is_empty() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(commits)
}

/// Whether a bundle declares prerequisite commits that require retained custody.
///
/// # Errors
/// Refuses malformed or oversized bundle headers and unavailable files.
pub fn requires_base(bundle: &Path) -> Result<bool> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(bundle)?;
    let mut source = BufReader::new(file);
    let mut consumed = 0usize;
    let mut prerequisite = false;
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(1024 * 1024)
            .read_until(b'\n', &mut line)?;
        if consumed == 0 && line != b"# v2 git bundle\n" && line != b"# v3 git bundle\n" {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        consumed = consumed
            .checked_add(count)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if count == 0 || !line.ends_with(b"\n") || consumed > 16 * 1024 * 1024 {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if line == b"\n" {
            return Ok(prerequisite);
        }
        prerequisite |= line.starts_with(b"-");
    }
}

pub(super) fn write_bundle(private: &Path, bundle: &Path, base: Option<&Path>) -> Result<()> {
    let Some(base) = base else {
        output(
            git(private)
                .args(["bundle", "create"])
                .arg(bundle)
                .arg("--all"),
        )?;
        return Ok(());
    };
    let commits = prerequisite_commits(private, base)?;
    let exclusions = commits.iter().fold(String::new(), |mut result, value| {
        result.push('^');
        result.push_str(value);
        result.push('\n');
        result
    });
    input(
        git(private)
            .args(["bundle", "create"])
            .arg(bundle)
            .args(["--all", "--stdin"]),
        exclusions.as_bytes(),
    )?;

    // Git omits excluded ref tips from bundle headers. Our HEAD may be exactly
    // a base tip, and staged/worktree commits deliberately have no parents.
    // Retain every advertised workspace ref and explicitly declare the base
    // commits needed by their trees. The pack remains entirely Git-generated.
    let mut source = BufReader::new(fs::File::open(bundle)?);
    let mut header = Vec::new();
    let mut consumed = 0usize;
    loop {
        let mut line = Vec::new();
        let count = source
            .by_ref()
            .take(1024 * 1024)
            .read_until(b'\n', &mut line)?;
        consumed = consumed
            .checked_add(count)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if count == 0 || !line.ends_with(b"\n") || consumed > 16 * 1024 * 1024 {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        if line == b"\n" {
            break;
        }
        if line.starts_with(b"# v") || line.starts_with(b"@") {
            header.extend_from_slice(&line);
        }
    }
    if !header.starts_with(b"# v2 git bundle\n") && !header.starts_with(b"# v3 git bundle\n") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    for value in commits {
        writeln!(header, "-{value} shared base")?;
    }
    writeln!(header, "{}\n", refs(private)?)?;
    let pending = bundle.with_extension("header-pending");
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)?;
    target.write_all(&header)?;
    std::io::copy(&mut source, &mut target)?;
    target.sync_all()?;
    fs::rename(&pending, bundle)?;
    fs::File::open(bundle.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) {
        output(
            git(repo)
                .args([
                    "-c",
                    "user.name=Bulkload test",
                    "-c",
                    "user.email=bulkload@localhost",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args),
        )
        .unwrap();
    }

    #[test]
    fn shared_base_preserves_workspace_refs_and_requires_imported_history() {
        let root = std::env::temp_dir().join(format!(
            "tcfs-shared-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        run(&source, &["init", "--template="]);
        let mut random = 1u32;
        let history: Vec<u8> = (0..262_144)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random.to_le_bytes().first().copied().unwrap()
            })
            .collect();
        fs::write(source.join("history"), &history).unwrap();
        fs::write(source.join("file"), b"base").unwrap();
        run(&source, &["add", "."]);
        run(&source, &["commit", "-m", "base"]);
        let head = text(git(&source).args(["rev-parse", "HEAD"])).unwrap();
        let base = export_base(&source, &root.join("base")).unwrap();
        fs::write(source.join("file"), b"staged").unwrap();
        run(&source, &["add", "file"]);
        fs::write(source.join("file"), b"dirty").unwrap();
        fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(source.join("ignored"), b"private ignored payload").unwrap();
        let status = output(git(&source).args(["status", "--porcelain=v1", "-z"])).unwrap();
        let delta =
            super::super::export_repository_with_prerequisite(&source, &root.join("delta"), &base)
                .unwrap();
        let standalone =
            super::super::export_repository(&source, &root.join("standalone")).unwrap();
        assert!(fs::metadata(&delta).unwrap().len() < fs::metadata(&standalone).unwrap().len());
        assert!(
            text(git(&source).args(["bundle", "list-heads"]).arg(&delta))
                .unwrap()
                .lines()
                .any(|line| line == format!("{head} refs/carry-export/head"))
        );
        assert_eq!(
            status,
            output(git(&source).args(["status", "--porcelain=v1", "-z"])).unwrap()
        );
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        run(&destination, &["init", "--template="]);
        assert!(super::super::import_bundle(&destination, &delta, "neo").is_err());
        super::super::import_bundle(&destination, &base, "neo").unwrap();
        let restored = root.join("restored");
        super::super::restore_linked(&delta, &destination, &restored, "neo").unwrap();
        assert_eq!(
            head,
            text(git(&restored).args(["rev-parse", "HEAD"])).unwrap()
        );
        assert_eq!(fs::read(restored.join("history")).unwrap(), history);
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"dirty");
        assert_eq!(
            output(git(&restored).args(["show", ":file"])).unwrap(),
            b"staged"
        );
        assert_eq!(
            fs::read(restored.join("ignored")).unwrap(),
            b"private ignored payload"
        );
        let linked_source = root.join("linked-source");
        output(
            git(&source)
                .args(["worktree", "add", "--detach"])
                .arg(&linked_source)
                .arg(&head),
        )
        .unwrap();
        fs::write(linked_source.join("file"), b"other workspace").unwrap();
        let second = super::super::export_repository_with_prerequisite(
            &linked_source,
            &root.join("second-delta"),
            &base,
        )
        .unwrap();
        let second_restored = root.join("second-restored");
        super::super::restore_linked(&second, &destination, &second_restored, "neo").unwrap();
        assert_eq!(
            fs::read(second_restored.join("file")).unwrap(),
            b"other workspace"
        );
        assert_eq!(fs::read(restored.join("file")).unwrap(), b"dirty");
        fs::remove_dir_all(root).unwrap();
    }
}
