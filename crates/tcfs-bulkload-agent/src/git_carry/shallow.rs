//! Explicit shallow-graph custody inside an ordinary metadata bundle.
//!
//! Git bundle prerequisites cannot encode a shallow frontier. The envelope's
//! pack is ordinary blob data until this adapter installs its explicit frontier
//! in new/private administration; no source parent or history is fabricated.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Stdio;

use super::{git, input, oid, output, refs, text};
use crate::{BulkloadRefusal, Result};

const CUSTODY: &str = "refs/carry-export/shallow-custody-v1";

pub(super) fn frontier(repository: &Path) -> Result<Vec<u8>> {
    let path = text(git(repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "shallow",
    ]))?;
    match fs::read(path) {
        Ok(bytes) => {
            validate_frontier(&bytes)?;
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn validate_frontier(bytes: &[u8]) -> Result<()> {
    if bytes.len() > 1024 * 1024 {
        return Err(BulkloadRefusal::BudgetExceeded);
    }
    let content = std::str::from_utf8(bytes).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    if !bytes.is_empty() && (!bytes.ends_with(b"\n") || content.lines().any(|line| !oid(line))) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(())
}

pub(super) fn write_bundle(private: &Path, bundle: &Path, boundary: &[u8]) -> Result<()> {
    validate_frontier(boundary)?;
    let parent = bundle.parent().ok_or(BulkloadRefusal::PathNotAbsolute)?;
    let pack = bundle.with_extension("objects.pack");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pack)?;
    let status = git(private)
        .args(["pack-objects", "--stdout", "--revs", "--all"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(file.try_clone()?))
        .status()?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    file.sync_all()?;
    let envelope = parent.join("shallow-envelope.git");
    let format = text(git(private).args(["rev-parse", "--show-object-format"]))?;
    output(
        git(parent)
            .args([
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
            ])
            .arg(&envelope),
    )?;
    let pack_oid = text(
        git(&envelope)
            .args(["hash-object", "-w", "--no-filters", "--"])
            .arg(&pack),
    )?;
    let inventory = refs(private)?;
    let manifest = postcard::to_allocvec(&(boundary, &inventory, &pack_oid))
        .map_err(|_| BulkloadRefusal::FrameCodec)?;
    // A metadata commit must actually reference the raw-pack blob so ordinary
    // bundle traversal carries it, without traversing the shallow source graph.
    let value = input(
        git(&envelope).args(["hash-object", "-w", "--stdin"]),
        &manifest,
    )?;
    let value = std::str::from_utf8(&value)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    let tree = input(
        git(&envelope).args(["mktree"]),
        format!("100644 blob {value}\tvalue\n100644 blob {pack_oid}\tobjects.pack\n").as_bytes(),
    )?;
    let tree = std::str::from_utf8(&tree)
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)?
        .trim();
    super::set_ref(
        &envelope,
        CUSTODY,
        &super::commit_tree(&envelope, tree, "bulkload explicit shallow graph custody")?,
    )?;
    output(
        git(&envelope)
            .args(["bundle", "create"])
            .arg(bundle)
            .arg(CUSTODY),
    )?;
    Ok(())
}

fn custody_oid(heads: &str) -> Option<&str> {
    heads.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(_, name)| *name == CUSTODY)
            .map(|(value, _)| value)
    })
}

pub(super) fn is_custody(heads: &str) -> bool {
    custody_oid(heads).is_some()
}

pub(super) fn headers(repository: &Path, bundle: &Path) -> Result<String> {
    let heads = text(git(repository).args(["bundle", "list-heads"]).arg(bundle))?;
    let Some(value) = custody_oid(&heads) else {
        return Ok(heads);
    };
    ensure_custody_objects(repository, bundle, value)?;
    let (_, inventory, _) = custody_manifest(repository, value)?;
    Ok(inventory)
}

fn custody_manifest(repository: &Path, value: &str) -> Result<(Vec<u8>, String, String)> {
    let object = text(git(repository).args(["rev-parse", &format!("{value}:value")]))?;
    let mut reader = super::batch_objects::BatchObjects::new(repository)?;
    let mut bytes = Vec::new();
    reader.copy_into(&object, &mut bytes, Some(16 * 1024 * 1024))?;
    reader.finish()?;
    postcard::from_bytes(&bytes).map_err(|_| BulkloadRefusal::FrameCodec)
}

fn ensure_custody_objects(repository: &Path, bundle: &Path, value: &str) -> Result<()> {
    if !oid(value) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    if output(git(repository).args(["cat-file", "-e", &format!("{value}:value")])).is_ok() {
        return Ok(());
    }
    output(
        git(repository)
            .args([
                "fetch",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
                "--no-tags",
                "--no-recurse-submodules",
            ])
            .arg(bundle)
            .arg(CUSTODY),
    )?;
    Ok(())
}

pub(super) fn unpack(repository: &Path, bundle: &Path, heads: &str) -> Result<Option<String>> {
    let Some(value) = custody_oid(heads) else {
        return Ok(None);
    };
    if !oid(value) || heads.lines().count() != 1 {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    ensure_custody_objects(repository, bundle, value)?;
    let (boundary, inventory, pack_oid) = custody_manifest(repository, value)?;
    validate_frontier(&boundary)?;
    if boundary.is_empty() || !oid(&pack_oid) {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    if text(git(repository).args(["rev-parse", &format!("{value}:objects.pack")]))? != pack_oid {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    for line in inventory.lines() {
        let (object, name) = line
            .split_once(' ')
            .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
        if !oid(object) || !name.starts_with("refs/carry-export/") {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        output(git(repository).args(["check-ref-format", name]))?;
    }
    let existing = frontier(repository)?;
    // Never globally truncate an unrelated destination's graph. A matching
    // frontier can union another capture; a fresh repository can adopt it.
    if existing != boundary
        && (!existing.is_empty()
            || !refs(repository)?.is_empty()
            || text(git(repository).args(["rev-parse", "--verify", "HEAD"])).is_ok())
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    let path = text(git(repository).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "shallow",
    ]))?;
    let reservation = if existing.is_empty() {
        Some(super::IndexReservation::acquire(std::path::PathBuf::from(
            format!("{path}.lock"),
        ))?)
    } else {
        None
    };
    if existing.is_empty() {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&boundary)?;
        file.sync_all()?;
    }
    let mut objects = super::batch_objects::BatchObjects::new(repository)?;
    let mut child = git(repository)
        .args(["index-pack", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let copied = child
        .stdin
        .take()
        .map_or(Err(BulkloadRefusal::Io(None)), |mut stdin| {
            objects.copy_into(&pack_oid, &mut stdin, None)
        });
    let status = child.wait()?;
    copied?;
    objects.finish()?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    output(git(repository).args([
        "fsck",
        "--connectivity-only",
        "--no-reflogs",
        "--no-dangling",
    ]))?;
    if frontier(repository)? != boundary {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    fs::File::open(
        Path::new(&path)
            .parent()
            .ok_or(BulkloadRefusal::PathNotAbsolute)?,
    )?
    .sync_all()?;
    if let Some(reservation) = reservation {
        reservation.release()?;
    }
    Ok(Some(inventory))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn actual_shallow_graph_roundtrips_without_touching_existing_history() {
        let root = std::env::temp_dir().join(format!("bulkload-shallow-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let full = root.join("full");
        fs::create_dir(&full).unwrap();
        output(git(&full).args(["init", "--template="])).unwrap();
        output(git(&full).args(["config", "user.name", "Test"])).unwrap();
        output(git(&full).args(["config", "user.email", "test@localhost"])).unwrap();
        let mut commits = Vec::new();
        for bytes in [b"first", b"second".as_slice(), b"third".as_slice()] {
            fs::write(full.join("tracked"), bytes).unwrap();
            output(git(&full).args(["add", "."])).unwrap();
            output(git(&full).args(["-c", "commit.gpgsign=false", "commit", "-m", "next"]))
                .unwrap();
            commits.push(text(git(&full).args(["rev-parse", "HEAD"])).unwrap());
        }
        let source = root.join("shallow");
        output(
            git(&root)
                .args(["clone", "--depth=2", "--no-local"])
                .arg(format!("file://{}", full.display()))
                .arg(&source),
        )
        .unwrap();
        output(git(&source).args([
            "config",
            "remote.origin.url",
            "https://example.test/shallow.git",
        ]))
        .unwrap();
        fs::write(source.join("tracked"), b"staged\0binary").unwrap();
        output(git(&source).args(["add", "."])).unwrap();
        fs::write(source.join("tracked"), b"dirty\0binary").unwrap();
        let boundary = frontier(&source).unwrap();
        assert!(!boundary.is_empty());
        let source_index = fs::read(source.join(".git/index")).unwrap();
        let source_head = text(git(&source).args(["rev-parse", "HEAD"])).unwrap();
        let capture = root.join("capture");
        let bundle = super::super::export_repository(&source, &capture).unwrap();
        assert!(is_custody(
            &text(git(&source).args(["bundle", "list-heads"]).arg(&bundle)).unwrap()
        ));
        output(git(&full).args(["bundle", "verify"]).arg(&bundle)).unwrap();
        let full_head = text(git(&full).args(["rev-parse", "HEAD"])).unwrap();
        let full_index = fs::read(full.join(".git/index")).unwrap();
        assert!(super::super::import_bundle(&full, &bundle, "neo").is_err());
        assert_eq!(
            full_head,
            text(git(&full).args(["rev-parse", "HEAD"])).unwrap()
        );
        assert_eq!(full_index, fs::read(full.join(".git/index")).unwrap());
        assert!(frontier(&full).unwrap().is_empty());
        let destination = root.join("restored");
        super::super::restore_bundle(&bundle, &destination, "neo").unwrap();
        assert_eq!(frontier(&destination).unwrap(), boundary);
        assert_eq!(
            text(git(&destination).args(["rev-parse", "HEAD"])).unwrap(),
            source_head
        );
        assert_eq!(
            text(git(&destination).args(["rev-list", "--count", "HEAD"])).unwrap(),
            "2"
        );
        assert!(
            output(git(&destination).args(["cat-file", "-e", commits.first().unwrap()])).is_err()
        );
        for args in [
            vec!["diff", "--cached", "--binary"],
            vec!["diff", "--binary"],
            vec!["status", "--porcelain"],
        ] {
            assert_eq!(
                output(git(&source).args(&args)).unwrap(),
                output(git(&destination).args(&args)).unwrap()
            );
        }
        assert_eq!(frontier(&source).unwrap(), boundary);
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), source_index);
        assert_eq!(
            text(git(&source).args(["rev-parse", "HEAD"])).unwrap(),
            source_head
        );
        fs::remove_dir_all(root).unwrap();
    }
}
