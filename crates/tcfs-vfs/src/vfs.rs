//! The `VirtualFilesystem` trait — platform-agnostic filesystem operations.
//!
//! This trait abstracts the filesystem logic shared between all backends:
//! - FUSE (Linux primary mount backend, macOS legacy)
//! - NFS loopback (Linux + macOS, no kernel modules)
//! - FileProvider (macOS + iOS native)
//! - fanotify pre-content (Linux 6.14+, future)
//!
//! Implementations translate between this trait and their protocol-specific
//! wire formats (fuse3 PathFilesystem, NFSv3 RPC, NSFileProvider callbacks).

use std::ffi::OsStr;

use anyhow::Result;
use async_trait::async_trait;

use crate::types::{VfsAttr, VfsDirEntry, VfsStatFs};

/// Platform-agnostic virtual filesystem operations.
///
/// All paths are relative to the filesystem root and use `/` separators.
/// The root directory is represented as `"/"`.
///
/// Implementations must be `Send + Sync` for use across async tasks.
#[async_trait]
pub trait VirtualFilesystem: Send + Sync {
    /// Get attributes for a path (stat equivalent).
    ///
    /// Returns `Ok(attr)` if the path exists, or an error if not found.
    async fn getattr(&self, path: &str) -> Result<VfsAttr>;

    /// Look up a child entry in a parent directory.
    ///
    /// Returns attributes of the child if it exists.
    async fn lookup(&self, parent: &str, name: &OsStr) -> Result<VfsAttr>;

    /// List entries in a directory.
    ///
    /// Returns all entries (files and subdirectories). Does NOT include
    /// `.` and `..` — those are synthesized by the protocol adapter.
    async fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>>;

    /// List entries in a directory with full attributes (readdirplus).
    ///
    /// Default implementation calls `readdir` and fills in attributes.
    /// Override for backends that can provide attrs cheaply during listing.
    async fn readdirplus(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
        self.readdir(path).await
    }

    /// Open a file and return its hydrated content.
    ///
    /// For remote-backed filesystems, this triggers hydration (fetching from
    /// remote storage or cache). Returns the full file content as bytes.
    ///
    /// The returned `u64` is a file handle ID for use with `read` and `release`.
    async fn open(&self, path: &str) -> Result<(u64, Vec<u8>)>;

    /// Read bytes from an open file handle.
    ///
    /// `fh` is the handle returned by `open`. Returns the requested slice.
    async fn read(&self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>>;

    /// Release (close) a file handle.
    ///
    /// For writable filesystems, this flushes any buffered writes to
    /// remote storage before releasing the handle.
    async fn release(&self, fh: u64) -> Result<()>;

    /// Filesystem statistics (statfs equivalent).
    async fn statfs(&self) -> Result<VfsStatFs> {
        Ok(VfsStatFs::default())
    }

    // ── Write operations ──────────────────────────────────────────────
    //
    // Default implementations return ENOSYS (not supported). Override in
    // backends that support writes (e.g., TcfsVfs with sync engine).

    /// Create a new file in a parent directory.
    ///
    /// Returns a file handle and attributes for the new file.
    /// The file starts empty; use `write()` to add content.
    async fn create(&self, _parent: &str, _name: &OsStr, _mode: u32) -> Result<(u64, VfsAttr)> {
        anyhow::bail!("ENOSYS: create not supported")
    }

    /// Write bytes to an open file handle at the given offset.
    ///
    /// Writes are buffered in memory until `release()` or `fsync()`.
    /// Returns the number of bytes written.
    async fn write(&self, _fh: u64, _offset: u64, _data: &[u8]) -> Result<u32> {
        anyhow::bail!("ENOSYS: write not supported")
    }

    /// Flush buffered writes to remote storage.
    ///
    /// Called on fsync(). For SeaweedFS-backed VFS, this chunks the file,
    /// uploads to S3, creates a manifest, and updates the index entry.
    async fn fsync(&self, _fh: u64, _datasync: bool) -> Result<()> {
        anyhow::bail!("ENOSYS: fsync not supported")
    }

    /// Create a directory.
    ///
    /// For SeaweedFS-backed VFS, directories are implicit (derived from
    /// index entry paths). This creates a directory marker index entry.
    async fn mkdir(&self, _parent: &str, _name: &OsStr, _mode: u32) -> Result<VfsAttr> {
        anyhow::bail!("ENOSYS: mkdir not supported")
    }

    /// Remove a file from a directory.
    ///
    /// Deletes the index entry, manifest, and chunks from remote storage.
    async fn unlink(&self, _parent: &str, _name: &OsStr) -> Result<()> {
        anyhow::bail!("ENOSYS: unlink not supported")
    }

    /// Remove an empty directory.
    async fn rmdir(&self, _parent: &str, _name: &OsStr) -> Result<()> {
        anyhow::bail!("ENOSYS: rmdir not supported")
    }

    /// Rename a file or directory.
    async fn rename(
        &self,
        _from_parent: &str,
        _from_name: &OsStr,
        _to_parent: &str,
        _to_name: &OsStr,
    ) -> Result<()> {
        anyhow::bail!("ENOSYS: rename not supported")
    }

    /// Set file attributes (permissions, timestamps, size).
    ///
    /// Supports truncate (setting size), chmod, and utimensat.
    async fn setattr(&self, _path: &str, _attr: &VfsAttr) -> Result<VfsAttr> {
        anyhow::bail!("ENOSYS: setattr not supported")
    }
}
