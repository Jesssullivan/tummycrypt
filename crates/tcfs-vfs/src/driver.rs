//! Concrete `VirtualFilesystem` implementation for tcfs.
//!
//! Maps a SeaweedFS prefix to a virtual directory tree with user-facing names.
//! Physical `.tc`/`.tcf` stub paths are still supported as a legacy lookup
//! fallback for offline/dehydrated sync-root paths, but exact remote filenames
//! always win so a real project file ending in `.tc` stays addressable as such.
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
//!     main.rs      (remote-backed, hydrated on open)
//!   /README.md     (remote-backed, hydrated on open)
//! ```

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use async_trait::async_trait;
use opendal::Operator;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use tcfs_sync::conflict::{compare_clocks, SyncOutcome, VectorClock};
use tcfs_sync::engine::{
    bind_indexed_publish_baseline, publish_directory_marker, publish_indexed_manifest,
    validate_indexed_manifest_entry_binding,
};
use tcfs_sync::index_entry::{
    manifest_key, manifest_object_id, parse_index_entry, RemoteEntryKind, RemoteIndexEntry,
};
use tcfs_sync::manifest::{SymlinkManifest, SyncManifest};

use crate::cache::DiskCache;
use crate::hydrate::fetch_cached_from_manifest_bytes;
use crate::negative_cache::NegativeCache;
use crate::stub::IndexEntry;
use crate::types::{VfsAttr, VfsDirEntry, VfsFileType, VfsStatFs};
use crate::vfs::VirtualFilesystem;

/// Maximum in-memory file buffer size (10 GB).
/// Writes exceeding this are rejected with EFBIG.
const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024 * 1024;

/// Sentinel filename written under empty directories so they appear
/// in S3 `list()` results. Filtered out of readdir output.
const DIR_MARKER: &str = ".tcfs_dir";

/// Convert the VFS callback's mount-absolute path into the one canonical
/// relative path admitted by the remote index namespace.
///
/// Exactly one transport slash is removed. Repeated slashes, an already
/// relative path, traversal, non-NFC spelling, and reserved Git aliases are
/// rejected instead of being silently normalized into another object name.
pub fn virtual_path_to_canonical_rel_path(vpath: &str) -> Result<&str> {
    let rel_path = vpath
        .strip_prefix('/')
        .context("VFS callback path must begin with exactly one '/' transport separator")?;
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .context("invalid VFS callback path")?;
    Ok(rel_path)
}

/// Positive directory hints learned from `readdir`.
///
/// S3-compatible backends often return prefix entries that are directories, not
/// readable index objects. A short hint lets the following FUSE `lookup`/`stat`
/// answer as a directory without first attempting a noisy missing object read.
const DIR_HINT_TTL: Duration = Duration::from_secs(2);

/// An open file handle — holds content in memory, tracks write state.
struct FileHandle {
    /// Virtual path (e.g., "/src/main.rs")
    path: String,
    /// File content in memory (hydrated on open, modified on write)
    data: Vec<u8>,
    /// True if data has been modified since open (needs flush on release)
    modified: bool,
}

/// An index entry paired with the logical path that selected it.
///
/// Legacy physical stub aliases (for example, `file.txt.tc`) resolve through
/// the unsuffixed index path, while exact remote `.tc` filenames retain their
/// suffix. Keeping that lookup provenance is required for manifest binding.
#[derive(Debug)]
struct ResolvedIndexEntry {
    entry: IndexEntry,
    logical_rel_path: String,
}

/// Result of consulting one exact physical index key.
///
/// A durable tombstone is authoritative absence and must block the legacy
/// `.tc`/`.tcf` suffix fallback. Physical absence alone may use that fallback.
enum ExactIndexLookup {
    Missing,
    Tombstoned,
    Visible(IndexEntry),
}

/// Callback invoked after a file is flushed to remote storage.
/// Parameters: (virtual_path, file_hash, manifest_object_id, size_bytes,
/// chunk_count, vclock)
pub type OnFlushCallback =
    Arc<dyn Fn(&str, &str, &str, u64, usize, &VectorClock) + Send + Sync + 'static>;

/// Result of an unsync (dehydration) operation.
#[derive(Debug)]
pub struct UnsyncResult {
    /// Virtual path that was unsynced.
    pub path: String,
    /// Bytes freed from disk cache.
    pub bytes_freed: u64,
    /// Whether the file was actually cached (false = already dehydrated).
    pub was_cached: bool,
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
    handles: Arc<RwLock<HashMap<u64, FileHandle>>>,
    /// Monotonically increasing file-handle counter
    next_fh: Arc<AtomicU64>,
    /// Mount timestamp (used as atime/mtime for all synthetic entries)
    mount_time: SystemTime,
    /// Optional callback after flush_to_remote (e.g., NATS publish)
    on_flush: Option<OnFlushCallback>,
    /// Device identifier for vector clock tracking (e.g., hostname)
    device_id: String,
    /// Per-file vector clocks for conflict detection
    vclocks: Arc<Mutex<HashMap<String, VectorClock>>>,
    /// Master key for E2E chunk encryption (None = plaintext mode).
    /// Shared Arc allows the daemon to inject the key after FUSE mount starts
    /// (unlock happens via gRPC after daemon is already serving).
    master_key: Arc<tokio::sync::Mutex<Option<tcfs_crypto::MasterKey>>>,
    /// Fail writes while the key is locked instead of silently publishing
    /// plaintext into a root configured for encryption.
    encryption_required_for_writes: bool,
    /// Client surfaces without mutation parity (currently NFS) remain
    /// hydration-only until their write contract is proven.
    writes_enabled: bool,
    /// If true, unlink moves index entries to .tcfs-trash/ instead of deleting.
    trash_enabled: bool,
    /// Recently observed remote directories from list results.
    known_dirs: Arc<RwLock<HashMap<String, Instant>>>,
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
        device_id: String,
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
            handles: Arc::new(RwLock::new(HashMap::new())),
            next_fh: Arc::new(AtomicU64::new(1)),
            mount_time: SystemTime::now(),
            on_flush: None,
            device_id,
            vclocks: Arc::new(Mutex::new(HashMap::new())),
            master_key: Arc::new(tokio::sync::Mutex::new(None)),
            encryption_required_for_writes: false,
            writes_enabled: true,
            trash_enabled: false,
            known_dirs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Enable sync trash (unlink moves to .tcfs-trash/ instead of deleting).
    pub fn with_trash(mut self, enabled: bool) -> Self {
        self.trash_enabled = enabled;
        self
    }

    /// Create a VFS with a shared master key mutex (for daemon integration).
    /// The daemon can inject the key after mount via the shared Arc.
    pub fn with_shared_master_key(
        mut self,
        mk: Arc<tokio::sync::Mutex<Option<tcfs_crypto::MasterKey>>>,
    ) -> Self {
        self.master_key = mk;
        self
    }

    pub fn require_encryption_for_writes(mut self, required: bool) -> Self {
        self.encryption_required_for_writes = required;
        self
    }

    pub fn hydration_only(mut self) -> Self {
        self.writes_enabled = false;
        self
    }

    async fn encryption_key_for_write(&self) -> Result<Option<tcfs_crypto::MasterKey>> {
        anyhow::ensure!(
            self.writes_enabled,
            "EROFS: this TCFS client is hydration-only until write parity is proven"
        );
        let key = self.master_key.lock().await.clone();
        anyhow::ensure!(
            !self.encryption_required_for_writes || key.is_some(),
            "EACCES: encrypted TCFS writes require an unlocked master key"
        );
        Ok(key)
    }

    async fn ensure_write_ready(&self) -> Result<()> {
        self.encryption_key_for_write().await.map(|_| ())
    }

    /// Set a callback invoked after each flush_to_remote.
    /// Used by the daemon to publish NATS FileSynced events.
    pub fn set_on_flush(&mut self, callback: OnFlushCallback) {
        self.on_flush = Some(callback);
    }

    /// Set the master key for E2E chunk encryption.
    /// When set, flush_to_remote encrypts chunks with XChaCha20-Poly1305.
    pub fn set_master_key(&self, key: tcfs_crypto::MasterKey) {
        // Use blocking lock since this is called from sync context
        let mut guard = self.master_key.blocking_lock();
        *guard = Some(key);
    }

    /// Access the underlying disk cache (for stats, inspection).
    pub fn disk_cache(&self) -> &DiskCache {
        &self.disk_cache
    }

    /// Invalidate the negative cache for a path, so the next lookup/readdir
    /// won't return ENOENT from cache. Called by the NATS handler when a
    /// remote device syncs a new file.
    pub fn invalidate_path(&self, path: &str) {
        self.negative_cache.remove(path);
        // Also invalidate parent directory so readdir picks up the new entry
        if let Some(parent) = path.rsplit_once('/').map(|(p, _)| p) {
            let parent = if parent.is_empty() { "/" } else { parent };
            self.negative_cache.remove(parent);
        }
    }

    /// Unsync (dehydrate) a file: evict it from the disk cache.
    ///
    /// After this the VFS keeps listing the clean path, but its cached content
    /// is gone and will be re-hydrated on demand when next accessed.
    pub async fn unsync_path(&self, vpath: &str) -> Result<UnsyncResult> {
        let clean = vpath.trim_start_matches('/');
        let index_key = if self.prefix.is_empty() {
            format!("index/{clean}")
        } else {
            format!("{}/index/{clean}", self.prefix)
        };

        // Read the index entry to get the manifest hash (= cache key)
        let idx_bytes = self
            .op
            .read(&index_key)
            .await
            .with_context(|| format!("unsync: reading index for {vpath}"))?;
        let idx_raw = idx_bytes.to_bytes();
        let idx_str = std::str::from_utf8(&idx_raw).context("unsync: index entry is not UTF-8")?;
        let manifest_hash = IndexEntry::parse(idx_str)
            .context("unsync: parsing index entry")?
            .manifest_hash;

        // Evict from disk cache
        let bytes_freed = self.disk_cache.evict(&manifest_hash).await?;

        // Close any open file handles for this path
        {
            let mut handles = self.handles.write().await;
            handles.retain(|_, h| h.path != vpath);
        }

        // Clear negative cache for this path (it's still a valid remote entry)
        self.negative_cache.remove(clean);

        Ok(UnsyncResult {
            path: vpath.to_string(),
            bytes_freed,
            was_cached: bytes_freed > 0,
        })
    }

    /// Flush modified file content to SeaweedFS (index + manifest + chunks).
    ///
    /// Uses FastCDC content-defined chunking for deduplication. Small files
    /// (<4KB) produce a single chunk; larger files are split at content
    /// boundaries for efficient cross-file dedup.
    async fn flush_to_remote(&self, vpath: &str, data: &[u8]) -> Result<()> {
        use tracing::info;

        let master_key = self.encryption_key_for_write().await?;

        let prefix = self.prefix.trim_end_matches('/');
        let rel_path = virtual_path_to_canonical_rel_path(vpath)?;
        let publish_baseline = bind_indexed_publish_baseline(&self.op, prefix, rel_path).await?;
        let remote_entry = publish_baseline.current().cloned();

        // Bind conflict classification to the same index identity consumed by
        // publication. Hydration records the clock selected by the index; a
        // local edit ticks that clock, so an independently advanced remote is
        // concurrent and must not be erased by a flush.
        let remote_manifest = if let Some(entry) = remote_entry.as_ref() {
            anyhow::ensure!(
                entry.kind == RemoteEntryKind::RegularFile,
                "cannot flush a regular file over a remote symlink: {rel_path}"
            );
            let manifest_path = manifest_key(&format!("{prefix}/manifests"), &entry.manifest_hash);
            let bytes = self
                .op
                .read(&manifest_path)
                .await
                .with_context(|| format!("reading remote manifest before flush: {manifest_path}"))?
                .to_bytes();
            validate_indexed_manifest_entry_binding(&bytes, &entry.manifest_hash, entry, rel_path)
                .with_context(|| {
                    format!("validating remote manifest before flush: {manifest_path}")
                })?;
            Some(SyncManifest::from_bytes(&bytes).with_context(|| {
                format!("parsing remote manifest before flush: {manifest_path}")
            })?)
        } else {
            None
        };

        // 1. Chunk the data using FastCDC (content-defined boundaries)
        let sizes = tcfs_chunks::ChunkSizes::for_path(std::path::Path::new(vpath));
        let chunks = tcfs_chunks::chunk_data(data, sizes);
        let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(data));

        // 2. Generate per-file encryption key if master key is available
        let file_key = if master_key.is_some() {
            Some(tcfs_crypto::generate_file_key())
        } else {
            None
        };
        let file_id_bytes: [u8; 32] = {
            let h = tcfs_chunks::hash_bytes(data);
            let mut arr = [0u8; 32];
            arr.copy_from_slice(h.as_bytes());
            arr
        };

        // 3. Upload each chunk (encrypt if master key available)
        let mut chunk_hashes = Vec::with_capacity(chunks.len());
        for (idx, chunk) in chunks.iter().enumerate() {
            let start = chunk.offset as usize;
            let end = start
                .checked_add(chunk.length)
                .context("chunk offset+length overflow")?;
            anyhow::ensure!(
                end <= data.len(),
                "chunk out of bounds: offset={start} length={} data_len={}",
                chunk.length,
                data.len()
            );
            let chunk_data = &data[start..end];

            let upload_data = if let Some(ref fk) = file_key {
                tcfs_crypto::encrypt_chunk(fk, idx as u64, &file_id_bytes, chunk_data)
                    .context("encrypting chunk")?
            } else {
                chunk_data.to_vec()
            };

            let chunk_hex = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&upload_data));
            let chunk_key = format!("{}/chunks/{}", prefix, chunk_hex);
            self.op
                .write(&chunk_key, upload_data)
                .await
                .with_context(|| format!("uploading chunk {}", chunk_hex))?;
            chunk_hashes.push(chunk_hex);
        }

        // 4. Wrap file key with master key for manifest storage
        let encrypted_file_key = match (master_key.as_ref(), &file_key) {
            (Some(mk), Some(fk)) => {
                let wrapped = tcfs_crypto::wrap_key(mk, fk).context("wrapping file key")?;
                Some(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &wrapped,
                ))
            }
            _ => None,
        };
        // 5. Build vector clock and create v2 manifest with conflict metadata
        let mut vclock = {
            let vclocks = self.vclocks.lock().await;
            vclocks.get(vpath).cloned().unwrap_or_default()
        };
        if !self.device_id.is_empty() {
            vclock.tick(&self.device_id);
        }
        if let Some(remote_manifest) = remote_manifest.as_ref() {
            match compare_clocks(
                &vclock,
                &remote_manifest.vclock,
                &file_hash,
                &remote_manifest.file_hash,
                rel_path,
                &self.device_id,
                &remote_manifest.written_by,
            ) {
                SyncOutcome::LocalNewer | SyncOutcome::UpToDate => {
                    vclock.merge(&remote_manifest.vclock);
                }
                SyncOutcome::RemoteNewer => {
                    anyhow::bail!(
                        "remote file advanced before VFS flush; refusing to overwrite: {rel_path}"
                    );
                }
                SyncOutcome::Conflict(_) => {
                    anyhow::bail!(
                        "concurrent remote file update detected before VFS flush: {rel_path}"
                    );
                }
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let manifest = SyncManifest {
            version: 2,
            file_hash: file_hash.clone(),
            file_size: data.len() as u64,
            chunks: chunk_hashes.clone(),
            vclock: vclock.clone(),
            written_by: self.device_id.clone(),
            written_at: now,
            rel_path: Some(rel_path.to_string()),
            mode: None,
            mtime: None,
            encrypted_file_key,
            wrapped_file_keys: Vec::new(),
        };
        let manifest_bytes = manifest.to_bytes().context("serializing manifest")?;
        let manifest_object_id = manifest_object_id(&manifest_bytes);
        let manifest_key = format!("{}/manifests/{}", prefix, manifest_object_id);
        // 4. Update index entry
        let index_entry = RemoteIndexEntry::new(
            manifest_object_id.clone(),
            data.len() as u64,
            chunk_hashes.len(),
        );
        publish_indexed_manifest(
            &self.op,
            prefix,
            rel_path,
            manifest_bytes,
            index_entry,
            publish_baseline,
        )
        .await
        .context("publishing bound manifest and index entry")?;

        // Store updated vclock only after the exact-baseline CAS succeeds.
        {
            let mut vclocks = self.vclocks.lock().await;
            vclocks.insert(vpath.to_string(), vclock.clone());
        }

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
            cb(
                vpath,
                &file_hash,
                &manifest_object_id,
                data.len() as u64,
                chunk_hashes.len(),
                &vclock,
            );
        }

        Ok(())
    }

    fn index_key_for_rel(&self, rel: &str) -> Option<String> {
        if rel.is_empty() {
            return None;
        }
        let prefix = self.prefix.trim_end_matches('/');
        if prefix.is_empty() {
            Some(format!("index/{}", rel))
        } else {
            Some(format!("{}/index/{}", prefix, rel))
        }
    }

    /// Build the exact index path for a virtual FS path.
    ///
    /// `/src/main.rs` -> `{prefix}/index/src/main.rs`
    /// `/tests/fail.tc` -> `{prefix}/index/tests/fail.tc`
    fn index_key_for(&self, vpath: &str) -> Option<String> {
        self.index_key_for_rel(vpath.trim_start_matches('/'))
    }

    /// Build the clean-path compatibility key for a physical stub path.
    ///
    /// `/src/main.rs.tc` -> `{prefix}/index/src/main.rs`.
    /// This is only a fallback after the exact `.tc` key was absent, because
    /// real source trees can legitimately contain files ending in `.tc`.
    fn legacy_stub_index_key_for(&self, vpath: &str) -> Option<String> {
        self.index_key_for_rel(Self::legacy_stub_rel_path(vpath)?)
    }

    fn legacy_stub_rel_path(vpath: &str) -> Option<&str> {
        let rel = vpath.trim_start_matches('/');
        rel.strip_suffix(".tc").or_else(|| rel.strip_suffix(".tcf"))
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

    /// Build the S3 key for a directory marker.
    /// `/newdir` -> `{prefix}/index/newdir/.tcfs_dir`
    fn dir_marker_key(&self, vpath: &str) -> String {
        let rel = vpath.trim_start_matches('/').trim_end_matches('/');
        let prefix = self.prefix.trim_end_matches('/');
        match (prefix.is_empty(), rel.is_empty()) {
            (true, true) => format!("index/{}", DIR_MARKER),
            (true, false) => format!("index/{}/{}", rel, DIR_MARKER),
            (false, true) => format!("{}/index/{}", prefix, DIR_MARKER),
            (false, false) => format!("{}/index/{}/{}", prefix, rel, DIR_MARKER),
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

    async fn get_index_entry_at_key(&self, vpath: &str, key: String) -> Result<ExactIndexLookup> {
        debug!(vpath = %vpath, key = %key, "get_index_entry: reading S3 key");
        let manifest_prefix = if self.prefix.trim_end_matches('/').is_empty() {
            "manifests".to_string()
        } else {
            format!("{}/manifests", self.prefix.trim_end_matches('/'))
        };
        let Some(record) =
            tcfs_sync::index_entry::read_index_entry_record_from_store(&self.op, &key)
                .await
                .with_context(|| format!("reading exact index entry for {vpath}: {key}"))?
        else {
            return Ok(ExactIndexLookup::Missing);
        };
        if record.state() == tcfs_sync::index_entry::IndexEntryState::Deleted {
            return Ok(ExactIndexLookup::Tombstoned);
        }

        let entry =
            tcfs_sync::index_entry::resolve_visible_index_entry(&self.op, &key, &manifest_prefix)
                .await
                .with_context(|| format!("resolving exact index entry for {vpath}: {key}"))?
                .with_context(|| {
                    format!("exact index entry has no visible value for {vpath}: {key}")
                })?;
        Ok(ExactIndexLookup::Visible(entry.into()))
    }

    /// Fetch an IndexEntry using exact lookup first, then legacy stub fallback.
    async fn get_index_entry_with_legacy_stub_fallback(
        &self,
        vpath: &str,
    ) -> Result<Option<ResolvedIndexEntry>> {
        let Some(exact_key) = self.index_key_for(vpath) else {
            return Ok(None);
        };
        match self.get_index_entry_at_key(vpath, exact_key).await? {
            ExactIndexLookup::Visible(entry) => {
                return Ok(Some(ResolvedIndexEntry {
                    entry,
                    logical_rel_path: vpath.trim_start_matches('/').to_string(),
                }));
            }
            ExactIndexLookup::Tombstoned => return Ok(None),
            ExactIndexLookup::Missing => {}
        }

        let Some(logical_rel_path) = Self::legacy_stub_rel_path(vpath) else {
            return Ok(None);
        };
        let Some(key) = self.legacy_stub_index_key_for(vpath) else {
            return Ok(None);
        };
        let entry = match self.get_index_entry_at_key(vpath, key).await? {
            ExactIndexLookup::Visible(entry) => entry,
            ExactIndexLookup::Missing | ExactIndexLookup::Tombstoned => return Ok(None),
        };
        Ok(Some(ResolvedIndexEntry {
            entry,
            logical_rel_path: logical_rel_path.to_string(),
        }))
    }

    /// Fetch attributes from an index entry by its S3 key.
    async fn read_index_entry_attr(&self, index_key: &str) -> Result<Option<VfsAttr>> {
        match self
            .get_index_entry_at_key(index_key, index_key.to_string())
            .await?
        {
            ExactIndexLookup::Visible(entry) => Ok(Some(self.attr_for_index_entry(&entry))),
            ExactIndexLookup::Missing | ExactIndexLookup::Tombstoned => Ok(None),
        }
    }

    /// Return whether an index subtree contains any logically visible entry.
    /// Physical v4 tombstones intentionally remain in object storage and must
    /// not keep directories visible by their key alone.
    async fn index_prefix_has_visible_entries(&self, index_prefix: &str) -> Result<bool> {
        let entries = self
            .op
            .list_with(index_prefix)
            .recursive(true)
            .await
            .with_context(|| format!("listing logical index subtree: {index_prefix}"))?;
        for entry in entries {
            let key = entry.path();
            if key.ends_with('/') {
                continue;
            }
            if key.ends_with("/.tcfs_dir") {
                if tcfs_sync::index_entry::directory_marker_is_visible(&self.op, key).await? {
                    return Ok(true);
                }
                continue;
            }
            let Some(record) =
                tcfs_sync::index_entry::read_index_entry_record_from_store(&self.op, key).await?
            else {
                continue;
            };
            if record.visible_entry().is_some() || record.pending_entry().is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Synthesize file attributes.
    fn file_attr(&self, size: u64) -> VfsAttr {
        VfsAttr::file(size, self.uid, self.gid, self.mount_time)
    }

    /// Synthesize symlink attributes.
    fn symlink_attr(&self, size: u64) -> VfsAttr {
        VfsAttr::symlink(size, self.uid, self.gid, self.mount_time)
    }

    /// Synthesize directory attributes.
    fn dir_attr(&self) -> VfsAttr {
        VfsAttr::dir(self.uid, self.gid, self.mount_time)
    }

    fn child_virtual_path(parent: &str, child: &str) -> String {
        if parent == "/" {
            format!("/{child}")
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), child)
        }
    }

    async fn remember_dir_hints<I>(&self, paths: I)
    where
        I: IntoIterator<Item = String>,
    {
        let now = Instant::now();
        let mut known_dirs = self.known_dirs.write().await;
        known_dirs.retain(|_, seen_at| now.duration_since(*seen_at) <= DIR_HINT_TTL);
        for path in paths {
            self.negative_cache.remove(&path);
            known_dirs.insert(path, now);
        }
    }

    async fn has_recent_dir_hint(&self, path: &str) -> bool {
        let now = Instant::now();
        let mut known_dirs = self.known_dirs.write().await;
        match known_dirs.get(path).copied() {
            Some(seen_at) if now.duration_since(seen_at) <= DIR_HINT_TTL => true,
            Some(_) => {
                known_dirs.remove(path);
                false
            }
            None => false,
        }
    }

    fn attr_for_index_entry(&self, entry: &IndexEntry) -> VfsAttr {
        match entry.kind {
            RemoteEntryKind::RegularFile => self.file_attr(entry.size),
            RemoteEntryKind::Symlink => self.symlink_attr(entry.size),
        }
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
        let mut pending_files: Vec<(String, String)> = Vec::new();
        let mut entries: Vec<VfsDirEntry> = Vec::new();
        let mut discovered_dir_paths: Vec<String> = Vec::new();

        for entry in raw_entries {
            let full_path = entry.path().to_string();
            let rel = full_path
                .trim_start_matches(&index_prefix)
                .trim_start_matches('/');
            if rel.is_empty() {
                continue;
            }

            let first_component = rel.split('/').next().unwrap_or(rel);

            // Skip directory marker sentinel entries
            if first_component == DIR_MARKER {
                continue;
            }

            let is_dir = entry.metadata().is_dir() || rel.contains('/') || rel.ends_with('/');

            if is_dir {
                let dir_name = first_component.trim_end_matches('/').to_string();
                if seen_dirs.contains(&dir_name) {
                    continue;
                }
                let child_path = Self::child_virtual_path(path, &dir_name);
                let child_prefix = self.index_prefix_for_dir(&child_path);
                if !self.index_prefix_has_visible_entries(&child_prefix).await? {
                    continue;
                }
                seen_dirs.insert(dir_name.clone());
                discovered_dir_paths.push(child_path);
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
                pending_files.push((first_component.to_string(), full_path));
            }
        }

        for (clean_name, full_path) in pending_files {
            if seen_dirs.contains(&clean_name) {
                debug!(
                    path = %path,
                    name = %clean_name,
                    key = %full_path,
                    "readdir: skipping leaf object shadowed by a directory prefix"
                );
                continue;
            }

            let Some(parsed_attr) = self.read_index_entry_attr(&full_path).await? else {
                debug!(
                    path = %path,
                    name = %clean_name,
                    key = %full_path,
                    "readdir: skipping missing or tombstoned index leaf object"
                );
                continue;
            };
            let kind = parsed_attr.kind;
            let attr = if with_attrs { Some(parsed_attr) } else { None };
            entries.push(VfsDirEntry {
                name: clean_name,
                kind,
                attr,
            });
        }

        if !discovered_dir_paths.is_empty() {
            self.remember_dir_hints(discovered_dir_paths).await;
        }

        // Fallback: if root dir is empty and prefix is empty, discover prefixes
        if path == "/" && self.prefix.is_empty() && entries.is_empty() {
            let prefixes = self.discover_prefixes().await;
            for pfx in prefixes {
                let probe = format!("{}/index/", pfx);
                if self.index_prefix_has_visible_entries(&probe).await? && !seen_dirs.contains(&pfx)
                {
                    seen_dirs.insert(pfx.clone());
                    self.remember_dir_hints([Self::child_virtual_path(path, &pfx)])
                        .await;
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

        if self.has_recent_dir_hint(path).await {
            return Ok(self.dir_attr());
        }

        // File: exact index lookup first; legacy physical-stub fallback only
        // when the exact `.tc`/`.tcf` filename is absent.
        if let Some(resolved) = self.get_index_entry_with_legacy_stub_fallback(path).await? {
            return Ok(self.attr_for_index_entry(&resolved.entry));
        }

        // Directory: check if any index entries exist under it
        let dir_prefix = self.index_prefix_for_dir(path);
        if self.index_prefix_has_visible_entries(&dir_prefix).await? {
            Ok(self.dir_attr())
        } else {
            self.negative_cache.insert(path);
            anyhow::bail!("ENOENT: {}", path);
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

    async fn readlink(&self, path: &str) -> Result<String> {
        let resolved = self
            .get_index_entry_with_legacy_stub_fallback(path)
            .await?
            .context(format!("index entry not found: {}", path))?;
        let entry = &resolved.entry;
        if entry.kind != RemoteEntryKind::Symlink {
            anyhow::bail!("EINVAL: not a symlink: {}", path);
        }
        let manifest_path = entry.manifest_path(&self.prefix);
        let manifest_bytes = self
            .op
            .read(&manifest_path)
            .await
            .with_context(|| format!("reading symlink manifest: {manifest_path}"))?
            .to_bytes();
        validate_indexed_manifest_entry_binding(
            &manifest_bytes,
            &entry.manifest_hash,
            &entry.as_remote_entry(),
            &resolved.logical_rel_path,
        )
        .with_context(|| format!("validating symlink manifest binding for {path}"))?;
        let manifest = SymlinkManifest::from_bytes(&manifest_bytes)
            .with_context(|| format!("parsing symlink manifest for {path}"))?;
        tcfs_sync::engine::validate_indexed_symlink_target(
            Path::new(&resolved.logical_rel_path),
            &manifest.symlink_target,
        )?;
        Ok(manifest.symlink_target)
    }

    async fn open(&self, path: &str) -> Result<(u64, Vec<u8>)> {
        let resolved = self
            .get_index_entry_with_legacy_stub_fallback(path)
            .await?
            .context(format!("index entry not found: {}", path))?;
        let entry = &resolved.entry;

        if entry.kind == RemoteEntryKind::Symlink {
            anyhow::bail!("ELOOP: open called on symlink: {}", path);
        }

        let manifest_path = entry.manifest_path(&self.prefix);
        let prefix = self.prefix.trim_end_matches('/');

        debug!(path = %path, manifest = %manifest_path, "hydrating on open");

        // Bind the index-selected object, kind, and path before consulting the
        // plaintext cache. A valid object for another path must not hydrate
        // through a forged or stale index entry.
        let manifest_bytes = self
            .op
            .read(&manifest_path)
            .await
            .with_context(|| format!("reading manifest binding: {manifest_path}"))?
            .to_bytes();
        validate_indexed_manifest_entry_binding(
            &manifest_bytes,
            &entry.manifest_hash,
            &entry.as_remote_entry(),
            &resolved.logical_rel_path,
        )
        .with_context(|| format!("validating manifest binding for {path}"))?;
        let bound_manifest = SyncManifest::from_bytes(&manifest_bytes)
            .with_context(|| format!("parsing manifest clock for {path}"))?;

        // Read master key from shared mutex (may be injected after mount via gRPC unlock)
        let mk_guard = self.master_key.lock().await;
        let mk_bytes: Option<[u8; 32]> = mk_guard.as_ref().map(|k| *k.as_bytes());
        drop(mk_guard);

        let data = fetch_cached_from_manifest_bytes(
            &self.op,
            &manifest_path,
            &manifest_bytes,
            prefix,
            &self.disk_cache,
            mk_bytes.as_ref(),
        )
        .await
        .with_context(|| format!("hydration failed: {}", path))?;

        self.vclocks
            .lock()
            .await
            .insert(path.to_string(), bound_manifest.vclock);

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.write().await.insert(
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
        let handles = self.handles.read().await;
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
        let handle = self.handles.write().await.remove(&fh);

        if let Some(h) = handle {
            if h.modified {
                // Flush modified content to SeaweedFS
                debug!(path = %h.path, bytes = h.data.len(), "flushing modified file to S3");
                if let Err(error) = self.flush_to_remote(&h.path, &h.data).await {
                    // A key lock or transient storage failure must not discard
                    // the only buffered copy. Keep the handle retryable.
                    self.handles.write().await.insert(fh, h);
                    return Err(error);
                }
            }
        }

        Ok(())
    }

    async fn write(&self, fh: u64, offset: u64, data: &[u8]) -> Result<u32> {
        self.ensure_write_ready().await?;
        let mut handles = self.handles.write().await;
        let handle = handles
            .get_mut(&fh)
            .context(format!("bad file handle: {}", fh))?;

        // Prevent OOM: reject writes that would exceed the maximum buffer size
        let end = offset as usize + data.len();
        if end > MAX_WRITE_SIZE {
            anyhow::bail!(
                "EFBIG: write would exceed maximum file size ({} bytes, limit {} bytes)",
                end,
                MAX_WRITE_SIZE
            );
        }

        // Extend buffer if write extends past current end
        if end > handle.data.len() {
            handle.data.resize(end, 0);
        }

        handle.data[offset as usize..end].copy_from_slice(data);
        handle.modified = true;

        Ok(data.len() as u32)
    }

    async fn truncate(&self, path: Option<&str>, fh: Option<u64>, size: u64) -> Result<VfsAttr> {
        self.ensure_write_ready().await?;
        if size as usize > MAX_WRITE_SIZE {
            anyhow::bail!(
                "EFBIG: truncate would exceed maximum file size ({} bytes, limit {} bytes)",
                size,
                MAX_WRITE_SIZE
            );
        }

        if let Some(fh) = fh {
            let mut handles = self.handles.write().await;
            let handle = handles
                .get_mut(&fh)
                .context(format!("truncate: bad file handle: {}", fh))?;
            handle.data.resize(size as usize, 0);
            handle.modified = true;
            return Ok(self.file_attr(size));
        }

        let path = path.context("truncate requires path or file handle")?;

        {
            let mut handles = self.handles.write().await;
            let mut matched = false;
            for handle in handles.values_mut().filter(|handle| handle.path == path) {
                handle.data.resize(size as usize, 0);
                handle.modified = true;
                matched = true;
            }
            if matched {
                return Ok(self.file_attr(size));
            }
        }

        let (fh, _) = self.open(path).await?;
        self.truncate(None, Some(fh), size).await?;
        self.release(fh).await?;

        Ok(self.file_attr(size))
    }

    async fn create(&self, parent: &str, name: &OsStr, _mode: u32) -> Result<(u64, VfsAttr)> {
        self.ensure_write_ready().await?;
        let name_str = name.to_str().context("non-UTF-8 filename")?;
        let vpath = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        debug!(path = %vpath, "creating new file");

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.write().await.insert(
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
        self.ensure_write_ready().await?;
        let name_str = name.to_str().context("non-UTF-8 filename")?;
        let vpath = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        if let Some(key) = self.index_key_for(&vpath) {
            if self.trash_enabled {
                // Move to trash instead of permanent delete
                let rel_path = vpath.trim_start_matches('/');
                debug!(path = %vpath, key = %key, "trashing index entry");
                crate::trash::trash_index_entry(&self.op, &self.prefix, &key, rel_path)
                    .await
                    .context("moving index entry to trash")?;
            } else {
                // Logical delete with compare-and-swap. The durable tombstone
                // avoids deleting a concurrent publisher's replacement.
                debug!(path = %vpath, key = %key, "tombstoning index entry");
                tcfs_sync::index_entry::tombstone_index_entry(
                    &self.op,
                    self.prefix.trim_end_matches('/'),
                    &key,
                )
                .await
                .context("tombstoning index entry")?;
            }
        }

        Ok(())
    }

    async fn mkdir(&self, parent: &str, name: &OsStr, _mode: u32) -> Result<VfsAttr> {
        self.ensure_write_ready().await?;
        let name_str = name.to_str().context("non-UTF-8 directory name")?;
        let vpath = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        debug!(path = %vpath, "creating directory");

        // Write directory marker so getattr/readdir can find empty directories
        let rel_path = vpath.trim_start_matches('/');
        publish_directory_marker(&self.op, &self.prefix, rel_path)
            .await
            .context("writing directory marker")?;

        // Clear negative cache for this path and parent
        self.negative_cache.remove(&vpath);
        self.negative_cache.remove(parent);

        Ok(self.dir_attr())
    }

    async fn rename(
        &self,
        from_parent: &str,
        from_name: &OsStr,
        to_parent: &str,
        to_name: &OsStr,
    ) -> Result<()> {
        self.ensure_write_ready().await?;
        let from_str = from_name.to_str().context("non-UTF-8 source name")?;
        let to_str = to_name.to_str().context("non-UTF-8 target name")?;

        let from_path = if from_parent == "/" {
            format!("/{}", from_str)
        } else {
            format!("{}/{}", from_parent.trim_end_matches('/'), from_str)
        };
        let to_path = if to_parent == "/" {
            format!("/{}", to_str)
        } else {
            format!("{}/{}", to_parent.trim_end_matches('/'), to_str)
        };

        debug!(from = %from_path, to = %to_path, "renaming");

        // Copy-then-delete (S3 has no native rename)
        let from_key = self
            .index_key_for(&from_path)
            .context("cannot compute source index key")?;
        let to_key = self
            .index_key_for(&to_path)
            .context("cannot compute target index key")?;

        let index_bytes = self
            .op
            .read(&from_key)
            .await
            .with_context(|| format!("reading source index: {}", from_key))?;
        let source_index_bytes = index_bytes.to_bytes();
        let mut index_entry = parse_index_entry(&source_index_bytes)
            .with_context(|| format!("parsing source index: {from_key}"))?;
        let from_rel_path = from_path.trim_start_matches('/');
        let to_rel_path = to_path.trim_start_matches('/');
        let publish_baseline =
            bind_indexed_publish_baseline(&self.op, &self.prefix, to_rel_path).await?;
        let source_manifest_key = manifest_key(
            &format!("{}/manifests", self.prefix.trim_end_matches('/')),
            &index_entry.manifest_hash,
        );
        let source_manifest_bytes = self
            .op
            .read(&source_manifest_key)
            .await
            .with_context(|| format!("reading source manifest: {source_manifest_key}"))?
            .to_bytes();
        validate_indexed_manifest_entry_binding(
            &source_manifest_bytes,
            &index_entry.manifest_hash,
            &index_entry,
            from_rel_path,
        )
        .with_context(|| format!("validating source manifest binding: {from_path}"))?;

        let rebound_manifest = match index_entry.kind {
            RemoteEntryKind::RegularFile => {
                let mut manifest = SyncManifest::from_bytes(&source_manifest_bytes)
                    .context("parsing regular manifest for rename")?;
                manifest.rel_path = Some(to_rel_path.to_string());
                manifest
                    .to_bytes()
                    .context("serializing path-rebound regular manifest")?
            }
            RemoteEntryKind::Symlink => {
                let mut manifest = SymlinkManifest::from_bytes(&source_manifest_bytes)
                    .context("parsing symlink manifest for rename")?;
                manifest.rel_path = Some(to_rel_path.to_string());
                manifest
                    .to_bytes()
                    .context("serializing path-rebound symlink manifest")?
            }
        };
        let rebound_object_id = manifest_object_id(&rebound_manifest);
        index_entry.manifest_hash = rebound_object_id;
        publish_indexed_manifest(
            &self.op,
            &self.prefix,
            to_rel_path,
            rebound_manifest,
            index_entry,
            publish_baseline,
        )
        .await
        .with_context(|| format!("publishing target index: {to_key}"))?;

        tcfs_sync::index_entry::tombstone_index_entry_if_exact(
            &self.op,
            self.prefix.trim_end_matches('/'),
            &from_key,
            &source_index_bytes,
        )
        .await
        .with_context(|| format!("tombstoning exact source index: {from_key}"))?;

        // Update any open file handles pointing to the old path
        {
            let mut handles = self.handles.write().await;
            for h in handles.values_mut() {
                if h.path == from_path {
                    h.path = to_path.clone();
                }
            }
        }

        self.negative_cache.remove(&from_path);
        self.negative_cache.remove(&to_path);

        Ok(())
    }

    async fn rmdir(&self, parent: &str, name: &OsStr) -> Result<()> {
        self.ensure_write_ready().await?;
        let name_str = name.to_str().context("non-UTF-8 directory name")?;
        let vpath = if parent == "/" {
            format!("/{}", name_str)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name_str)
        };

        debug!(path = %vpath, "removing directory");

        // Check that directory is empty (only the marker should exist)
        let dir_prefix = self.index_prefix_for_dir(&vpath);
        let entries = self
            .op
            .list(&dir_prefix)
            .await
            .context("listing directory for rmdir")?;

        let marker_key = self.dir_marker_key(&vpath);
        for entry in entries {
            let key = entry.path();
            if key == marker_key {
                continue;
            }
            if key.ends_with('/') {
                if self.index_prefix_has_visible_entries(key).await? {
                    anyhow::bail!("ENOTEMPTY: directory not empty: {}", vpath);
                }
                continue;
            }
            if key.ends_with("/.tcfs_dir") {
                if tcfs_sync::index_entry::directory_marker_is_visible(&self.op, key).await? {
                    anyhow::bail!("ENOTEMPTY: directory not empty: {}", vpath);
                }
                continue;
            }
            let Some(record) =
                tcfs_sync::index_entry::read_index_entry_record_from_store(&self.op, key).await?
            else {
                continue;
            };
            if record.visible_entry().is_some() || record.pending_entry().is_some() {
                anyhow::bail!("ENOTEMPTY: directory not empty: {}", vpath);
            }
        }

        // Atomically hide the directory marker without removing a concurrent
        // replacement or discarding its durable evidence.
        let marker_bytes = self
            .op
            .read(&marker_key)
            .await
            .context("reading directory marker before tombstone")?
            .to_vec();
        tcfs_sync::index_entry::tombstone_directory_marker_if_exact(
            &self.op,
            self.prefix.trim_end_matches('/'),
            &marker_key,
            &marker_bytes,
        )
        .await
        .context("tombstoning directory marker")?;

        Ok(())
    }

    async fn fsync(&self, fh: u64, _datasync: bool) -> Result<()> {
        // Clone data out of the handle to avoid holding the lock during flush
        let (path, data, modified) = {
            let handles = self.handles.read().await;
            let handle = handles
                .get(&fh)
                .context(format!("fsync: bad file handle: {}", fh))?;
            (handle.path.clone(), handle.data.clone(), handle.modified)
        };

        if !modified {
            return Ok(());
        }

        self.flush_to_remote(&path, &data).await?;
        if let Some(handle) = self.handles.write().await.get_mut(&fh) {
            if handle.data == data {
                handle.modified = false;
            }
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
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        TcfsVfs::new(
            op,
            prefix.to_string(),
            std::path::PathBuf::from("/tmp/tcfs-test-cache"),
            64 * 1024 * 1024,
            Duration::from_secs(30),
            "test-host".to_string(),
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
            vfs.index_key_for("/file.txt"),
            Some("index/file.txt".to_string())
        );
    }

    #[test]
    fn test_index_key_with_prefix() {
        let vfs = make_vfs("data");
        assert_eq!(
            vfs.index_key_for("/file.txt"),
            Some("data/index/file.txt".to_string())
        );
    }

    #[test]
    fn test_index_key_preserves_tc_suffix_for_exact_lookup() {
        let vfs = make_vfs("data");
        assert_eq!(
            vfs.index_key_for("/src/main.rs.tc"),
            Some("data/index/src/main.rs.tc".to_string())
        );
        assert_eq!(
            vfs.legacy_stub_index_key_for("/src/main.rs.tc"),
            Some("data/index/src/main.rs".to_string())
        );
    }

    #[test]
    fn test_index_key_preserves_tcf_suffix_for_exact_lookup() {
        let vfs = make_vfs("data");
        assert_eq!(
            vfs.index_key_for("/doc.pdf.tcf"),
            Some("data/index/doc.pdf.tcf".to_string())
        );
        assert_eq!(
            vfs.legacy_stub_index_key_for("/doc.pdf.tcf"),
            Some("data/index/doc.pdf".to_string())
        );
    }

    #[tokio::test]
    async fn corrupt_exact_tc_entry_never_falls_back_to_unsuffixed_legacy_key() {
        let vfs = make_vfs("data");
        vfs.op
            .write("data/index/src/main.rs.tc", b"{not-json".to_vec())
            .await
            .unwrap();
        vfs.op
            .write(
                "data/index/src/main.rs",
                b"manifest_hash=legacy\nsize=1\nchunks=1".to_vec(),
            )
            .await
            .unwrap();

        let error = vfs
            .get_index_entry_with_legacy_stub_fallback("/src/main.rs.tc")
            .await
            .expect_err("corrupt exact entry must fail closed before legacy fallback");

        assert!(format!("{error:#}").contains("parsing versioned index entry"));
    }

    #[tokio::test]
    async fn tombstoned_exact_tc_entry_blocks_unsuffixed_legacy_fallback() {
        let vfs = make_vfs("data");
        let tombstone = tcfs_sync::index_entry::VersionedIndexEntry::deleted()
            .to_json_bytes()
            .unwrap();
        vfs.op
            .write("data/index/src/main.rs.tc", tombstone)
            .await
            .unwrap();
        vfs.op
            .write(
                "data/index/src/main.rs",
                b"manifest_hash=legacy\nsize=1\nchunks=1".to_vec(),
            )
            .await
            .unwrap();

        let resolved = vfs
            .get_index_entry_with_legacy_stub_fallback("/src/main.rs.tc")
            .await
            .unwrap();

        assert!(
            resolved.is_none(),
            "an exact deletion tombstone must not expose the legacy unsuffixed entry"
        );
    }

    #[test]
    fn test_index_key_root_returns_none() {
        let vfs = make_vfs("data");
        assert_eq!(vfs.index_key_for("/"), None);
    }
}
