//! Platform-agnostic filesystem types.
//!
//! These types are used by the `VirtualFilesystem` trait and its implementations
//! (FUSE, NFS, FileProvider). They deliberately avoid any dependency on fuse3,
//! NFS protocol types, or Apple framework types.

use std::time::SystemTime;

/// File type (regular file or directory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsFileType {
    RegularFile,
    Directory,
}

/// File/directory attributes — the VFS equivalent of `stat`.
#[derive(Debug, Clone)]
pub struct VfsAttr {
    /// File size in bytes (for directories, 0)
    pub size: u64,
    /// Number of 512-byte blocks
    pub blocks: u64,
    /// Last access time
    pub atime: SystemTime,
    /// Last modification time
    pub mtime: SystemTime,
    /// Last status change time
    pub ctime: SystemTime,
    /// File type
    pub kind: VfsFileType,
    /// Permission bits (e.g. 0o444)
    pub perm: u16,
    /// Number of hard links
    pub nlink: u32,
    /// Owner user ID
    pub uid: u32,
    /// Owner group ID
    pub gid: u32,
}

impl VfsAttr {
    /// Create attributes for a regular file with the given size.
    pub fn file(size: u64, uid: u32, gid: u32, mtime: SystemTime) -> Self {
        VfsAttr {
            size,
            blocks: size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
            kind: VfsFileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid,
            gid,
        }
    }

    /// Create attributes for a directory.
    pub fn dir(uid: u32, gid: u32, mtime: SystemTime) -> Self {
        VfsAttr {
            size: 0,
            blocks: 0,
            atime: mtime,
            mtime,
            ctime: mtime,
            kind: VfsFileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid,
            gid,
        }
    }
}

/// A directory entry returned by `readdir`.
#[derive(Debug, Clone)]
pub struct VfsDirEntry {
    /// Entry name (file or subdirectory name, not full path)
    pub name: String,
    /// File type
    pub kind: VfsFileType,
    /// Optional attributes (for readdirplus-style responses)
    pub attr: Option<VfsAttr>,
}

/// Statistics about the filesystem (statfs equivalent).
#[derive(Debug, Clone)]
pub struct VfsStatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

impl Default for VfsStatFs {
    fn default() -> Self {
        VfsStatFs {
            blocks: 1 << 30,
            bfree: 1 << 29,
            bavail: 1 << 29,
            files: 1 << 20,
            ffree: 1 << 19,
            bsize: 4096,
            namelen: 255,
            frsize: 4096,
        }
    }
}
