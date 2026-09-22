//! Raw tree construction in one Git process, with one regular-file byte pass.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Stdio;

use super::{commit_tree, git, output, safe_destination, text, DriftKind, DriftRow};
use crate::{BulkloadRefusal, Result, RowSchema};
use tcfs_bulkload_proto::FileKind;

/// Blobs a previous capture already holds, keyed by relative path.
///
/// A seat whose `StatIdentity` is unchanged since that capture is emitted by
/// object name instead of being opened again. This is the R25 never-rewalk
/// clause: an unchanged seat costs zero source bytes on a subsequent pass.
pub(super) type Reuse = std::collections::BTreeMap<Vec<u8>, String>;

// Git fast-import accepts C-quoted arbitrary byte paths. Quote every byte in
// octal, including spaces, newlines, quotes, backslashes and non-UTF8 bytes.
fn quoted(path: &[u8]) -> Vec<u8> {
    let mut value = Vec::with_capacity(path.len().saturating_mul(4).saturating_add(2));
    value.push(b'"');
    for byte in path {
        value.extend_from_slice(&[
            b'\\',
            b'0' + (byte >> 6),
            b'0' + ((byte >> 3) & 7),
            b'0' + (byte & 7),
        ]);
    }
    value.push(b'"');
    value
}

/// One seat's outcome in the single byte pass.
enum Seat {
    /// The seat was streamed whole; its source byte count.
    Captured(u64),
    /// The seat changed under the pass and is reported as drift, not refused.
    Drifted(DriftKind),
}

// A seat that vanished or changed identity before its `M` line is written costs
// the stream nothing: no obligation has been announced, so it is simply skipped.
fn opened(path: &Path, row: &RowSchema) -> Result<std::result::Result<fs::File, DriftKind>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        // Only ENOENT is drift. Every other IO fault is a real refusal, so
        // drift tolerance never swallows a permission or media error.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Err(DriftKind::SeatRemoved))
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || crate::freshness::StatIdentity::from_metadata(&metadata)
            != crate::freshness::StatIdentity::from_row(row)
    {
        return Ok(Err(DriftKind::SeatChanged));
    }
    Ok(Ok(file))
}

// The `data N` header obliges exactly N bytes. A post-read identity mismatch is
// therefore finished deterministically (zero fill) and the seat is deleted from
// the tree afterwards, rather than presenting partial bytes as truth.
fn regular(
    writer: &mut impl Write,
    file: &mut fs::File,
    path: &Path,
    row: &RowSchema,
) -> Result<Seat> {
    writeln!(writer, "data {}", row.size)?;
    let copied = std::io::copy(&mut Read::by_ref(file).take(row.size), writer)?;
    let identity = crate::freshness::StatIdentity::from_row(row);
    let drifted = copied != row.size
        || crate::freshness::StatIdentity::from_metadata(&file.metadata()?) != identity
        || match fs::symlink_metadata(path) {
            Ok(metadata) => crate::freshness::StatIdentity::from_metadata(&metadata) != identity,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.into()),
        };
    if drifted {
        let remaining = row.size.saturating_sub(copied);
        std::io::copy(&mut std::io::repeat(0).take(remaining), writer)?;
        writer.write_all(b"\n")?;
        return Ok(Seat::Drifted(DriftKind::SeatChanged));
    }
    writer.write_all(b"\n")?;
    Ok(Seat::Captured(copied))
}

pub(super) const fn mode_of(row: &RowSchema) -> Result<&'static str> {
    Ok(match row.kind {
        FileKind::Regular if row.mode & 0o100 != 0 => "100755",
        FileKind::Regular => "100644",
        FileKind::Symlink => "120000",
        _ => return Err(BulkloadRefusal::GitInventoryMalformed),
    })
}

fn stream(
    writer: &mut impl Write,
    repo: &Path,
    rows: &[RowSchema],
    reuse: &Reuse,
    drift: &mut Vec<DriftRow>,
) -> Result<u64> {
    writer.write_all(
        b"commit refs/bulkload-raw-tree\ncommitter Bulkload <bulkload@localhost> 946684800 +0000\ndata 0\n\ndeleteall\n",
    )?;
    let mut bytes_read = 0u64;
    for row in rows {
        let relative = Path::new(std::ffi::OsStr::from_bytes(&row.rel_path));
        let path = safe_destination(repo, relative)?;
        if row.kind == FileKind::Directory {
            continue;
        }
        let mode = mode_of(row)?;
        // A seat a retained capture already holds at this exact StatIdentity is
        // emitted by object name. No descriptor is opened and no byte is re-read.
        if let Some(object) = reuse.get(&row.rel_path) {
            write!(writer, "M {mode} {object} ")?;
            writer.write_all(&quoted(&row.rel_path))?;
            writer.write_all(b"\n")?;
            continue;
        }
        if row.kind == FileKind::Regular {
            let mut file = match opened(&path, row)? {
                Ok(file) => file,
                Err(kind) => {
                    drift.push(DriftRow::seat(kind, &row.rel_path));
                    continue;
                }
            };
            write!(writer, "M {mode} inline ")?;
            writer.write_all(&quoted(&row.rel_path))?;
            writer.write_all(b"\n")?;
            match regular(writer, &mut file, &path, row)? {
                Seat::Captured(copied) => {
                    bytes_read = bytes_read
                        .checked_add(copied)
                        .ok_or(BulkloadRefusal::BudgetExceeded)?;
                }
                Seat::Drifted(kind) => {
                    // The announced payload is complete; withdraw the seat.
                    writer.write_all(b"D ")?;
                    writer.write_all(&quoted(&row.rel_path))?;
                    writer.write_all(b"\n")?;
                    drift.push(DriftRow::seat(kind, &row.rel_path));
                }
            }
        } else {
            let target = row
                .link_target
                .as_deref()
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
            write!(writer, "M {mode} inline ")?;
            writer.write_all(&quoted(&row.rel_path))?;
            writer.write_all(b"\n")?;
            writeln!(writer, "data {}", target.len())?;
            writer.write_all(target)?;
            writer.write_all(b"\n")?;
        }
    }
    writer.write_all(b"\ndone\n")?;
    Ok(bytes_read)
}

// The regular byte counter counts actual bytes streamed from source file
// descriptors. Metadata censuses, symlink reads and Git repacking are separate.
/// The result of one raw byte pass over a checkout.
pub(super) struct Pass {
    pub tree: String,
    pub bytes_read: u64,
    pub drift: Vec<DriftRow>,
}

pub(super) fn capture(
    private: &Path,
    repo: &Path,
    rows: &[RowSchema],
    reuse: &Reuse,
) -> Result<Pass> {
    if git(private)
        .args(["show-ref", "--verify", "--quiet", "refs/bulkload-raw-tree"])
        .status()?
        .success()
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let mut child = git(private)
        .args(["fast-import", "--quiet", "--done"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
    let mut drift = Vec::new();
    let result = stream(&mut stdin, repo, rows, reuse, &mut drift);
    drop(stdin);
    // Even on a source refusal close the stream and reap our child normally.
    // No process is signaled, and a partial private capture remains diagnostic.
    let status = child.wait()?;
    let bytes_read = result?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax, not interpolation.
    let tree = text(git(private).args(["rev-parse", "refs/bulkload-raw-tree^{tree}"]))?;
    output(git(private).args(["update-ref", "-d", "refs/bulkload-raw-tree"]))?;
    Ok(Pass {
        tree,
        bytes_read,
        drift,
    })
}

/// Withdraw seats the post-pass census found drifted, without a second byte pass.
///
/// A seat whose identity changed only after its bytes were streamed is already
/// in the tree. It is deleted here so the captured tree never claims bytes the
/// pass could not vouch for; the drift sidecar is the statement that it is absent.
pub(super) fn prune(private: &Path, tree: &str, paths: &[Vec<u8>]) -> Result<String> {
    if paths.is_empty() {
        return Ok(tree.to_owned());
    }
    if git(private)
        .args(["show-ref", "--verify", "--quiet", "refs/bulkload-raw-prune"])
        .status()?
        .success()
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let parent = commit_tree(private, tree, "bulkload pre-prune worktree")?;
    let mut child = git(private)
        .args(["fast-import", "--quiet", "--done"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or(BulkloadRefusal::Io(None))?;
    let result = (|| -> Result<()> {
        stdin.write_all(
            b"commit refs/bulkload-raw-prune\ncommitter Bulkload <bulkload@localhost> 946684800 +0000\ndata 0\n\n",
        )?;
        writeln!(stdin, "from {parent}")?;
        for path in paths {
            stdin.write_all(b"D ")?;
            stdin.write_all(&quoted(path))?;
            stdin.write_all(b"\n")?;
        }
        stdin.write_all(b"\ndone\n")?;
        Ok(())
    })();
    drop(stdin);
    let status = child.wait()?;
    result?;
    if !status.success() {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    #[allow(clippy::literal_string_with_formatting_args)] // Git revision syntax, not interpolation.
    let pruned = text(git(private).args(["rev-parse", "refs/bulkload-raw-prune^{tree}"]))?;
    output(git(private).args(["update-ref", "-d", "refs/bulkload-raw-prune"]))?;
    Ok(pruned)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};

    #[test]
    fn one_raw_pass_preserves_paths_links_modes_and_never_runs_filters() {
        let root = std::env::temp_dir().join(format!("tcfs-raw-tree-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let repo = root.join("source");
        fs::create_dir(&repo).unwrap();
        output(git(&repo).args(["init", "--template="])).unwrap();
        output(git(&repo).args(["config", "filter.poison.clean", "false"])).unwrap();
        output(git(&repo).args(["config", "filter.poison.required", "true"])).unwrap();
        fs::write(
            repo.join(".gitattributes"),
            b"* filter=poison text eol=lf\n",
        )
        .unwrap();
        fs::write(repo.join(".gitignore"), b"ignored\n").unwrap();
        fs::write(repo.join("ignored"), b"\0private\r\nraw\xff").unwrap();
        let unusual = "quote\" tab\t newline\n slash\\";
        fs::write(repo.join(unusual), b"raw\r\n\0\xff").unwrap();
        fs::set_permissions(repo.join(unusual), fs::Permissions::from_mode(0o750)).unwrap();
        symlink("missing\nsymlink-target", repo.join("link")).unwrap();
        let rows = super::super::filesystem_rows(&repo).unwrap();
        let expected_bytes: u64 = rows
            .iter()
            .filter(|row| row.kind == FileKind::Regular)
            .map(|row| row.size)
            .sum();
        let private = super::super::prepare_private(&repo, &root).unwrap();
        let Pass {
            tree, bytes_read, ..
        } = capture(&private, &repo, &rows, &Reuse::new()).unwrap();
        assert_eq!(bytes_read, expected_bytes);
        assert_eq!(
            output(git(&private).args(["show", &format!("{tree}:{unusual}")])).unwrap(),
            b"raw\r\n\0\xff"
        );
        assert_eq!(
            output(git(&private).args(["show", &format!("{tree}:ignored")])).unwrap(),
            b"\0private\r\nraw\xff"
        );
        assert_eq!(
            output(git(&private).args(["show", &format!("{tree}:link")])).unwrap(),
            b"missing\nsymlink-target"
        );
        assert!(
            output(git(&private).args(["ls-tree", &tree, "--", unusual]))
                .unwrap()
                .starts_with(b"100755 blob ")
        );
        assert!(!repo.join(".git/index").exists());
        assert_eq!(
            quoted(&[b'\n', b'"', b'\\', 0xff]),
            b"\"\\012\\042\\134\\377\""
        );
        fs::remove_dir_all(root).unwrap();
    }

    struct MutatingWriter {
        path: std::path::PathBuf,
        changed: bool,
    }

    impl Write for MutatingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.changed {
                fs::write(&self.path, b"changed while descriptor was open")?;
                self.changed = true;
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn raw_pass_reports_a_file_changed_after_open_as_drift() {
        let root = std::env::temp_dir().join(format!("tcfs-raw-mutation-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("file");
        fs::write(&path, b"original bytes").unwrap();
        let rows = super::super::filesystem_rows(&root).unwrap();
        let mut writer = MutatingWriter {
            path,
            changed: false,
        };
        let mut drift = Vec::new();
        // The stream obligation is still satisfied exactly, and the seat is
        // withdrawn from the tree rather than the whole pass being refused.
        stream(&mut writer, &root, &rows, &Reuse::new(), &mut drift).unwrap();
        assert_eq!(drift.len(), 1);
        assert_eq!(drift.first().unwrap().kind, DriftKind::SeatChanged);
        assert_eq!(drift.first().unwrap().name, b"file".to_vec());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_seat_removed_before_its_bytes_are_announced_is_drift() {
        let root = std::env::temp_dir().join(format!("tcfs-raw-removed-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::write(root.join("file"), b"original bytes").unwrap();
        let rows = super::super::filesystem_rows(&root).unwrap();
        fs::remove_file(root.join("file")).unwrap();
        let mut writer = Vec::new();
        let mut drift = Vec::new();
        stream(&mut writer, &root, &rows, &Reuse::new(), &mut drift).unwrap();
        assert_eq!(drift.first().unwrap().kind, DriftKind::SeatRemoved);
        // No obligation was announced for the vanished seat.
        assert!(!writer.windows(2).any(|pair| pair == b"M "));
        fs::remove_dir_all(root).unwrap();
    }
}
