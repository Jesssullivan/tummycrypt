//! tcfs-nfs: FUSE-free NFS loopback mount for tcfs.
//!
//! This crate provides an embedded NFSv3 server that serves tcfs content
//! via the kernel's built-in NFS client. No kernel modules required.
//!
//! ## Architecture
//!
//! ```text
//! Application (cat, vim, ls)
//!   → Kernel VFS
//!   → Kernel NFS client (built-in)
//!   → localhost:PORT (TCP)
//!   → NfsAdapter (this crate)
//!   → VirtualFilesystem (tcfs-vfs)
//!   → OpenDAL → SeaweedFS
//! ```
//!
//! ## Usage
//!
//! ```no_run
//! use tcfs_nfs::{NfsMountConfig, serve_and_mount};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let cfg = NfsMountConfig {
//!     op: todo!("OpenDAL operator"),
//!     prefix: "mydata".to_string(),
//!     mountpoint: "/mnt/tcfs".into(),
//!     cache_dir: "/tmp/tcfs-cache".into(),
//!     cache_max_bytes: 256 * 1024 * 1024,
//!     negative_ttl_secs: 30,
//!     port: 0, // auto-assign
//! };
//! serve_and_mount(cfg).await?;
//! # Ok(())
//! # }
//! ```

pub mod adapter;
pub mod inode;
pub mod server;

pub use adapter::NfsAdapter;
pub use server::{is_mounted, serve_and_mount, serve_only, NfsMount, NfsMountConfig};
