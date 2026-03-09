//! tcfs-fuse: FUSE mount adapter for `tcfs-vfs::VirtualFilesystem`.
//!
//! This crate provides the FUSE-specific mount logic. All filesystem operations
//! are delegated to `tcfs-vfs::TcfsVfs` via the `VirtualFilesystem` trait.
//!
//! Shared infrastructure (DiskCache, NegativeCache, stub formats, hydration)
//! lives in `tcfs-vfs` and is re-exported here for backward compatibility.

pub mod driver;
pub mod erofs;

// ── Re-exports from tcfs-vfs (backward compatibility) ────────────────────────
//
// These types previously lived in this crate. They have been moved to tcfs-vfs
// but are re-exported here so that existing consumers (tcfs-cli, tcfsd) continue
// to compile without changes.

// Modules re-exported as submodules
pub use tcfs_vfs::cache;
pub use tcfs_vfs::hydrate;
pub use tcfs_vfs::negative_cache;
pub use tcfs_vfs::stub;

// Direct type re-exports
pub use tcfs_vfs::{
    cache_key_for_path, fetch_cached, fetch_content, is_stub_path, real_to_stub_name,
    stub_to_real_name, CacheStats, DiskCache, IndexEntry, NegativeCache, StubMeta, TcfsVfs,
    VfsAttr, VfsDirEntry, VfsFileType, VfsStatFs, VirtualFilesystem,
};

// Re-export the mount API when the fuse feature is enabled
#[cfg(feature = "fuse")]
pub use driver::{mount, MountConfig};
