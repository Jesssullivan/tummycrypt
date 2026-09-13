//! Restore missing payload without replacing retained linked administration.

use super::{
    attachment_policy_matches, capture_revision, common_repository, git, import_bundle, output,
    require_missing, restore_entry, restore_filesystem_rows, snapshot_command, sync_private_tree,
    text, write_git_pointer, IndexReservation,
};
use crate::{BulkloadRefusal, Result};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

type Image = Vec<(PathBuf, u32, Vec<u8>)>;

fn image(root: &Path) -> Result<Image> {
    let mut pending = vec![root.to_path_buf()];
    let mut result = Vec::new();
    let mut remaining = 64 * 1024 * 1024u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if path == root.join("index.lock") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let mut file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)?;
                let before = crate::freshness::StatIdentity::from_metadata(&metadata);
                if crate::freshness::StatIdentity::from_metadata(&file.metadata()?) != before {
                    return Err(BulkloadRefusal::GitAuthorityChanged);
                }
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(remaining + 1)
                    .read_to_end(&mut bytes)?;
                remaining = remaining
                    .checked_sub(bytes.len() as u64)
                    .ok_or(BulkloadRefusal::BudgetExceeded)?;
                if crate::freshness::StatIdentity::from_metadata(&file.metadata()?) != before
                    || crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(&path)?)
                        != before
                {
                    return Err(BulkloadRefusal::GitAuthorityChanged);
                }
                result.push((
                    path.strip_prefix(root)
                        .map_err(|_| BulkloadRefusal::PathEscapesRoot)?
                        .to_path_buf(),
                    metadata.mode(),
                    bytes,
                ));
            } else {
                return Err(BulkloadRefusal::GitInventoryMalformed);
            }
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

fn authority(repository: &Path, admin: &Path, head: &str, staged: &str) -> Result<()> {
    let index = admin.join("index");
    if text(snapshot_command(admin, repository, &index).args(["rev-parse", "HEAD"]))? != head {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    if !snapshot_command(admin, repository, &index)
        .args(["diff", "--cached", "--quiet", staged, "--"])
        .status()?
        .success()
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    Ok(())
}

fn complete(
    receipt: &Path,
    destination: &Path,
    admin: &Path,
    head: &str,
    staged: &str,
) -> Result<()> {
    let path = receipt.join("completed.postcard");
    fs::write(
        &path,
        postcard::to_allocvec(&(destination, admin, head, staged))
            .map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    fs::File::open(path)?.sync_all()?;
    fs::File::open(receipt)?.sync_all()?;
    Ok(())
}

/// Reuse an exact retained registration, index and HEAD; create missing payload only.
///
/// # Errors
/// Refuses occupied payload, differing retained staging/HEAD/policy, active Git
/// operations, malformed administration or changed authority. Partial new payload
/// and its receipt remain on refusal; existing administration is never replaced.
#[allow(clippy::too_many_arguments)]
pub fn restore(
    bundle: &Path,
    repository: &Path,
    destination: &Path,
    admin: &Path,
    source: &str,
    receipt: &Path,
) -> Result<()> {
    let repository = fs::canonicalize(repository)?;
    let common = common_repository(&repository)?;
    let admin = fs::canonicalize(admin)?;
    if admin.parent() != Some(common.join("worktrees").as_path()) {
        return Err(BulkloadRefusal::GitAuthorityOutsideRoot);
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
    require_missing(&destination)?;
    if fs::read_to_string(admin.join("gitdir"))?.trim_end()
        != destination
            .join(".git")
            .to_str()
            .ok_or(BulkloadRefusal::PathNotPortable)?
        || fs::canonicalize(admin.join(fs::read_to_string(admin.join("commondir"))?.trim_end()))?
            != common
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    for marker in [
        "index.lock",
        "locked",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        require_missing(&admin.join(marker))?;
    }
    let before = image(&admin)?;
    let receipt_parent =
        fs::canonicalize(receipt.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?)?;
    let receipt = receipt_parent.join(
        receipt
            .file_name()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    );
    if receipt.starts_with(&admin) || receipt.starts_with(&destination) {
        return Err(BulkloadRefusal::PathEscapesRoot);
    }
    fs::DirBuilder::new().mode(0o700).create(&receipt)?;
    fs::write(
        receipt.join("original-administration.postcard"),
        postcard::to_allocvec(&before).map_err(|_| BulkloadRefusal::FrameCodec)?,
    )?;
    fs::copy(bundle, receipt.join("capture.bundle"))?;
    sync_private_tree(&receipt)?;
    fs::File::open(&receipt_parent)?.sync_all()?;
    import_bundle(&repository, &receipt.join("capture.bundle"), source)?;
    let heads = text(
        git(&repository)
            .args(["bundle", "list-heads"])
            .arg(receipt.join("capture.bundle")),
    )?;
    let head = capture_revision(&heads, "head")?;
    let staged = capture_revision(&heads, "staged")?;
    authority(&repository, &admin, &head, &staged)?;
    attachment_policy_matches(&repository, &repository, &heads)?;
    let reservation = IndexReservation::acquire(admin.join("index.lock"))?;
    if image(&admin)? != before {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    authority(&repository, &admin, &head, &staged)?;
    fs::DirBuilder::new().mode(0o700).create(&destination)?;
    let pointer = write_git_pointer(&receipt, &admin)?;
    fs::File::open(&pointer)?.sync_all()?;
    fs::hard_link(pointer, destination.join(".git"))?;
    let entries = output(git(&repository).args([
        "ls-tree",
        "-r",
        "-z",
        &capture_revision(&heads, "worktree")?,
    ]))?;
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        restore_entry(&destination, entry)?;
    }
    restore_filesystem_rows(&destination, &capture_revision(&heads, "filesystem-v1")?)?;
    if image(&admin)? != before {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    authority(&repository, &admin, &head, &staged)?;
    attachment_policy_matches(&repository, &repository, &heads)?;
    fs::File::open(&destination)?.sync_all()?;
    reservation.release()?;
    complete(&receipt, &destination, &admin, &head, &staged)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::export_repository;
    use super::*;

    #[test]
    fn retained_registration_restores_payload_without_replacing_staging() {
        let root = std::env::temp_dir().join(format!("bulkload-registered-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        output(git(&repository).args(["init", "--template="])).unwrap();
        output(git(&repository).args(["config", "user.name", "Test"])).unwrap();
        output(git(&repository).args(["config", "user.email", "test@localhost"])).unwrap();
        fs::write(repository.join("tracked"), b"base").unwrap();
        output(git(&repository).args(["add", "."])).unwrap();
        output(git(&repository).args(["-c", "commit.gpgsign=false", "commit", "-m", "base"]))
            .unwrap();
        let target = root.join("linked");
        output(
            git(&repository)
                .args(["worktree", "add", "-b", "linked"])
                .arg(&target),
        )
        .unwrap();
        fs::write(target.join("tracked"), b"staged").unwrap();
        output(git(&target).args(["add", "tracked"])).unwrap();
        fs::write(target.join("tracked"), b"unstaged\0bytes").unwrap();
        fs::create_dir(target.join("empty")).unwrap();
        let bundle = export_repository(&target, &root.join("capture")).unwrap();
        let admin =
            PathBuf::from(text(git(&target).args(["rev-parse", "--absolute-git-dir"])).unwrap());
        let before = image(&admin).unwrap();
        let index = fs::read(admin.join("index")).unwrap();
        fs::rename(&target, root.join("retained-original-payload")).unwrap();
        output(
            snapshot_command(&admin, &repository, &admin.join("index")).args(["read-tree", "HEAD"]),
        )
        .unwrap();
        assert_eq!(
            restore(
                &bundle,
                &repository,
                &target,
                &admin,
                "neo",
                &root.join("refused")
            ),
            Err(BulkloadRefusal::GitAuthorityChanged)
        );
        assert!(!target.exists());
        fs::write(admin.join("index"), index).unwrap();
        restore(
            &bundle,
            &repository,
            &target,
            &admin,
            "neo",
            &root.join("receipt"),
        )
        .unwrap();
        assert_eq!(image(&admin).unwrap(), before);
        assert_eq!(
            fs::read(target.join("tracked")).unwrap(),
            b"unstaged\0bytes"
        );
        assert!(target.join("empty").is_dir());
        assert_eq!(
            text(git(&target).args(["rev-parse", "--absolute-git-dir"])).unwrap(),
            admin.to_str().unwrap()
        );
        assert!(restore(
            &bundle,
            &repository,
            &target,
            &admin,
            "neo",
            &root.join("occupied")
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
