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

/// An open file handle — holds content in memory, tracks write state.
struct FileHandle {
    /// Virtual path (e.g., "/src/main.rs.tc")
    path: String,
    /// File content in memory (hydrated on open, modified on write)
    data: Vec<u8>,
    /// True if data has been modified since open (needs flush on release)
    modified: bool,
}

/// Callback invoked after a file is flushed to remote storage.
/// Parameters: (virtual_path, file_hash, size_bytes, chunk_count)
pub type OnFlushCallback = Arc<dyn Fn(&str, &str, u64, usize) + Send + Sync + 'static>;

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
    /// Optional callback after flush_to_remote (e.g., NATS publish)
    on_flush: Option<OnFlushCallback>,
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
            on_flush: None,
        }
    }

    /// Set a callback invoked after each flush_to_remote.
    /// Used by the daemon to publish NATS FileSynced events.
    pub fn set_on_flush(&mut self, callback: OnFlushCallback) {
        self.on_flush = Some(callback);
    }

    /// Access the underlying disk cache (for stats, inspection).
    pub fn disk_cache(&self) -> &DiskCache {
        &self.disk_cache
    }

    /// Flush modified file content to SeaweedFS (index + manifest + chunks).
    ///
    /// Uses FastCDC content-defined chunking for deduplication. Small files
    /// (<4KB) produce a single chunk; larger files are split at content
    /// boundaries for efficient cross-file dedup.
    async fn flush_to_remote(&self, vpath: &str, data: &[u8]) -> Result<()> {
        use tracing::info;

        let prefix = self.prefix.trim_end_matches('/');

        // 1. Chunk the data using FastCDC (content-defined boundaries)
        let sizes = tcfs_chunks::ChunkSizes::for_path(std::path::Path::new(vpath));
        let chunks = tcfs_chunks::chunk_data(data, sizes);
        let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(data));

        // 2. Upload each chunk (dedup: same content hash = same chunk)
        let mut chunk_hashes = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            let chunk_data = &data[chunk.offset as usize..chunk.offset as usize + chunk.length];
            let chunk_hex = tcfs_chunks::hash_to_hex(&chunk.hash);
            let chunk_key = format!("{}/chunks/{}", prefix, chunk_hex);
            self.op
                .write(&chunk_key, chunk_data.to_vec())
                .await
                .with_context(|| format!("uploading chunk {}", chunk_hex))?;
            chunk_hashes.push(chunk_hex);
        }

        // 3. Create JSON v2 manifest
        let manifest = serde_json::json!({
            "version": 2,
            "file_hash": file_hash,
            "file_size": data.len(),
            "chunks": chunk_hashes,
            "written_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
        let manifest_key = format!("{}/manifests/{}", prefix, file_hash);
        self.op
            .write(&manifest_key, manifest.to_string().into_bytes())
            .await
            .context("uploading manifest")?;

        // 4. Update index entry
        let index_key = self
            .index_key_for(vpath)
            .context("cannot compute index key")?;
        let index_content = format!(
            "manifest_hash={}\nsize={}\nchunks={}\n",
            file_hash,
            data.len(),
            chunk_hashes.len()
        );
        self.op
            .write(&index_key, index_content.into_bytes())
            .await
            .context("writing index entry")?;

        info!(
            path = %vpath,
            bytes = data.len(),
            chunks = chunk_hashes.len(),
            manifest = %manifest_key,
            "flushed to SeaweedFS"
        );

        // 5. Invalidate negative cache
        self.negative_cache.remove(vpath);

        // 6. Notify listeners (e.g., NATS FileSynced publish)
        if let Some(ref cb) = self.on_flush {
            cb(vpath, &file_hash, data.len() as u64, chunk_hashes.len());
        }

        Ok(())
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
        debug!(vpath = %vpath, key = %key, "get_index_entry: reading S3 key");
        let data = match self.op.read(&key).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(vpath = %vpath, key = %key, error = %e, "get_index_entry: S3 read failed");
                return None;
            }
        };
        let text = match String::from_utf8(data.to_bytes().to_vec()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(vpath = %vpath, key = %key, error = %e, "get_index_entry: non-UTF8 data");
                return None;
            }
        };
        debug!(vpath = %vpath, key = %key, text_len = text.len(), "get_index_entry: parsing");
        match IndexEntry::parse(&text) {
            Ok(entry) => Some(entry),
            Err(e) => {
                tracing::warn!(vpath = %vpath, key = %key, error = %e, text = %text, "get_index_entry: parse failed");
                None
            }
        }
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
                let stub_name = first_component.to_string();
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

        // File: try index lookup (with and without .tc for backward compat)
        if let Some(entry) = self.get_index_entry(path).await {
            return Ok(self.file_attr(entry.size));
        }
        // Try with .tc suffix (old index entries)
        let with_tc = format!("{}.tc", path.trim_end_matches('/'));
        if let Some(entry) = self.get_index_entry(&with_tc).await {
            return Ok(self.file_attr(entry.size));
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
        // Try index lookup as-is, then with .tc suffix for backward compat
        let entry = match self.get_index_entry(path).await {
            Some(e) => e,
            None => {
                let with_tc = format!("{}.tc", path.trim_end_matches('/'));
                self.get_index_entry(&with_tc)
                    .await
                    .context(format!("index entry not found: {}", path))?
            }
        };

        let manifest_path = entry.manifest_path(&self.prefix);
        let prefix = self.prefix.trim_end_matches('/');

        debug!(path = %path, manifest = %manifest_path, "hydrating on open");

        let data = fetch_cached(&self.op, &manifest_path, prefix, &self.disk_cache)
            .await
            .with_context(|| format!("hydration failed: {}", path))?;

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().await.insert(
            fh,
            FileHandle {
                path: path.to_string(),
                data: data.clone(),
                modified: false,
            },
        );

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
        let handle = self.handles.lock().await.remove(&fh);

        if let Some(h) = handle {
            if h.modified {
                // Flush modified content to SeaweedFS
                debug!(path = %h.path, bytes = h.data.len(), "flushing modified file to S3");
                self.flush_to_remote(&h.path, &h.data).await?;
            }
        }

        Ok(())
    }

    async fn write(&self, fh: u64, offset: u64, data: &[u8]) -> Result<u32> {
        let mut handles = self.handles.lock().await;
        let handle = handles
            .get_mut(&fh)
            .context(format!("bad file handle: {}", fh))?;

        // Extend buffer if write extends past current end
        let end = offset as usize + data.len();
        if end > handle.data.len() {
            handle.data.resize(end, 0);
        }

        handle.data[offset as usize..end].copy_from_slice(data);
        handle.modified = true;

        Ok(data.len() as u32)
    }

    async fn create(&self, parent: &str, name: &OsStr, _mode: u32) -> Result<(u64, VfsAttr)> {
        let name_str = name.to_str().context("non-UTF-8 filename")?;
        let vpath = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        debug!(path = %vpath, "creating new file");

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().await.insert(
            fh,
            FileHandle {
                path: vpath,
                data: Vec::new(),
                modified: true, // new file = modified (needs flush)
            },
        );

        // Clear negative cache for this path
        self.negative_cache.remove(name_str);

        let attr = self.file_attr(0);
        Ok((fh, attr))
    }

    async fn unlink(&self, parent: &str, name: &OsStr) -> Result<()> {
        let name_str = name.to_str().context("non-UTF-8 filename")?;
        let vpath = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        // Delete index entry from S3
        if let Some(key) = self.index_key_for(&vpath) {
            debug!(path = %vpath, key = %key, "deleting index entry");
            self.op.delete(&key).await.context("deleting index entry")?;
        }

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
