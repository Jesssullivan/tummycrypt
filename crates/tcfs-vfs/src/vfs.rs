//! The `VirtualFilesystem` trait — platform-agnostic filesystem operations.
//!
//! This trait abstracts the filesystem logic shared between all backends:
//! - FUSE (Linux legacy, macOS legacy)
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
    /// For stub-based filesystems, this triggers hydration (fetching from
    /// remote storage). Returns the full file content as bytes.
    ///
    /// The returned `u64` is a file handle ID for use with `read` and `release`.
    async fn open(&self, path: &str) -> Result<(u64, Vec<u8>)>;

    /// Read bytes from an open file handle.
    ///
    /// `fh` is the handle returned by `open`. Returns the requested slice.
    async fn read(&self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>>;

    /// Release (close) a file handle.
    async fn release(&self, fh: u64) -> Result<()>;

    /// Filesystem statistics (statfs equivalent).
    async fn statfs(&self) -> Result<VfsStatFs> {
        Ok(VfsStatFs::default())
    }
}
