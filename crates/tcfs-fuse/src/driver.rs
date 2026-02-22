//! FUSE filesystem driver: mounts a SeaweedFS prefix as a local directory.
//!
//! ## Virtual filesystem layout
//!
//! The driver maps SeaweedFS index entries to a virtual directory tree:
//!
//! ```text
//! SeaweedFS:
//!   {prefix}/index/src/main.rs     → size, hash
//!   {prefix}/index/src/lib.rs      → size, hash
//!   {prefix}/index/README.md       → size, hash
//!
//! FUSE mountpoint /mnt/tcfs:
//!   /mnt/tcfs/
//!     src/
//!       main.rs.tc   (0-byte stub shown as real size from index)
//!       lib.rs.tc
//!     README.md.tc
//! ```
//!
//! On `open()` of a `.tc` file, the content is fetched from SeaweedFS (via
//! the manifest) and served transparently. Fetched content is cached in `DiskCache`.

#[cfg(feature = "fuse")]
mod inner {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use bytes::Bytes;
    use fuse3::path::prelude::*;
    use fuse3::{Errno, FileType, MountOptions};
    use futures_util::stream;
    use opendal::Operator;
    use tokio::sync::Mutex;
    use tracing::{debug, info, warn};

    use crate::cache::DiskCache;
    use crate::hydrate::fetch_cached;
    use crate::negative_cache::NegativeCache;
    use crate::stub::IndexEntry;

    // ── Configuration ─────────────────────────────────────────────────────────

    /// TTL for positive dentry/attr cache entries (FUSE kernel cache)
    const ATTR_TTL: Duration = Duration::from_secs(5);

    /// Fake uid/gid used for all files (real process uid/gid set at mount)
    const PERM_FILE: u16 = 0o444; // r--r--r--
    const PERM_DIR: u16 = 0o555; // r-xr-xr-x

    // ── File handle table ─────────────────────────────────────────────────────

    /// An open file handle — holds hydrated content in memory.
    struct FileHandle {
        data: Vec<u8>,
    }

    // ── TcfsFs ────────────────────────────────────────────────────────────────

    /// The FUSE filesystem driver.
    pub struct TcfsFs {
        op: Operator,
        prefix: String,
        uid: u32,
        gid: u32,
        negative_cache: Arc<NegativeCache>,
        disk_cache: Arc<DiskCache>,
        /// Open file handles: fh → hydrated bytes
        handles: Arc<Mutex<HashMap<u64, FileHandle>>>,
        /// Monotonically increasing file-handle counter
        next_fh: Arc<AtomicU64>,
        /// Mount timestamp (used as atime/mtime for all synthetic entries)
        mount_time: SystemTime,
    }

    impl TcfsFs {
        /// Create a new FUSE filesystem driver.
        ///
        /// - `op` — OpenDAL operator for the SeaweedFS bucket
        /// - `prefix` — remote prefix (e.g. `mydata`)
        /// - `cache_dir` — local dir for hydrated file cache
        /// - `cache_max_bytes` — max disk cache size
        /// - `negative_ttl` — TTL for negative dentry cache
        pub fn new(
            op: Operator,
            prefix: String,
            cache_dir: std::path::PathBuf,
            cache_max_bytes: u64,
            negative_ttl: Duration,
        ) -> Self {
            let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
            TcfsFs {
                op,
                prefix,
                uid,
                gid,
                negative_cache: Arc::new(NegativeCache::new(negative_ttl)),
                disk_cache: Arc::new(DiskCache::new(cache_dir, cache_max_bytes)),
                handles: Arc::new(Mutex::new(HashMap::new())),
                next_fh: Arc::new(AtomicU64::new(1)),
                mount_time: SystemTime::now(),
            }
        }

        /// Build the index path for a virtual FS path.
        ///
        /// `/src/main.rs.tc` → `{prefix}/index/src/main.rs`
        fn index_key_for(&self, vpath: &str) -> Option<String> {
            // Strip leading slash
            let rel = vpath.trim_start_matches('/');
            if rel.is_empty() {
                return None; // root directory — no index key
            }
            // Strip .tc suffix to get the real filename
            let real = rel
                .strip_suffix(".tc")
                .or_else(|| rel.strip_suffix(".tcf"))
                .unwrap_or(rel);
            Some(format!(
                "{}/index/{}",
                self.prefix.trim_end_matches('/'),
                real
            ))
        }

        /// The index prefix for directory listing: `{prefix}/index/{rel_dir}/`
        fn index_prefix_for_dir(&self, vdir: &str) -> String {
            let rel = vdir.trim_start_matches('/').trim_end_matches('/');
            let prefix = self.prefix.trim_end_matches('/');
            if rel.is_empty() {
                format!("{}/index/", prefix)
            } else {
                format!("{}/index/{}/", prefix, rel)
            }
        }

        /// Fetch and parse an IndexEntry for a virtual path.
        async fn get_index_entry(&self, vpath: &str) -> Option<IndexEntry> {
            let key = self.index_key_for(vpath)?;
            let data = self.op.read(&key).await.ok()?;
            let text = String::from_utf8(data.to_bytes().to_vec()).ok()?;
            IndexEntry::parse(&text).ok()
        }

        /// Fetch the real file size from an index entry by its S3 key.
        async fn read_index_entry_size(&self, index_key: &str) -> u64 {
            match self.op.read(index_key).await {
                Ok(data) => {
                    let text = String::from_utf8(data.to_bytes().to_vec()).unwrap_or_default();
                    IndexEntry::parse(&text).map(|e| e.size).unwrap_or(0)
                }
                Err(_) => 0,
            }
        }

        /// Synthesize a `FileAttr` for a stub file given its size.
        fn file_attr(&self, size: u64) -> FileAttr {
            FileAttr {
                size,
                blocks: size.div_ceil(512),
                atime: self.mount_time,
                mtime: self.mount_time,
                ctime: self.mount_time,
                #[cfg(target_os = "macos")]
                crtime: self.mount_time,
                kind: FileType::RegularFile,
                perm: PERM_FILE,
                nlink: 1,
                uid: self.uid,
                gid: self.gid,
                rdev: 0,
                blksize: 4096,
                #[cfg(target_os = "macos")]
                flags: 0,
            }
        }

        /// Synthesize a `FileAttr` for a directory.
        fn dir_attr(&self) -> FileAttr {
            FileAttr {
                size: 0,
                blocks: 0,
                atime: self.mount_time,
                mtime: self.mount_time,
                ctime: self.mount_time,
                #[cfg(target_os = "macos")]
                crtime: self.mount_time,
                kind: FileType::Directory,
                perm: PERM_DIR,
                nlink: 2,
                uid: self.uid,
                gid: self.gid,
                rdev: 0,
                blksize: 4096,
                #[cfg(target_os = "macos")]
                flags: 0,
            }
        }
    }

    // ── PathFilesystem impl ────────────────────────────────────────────────────

    impl PathFilesystem for TcfsFs {
        async fn init(&self, _req: Request) -> fuse3::Result<ReplyInit> {
            debug!(prefix = %self.prefix, "tcfs-fuse init");
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

            // Root directory
            if path_str == "/" {
                return Ok(ReplyAttr {
                    ttl: ATTR_TTL,
                    attr: self.dir_attr(),
                });
            }

            // Negative cache short-circuit
            if self.negative_cache.is_negative(path_str) {
                return Err(Errno::from(libc::ENOENT));
            }

            // Check if it's a stub file (.tc)
            if path_str.ends_with(".tc") || path_str.ends_with(".tcf") {
                match self.get_index_entry(path_str).await {
                    Some(entry) => {
                        return Ok(ReplyAttr {
                            ttl: ATTR_TTL,
                            attr: self.file_attr(entry.size),
                        });
                    }
                    None => {
                        self.negative_cache.insert(path_str);
                        return Err(Errno::from(libc::ENOENT));
                    }
                }
            }

            // Otherwise treat as a directory: check if any index entries exist under it
            let dir_prefix = self.index_prefix_for_dir(path_str);
            match self.op.list(&dir_prefix).await {
                Ok(entries) if !entries.is_empty() => Ok(ReplyAttr {
                    ttl: ATTR_TTL,
                    attr: self.dir_attr(),
                }),
                _ => {
                    self.negative_cache.insert(path_str);
                    Err(Errno::from(libc::ENOENT))
                }
            }
        }

        async fn lookup(
            &self,
            _req: Request,
            parent: &OsStr,
            name: &OsStr,
        ) -> fuse3::Result<ReplyEntry> {
            let parent_str = parent.to_str().unwrap_or("/");
            let name_str = name.to_str().ok_or(Errno::from(libc::ENOENT))?;

            let full_path = if parent_str == "/" {
                format!("/{}", name_str)
            } else {
                format!("{}/{}", parent_str.trim_end_matches('/'), name_str)
            };

            // Negative cache
            if self.negative_cache.is_negative(&full_path) {
                return Err(Errno::from(libc::ENOENT));
            }

            // Stub file lookup
            if name_str.ends_with(".tc") || name_str.ends_with(".tcf") {
                match self.get_index_entry(&full_path).await {
                    Some(entry) => {
                        return Ok(ReplyEntry {
                            ttl: ATTR_TTL,
                            attr: self.file_attr(entry.size),
                        });
                    }
                    None => {
                        self.negative_cache.insert(&full_path);
                        return Err(Errno::from(libc::ENOENT));
                    }
                }
            }

            // Directory lookup
            let dir_prefix = self.index_prefix_for_dir(&full_path);
            match self.op.list(&dir_prefix).await {
                Ok(entries) if !entries.is_empty() => Ok(ReplyEntry {
                    ttl: ATTR_TTL,
                    attr: self.dir_attr(),
                }),
                _ => {
                    self.negative_cache.insert(&full_path);
                    Err(Errno::from(libc::ENOENT))
                }
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
            let index_prefix = self.index_prefix_for_dir(path_str);

            let raw_entries = self
                .op
                .list(&index_prefix)
                .await
                .map_err(|_| Errno::from(libc::EIO))?;

            let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            for entry in raw_entries {
                let full_path = entry.path();
                let rel = full_path
                    .trim_start_matches(&index_prefix)
                    .trim_start_matches('/');
                if rel.is_empty() {
                    continue;
                }

                let first_component = rel.split('/').next().unwrap_or(rel);
                let is_dir = rel.contains('/') || rel.ends_with('/');

                let (dir_entry_name, kind) = if is_dir {
                    let dir_name = first_component.trim_end_matches('/').to_string();
                    if seen_dirs.contains(&dir_name) {
                        continue;
                    }
                    seen_dirs.insert(dir_name.clone());
                    (dir_name, FileType::Directory)
                } else {
                    let stub_name = format!("{}.tc", first_component);
                    (stub_name, FileType::RegularFile)
                };

                if next_offset > offset {
                    entries.push(Ok(DirectoryEntry {
                        kind,
                        name: dir_entry_name.into(),
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
            let index_prefix = self.index_prefix_for_dir(path_str);

            let raw_entries = self
                .op
                .list(&index_prefix)
                .await
                .map_err(|_| Errno::from(libc::EIO))?;

            let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut entries: Vec<fuse3::Result<DirectoryEntryPlus>> = Vec::new();
            let offset = offset as i64;

            if offset == 0 {
                entries.push(Ok(DirectoryEntryPlus {
                    kind: FileType::Directory,
                    name: ".".into(),
                    offset: 1,
                    attr: self.dir_attr(),
                    entry_ttl: ATTR_TTL,
                    attr_ttl: ATTR_TTL,
                }));
            }
            if offset <= 1 {
                entries.push(Ok(DirectoryEntryPlus {
                    kind: FileType::Directory,
                    name: "..".into(),
                    offset: 2,
                    attr: self.dir_attr(),
                    entry_ttl: ATTR_TTL,
                    attr_ttl: ATTR_TTL,
                }));
            }

            let mut next_offset = 3i64;
            for entry in raw_entries {
                let full_path = entry.path().to_string();
                let rel = full_path
                    .trim_start_matches(&index_prefix)
                    .trim_start_matches('/');
                if rel.is_empty() {
                    continue;
                }

                let first_component = rel.split('/').next().unwrap_or(rel);
                let is_dir = rel.contains('/') || rel.ends_with('/');

                let (dir_entry_name, kind, attr) = if is_dir {
                    let dir_name = first_component.trim_end_matches('/').to_string();
                    if seen_dirs.contains(&dir_name) {
                        continue;
                    }
                    seen_dirs.insert(dir_name.clone());
                    (dir_name, FileType::Directory, self.dir_attr())
                } else {
                    let stub_name = format!("{}.tc", first_component);
                    // Read actual file size from the index entry content
                    let size = self.read_index_entry_size(&full_path).await;
                    (stub_name, FileType::RegularFile, self.file_attr(size))
                };

                if next_offset > offset {
                    entries.push(Ok(DirectoryEntryPlus {
                        kind,
                        name: dir_entry_name.into(),
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

            // Only handle .tc stub files
            if !path_str.ends_with(".tc") && !path_str.ends_with(".tcf") {
                return Err(Errno::from(libc::ENOENT));
            }

            let entry = self
                .get_index_entry(path_str)
                .await
                .ok_or(Errno::from(libc::ENOENT))?;

            let manifest_path = entry.manifest_path(&self.prefix);
            let prefix = self.prefix.trim_end_matches('/');

            debug!(path = %path_str, manifest = %manifest_path, "hydrating on open");

            // Fetch content (disk-cache backed)
            let data = fetch_cached(&self.op, &manifest_path, prefix, &self.disk_cache)
                .await
                .map_err(|e| {
                    warn!(path = %path_str, "hydration failed: {e}");
                    Errno::from(libc::EIO)
                })?;

            // Store in handle table
            let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
            self.handles.lock().await.insert(fh, FileHandle { data });

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
            let handles = self.handles.lock().await;
            let handle = handles.get(&fh).ok_or(Errno::from(libc::EBADF))?;

            let data = &handle.data;
            let start = offset as usize;
            if start >= data.len() {
                return Ok(ReplyData { data: Bytes::new() });
            }
            let end = (start + size as usize).min(data.len());
            let slice = Bytes::copy_from_slice(&data[start..end]);

            Ok(ReplyData { data: slice })
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
            self.handles.lock().await.remove(&fh);
            Ok(())
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
            Ok(ReplyStatFs {
                blocks: 1 << 30, // fake 1T blocks
                bfree: 1 << 29,
                bavail: 1 << 29,
                files: 1 << 20,
                ffree: 1 << 19,
                bsize: 4096,
                namelen: 255,
                frsize: 4096,
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
    /// Call from an async context. Returns when the filesystem is unmounted
    /// (e.g. via `fusermount3 -u <mountpoint>` or `tcfs unmount`).
    pub async fn mount(cfg: MountConfig) -> std::io::Result<()> {
        let fs = TcfsFs::new(
            cfg.op,
            cfg.prefix,
            cfg.cache_dir,
            cfg.cache_max_bytes,
            Duration::from_secs(cfg.negative_ttl_secs),
        );

        let mut opts = MountOptions::default();
        opts.fs_name("tcfs");
        opts.read_only(cfg.read_only);
        opts.force_readdir_plus(true);
        if cfg.allow_other {
            opts.allow_other(true);
        }

        info!(mountpoint = %cfg.mountpoint.display(), "mounting tcfs (unprivileged via fusermount3)");

        let handle = Session::new(opts)
            .mount_with_unprivileged(fs, &cfg.mountpoint)
            .await?;

        handle.await
    }
}

#[cfg(feature = "fuse")]
pub use inner::{mount, MountConfig, TcfsFs};
