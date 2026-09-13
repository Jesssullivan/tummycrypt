//! Descriptor-relative, no-clobber output publication.

use std::ffi::CString;
use std::fs::{File, Permissions};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::freshness::StatIdentity;
use crate::transfer_store::{Manifest, Store};
use crate::{BulkloadRefusal, Result, RowSchema};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// An opened destination root; descendants never traverse symlinks.
pub struct Destination {
    root: File,
    path: PathBuf,
    directories: Vec<PendingDirectory>,
}

struct PendingDirectory {
    path: Vec<u8>,
    mode: u32,
    dev: u64,
    ino: u64,
    key: Vec<u8>,
}

impl Destination {
    /// Open an existing destination directory without following its final link.
    ///
    /// # Errors
    /// Refuses a missing root or symlink root.
    pub fn open(path: &Path) -> Result<Self> {
        let root = open_dir(libc::AT_FDCWD, &cstring(path.as_os_str().as_bytes())?)?;
        Ok(Self {
            root,
            path: std::fs::canonicalize(path)?,
            directories: Vec::new(),
        })
    }

    /// Canonical destination root for completion-store namespacing.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Identity of an existing regular output. No symlink is followed.
    ///
    /// # Errors
    /// Refuses a conflicting node or unsafe ancestor.
    pub fn identity(&self, row: &RowSchema) -> Result<Option<StatIdentity>> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        match open_regular(&parent, &leaf) {
            Ok(file) => Ok(Some(StatIdentity::from_metadata(&file.metadata()?))),
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Create a directory, retaining an existing directory's metadata.
    ///
    /// # Errors
    /// Refuses non-directory conflicts and divergent existing modes.
    pub fn directory(&mut self, row: &RowSchema, store: &Store, authority: &[u8]) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let key = postcard::to_stdvec(&(authority, &row.rel_path))?;
        let mode = row.mode & 0o7777;
        // SAFETY: both descriptors and the NUL-terminated leaf remain valid.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) };
        if created == 0 {
            let metadata = open_dir(parent.as_raw_fd(), &leaf)?.metadata()?;
            store.pending_directory(&key, metadata.dev(), metadata.ino(), mode, true)?;
            self.directories.push(PendingDirectory {
                path: row.rel_path.clone(),
                mode,
                dev: metadata.dev(),
                ino: metadata.ino(),
                key,
            });
            parent.sync_all()?;
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        let existing = open_dir(parent.as_raw_fd(), &leaf)?;
        let metadata = existing.metadata()?;
        if store.pending_directory(&key, metadata.dev(), metadata.ino(), mode, false)? {
            self.directories.push(PendingDirectory {
                path: row.rel_path.clone(),
                mode,
                dev: metadata.dev(),
                ino: metadata.ino(),
                key,
            });
        } else if metadata.mode() & 0o7777 != mode {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// Apply final modes to directories this invocation created, deepest first.
    ///
    /// # Errors
    /// Refuses changed/removed directories and failed durable metadata writes.
    pub fn finish_directories(&self, store: &Store) -> Result<()> {
        for pending in self.directories.iter().rev() {
            let (parent, leaf) = self.parent(&pending.path)?;
            let directory = open_dir(parent.as_raw_fd(), &leaf)?;
            let metadata = directory.metadata()?;
            if metadata.dev() != pending.dev || metadata.ino() != pending.ino {
                return Err(BulkloadRefusal::GitDestinationOccupied);
            }
            directory.set_permissions(Permissions::from_mode(pending.mode))?;
            directory.sync_all()?;
            store.complete_directory(&pending.key)?;
        }
        Ok(())
    }

    /// Preserve the literal target of a source symlink, without following it.
    ///
    /// # Errors
    /// Refuses a different existing target or any unsafe ancestor.
    pub fn symlink(&self, row: &RowSchema) -> Result<()> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        let target = row
            .link_target
            .as_ref()
            .ok_or(BulkloadRefusal::RequiredFieldMissing)?;
        let target_c = cstring(target)?;
        // SAFETY: descriptor and both NUL-terminated strings remain valid.
        let result =
            unsafe { libc::symlinkat(target_c.as_ptr(), parent.as_raw_fd(), leaf.as_ptr()) };
        if result == 0 {
            parent.sync_all()?;
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        let mut bytes = vec![0_u8; target.len().saturating_add(1)];
        // SAFETY: bytes is writable for its full length; strings/descriptors valid.
        let read = unsafe {
            libc::readlinkat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if usize::try_from(read).ok() != Some(target.len())
            || bytes.get(..target.len()) != Some(target.as_slice())
        {
            return Err(BulkloadRefusal::GitDestinationOccupied);
        }
        Ok(())
    }

    /// Verify chunks and publish a complete file with link-at no-replace semantics.
    ///
    /// # Errors
    /// Refuses corrupt/missing chunks, size/digest mismatches or destination divergence.
    pub fn file(
        &self,
        row: &RowSchema,
        manifest: &Manifest,
        store: &Store,
    ) -> Result<StatIdentity> {
        let (parent, leaf) = self.parent(&row.rel_path)?;
        match open_regular(&parent, &leaf) {
            Ok(file) => return verify_existing(file, row, manifest),
            Err(BulkloadRefusal::Io(Some(libc::ENOENT))) => (),
            Err(error) => return Err(error),
        }
        let temporary = cstring(
            format!(
                ".bulkload-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        )?;
        // SAFETY: parent descriptor and path are valid; mode accompanies O_CREAT.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: fd is a newly-created uniquely owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = write_chunks(&mut file, row, manifest, store).and_then(|()| {
            // SAFETY: all descriptors/paths valid; linkat never replaces an existing leaf.
            let linked = unsafe {
                libc::linkat(
                    parent.as_raw_fd(),
                    temporary.as_ptr(),
                    parent.as_raw_fd(),
                    leaf.as_ptr(),
                    0,
                )
            };
            if linked != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            parent.sync_all()?;
            Ok(())
        });
        // SAFETY: remove only the unique temporary name created above.
        let removed = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
        if removed != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        result?;
        verify_existing(file, row, manifest)
    }

    fn parent(&self, path: &[u8]) -> Result<(File, CString)> {
        let mut parts = path.split(|byte| *byte == b'/').peekable();
        let mut directory = self.root.try_clone()?;
        while let Some(part) = parts.next() {
            if part.is_empty() || part == b"." || part == b".." {
                return Err(BulkloadRefusal::PathEscapesRoot);
            }
            let component = cstring(part)?;
            if parts.peek().is_none() {
                return Ok((directory, component));
            }
            directory = open_dir(directory.as_raw_fd(), &component)?;
        }
        Err(BulkloadRefusal::PathEscapesRoot)
    }
}

fn write_chunks(
    file: &mut File,
    row: &RowSchema,
    manifest: &Manifest,
    store: &Store,
) -> Result<()> {
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    for chunk in &manifest.chunks {
        let data = store
            .chunk(&chunk.digest)?
            .ok_or(BulkloadRefusal::SealedObjectMissing)?;
        if data.len() as u64 != chunk.size {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        size = size
            .checked_add(chunk.size)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        if size > row.size {
            return Err(BulkloadRefusal::DigestMismatch);
        }
        hasher.update(&data);
        file.write_all(&data)?;
    }
    if size != row.size || *hasher.finalize().as_bytes() != manifest.digest {
        return Err(BulkloadRefusal::DigestMismatch);
    }
    file.set_permissions(Permissions::from_mode(row.mode & 0o7777))?;
    file.sync_all()?;
    Ok(())
}

fn verify_existing(mut file: File, row: &RowSchema, manifest: &Manifest) -> Result<StatIdentity> {
    file.rewind()?;
    let before = file.metadata()?;
    if before.len() != row.size || before.mode() & 0o7777 != row.mode & 0o7777 {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    let identity = StatIdentity::from_metadata(&before);
    let mut buffer = vec![0; 256 * 1024];
    let mut hasher = blake3::Hasher::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).ok_or(BulkloadRefusal::Io(None))?);
    }
    if *hasher.finalize().as_bytes() != manifest.digest
        || StatIdentity::from_metadata(&file.metadata()?) != identity
    {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(identity)
}

fn open_dir(parent: i32, name: &CString) -> Result<File> {
    // SAFETY: name is NUL-terminated; successful descriptor is uniquely owned.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is a newly opened descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_regular(parent: &File, name: &CString) -> Result<File> {
    // SAFETY: parent and name are valid; no create flag needs a mode argument.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is uniquely owned.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(BulkloadRefusal::GitDestinationOccupied);
    }
    Ok(file)
}

fn cstring(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| BulkloadRefusal::PathNotPortable)
}
