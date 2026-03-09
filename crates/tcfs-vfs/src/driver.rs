//! Concrete `VirtualFilesystem` implementation for tcfs.
//!
//! Maps a SeaweedFS prefix to a virtual directory tree with `.tc` stub files.
//! This is the core filesystem logic, decoupled from any specific mount
//! protocol (FUSE, NFS, FileProvider).
//!
//! ## Virtual filesystem layout
//!
//! ```text
//! SeaweedFS:
//!   {prefix}/index/src/main.rs     -> size, hash
//!   {prefix}/index/README.md       -> size, hash
//!
//! Virtual tree:
//!   /src/
//!     main.rs.tc   (0-byte stub shown as real size from index)
//!   /README.md.tc
//! ```

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use async_trait::async_trait;
use opendal::Operator;
use tokio::sync::Mutex;
use tracing::debug;

use crate::cache::DiskCache;
use crate::hydrate::fetch_cached;
use crate::negative_cache::NegativeCache;
use crate::stub::IndexEntry;
use crate::types::{VfsAttr, VfsDirEntry, VfsFileType, VfsStatFs};
use crate::vfs::VirtualFilesystem;

/// An open file handle — holds hydrated content in memory.
struct FileHandle {
    data: Vec<u8>,
}

/// The tcfs virtual filesystem driver.
///
/// Protocol-agnostic: implements `VirtualFilesystem` for use by FUSE, NFS,
/// or any other mount backend.
pub struct TcfsVfs {
    op: Operator,
    prefix: String,
    uid: u32,
    gid: u32,
    negative_cache: Arc<NegativeCache>,
    disk_cache: Arc<DiskCache>,
    /// Open file handles: fh -> hydrated bytes
    handles: Arc<Mutex<HashMap<u64, FileHandle>>>,
    /// Monotonically increasing file-handle counter
    next_fh: Arc<AtomicU64>,
    /// Mount timestamp (used as atime/mtime for all synthetic entries)
    mount_time: SystemTime,
}

impl TcfsVfs {
    /// Create a new virtual filesystem driver.
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
        #[cfg(unix)]
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (0u32, 0u32);
        TcfsVfs {
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

    /// Access the underlying disk cache (for stats, inspection).
    pub fn disk_cache(&self) -> &DiskCache {
        &self.disk_cache
    }

    /// Build the index path for a virtual FS path.
    ///
    /// `/src/main.rs.tc` -> `{prefix}/index/src/main.rs`
    fn index_key_for(&self, vpath: &str) -> Option<String> {
        let rel = vpath.trim_start_matches('/');
        if rel.is_empty() {
            return None;
        }
        let real = rel
            .strip_suffix(".tc")
            .or_else(|| rel.strip_suffix(".tcf"))
            .unwrap_or(rel);
        let prefix = self.prefix.trim_end_matches('/');
        if prefix.is_empty() {
            Some(format!("index/{}", real))
        } else {
            Some(format!("{}/index/{}", prefix, real))
        }
    }

    /// The index prefix for directory listing: `{prefix}/index/{rel_dir}/`
    fn index_prefix_for_dir(&self, vdir: &str) -> String {
        let rel = vdir.trim_start_matches('/').trim_end_matches('/');
        let prefix = self.prefix.trim_end_matches('/');
        match (prefix.is_empty(), rel.is_empty()) {
            (true, true) => "index/".to_string(),
            (true, false) => format!("index/{}/", rel),
            (false, true) => format!("{}/index/", prefix),
            (false, false) => format!("{}/index/{}/", prefix, rel),
        }
    }

    /// Discover prefixes that have index entries (bucket root scan).
    async fn discover_prefixes(&self) -> Vec<String> {
        let entries = match self.op.list("/").await {
            Ok(e) => e,
            Err(_) => match self.op.list("").await {
                Ok(e) => e,
                Err(_) => return vec![],
            },
        };
        entries
            .into_iter()
            .filter_map(|e| {
                let p = e.path().trim_end_matches('/').to_string();
                if !p.is_empty() && !p.contains('/') {
                    Some(p)
                } else {
                    None
                }
            })
            .collect()
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

    /// Synthesize file attributes.
    fn file_attr(&self, size: u64) -> VfsAttr {
        VfsAttr::file(size, self.uid, self.gid, self.mount_time)
    }

    /// Synthesize directory attributes.
    fn dir_attr(&self) -> VfsAttr {
        VfsAttr::dir(self.uid, self.gid, self.mount_time)
    }

    /// Core readdir logic returning `VfsDirEntry` with optional attrs.
    async fn readdir_impl(&self, path: &str, with_attrs: bool) -> Result<Vec<VfsDirEntry>> {
        let index_prefix = self.index_prefix_for_dir(path);

        let raw_entries = self
            .op
            .list(&index_prefix)
            .await
            .context("listing index entries")?;

        let mut seen_dirs: HashSet<String> = HashSet::new();
        let mut entries: Vec<VfsDirEntry> = Vec::new();

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

            if is_dir {
                let dir_name = first_component.trim_end_matches('/').to_string();
                if seen_dirs.contains(&dir_name) {
                    continue;
                }
                seen_dirs.insert(dir_name.clone());
                entries.push(VfsDirEntry {
                    name: dir_name,
                    kind: VfsFileType::Directory,
                    attr: if with_attrs {
                        Some(self.dir_attr())
                    } else {
                        None
                    },
                });
            } else {
                let stub_name = format!("{}.tc", first_component);
                let attr = if with_attrs {
                    let size = self.read_index_entry_size(&full_path).await;
                    Some(self.file_attr(size))
                } else {
                    None
                };
                entries.push(VfsDirEntry {
                    name: stub_name,
                    kind: VfsFileType::RegularFile,
                    attr,
                });
            }
        }

        // Fallback: if root dir is empty and prefix is empty, discover prefixes
        if path == "/" && self.prefix.is_empty() && entries.is_empty() {
            let prefixes = self.discover_prefixes().await;
            for pfx in prefixes {
                let probe = format!("{}/index/", pfx);
                if let Ok(idx_entries) = self.op.list(&probe).await {
                    if !idx_entries.is_empty() && !seen_dirs.contains(&pfx) {
                        seen_dirs.insert(pfx.clone());
                        entries.push(VfsDirEntry {
                            name: pfx,
                            kind: VfsFileType::Directory,
                            attr: if with_attrs {
                                Some(self.dir_attr())
                            } else {
                                None
                            },
                        });
                    }
                }
            }
        }

        Ok(entries)
    }
}

#[async_trait]
impl VirtualFilesystem for TcfsVfs {
    async fn getattr(&self, path: &str) -> Result<VfsAttr> {
        // Root directory
        if path == "/" {
            return Ok(self.dir_attr());
        }

        // Negative cache short-circuit
        if self.negative_cache.is_negative(path) {
            anyhow::bail!("ENOENT (negative cache): {}", path);
        }

        // Stub file (.tc / .tcf)
        if path.ends_with(".tc") || path.ends_with(".tcf") {
            match self.get_index_entry(path).await {
                Some(entry) => return Ok(self.file_attr(entry.size)),
                None => {
                    self.negative_cache.insert(path);
                    anyhow::bail!("ENOENT: {}", path);
                }
            }
        }

        // Directory: check if any index entries exist under it
        let dir_prefix = self.index_prefix_for_dir(path);
        match self.op.list(&dir_prefix).await {
            Ok(entries) if !entries.is_empty() => Ok(self.dir_attr()),
            _ => {
                self.negative_cache.insert(path);
                anyhow::bail!("ENOENT: {}", path);
            }
        }
    }

    async fn lookup(&self, parent: &str, name: &OsStr) -> Result<VfsAttr> {
        let name_str = name.to_str().context("non-UTF-8 filename")?;

        let full_path = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        self.getattr(&full_path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
        self.readdir_impl(path, false).await
    }

    async fn readdirplus(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
        self.readdir_impl(path, true).await
    }

    async fn open(&self, path: &str) -> Result<(u64, Vec<u8>)> {
        // Only handle .tc stub files
        if !path.ends_with(".tc") && !path.ends_with(".tcf") {
            anyhow::bail!("not a stub file: {}", path);
        }

        let entry = self
            .get_index_entry(path)
            .await
            .context(format!("index entry not found: {}", path))?;

        let manifest_path = entry.manifest_path(&self.prefix);
        let prefix = self.prefix.trim_end_matches('/');

        debug!(path = %path, manifest = %manifest_path, "hydrating on open");

        let data = fetch_cached(&self.op, &manifest_path, prefix, &self.disk_cache)
            .await
            .with_context(|| format!("hydration failed: {}", path))?;

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles
            .lock()
            .await
            .insert(fh, FileHandle { data: data.clone() });

        Ok((fh, data))
    }

    async fn read(&self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>> {
        let handles = self.handles.lock().await;
        let handle = handles
            .get(&fh)
            .context(format!("bad file handle: {}", fh))?;

        let data = &handle.data;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(Vec::new());
        }
        let end = (start + size as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }

    async fn release(&self, fh: u64) -> Result<()> {
        self.handles.lock().await.remove(&fh);
        Ok(())
    }

    async fn statfs(&self) -> Result<VfsStatFs> {
        Ok(VfsStatFs::default())
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a TcfsVfs with a given prefix
    fn make_vfs(prefix: &str) -> TcfsVfs {
        let op = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        TcfsVfs::new(
            op,
            prefix.to_string(),
            std::path::PathBuf::from("/tmp/tcfs-test-cache"),
            64 * 1024 * 1024,
            Duration::from_secs(30),
        )
    }

    #[test]
    fn test_index_prefix_empty_prefix_root() {
        let vfs = make_vfs("");
        assert_eq!(vfs.index_prefix_for_dir("/"), "index/");
    }

    #[test]
    fn test_index_prefix_empty_prefix_subdir() {
        let vfs = make_vfs("");
        assert_eq!(vfs.index_prefix_for_dir("/src"), "index/src/");
    }

    #[test]
    fn test_index_prefix_with_prefix_root() {
        let vfs = make_vfs("data");
        assert_eq!(vfs.index_prefix_for_dir("/"), "data/index/");
    }

    #[test]
    fn test_index_prefix_with_prefix_subdir() {
        let vfs = make_vfs("data");
        assert_eq!(vfs.index_prefix_for_dir("/src"), "data/index/src/");
    }

    #[test]
    fn test_index_key_empty_prefix() {
        let vfs = make_vfs("");
        assert_eq!(
            vfs.index_key_for("/file.txt.tc"),
            Some("index/file.txt".to_string())
        );
    }

    #[test]
    fn test_index_key_with_prefix() {
        let vfs = make_vfs("data");
        assert_eq!(
            vfs.index_key_for("/file.txt.tc"),
            Some("data/index/file.txt".to_string())
        );
    }

    #[test]
    fn test_index_key_strips_tc_suffix() {
        let vfs = make_vfs("data");
        assert_eq!(
            vfs.index_key_for("/src/main.rs.tc"),
            Some("data/index/src/main.rs".to_string())
        );
    }

    #[test]
    fn test_index_key_strips_tcf_suffix() {
        let vfs = make_vfs("data");
        assert_eq!(
            vfs.index_key_for("/doc.pdf.tcf"),
            Some("data/index/doc.pdf".to_string())
        );
    }

    #[test]
    fn test_index_key_root_returns_none() {
        let vfs = make_vfs("data");
        assert_eq!(vfs.index_key_for("/"), None);
    }
}
