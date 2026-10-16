//! On-demand hydration: fetch a manifest's content from SeaweedFS chunks.
//!
//! Three shapes, all verifying every stored chunk against its content
//! address before decrypting it (TIN-1419, streaming large-file IO):
//!
//! - [`hydrate_cached_file_from_manifest_bytes`] streams chunks straight into
//!   a private disk-cache file and returns a descriptor to it. Memory is
//!   bounded by the fetch window ([`DEFAULT_HYDRATION_WINDOW`] chunks), not by
//!   the file size, and the whole-file size and BLAKE3 identity are verified
//!   incrementally as the bytes go to disk. The VFS serves reads from that
//!   descriptor by range.
//! - [`fetch_range_from_manifest_bytes`] fetches only the chunks that cover a
//!   byte range, for callers that want a slice without hydrating the file.
//! - [`fetch_content`] / [`fetch_cached`] keep the older whole-file-in-memory
//!   API, now built on the same bounded, parallel chunk window.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine as _;
use opendal::Operator;
use tcfs_sync::manifest::SyncManifest;
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::cache::{cache_key_for_path, DiskCache};

/// Chunks fetched ahead of the writer during streaming hydration. Peak
/// memory is about this many stored chunks, whatever the file size.
pub const DEFAULT_HYDRATION_WINDOW: usize = 8;

/// Longest one chunk download may take. A storage read whose body stalls
/// (no transport timeout fires on an idle keep-alive connection) fails here
/// instead of wedging the hydration, and every open joined to it, forever.
pub const DEFAULT_CHUNK_FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Longest a streaming hydration may go without any chunk completing.
pub const DEFAULT_HYDRATION_STALL_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds on one streaming hydration.
#[derive(Debug, Clone)]
pub struct HydrationLimits {
    /// Chunks fetched ahead of the writer.
    pub window: usize,
    /// Deadline for each chunk download.
    pub chunk_timeout: Duration,
    /// Deadline for any chunk to complete while the hydration waits.
    pub stall_timeout: Duration,
    /// Shared cap on hydrations fetching from storage at once. A permit is
    /// taken only for a fetch, after the cache was consulted, so a cache hit
    /// never waits behind downloads.
    pub admission: Option<Arc<tokio::sync::Semaphore>>,
}

impl Default for HydrationLimits {
    fn default() -> Self {
        Self {
            window: DEFAULT_HYDRATION_WINDOW,
            chunk_timeout: DEFAULT_CHUNK_FETCH_TIMEOUT,
            stall_timeout: DEFAULT_HYDRATION_STALL_TIMEOUT,
            admission: None,
        }
    }
}

/// Bytes an encrypted chunk adds to its plaintext (XChaCha20 nonce + tag).
const ENCRYPTED_CHUNK_OVERHEAD: u64 = (tcfs_crypto::NONCE_SIZE + tcfs_crypto::TAG_SIZE) as u64;

/// What one streaming hydration did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HydrationStats {
    /// Plaintext bytes produced.
    pub bytes: u64,
    /// Chunks fetched and verified.
    pub chunks: usize,
    /// Most fetched-but-not-yet-written stored chunk bytes held at once.
    /// Bounded by the window times the largest chunk, never the file size.
    pub peak_buffered_bytes: u64,
}

/// Hydrated content held in the disk cache and read by range.
#[derive(Debug)]
pub struct CachedContent {
    /// Read descriptor on the verified content. It stays valid if the cache
    /// entry is evicted or replaced after this returns.
    pub file: std::fs::File,
    /// Verified plaintext length.
    pub len: u64,
    /// `Some` when this call fetched from storage, `None` on a cache hit.
    pub stats: Option<HydrationStats>,
}

struct HydrationCrypto {
    file_key: tcfs_crypto::FileKey,
    file_id: [u8; 32],
}

/// Parse and validate the regular-file manifest shape used by the VFS reader.
///
/// Legacy v1 manifests remain readable, but they carry no whole-file size or
/// hash; their chunks are still verified individually during hydration. Modern
/// manifests must carry a valid whole-file BLAKE3 identity. This VFS surface
/// currently has only a master key, so per-device-only v3 content fails closed
/// instead of returning ciphertext.
fn prepare_manifest(
    manifest_bytes: &[u8],
    manifest_path: &str,
    master_key: Option<&[u8; 32]>,
) -> Result<(SyncManifest, Option<HydrationCrypto>)> {
    let manifest = SyncManifest::from_bytes(manifest_bytes)
        .with_context(|| format!("parsing regular-file manifest: {manifest_path}"))?;

    match manifest.version {
        1 => {
            anyhow::ensure!(
                manifest.file_hash.is_empty()
                    && manifest.file_size == 0
                    && manifest.encrypted_file_key.is_none()
                    && manifest.wrapped_file_keys.is_empty(),
                "legacy v1 manifest carries unsupported modern metadata: {manifest_path}"
            );
        }
        2 => {}
        3 => {
            anyhow::bail!(
                "per-device-only manifest v3 is not supported by VFS hydration: {manifest_path}"
            );
        }
        version => {
            anyhow::bail!("unsupported regular-file manifest version {version}: {manifest_path}");
        }
    }

    for (index, hash) in manifest.chunk_hashes().iter().enumerate() {
        tcfs_chunks::hash_from_hex(hash).with_context(|| {
            format!("invalid chunk hash at index {index} in manifest: {manifest_path}")
        })?;
    }

    if manifest.is_legacy() {
        return Ok((manifest, None));
    }

    let file_hash = tcfs_chunks::hash_from_hex(&manifest.file_hash)
        .with_context(|| format!("invalid file hash in manifest: {manifest_path}"))?;

    let crypto = match manifest.encrypted_file_key.as_deref() {
        Some(wrapped_b64) => {
            let master_bytes = master_key.with_context(|| {
                format!(
                    "manifest is encrypted but no master key is available for VFS hydration: {manifest_path}"
                )
            })?;
            let wrapped = base64::engine::general_purpose::STANDARD
                .decode(wrapped_b64)
                .with_context(|| {
                    format!("decoding encrypted_file_key in manifest: {manifest_path}")
                })?;
            let master = tcfs_crypto::MasterKey::from_bytes(*master_bytes);
            let file_key = tcfs_crypto::unwrap_key(&master, &wrapped)
                .with_context(|| format!("unwrapping file key for manifest: {manifest_path}"))?;
            Some(HydrationCrypto {
                file_key,
                file_id: *file_hash.as_bytes(),
            })
        }
        None if !manifest.wrapped_file_keys.is_empty() => {
            anyhow::bail!(
                "manifest has per-device wrapped keys but no master wrap; VFS hydration cannot decrypt it: {manifest_path}"
            );
        }
        None => None,
    };

    Ok((manifest, crypto))
}

fn validate_plaintext(
    manifest: &SyncManifest,
    plaintext: &[u8],
    manifest_path: &str,
) -> Result<()> {
    // v1 has no whole-file identity. Its chunks are verified against their
    // content addresses while fetching, but cached v1 content cannot be
    // independently revalidated and is therefore bypassed below.
    if manifest.is_legacy() {
        return Ok(());
    }

    anyhow::ensure!(
        plaintext.len() as u64 == manifest.file_size,
        "hydrated file size mismatch for {manifest_path}: expected {}, got {}",
        manifest.file_size,
        plaintext.len()
    );
    let actual = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(plaintext));
    anyhow::ensure!(
        actual == manifest.file_hash.as_str(),
        "hydrated file integrity failure for {manifest_path}: expected {}, got {actual}",
        manifest.file_hash
    );
    Ok(())
}

async fn load_manifest(
    op: &Operator,
    manifest_path: &str,
    master_key: Option<&[u8; 32]>,
) -> Result<(SyncManifest, Option<HydrationCrypto>)> {
    let manifest_bytes = op
        .read(manifest_path)
        .await
        .with_context(|| format!("reading manifest: {manifest_path}"))?
        .to_bytes();
    prepare_manifest(&manifest_bytes, manifest_path, master_key)
}

/// Fetch the fully-assembled content for a manifest path.
///
/// Reads the manifest to get chunk hashes, verifies every stored chunk before
/// decrypting it, and verifies the final plaintext size and BLAKE3 identity for
/// modern manifests.
///
/// # Arguments
/// - `op` — OpenDAL operator pointing at the SeaweedFS bucket
/// - `manifest_path` — full path of the manifest object (e.g. `data/manifests/abc123`)
/// - `remote_prefix` — prefix used to look up chunks (e.g. `data`)
/// - `master_key` — optional master encryption key for decrypting chunks
pub async fn fetch_content(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    let (manifest, crypto) = load_manifest(op, manifest_path, master_key).await?;
    fetch_prepared_content(op, manifest_path, remote_prefix, &manifest, crypto.as_ref()).await
}

async fn fetch_prepared_content(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    manifest: &SyncManifest,
    crypto: Option<&HydrationCrypto>,
) -> Result<Vec<u8>> {
    let mut assembled = Vec::with_capacity(if manifest.is_legacy() {
        0
    } else {
        usize::try_from(manifest.file_size).unwrap_or(0)
    });
    stream_prepared_content(
        op,
        manifest_path,
        remote_prefix,
        manifest,
        crypto,
        &HydrationLimits::default(),
        |plaintext| {
            assembled.extend_from_slice(plaintext);
            Ok(())
        },
    )
    .await?;
    Ok(assembled)
}

/// Fetch one stored chunk, within `timeout`, and verify it against its
/// content address.
async fn fetch_verified_chunk(
    op: Operator,
    chunk_key: String,
    expected_hash: String,
    index: usize,
    total: usize,
    manifest_path: String,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let chunk = tokio::time::timeout(timeout, op.read(&chunk_key))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out after {timeout:?} downloading chunk {}/{total}: {chunk_key}",
                index + 1
            )
        })?
        .with_context(|| format!("downloading chunk {}/{total}: {chunk_key}", index + 1))?;
    let chunk_bytes = chunk.to_vec();
    let actual_chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
    anyhow::ensure!(
        actual_chunk_hash == expected_hash,
        "chunk integrity failure at index {index} for {manifest_path}: expected {expected_hash}, got {actual_chunk_hash}"
    );
    Ok(chunk_bytes)
}

fn chunk_plaintext(
    crypto: Option<&HydrationCrypto>,
    index: usize,
    total: usize,
    stored: Vec<u8>,
    manifest_path: &str,
) -> Result<Vec<u8>> {
    match crypto {
        Some(crypto) => {
            tcfs_crypto::decrypt_chunk(&crypto.file_key, index as u64, &crypto.file_id, &stored)
                .with_context(|| {
                    format!("decrypting chunk {}/{total} for {manifest_path}", index + 1)
                })
        }
        None => Ok(stored),
    }
}

/// Fetch, verify, decrypt and hand each chunk's plaintext to `sink` in file
/// order, with at most `limits.window` chunks fetched ahead. The whole-file
/// size and BLAKE3 identity of a modern manifest are verified incrementally;
/// the sink has seen every byte by the time an integrity failure is
/// reported, so a caller writing to disk must discard its output on error.
///
/// Every chunk download is bounded by `limits.chunk_timeout`, and the wait
/// for the next completed chunk by `limits.stall_timeout`. On any error the
/// outstanding downloads are aborted with the window.
async fn stream_prepared_content<F>(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    manifest: &SyncManifest,
    crypto: Option<&HydrationCrypto>,
    limits: &HydrationLimits,
    mut sink: F,
) -> Result<HydrationStats>
where
    F: FnMut(&[u8]) -> Result<()>,
{
    let window = limits.window.max(1);
    debug!(manifest = %manifest_path, window, "hydrating");

    let prefix = remote_prefix.trim_end_matches('/');
    let hashes = manifest.chunk_hashes();
    let total = hashes.len();

    let mut pending: JoinSet<(usize, Result<Vec<u8>>)> = JoinSet::new();
    let mut ready: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut next_spawn = 0usize;
    let mut next_emit = 0usize;
    let mut buffered = 0u64;
    let mut stats = HydrationStats::default();
    let mut hasher = blake3::Hasher::new();

    while next_emit < total {
        // Keep the window full: fetched-ahead chunks plus in-flight fetches
        // never exceed `window`, so memory is bounded by the window.
        while next_spawn < total && pending.len() + ready.len() < window {
            let index = next_spawn;
            let fetch = fetch_verified_chunk(
                op.clone(),
                format!("{prefix}/chunks/{}", hashes[index]),
                hashes[index].clone(),
                index,
                total,
                manifest_path.to_string(),
                limits.chunk_timeout,
            );
            pending.spawn(async move { (index, fetch.await) });
            next_spawn += 1;
        }

        if let Some(stored) = ready.remove(&next_emit) {
            buffered -= stored.len() as u64;
            let plaintext = chunk_plaintext(crypto, next_emit, total, stored, manifest_path)?;
            stats.bytes += plaintext.len() as u64;
            if !manifest.is_legacy() {
                anyhow::ensure!(
                    stats.bytes <= manifest.file_size,
                    "hydrated data exceeds declared file size for {manifest_path}: expected {}, got at least {}",
                    manifest.file_size,
                    stats.bytes
                );
            }
            hasher.update(&plaintext);
            sink(&plaintext)?;
            stats.chunks += 1;
            next_emit += 1;
            continue;
        }

        // `next_emit` is not ready, so it is still in flight (chunks are
        // spawned in order and the window always admits the oldest).
        let (index, fetched) = tokio::time::timeout(limits.stall_timeout, pending.join_next())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "hydration of {manifest_path} stalled: no chunk completed within {:?}",
                    limits.stall_timeout
                )
            })?
            .context("hydration window drained before every chunk was fetched")?
            .context("hydration chunk task failed")?;
        let stored = fetched?;
        buffered += stored.len() as u64;
        stats.peak_buffered_bytes = stats.peak_buffered_bytes.max(buffered);
        ready.insert(index, stored);
    }

    // v1 has no whole-file identity. Its chunks were verified against their
    // content addresses while fetching.
    if !manifest.is_legacy() {
        anyhow::ensure!(
            stats.bytes == manifest.file_size,
            "hydrated file size mismatch for {manifest_path}: expected {}, got {}",
            manifest.file_size,
            stats.bytes
        );
        let actual = hasher.finalize().to_hex().to_string();
        anyhow::ensure!(
            actual == manifest.file_hash.as_str(),
            "hydrated file integrity failure for {manifest_path}: expected {}, got {actual}",
            manifest.file_hash
        );
    }

    debug!(
        manifest = %manifest_path,
        bytes = stats.bytes,
        chunks = stats.chunks,
        peak_buffered_bytes = stats.peak_buffered_bytes,
        encrypted = crypto.is_some(),
        "hydrated"
    );

    Ok(stats)
}

/// Verify a cached plaintext file against a modern manifest by streaming it
/// (bounded memory), returning its length.
fn verify_cached_file(file: &mut std::fs::File, manifest: &SyncManifest) -> Result<u64> {
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0))
        .context("rewinding cache entry")?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buf).context("reading cache entry")?;
        if n == 0 {
            break;
        }
        len += n as u64;
        anyhow::ensure!(
            len <= manifest.file_size,
            "cache entry longer than manifest"
        );
        hasher.update(&buf[..n]);
    }
    anyhow::ensure!(len == manifest.file_size, "cache entry size mismatch");
    anyhow::ensure!(
        hasher.finalize().to_hex().as_str() == manifest.file_hash.as_str(),
        "cache entry hash mismatch"
    );
    Ok(len)
}

/// Hydrate into the disk cache with bounded memory and return a read
/// descriptor on the verified content.
///
/// A modern cached entry is re-verified (streamed, bounded memory) before it
/// is trusted; a corrupt one is evicted and refetched. Legacy v1 content has
/// no whole-file identity, so it is never cached: it is hydrated into an
/// unlinked private file every time, as before.
pub async fn hydrate_cached_file_from_manifest_bytes(
    op: &Operator,
    manifest_path: &str,
    manifest_bytes: &[u8],
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
) -> Result<CachedContent> {
    hydrate_cached_file_with_window(
        op,
        manifest_path,
        manifest_bytes,
        remote_prefix,
        cache,
        master_key,
        DEFAULT_HYDRATION_WINDOW,
    )
    .await
}

/// [`hydrate_cached_file_from_manifest_bytes`] with an explicit fetch window.
pub async fn hydrate_cached_file_with_window(
    op: &Operator,
    manifest_path: &str,
    manifest_bytes: &[u8],
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
    window: usize,
) -> Result<CachedContent> {
    hydrate_cached_file_with_limits(
        op,
        manifest_path,
        manifest_bytes,
        remote_prefix,
        cache,
        master_key,
        &HydrationLimits {
            window,
            ..HydrationLimits::default()
        },
    )
    .await
}

/// [`hydrate_cached_file_from_manifest_bytes`] with explicit limits.
///
/// A modern cache entry that this cache already verified against the same
/// manifest, and whose file is unchanged since (same device, inode, size,
/// mtime and ctime), is served without re-hashing it; any other entry is
/// re-verified in full first.
pub async fn hydrate_cached_file_with_limits(
    op: &Operator,
    manifest_path: &str,
    manifest_bytes: &[u8],
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
    limits: &HydrationLimits,
) -> Result<CachedContent> {
    let (manifest, crypto) = prepare_manifest(manifest_bytes, manifest_path, master_key)?;
    let key = cache_key_for_path(manifest_path);

    if !manifest.is_legacy() {
        if let Some(file) = cache.open_entry(&key)? {
            if cache.is_verified(&key, &file, &manifest.file_hash, manifest.file_size) {
                debug!(manifest = %manifest_path, "hydration cache hit (already verified)");
                return Ok(CachedContent {
                    file,
                    len: manifest.file_size,
                    stats: None,
                });
            }
            cache.note_full_verification();
            let verifying = manifest.clone();
            let checked = tokio::task::spawn_blocking(move || {
                let mut file = file;
                verify_cached_file(&mut file, &verifying).map(|len| (file, len))
            })
            .await
            .context("cache verification task failed")?;
            match checked {
                Ok((file, len)) => {
                    debug!(manifest = %manifest_path, "hydration cache hit");
                    cache.remember_verified(&key, &file, &manifest.file_hash, len);
                    return Ok(CachedContent {
                        file,
                        len,
                        stats: None,
                    });
                }
                Err(error) => {
                    warn!(manifest = %manifest_path, %error, "discarding unverifiable or corrupt hydration cache entry");
                    if let Err(error) = cache.evict(&key).await {
                        warn!(manifest = %manifest_path, %error, "failed to evict invalid hydration cache entry");
                    }
                }
            }
        }
    }

    // Only a fetch takes an admission slot, and only for as long as it runs.
    let _admitted = match &limits.admission {
        Some(admission) => Some(
            Arc::clone(admission)
                .acquire_owned()
                .await
                .context("hydration admission closed")?,
        ),
        None => None,
    };

    // `tmp` removes the temporary name when it is dropped uncommitted: on an
    // integrity or fetch error, and also when this future is dropped part way
    // (a FUSE/NFS protocol timeout). Unverified bytes are never left where a
    // later open, or the cache's size accounting, could miss them.
    let (tmp, mut tmp_file) = cache.create_tmp(&key)?;
    let stats = stream_prepared_content(
        op,
        manifest_path,
        remote_prefix,
        &manifest,
        crypto.as_ref(),
        limits,
        |plaintext| {
            tmp_file
                .write_all(plaintext)
                .context("writing hydrated chunk to cache")
        },
    )
    .await?;
    tmp_file.flush().context("flushing hydrated cache file")?;

    if manifest.is_legacy() {
        // Keep the open descriptor, drop the name: v1 content is not cached.
        drop(tmp);
    } else {
        match cache.commit_tmp(tmp, &key).await {
            // Just verified while streaming: a later open need not re-hash it.
            Ok(()) => cache.remember_verified(&key, &tmp_file, &manifest.file_hash, stats.bytes),
            // The verified bytes stay readable through the descriptor.
            Err(error) => {
                warn!(manifest = %manifest_path, %error, "failed to commit hydrated content to cache")
            }
        }
    }

    Ok(CachedContent {
        file: tmp_file,
        len: stats.bytes,
        stats: Some(stats),
    })
}

/// Plaintext layout of a manifest's chunks: `(offset, length)` per chunk,
/// derived from the stored object sizes (stat only, no content reads).
async fn chunk_layout(
    op: &Operator,
    prefix: &str,
    manifest: &SyncManifest,
    encrypted: bool,
    manifest_path: &str,
) -> Result<Vec<(u64, u64)>> {
    let hashes = manifest.chunk_hashes();
    let mut stats: JoinSet<(usize, Result<u64>)> = JoinSet::new();
    let mut lengths = vec![0u64; hashes.len()];
    let mut next = 0usize;
    let mut done = 0usize;
    while done < hashes.len() {
        while next < hashes.len() && stats.len() < DEFAULT_HYDRATION_WINDOW * 2 {
            let index = next;
            let op = op.clone();
            let key = format!("{prefix}/chunks/{}", hashes[index]);
            stats.spawn(async move {
                let stored = op
                    .stat(&key)
                    .await
                    .map(|meta| meta.content_length())
                    .with_context(|| format!("stat chunk {key}"));
                (index, stored)
            });
            next += 1;
        }
        let (index, stored) = stats
            .join_next()
            .await
            .context("chunk layout drained early")?
            .context("chunk layout task failed")?;
        let stored = stored?;
        lengths[index] =
            if encrypted {
                stored.checked_sub(ENCRYPTED_CHUNK_OVERHEAD).with_context(|| {
                format!("stored chunk {index} shorter than encryption overhead for {manifest_path}")
            })?
            } else {
                stored
            };
        done += 1;
    }

    let mut layout = Vec::with_capacity(lengths.len());
    let mut offset = 0u64;
    for length in lengths {
        layout.push((offset, length));
        offset = offset
            .checked_add(length)
            .context("chunk layout overflows u64")?;
    }
    if !manifest.is_legacy() {
        anyhow::ensure!(
            offset == manifest.file_size,
            "chunk layout for {manifest_path} covers {offset} bytes but the manifest declares {}",
            manifest.file_size
        );
    }
    Ok(layout)
}

/// Fetch `len` bytes at `offset` by fetching only the chunks that cover the
/// range (ranged hydration). Each fetched chunk is verified against its
/// content address and, when encrypted, authenticated by AEAD under its
/// index; its plaintext length must match the layout. The whole-file hash
/// cannot be checked for a partial read. Reads past EOF are clamped.
///
/// **Not wired to any surface yet, and must not be until the layout is bound
/// to the signed manifest.** Chunk offsets come from storage `stat` sizes,
/// which only the fetched chunks' content addresses verify. The sum check
/// against the manifest's `file_size` catches one truncated or corrupt
/// unfetched chunk, but two compensating size changes would shift every
/// later offset undetected. Carrying per-chunk plaintext lengths in the
/// manifest closes that; until then this is a building block, not a read
/// path. Legacy v1 manifests carry no `file_size` at all, so they are
/// refused here.
pub async fn fetch_range_from_manifest_bytes(
    op: &Operator,
    manifest_path: &str,
    manifest_bytes: &[u8],
    remote_prefix: &str,
    master_key: Option<&[u8; 32]>,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let (manifest, crypto) = prepare_manifest(manifest_bytes, manifest_path, master_key)?;
    anyhow::ensure!(
        !manifest.is_legacy(),
        "ranged hydration refuses legacy v1 manifest {manifest_path}: it declares no file size, \
         so a corrupt unfetched chunk could shift the layout undetected"
    );
    let prefix = remote_prefix.trim_end_matches('/');
    let layout = chunk_layout(op, prefix, &manifest, crypto.is_some(), manifest_path).await?;
    let file_len = layout
        .last()
        .map(|(start, length)| start + length)
        .unwrap_or(0);
    if offset >= file_len || len == 0 {
        return Ok(Vec::new());
    }
    let end = offset.saturating_add(len).min(file_len);
    let hashes = manifest.chunk_hashes();
    let total = hashes.len();
    let first = layout.partition_point(|(start, length)| start + length <= offset);

    let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
    for (index, &(start, length)) in layout.iter().enumerate().skip(first) {
        if start >= end {
            break;
        }
        let stored = fetch_verified_chunk(
            op.clone(),
            format!("{prefix}/chunks/{}", hashes[index]),
            hashes[index].clone(),
            index,
            total,
            manifest_path.to_string(),
            DEFAULT_CHUNK_FETCH_TIMEOUT,
        )
        .await?;
        let plaintext = chunk_plaintext(crypto.as_ref(), index, total, stored, manifest_path)?;
        anyhow::ensure!(
            plaintext.len() as u64 == length,
            "chunk {index} for {manifest_path} decoded to {} bytes, layout expected {length}",
            plaintext.len()
        );
        let from = usize::try_from(offset.max(start) - start).context("range offset")?;
        let to = usize::try_from(end.min(start + length) - start).context("range end")?;
        out.extend_from_slice(&plaintext[from..to]);
    }
    Ok(out)
}

/// Fetch content using the disk cache as a read-through layer.
///
/// Modern cached plaintext is checked against the current manifest before it
/// is returned. Corrupt cache entries are evicted and repaired from remote
/// chunks. Legacy v1 manifests have no whole-file identity, so their cache
/// entries are bypassed and their chunks are reverified on every hydration.
pub async fn fetch_cached(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    let (manifest, crypto) = load_manifest(op, manifest_path, master_key).await?;
    fetch_cached_prepared(
        op,
        manifest_path,
        remote_prefix,
        cache,
        &manifest,
        crypto.as_ref(),
    )
    .await
}

/// Hydrate using manifest bytes already bound to an exact index snapshot.
///
/// Callers that validate index-to-manifest identity must pass those same bytes
/// here so a replacement of the manifest key cannot split authorization,
/// vector-clock state, cache validation, and chunk selection across two reads.
pub async fn fetch_cached_from_manifest_bytes(
    op: &Operator,
    manifest_path: &str,
    manifest_bytes: &[u8],
    remote_prefix: &str,
    cache: &DiskCache,
    master_key: Option<&[u8; 32]>,
) -> Result<Vec<u8>> {
    let (manifest, crypto) = prepare_manifest(manifest_bytes, manifest_path, master_key)?;
    fetch_cached_prepared(
        op,
        manifest_path,
        remote_prefix,
        cache,
        &manifest,
        crypto.as_ref(),
    )
    .await
}

async fn fetch_cached_prepared(
    op: &Operator,
    manifest_path: &str,
    remote_prefix: &str,
    cache: &DiskCache,
    manifest: &SyncManifest,
    crypto: Option<&HydrationCrypto>,
) -> Result<Vec<u8>> {
    let key = cache_key_for_path(manifest_path);

    if let Some(data) = cache.get(&key).await {
        if !manifest.is_legacy() && validate_plaintext(manifest, &data, manifest_path).is_ok() {
            debug!(manifest = %manifest_path, "hydration cache hit");
            return Ok(data);
        }

        warn!(manifest = %manifest_path, "discarding unverifiable or corrupt hydration cache entry");
        if let Err(error) = cache.evict(&key).await {
            warn!(manifest = %manifest_path, %error, "failed to evict invalid hydration cache entry");
        }
    }

    let data = fetch_prepared_content(op, manifest_path, remote_prefix, manifest, crypto).await?;

    if let Err(error) = cache.put(&key, &data).await {
        warn!(manifest = %manifest_path, %error, "failed to cache hydrated content");
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;
    use tcfs_sync::conflict::VectorClock;
    use tcfs_sync::manifest::WrappedFileKey;

    const PREFIX: &str = "data";
    const MANIFEST_PATH: &str = "data/manifests/test";

    fn memory_op() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    fn manifest(content: &[u8], chunks: Vec<String>) -> SyncManifest {
        SyncManifest {
            version: 2,
            file_hash: tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content)),
            file_size: content.len() as u64,
            chunks,
            vclock: VectorClock::new(),
            written_by: "test-device".into(),
            written_at: 0,
            rel_path: Some("test.bin".into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        }
    }

    async fn write_manifest(op: &Operator, manifest: &SyncManifest) {
        op.write(MANIFEST_PATH, manifest.to_bytes().unwrap())
            .await
            .unwrap();
    }

    async fn write_plain_chunk(op: &Operator, content: &[u8]) -> String {
        let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
        op.write(&format!("{PREFIX}/chunks/{hash}"), content.to_vec())
            .await
            .unwrap();
        hash
    }

    #[tokio::test]
    async fn hydrates_typed_plaintext_manifest_with_integrity_checks() {
        let op = memory_op();
        let content = b"typed hydration content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        write_manifest(&op, &manifest(content, vec![chunk_hash])).await;

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap();
        assert_eq!(hydrated, content);
    }

    #[tokio::test]
    async fn supports_zero_byte_files_without_chunks() {
        let op = memory_op();
        write_manifest(&op, &manifest(b"", Vec::new())).await;

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap();
        assert!(hydrated.is_empty());
    }

    #[tokio::test]
    async fn rejects_corrupt_stored_chunk_before_assembly() {
        let op = memory_op();
        let content = b"expected chunk";
        let declared = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(content));
        op.write(
            &format!("{PREFIX}/chunks/{declared}"),
            b"corrupt chunk".to_vec(),
        )
        .await
        .unwrap();
        write_manifest(&op, &manifest(content, vec![declared])).await;

        let error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("chunk integrity failure"));
    }

    #[tokio::test]
    async fn rejects_final_plaintext_size_and_hash_mismatches() {
        let op = memory_op();
        let content = b"assembled content";
        let chunk_hash = write_plain_chunk(&op, content).await;

        let mut wrong_size = manifest(content, vec![chunk_hash.clone()]);
        wrong_size.file_size += 1;
        write_manifest(&op, &wrong_size).await;
        let size_error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{size_error:#}").contains("file size mismatch"));

        let mut wrong_hash = manifest(content, vec![chunk_hash]);
        wrong_hash.file_hash =
            tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b"different content"));
        write_manifest(&op, &wrong_hash).await;
        let hash_error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{hash_error:#}").contains("file integrity failure"));
    }

    #[tokio::test]
    async fn decrypts_master_wrapped_v2_and_fails_closed_without_valid_master() {
        let op = memory_op();
        let content = b"encrypted VFS content";
        let content_hash = tcfs_chunks::hash_bytes(content);
        let file_id = *content_hash.as_bytes();
        let file_key = tcfs_crypto::generate_file_key();
        let ciphertext = tcfs_crypto::encrypt_chunk(&file_key, 0, &file_id, content).unwrap();
        let chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
        op.write(&format!("{PREFIX}/chunks/{chunk_hash}"), ciphertext)
            .await
            .unwrap();

        let master_bytes = [7u8; 32];
        let master = tcfs_crypto::MasterKey::from_bytes(master_bytes);
        let wrapped = tcfs_crypto::wrap_key(&master, &file_key).unwrap();
        let mut encrypted_manifest = manifest(content, vec![chunk_hash]);
        encrypted_manifest.encrypted_file_key =
            Some(base64::engine::general_purpose::STANDARD.encode(wrapped));
        write_manifest(&op, &encrypted_manifest).await;

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, Some(&master_bytes))
            .await
            .unwrap();
        assert_eq!(hydrated, content);

        let missing = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{missing:#}").contains("no master key"));

        let wrong_master = [8u8; 32];
        let wrong = fetch_content(&op, MANIFEST_PATH, PREFIX, Some(&wrong_master))
            .await
            .unwrap_err();
        assert!(format!("{wrong:#}").contains("unwrapping file key"));
    }

    #[tokio::test]
    async fn rejects_per_device_only_v3_without_reading_ciphertext() {
        let op = memory_op();
        let mut per_device = manifest(
            b"secret",
            vec![tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(
                b"ciphertext",
            ))],
        );
        per_device.version = 3;
        per_device.wrapped_file_keys = vec![WrappedFileKey {
            recipient_device_id: "device-a".into(),
            recipient: "age1recipient".into(),
            algorithm: "age-x25519-file-key-v1".into(),
            wrapped_key: "wrapped".into(),
        }];
        write_manifest(&op, &per_device).await;

        let error = fetch_content(&op, MANIFEST_PATH, PREFIX, Some(&[7u8; 32]))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("per-device-only manifest v3"));
    }

    #[tokio::test]
    async fn malformed_odd_length_file_hash_returns_error_without_panicking() {
        let op = memory_op();
        let mut malformed = manifest(b"", Vec::new());
        malformed.file_hash = "abc".into();
        write_manifest(&op, &malformed).await;

        let error = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid file hash"));
    }

    #[tokio::test]
    async fn corrupt_cache_entry_is_evicted_and_rehydrated() {
        let op = memory_op();
        let content = b"authoritative remote content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        write_manifest(&op, &manifest(content, vec![chunk_hash])).await;

        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 1024 * 1024);
        cache.put("test", b"corrupt cached bytes").await.unwrap();

        let hydrated = fetch_cached(&op, MANIFEST_PATH, PREFIX, &cache, None)
            .await
            .unwrap();
        assert_eq!(hydrated, content);
        assert_eq!(cache.get("test").await.unwrap(), content);
    }

    #[tokio::test]
    async fn bound_manifest_bytes_control_cache_validation_and_chunk_selection() {
        let op = memory_op();
        let bound_content = b"content selected by the bound index";
        let replacement_content = b"replacement at the same manifest key";
        let bound_chunk = write_plain_chunk(&op, bound_content).await;
        let replacement_chunk = write_plain_chunk(&op, replacement_content).await;
        let bound_manifest = manifest(bound_content, vec![bound_chunk]);
        let replacement_manifest = manifest(replacement_content, vec![replacement_chunk]);
        let bound_bytes = bound_manifest.to_bytes().unwrap();

        // Model replacement after the caller bound and validated the original
        // bytes. Hydration must not authorize with one generation and reread
        // another generation for its chunks or cached plaintext.
        write_manifest(&op, &replacement_manifest).await;

        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 1024 * 1024);
        let hydrated = fetch_cached_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &bound_bytes,
            PREFIX,
            &cache,
            None,
        )
        .await
        .unwrap();

        assert_eq!(hydrated, bound_content);
    }

    /// Publish `content` as `chunk_len`-byte plaintext chunks.
    async fn publish_chunked(op: &Operator, content: &[u8], chunk_len: usize) -> SyncManifest {
        let mut hashes = Vec::new();
        for piece in content.chunks(chunk_len) {
            hashes.push(write_plain_chunk(op, piece).await);
        }
        let manifest = manifest(content, hashes);
        write_manifest(op, &manifest).await;
        manifest
    }

    fn patterned(len: usize) -> Vec<u8> {
        (0..len as u64)
            .map(|i| (i.wrapping_mul(29) ^ (i >> 9)) as u8)
            .collect()
    }

    #[tokio::test]
    async fn streaming_hydration_holds_at_most_window_chunks() {
        let op = memory_op();
        let chunk_len = 1024;
        let content = patterned(64 * chunk_len);
        let manifest = publish_chunked(&op, &content, chunk_len).await;
        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 64 * 1024 * 1024);

        let window = 4;
        let hydrated = hydrate_cached_file_with_window(
            &op,
            MANIFEST_PATH,
            &manifest.to_bytes().unwrap(),
            PREFIX,
            &cache,
            None,
            window,
        )
        .await
        .unwrap();

        let stats = hydrated.stats.expect("a cold cache must fetch");
        assert_eq!(stats.chunks, 64);
        assert_eq!(stats.bytes, content.len() as u64);
        assert!(
            stats.peak_buffered_bytes <= (window * chunk_len) as u64,
            "peak {} exceeds the {window}-chunk window",
            stats.peak_buffered_bytes
        );
        assert_eq!(hydrated.len, content.len() as u64);
        let mut file = hydrated.file;
        let mut on_disk = Vec::new();
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_to_end(&mut on_disk).unwrap();
        assert_eq!(on_disk, content);

        // Second open is a verified cache hit with no fetch.
        let again = hydrate_cached_file_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &manifest.to_bytes().unwrap(),
            PREFIX,
            &cache,
            None,
        )
        .await
        .unwrap();
        assert!(again.stats.is_none());
        assert_eq!(again.len, content.len() as u64);
    }

    /// Counts chunk reads as storage sees them, independent of the
    /// hydration code's own bookkeeping.
    #[derive(Debug, Clone, Default)]
    struct CountChunkReads {
        started: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct CountChunkReadsAccessor<A> {
        inner: A,
        started: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl<A: opendal::raw::Access> opendal::raw::Layer<A> for CountChunkReads {
        type LayeredAccess = CountChunkReadsAccessor<A>;

        fn layer(&self, inner: A) -> Self::LayeredAccess {
            CountChunkReadsAccessor {
                inner,
                started: self.started.clone(),
            }
        }
    }

    impl<A: opendal::raw::Access> opendal::raw::LayeredAccess for CountChunkReadsAccessor<A> {
        type Inner = A;
        type Reader = A::Reader;
        type Writer = A::Writer;
        type Lister = A::Lister;
        type Deleter = A::Deleter;

        fn inner(&self) -> &A {
            &self.inner
        }

        async fn read(
            &self,
            path: &str,
            args: opendal::raw::OpRead,
        ) -> opendal::Result<(opendal::raw::RpRead, Self::Reader)> {
            if path.contains("/chunks/") {
                self.started
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            self.inner.read(path, args).await
        }

        async fn write(
            &self,
            path: &str,
            args: opendal::raw::OpWrite,
        ) -> opendal::Result<(opendal::raw::RpWrite, Self::Writer)> {
            self.inner.write(path, args).await
        }

        async fn delete(&self) -> opendal::Result<(opendal::raw::RpDelete, Self::Deleter)> {
            self.inner.delete().await
        }

        async fn list(
            &self,
            path: &str,
            args: opendal::raw::OpList,
        ) -> opendal::Result<(opendal::raw::RpList, Self::Lister)> {
            self.inner.list(path, args).await
        }
    }

    #[tokio::test]
    async fn chunks_fetched_ahead_of_the_writer_never_exceed_the_window() {
        // Measured at the storage boundary: every chunk read that has started
        // but not yet reached the sink may hold a chunk in memory (in flight,
        // finished but not joined, or waiting its turn). That count, not the
        // hydration code's own statistic, must stay within the window.
        let counter = CountChunkReads::default();
        let op = Operator::new(Memory::default())
            .unwrap()
            .finish()
            .layer(counter.clone());
        let chunk_len = 512;
        let content = patterned(64 * chunk_len);
        let manifest = publish_chunked(&op, &content, chunk_len).await;

        for window in [1usize, 3, 8] {
            counter
                .started
                .store(0, std::sync::atomic::Ordering::SeqCst);
            let mut emitted = 0usize;
            let mut max_outstanding = 0usize;
            let mut out = Vec::new();
            stream_prepared_content(
                &op,
                MANIFEST_PATH,
                PREFIX,
                &manifest,
                None,
                &HydrationLimits {
                    window,
                    ..HydrationLimits::default()
                },
                |plaintext| {
                    let started = counter.started.load(std::sync::atomic::Ordering::SeqCst);
                    max_outstanding = max_outstanding.max(started - emitted);
                    emitted += 1;
                    out.extend_from_slice(plaintext);
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert_eq!(out, content);
            assert_eq!(
                counter.started.load(std::sync::atomic::Ordering::SeqCst),
                64,
                "each chunk is fetched exactly once"
            );
            assert!(
                max_outstanding <= window,
                "{max_outstanding} chunks were fetched ahead of the writer with window {window}"
            );
        }
    }

    #[tokio::test]
    async fn streaming_hydration_never_caches_unverified_bytes() {
        let op = memory_op();
        let content = patterned(8 * 1024);
        let mut manifest = publish_chunked(&op, &content, 1024).await;
        manifest.file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b"other"));
        write_manifest(&op, &manifest).await;
        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 64 * 1024 * 1024);

        let error = hydrate_cached_file_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &manifest.to_bytes().unwrap(),
            PREFIX,
            &cache,
            None,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("file integrity failure"));
        assert_eq!(cache.stats().await.unwrap().entry_count, 0);
        let leftovers: Vec<_> = walk_files(temp.path());
        assert!(
            leftovers.is_empty(),
            "temp bytes left behind: {leftovers:?}"
        );
    }

    fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk_files(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[tokio::test]
    async fn corrupt_cached_file_is_evicted_and_refetched() {
        let op = memory_op();
        let content = patterned(4096);
        let manifest = publish_chunked(&op, &content, 1024).await;
        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 64 * 1024 * 1024);
        cache.put("test", b"corrupt cached bytes").await.unwrap();

        let hydrated = hydrate_cached_file_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &manifest.to_bytes().unwrap(),
            PREFIX,
            &cache,
            None,
        )
        .await
        .unwrap();
        assert!(
            hydrated.stats.is_some(),
            "a corrupt entry must be refetched"
        );
        assert_eq!(cache.get("test").await.unwrap(), content);
    }

    #[tokio::test]
    async fn fetch_range_reads_only_covering_chunks() {
        let op = memory_op();
        let chunk_len = 1000;
        let content = patterned(10 * chunk_len);
        let manifest = publish_chunked(&op, &content, chunk_len).await;
        let bytes = manifest.to_bytes().unwrap();

        // Destroy every chunk outside chunks 2..=3. A ranged read inside
        // them must still succeed because it never fetches the others.
        for (index, hash) in manifest.chunk_hashes().iter().enumerate() {
            if !(2..=3).contains(&index) {
                op.write(&format!("{PREFIX}/chunks/{hash}"), vec![0u8; chunk_len])
                    .await
                    .unwrap();
            }
        }

        let slice =
            fetch_range_from_manifest_bytes(&op, MANIFEST_PATH, &bytes, PREFIX, None, 2500, 1000)
                .await
                .unwrap();
        assert_eq!(slice, &content[2500..3500]);

        // A range touching a corrupted chunk fails closed.
        let error =
            fetch_range_from_manifest_bytes(&op, MANIFEST_PATH, &bytes, PREFIX, None, 3900, 200)
                .await
                .unwrap_err();
        assert!(format!("{error:#}").contains("chunk integrity failure"));

        // Past EOF is clamped, not an error.
        let tail =
            fetch_range_from_manifest_bytes(&op, MANIFEST_PATH, &bytes, PREFIX, None, 20_000, 10)
                .await
                .unwrap();
        assert!(tail.is_empty());
    }

    #[tokio::test]
    async fn fetch_range_refuses_layout_shifted_by_truncated_unfetched_chunk() {
        let op = memory_op();
        let chunk_len = 1000;
        let content = patterned(6 * chunk_len);
        let manifest = publish_chunked(&op, &content, chunk_len).await;
        let bytes = manifest.to_bytes().unwrap();

        // Truncate chunk 1, which a read inside chunk 4 never fetches. Its
        // shorter stat size would shift chunk 4's offset by 100 bytes.
        let hashes = manifest.chunk_hashes();
        op.write(
            &format!("{PREFIX}/chunks/{}", hashes[1]),
            content[chunk_len..2 * chunk_len - 100].to_vec(),
        )
        .await
        .unwrap();

        let error =
            fetch_range_from_manifest_bytes(&op, MANIFEST_PATH, &bytes, PREFIX, None, 4200, 100)
                .await
                .expect_err("a shifted layout must fail closed, not return misplaced bytes");
        assert!(
            format!("{error:#}").contains("chunk layout"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn fetch_range_refuses_legacy_v1_manifests() {
        let op = memory_op();
        let content = b"legacy content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        let legacy = format!("{chunk_hash}\n").into_bytes();

        let error =
            fetch_range_from_manifest_bytes(&op, MANIFEST_PATH, &legacy, PREFIX, None, 0, 4)
                .await
                .expect_err("legacy v1 has no file size to bind the layout");
        assert!(format!("{error:#}").contains("legacy v1"));
    }

    #[tokio::test]
    async fn cancelled_hydration_leaves_no_temp_file() {
        let op = memory_op();
        let content = patterned(16 * 1024);
        let manifest = publish_chunked(&op, &content, 1024).await;
        let temp = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(temp.path().to_path_buf(), 64 * 1024 * 1024);
        let manifest_bytes = manifest.to_bytes().unwrap();

        // Drive the hydration until its temp file exists and the first chunk
        // fetches are in flight, then drop it, as a protocol timeout does.
        let mut hydration = Box::pin(hydrate_cached_file_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &manifest_bytes,
            PREFIX,
            &cache,
            None,
        ));
        let cache_dir = temp.path().to_path_buf();
        let cancelled = tokio::select! {
            biased;
            _ = &mut hydration => false,
            _ = async {
                while walk_files(&cache_dir).is_empty() {
                    tokio::task::yield_now().await;
                }
            } => true,
        };
        assert!(cancelled, "fixture must cancel mid-hydration");
        drop(hydration);

        let leftovers = walk_files(temp.path());
        assert!(
            leftovers.is_empty(),
            "a cancelled hydration left files behind: {leftovers:?}"
        );
        assert_eq!(cache.stats().await.unwrap().entry_count, 0);
    }

    #[tokio::test]
    async fn fetch_range_decrypts_and_checks_layout() {
        let op = memory_op();
        let content = patterned(3000);
        let file_id = *tcfs_chunks::hash_bytes(&content).as_bytes();
        let file_key = tcfs_crypto::generate_file_key();
        let mut hashes = Vec::new();
        for (index, piece) in content.chunks(1000).enumerate() {
            let ciphertext =
                tcfs_crypto::encrypt_chunk(&file_key, index as u64, &file_id, piece).unwrap();
            let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
            op.write(&format!("{PREFIX}/chunks/{hash}"), ciphertext)
                .await
                .unwrap();
            hashes.push(hash);
        }
        let master_bytes = [9u8; 32];
        let master = tcfs_crypto::MasterKey::from_bytes(master_bytes);
        let mut encrypted = manifest(&content, hashes);
        encrypted.encrypted_file_key = Some(
            base64::engine::general_purpose::STANDARD
                .encode(tcfs_crypto::wrap_key(&master, &file_key).unwrap()),
        );
        let bytes = encrypted.to_bytes().unwrap();

        let slice = fetch_range_from_manifest_bytes(
            &op,
            MANIFEST_PATH,
            &bytes,
            PREFIX,
            Some(&master_bytes),
            999,
            1002,
        )
        .await
        .unwrap();
        assert_eq!(slice, &content[999..2001]);
    }

    #[tokio::test]
    async fn legacy_v1_verifies_each_chunk_content_address() {
        let op = memory_op();
        let content = b"legacy content";
        let chunk_hash = write_plain_chunk(&op, content).await;
        op.write(MANIFEST_PATH, format!("{chunk_hash}\n").into_bytes())
            .await
            .unwrap();

        let hydrated = fetch_content(&op, MANIFEST_PATH, PREFIX, None)
            .await
            .unwrap();
        assert_eq!(hydrated, content);
    }
}
