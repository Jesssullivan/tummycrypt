//! tcfs-fuse: async FUSE driver with .tc/.tcf stub support and on-demand hydration
//!
//! Linux: fuse3 crate (kernel FUSE)
//! macOS: fuse3 with macFUSE 4.x (feature: macos-fuse)
//! EROFS/fscache: Linux 5.19+ (feature: erofs, stretch goal)

pub mod cache;
pub mod driver;
pub mod erofs;
pub mod hydrate;
pub mod negative_cache;
pub mod stub;

// Re-export the mount API when the fuse feature is enabled
#[cfg(feature = "fuse")]
pub use driver::{mount, MountConfig};

pub use cache::DiskCache;
pub use negative_cache::NegativeCache;
pub use stub::{is_stub_path, real_to_stub_name, stub_to_real_name, IndexEntry, StubMeta};
