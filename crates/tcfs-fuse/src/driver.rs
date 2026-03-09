//! FUSE mount adapter: translates fuse3 `PathFilesystem` calls to
//! `tcfs_vfs::VirtualFilesystem`.
//!
//! This is a thin protocol adapter. All filesystem logic lives in `tcfs-vfs`.

#[cfg(feature = "fuse")]
mod inner {
    use std::ffi::OsStr;
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use fuse3::path::prelude::*;
    use fuse3::{Errno, FileType, MountOptions};
    use futures_util::stream;
    use opendal::Operator;
    use tracing::{debug, info};

    use tcfs_vfs::types::VfsFileType;
    use tcfs_vfs::{TcfsVfs, VfsAttr, VirtualFilesystem};

    // ── Configuration ─────────────────────────────────────────────────────────

    /// TTL for positive dentry/attr cache entries (FUSE kernel cache)
    const ATTR_TTL: Duration = Duration::from_secs(5);

    // ── TcfsFs ────────────────────────────────────────────────────────────────

    /// The FUSE filesystem driver — thin wrapper around `TcfsVfs`.
    pub struct TcfsFs {
        vfs: Arc<TcfsVfs>,
    }

    impl TcfsFs {
        pub fn new(vfs: Arc<TcfsVfs>) -> Self {
            TcfsFs { vfs }
        }
    }

    /// Convert VfsAttr to fuse3 FileAttr.
    fn to_fuse_attr(attr: &VfsAttr) -> FileAttr {
        FileAttr {
            size: attr.size,
            blocks: attr.blocks,
            atime: attr.atime,
            mtime: attr.mtime,
            ctime: attr.ctime,
            #[cfg(target_os = "macos")]
            crtime: attr.mtime,
            kind: match attr.kind {
                VfsFileType::RegularFile => FileType::RegularFile,
                VfsFileType::Directory => FileType::Directory,
            },
            perm: attr.perm,
            nlink: attr.nlink,
            uid: attr.uid,
            gid: attr.gid,
            rdev: 0,
            blksize: 4096,
            #[cfg(target_os = "macos")]
            flags: 0,
        }
    }

    fn to_fuse_file_type(kind: VfsFileType) -> FileType {
        match kind {
            VfsFileType::RegularFile => FileType::RegularFile,
            VfsFileType::Directory => FileType::Directory,
        }
    }

    // ── PathFilesystem impl ────────────────────────────────────────────────────

    impl PathFilesystem for TcfsFs {
        async fn init(&self, _req: Request) -> fuse3::Result<ReplyInit> {
            debug!("tcfs-fuse init (delegating to tcfs-vfs)");
            Ok(ReplyInit {
                max_write: NonZeroU32::new(128 * 1024).unwrap(),
            })
        }

        async fn destroy(&self, _req: Request) {
            info!("tcfs-fuse unmounted");
        }

        async fn getattr(
            &self,
            _req: Request,
            path: Option<&OsStr>,
            _fh: Option<u64>,
            _flags: u32,
        ) -> fuse3::Result<ReplyAttr> {
            let path_str = match path.and_then(|p| p.to_str()) {
                Some(p) => p,
                None => return Err(Errno::from(libc::ENOENT)),
            };

            match self.vfs.getattr(path_str).await {
                Ok(attr) => Ok(ReplyAttr {
                    ttl: ATTR_TTL,
                    attr: to_fuse_attr(&attr),
                }),
                Err(_) => Err(Errno::from(libc::ENOENT)),
            }
        }

        async fn lookup(
            &self,
            _req: Request,
            parent: &OsStr,
            name: &OsStr,
        ) -> fuse3::Result<ReplyEntry> {
            let parent_str = parent.to_str().unwrap_or("/");

            match self.vfs.lookup(parent_str, name).await {
                Ok(attr) => Ok(ReplyEntry {
                    ttl: ATTR_TTL,
                    attr: to_fuse_attr(&attr),
                }),
                Err(_) => Err(Errno::from(libc::ENOENT)),
            }
        }

        // Directory entry stream types
        type DirEntryStream<'a>
            = stream::Iter<std::vec::IntoIter<fuse3::Result<DirectoryEntry>>>
        where
            Self: 'a;

        type DirEntryPlusStream<'a>
            = stream::Iter<std::vec::IntoIter<fuse3::Result<DirectoryEntryPlus>>>
        where
            Self: 'a;

        async fn readdir<'a>(
            &'a self,
            _req: Request,
            path: &'a OsStr,
            _fh: u64,
            offset: i64,
        ) -> fuse3::Result<ReplyDirectory<Self::DirEntryStream<'a>>> {
            let path_str = path.to_str().unwrap_or("/");

            let vfs_entries = self
                .vfs
                .readdir(path_str)
                .await
                .map_err(|_| Errno::from(libc::EIO))?;

            let mut entries: Vec<fuse3::Result<DirectoryEntry>> = Vec::new();

            if offset == 0 {
                entries.push(Ok(DirectoryEntry {
                    kind: FileType::Directory,
                    name: ".".into(),
                    offset: 1,
                }));
            }
            if offset <= 1 {
                entries.push(Ok(DirectoryEntry {
                    kind: FileType::Directory,
                    name: "..".into(),
                    offset: 2,
                }));
            }

            let mut next_offset = 3i64;
            for vfs_entry in vfs_entries {
                if next_offset > offset {
                    entries.push(Ok(DirectoryEntry {
                        kind: to_fuse_file_type(vfs_entry.kind),
                        name: vfs_entry.name.into(),
                        offset: next_offset,
                    }));
                }
                next_offset += 1;
            }

            Ok(ReplyDirectory {
                entries: stream::iter(entries),
            })
        }

        async fn readdirplus<'a>(
            &'a self,
            _req: Request,
            path: &'a OsStr,
            _fh: u64,
            offset: u64,
            _lock_owner: u64,
        ) -> fuse3::Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'a>>> {
            let path_str = path.to_str().unwrap_or("/");
            let offset = offset as i64;

            let vfs_entries = self
                .vfs
                .readdirplus(path_str)
                .await
                .map_err(|_| Errno::from(libc::EIO))?;

            // We need a fallback attr for . and ..
            let dir_attr = self
                .vfs
                .getattr("/")
                .await
                .map(|a| to_fuse_attr(&a))
                .unwrap_or_else(|_| {
                    to_fuse_attr(&VfsAttr::dir(0, 0, std::time::SystemTime::now()))
                });

            let mut entries: Vec<fuse3::Result<DirectoryEntryPlus>> = Vec::new();

            if offset == 0 {
                entries.push(Ok(DirectoryEntryPlus {
                    kind: FileType::Directory,
                    name: ".".into(),
                    offset: 1,
                    attr: dir_attr,
                    entry_ttl: ATTR_TTL,
                    attr_ttl: ATTR_TTL,
                }));
            }
            if offset <= 1 {
                entries.push(Ok(DirectoryEntryPlus {
                    kind: FileType::Directory,
                    name: "..".into(),
                    offset: 2,
                    attr: dir_attr,
                    entry_ttl: ATTR_TTL,
                    attr_ttl: ATTR_TTL,
                }));
            }

            let mut next_offset = 3i64;
            for vfs_entry in vfs_entries {
                if next_offset > offset {
                    let attr = match vfs_entry.attr {
                        Some(ref a) => to_fuse_attr(a),
                        None => dir_attr, // fallback
                    };
                    entries.push(Ok(DirectoryEntryPlus {
                        kind: to_fuse_file_type(vfs_entry.kind),
                        name: vfs_entry.name.into(),
                        offset: next_offset,
                        attr,
                        entry_ttl: ATTR_TTL,
                        attr_ttl: ATTR_TTL,
                    }));
                }
                next_offset += 1;
            }

            Ok(ReplyDirectoryPlus {
                entries: stream::iter(entries),
            })
        }

        async fn opendir(
            &self,
            _req: Request,
            _path: &OsStr,
            _flags: u32,
        ) -> fuse3::Result<ReplyOpen> {
            Ok(ReplyOpen { fh: 0, flags: 0 })
        }

        async fn open(&self, _req: Request, path: &OsStr, _flags: u32) -> fuse3::Result<ReplyOpen> {
            let path_str = path.to_str().ok_or(Errno::from(libc::ENOENT))?;

            let (fh, _data) = self
                .vfs
                .open(path_str)
                .await
                .map_err(|_| Errno::from(libc::ENOENT))?;

            Ok(ReplyOpen { fh, flags: 0 })
        }

        async fn read(
            &self,
            _req: Request,
            _path: Option<&OsStr>,
            fh: u64,
            offset: u64,
            size: u32,
        ) -> fuse3::Result<ReplyData> {
            let data = self
                .vfs
                .read(fh, offset, size)
                .await
                .map_err(|_| Errno::from(libc::EBADF))?;

            Ok(ReplyData {
                data: Bytes::from(data),
            })
        }

        async fn release(
            &self,
            _req: Request,
            _path: Option<&OsStr>,
            fh: u64,
            _flags: u32,
            _lock_owner: u64,
            _flush: bool,
        ) -> fuse3::Result<()> {
            self.vfs
                .release(fh)
                .await
                .map_err(|_| Errno::from(libc::EIO))
        }

        async fn flush(
            &self,
            _req: Request,
            _path: Option<&OsStr>,
            _fh: u64,
            _lock_owner: u64,
        ) -> fuse3::Result<()> {
            Ok(())
        }

        async fn statfs(&self, _req: Request, _path: &OsStr) -> fuse3::Result<ReplyStatFs> {
            let stats = self
                .vfs
                .statfs()
                .await
                .map_err(|_| Errno::from(libc::EIO))?;

            Ok(ReplyStatFs {
                blocks: stats.blocks,
                bfree: stats.bfree,
                bavail: stats.bavail,
                files: stats.files,
                ffree: stats.ffree,
                bsize: stats.bsize,
                namelen: stats.namelen,
                frsize: stats.frsize,
            })
        }
    }

    // ── Public mount API ──────────────────────────────────────────────────────

    /// Mount configuration
    pub struct MountConfig {
        pub op: Operator,
        pub prefix: String,
        pub mountpoint: std::path::PathBuf,
        pub cache_dir: std::path::PathBuf,
        pub cache_max_bytes: u64,
        pub negative_ttl_secs: u64,
        pub read_only: bool,
        pub allow_other: bool,
    }

    /// Mount the FUSE filesystem and block until unmounted.
    ///
    /// Creates a `TcfsVfs` and wraps it with the FUSE `PathFilesystem` adapter.
    pub async fn mount(cfg: MountConfig) -> std::io::Result<()> {
        let vfs = Arc::new(TcfsVfs::new(
            cfg.op,
            cfg.prefix,
            cfg.cache_dir,
            cfg.cache_max_bytes,
            Duration::from_secs(cfg.negative_ttl_secs),
        ));

        let fs = TcfsFs::new(vfs);

        let mut opts = MountOptions::default();
        opts.fs_name("tcfs");
        opts.read_only(cfg.read_only);
        opts.force_readdir_plus(true);
        if cfg.allow_other {
            opts.allow_other(true);
        }

        info!(mountpoint = %cfg.mountpoint.display(), "mounting tcfs-fuse (via tcfs-vfs)");

        let handle = Session::new(opts)
            .mount_with_unprivileged(fs, &cfg.mountpoint)
            .await?;

        handle.await
    }
}

#[cfg(feature = "fuse")]
pub use inner::{mount, MountConfig, TcfsFs};
