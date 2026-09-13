//! Raw tree construction in one Git process, with one regular-file byte pass.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Stdio;

use super::{git, output, safe_destination, text};
use crate::{BulkloadRefusal, Result, RowSchema};
use tcfs_bulkload_proto::FileKind;

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

fn regular(writer: &mut impl Write, path: &Path, row: &RowSchema) -> Result<u64> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || crate::freshness::StatIdentity::from_metadata(&metadata)
            != crate::freshness::StatIdentity::from_row(row)
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    writeln!(writer, "data {}", row.size)?;
    let copied = std::io::copy(&mut Read::by_ref(&mut file).take(row.size), writer)?;
    if copied != row.size
        || crate::freshness::StatIdentity::from_metadata(&file.metadata()?)
            != crate::freshness::StatIdentity::from_row(row)
        || crate::freshness::StatIdentity::from_metadata(&fs::symlink_metadata(path)?)
            != crate::freshness::StatIdentity::from_row(row)
    {
        return Err(BulkloadRefusal::GitAuthorityChanged);
    }
    writer.write_all(b"\n")?;
    Ok(copied)
}

fn stream(writer: &mut impl Write, repo: &Path, rows: &[RowSchema]) -> Result<u64> {
    writer.write_all(
        b"commit refs/bulkload-raw-tree\ncommitter Bulkload <bulkload@localhost> 946684800 +0000\ndata 0\n\ndeleteall\n",
    )?;
    let mut bytes_read = 0u64;
    for row in rows {
        let relative = Path::new(std::ffi::OsStr::from_bytes(&row.rel_path));
        let path = safe_destination(repo, relative)?;
        let mode = match row.kind {
            FileKind::Directory => continue,
            FileKind::Regular if row.mode & 0o100 != 0 => "100755",
            FileKind::Regular => "100644",
            FileKind::Symlink => "120000",
            _ => return Err(BulkloadRefusal::GitInventoryMalformed),
        };
        write!(writer, "M {mode} inline ")?;
        writer.write_all(&quoted(&row.rel_path))?;
        writer.write_all(b"\n")?;
        if row.kind == FileKind::Regular {
            bytes_read = bytes_read
                .checked_add(regular(writer, &path, row)?)
                .ok_or(BulkloadRefusal::BudgetExceeded)?;
        } else {
            let target = row
                .link_target
                .as_deref()
                .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
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
pub(super) fn capture(private: &Path, repo: &Path, rows: &[RowSchema]) -> Result<(String, u64)> {
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
    let result = stream(&mut stdin, repo, rows);
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
    Ok((tree, bytes_read))
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
        let (tree, bytes_read) = capture(&private, &repo, &rows).unwrap();
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
    fn raw_pass_refuses_a_file_changed_after_open() {
        let root = std::env::temp_dir().join(format!("tcfs-raw-mutation-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("file");
        fs::write(&path, b"original bytes").unwrap();
        let rows = super::super::filesystem_rows(&root).unwrap();
        let row = rows.first().unwrap();
        let mut writer = MutatingWriter {
            path: path.clone(),
            changed: false,
        };
        assert!(matches!(
            regular(&mut writer, &path, row),
            Err(BulkloadRefusal::GitAuthorityChanged)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
