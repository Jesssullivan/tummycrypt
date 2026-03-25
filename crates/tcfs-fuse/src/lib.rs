//! tcfs-fuse: FUSE3 mount adapter for `tcfs-vfs::VirtualFilesystem`.
//!
//! Presents SeaweedFS files as a local FUSE mount. All filesystem operations
//! are delegated to `tcfs-vfs::TcfsVfs` via the `VirtualFilesystem` trait.
//!
//! Uses `fuse3` crate with `unprivileged` feature — mounts via `fusermount3`
//! without needing root. No kernel modules beyond the built-in FUSE module.

#[cfg(feature = "fuse")]
pub mod driver;

#[cfg(feature = "fuse")]
pub use driver::{mount, MountConfig};
