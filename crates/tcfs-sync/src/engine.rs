//! Sync engine: upload and download workers using OpenDAL + tcfs-chunks
//!
//! Phase 2 implementation covers:
//!   - `upload_file`: chunk → hash → skip if remote exists → upload via OpenDAL
//!   - `download_file`: fetch chunk objects → reassemble → write to local path
//!   - `push_tree`: walk a directory tree, upload changed files
//!   - `pull_file`: download a single remote path to local
//!
//! Phase 6 additions:
//!   - SyncManifest v2 (JSON with vector clocks)
//!   - Conflict detection via VectorClock comparison
//!   - Config-driven file collection (.git handling, exclude patterns)

use anyhow::{Context, Result};
use opendal::Operator;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::conflict::{compare_clocks, SyncOutcome};
use crate::manifest::SyncManifest;
use crate::state::{make_sync_state_full, StateCache};

/// Optional encryption context for E2E encrypted push/pull.
///
/// When present, chunks are encrypted before upload and decrypted after download
/// using XChaCha20-Poly1305 with per-file keys wrapped by the master key.
#[cfg(feature = "crypto")]
pub struct EncryptionContext {
    pub master_key: tcfs_crypto::MasterKey,
}

/// Type alias for optional encryption context (feature-gated).
#[cfg(feature = "crypto")]
pub type OptionalEncryption<'a> = Option<&'a EncryptionContext>;

/// Stub type when crypto feature is disabled — always None.
#[cfg(not(feature = "crypto"))]
pub type OptionalEncryption<'a> = Option<&'a ()>;

/// Progress callback type (bytes_done, bytes_total, message)
pub type ProgressFn = Box<dyn Fn(u64, u64, &str) + Send + Sync>;

/// Configuration for file collection (which files to include/exclude).
#[derive(Debug, Clone)]
pub struct CollectConfig {
    /// Whether to include .git directories
    pub sync_git_dirs: bool,
    /// Git sync mode: "bundle" or "raw"
    pub git_sync_mode: String,
    /// Whether to include hidden directories (dotfiles/dotdirs)
    pub sync_hidden_dirs: bool,
    /// Glob patterns to exclude
    pub exclude_patterns: Vec<String>,
    /// Whether to follow symlinks (default: false — skip with warning)
    pub follow_symlinks: bool,
    /// Whether to sync empty directories via `.tcfs_dir` markers
    pub sync_empty_dirs: bool,
}

impl Default for CollectConfig {
    fn default() -> Self {
        Self {
            sync_git_dirs: false,
            git_sync_mode: "bundle".into(),
            sync_hidden_dirs: false,
            exclude_patterns: Vec::new(),
            follow_symlinks: false,
            sync_empty_dirs: true,
        }
    }
}

/// Result of collecting files and empty directories from a local tree.
#[derive(Debug, Clone)]
pub struct CollectResult {
    /// Regular files to upload.
    pub files: Vec<PathBuf>,
    /// Empty directories (no files after exclusions) to create markers for.
    pub empty_dirs: Vec<PathBuf>,
}

/// Result of uploading a single file
#[derive(Debug)]
pub struct UploadResult {
    pub path: PathBuf,
    pub remote_path: String,
    pub hash: String,
    pub chunks: usize,
    pub bytes: u64,
    /// true if file was already up-to-date (skipped)
    pub skipped: bool,
    /// Sync outcome if conflict detection was performed
    pub outcome: Option<SyncOutcome>,
}

/// Result of downloading a single file
#[derive(Debug)]
pub struct DownloadResult {
    pub remote_path: String,
    pub local_path: PathBuf,
    pub bytes: u64,
}

/// Upload a single file to SeaweedFS, chunking it via FastCDC.
///
/// If the file is unchanged since the last sync (per state cache), the upload
/// is skipped and the cached state is returned.
///
/// Each chunk is stored at `{bucket_prefix}/chunks/{hash}`. A manifest object
/// at `{bucket_prefix}/manifests/{file_hash}` lists the chunk hashes in order.
///
/// When `device_id` is provided, vector clock comparison is performed against
/// the remote manifest to detect conflicts.
pub async fn upload_file(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
) -> Result<UploadResult> {
    upload_file_with_device(
        op,
        local_path,
        remote_prefix,
        state,
        progress,
        "",
        None,
        None,
    )
    .await
}

/// Upload with device identity, vector clock awareness, and optional encryption.
#[allow(unused_variables)]
#[allow(clippy::too_many_arguments)]
pub async fn upload_file_with_device(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
    device_id: &str,
    rel_path: Option<&str>,
    encryption: OptionalEncryption<'_>,
) -> Result<UploadResult> {
    // Fast-path: check if file is already up-to-date
    match state.needs_sync(local_path)? {
        None => {
            let cached = state.get(local_path).ok_or_else(|| {
                anyhow::anyhow!(
                    "state entry vanished during upload for {}",
                    local_path.display()
                )
            })?;
            let result = UploadResult {
                path: local_path.to_path_buf(),
                remote_path: cached.remote_path.clone(),
                hash: cached.blake3.clone(),
                chunks: cached.chunk_count,
                bytes: cached.size,
                skipped: true,
                outcome: Some(SyncOutcome::UpToDate),
            };
            debug!(path = %local_path.display(), "skip: unchanged since last sync");
            return Ok(result);
        }
        Some(reason) => {
            debug!(path = %local_path.display(), reason = %reason, "uploading");
        }
    }

    // Tiered chunking: files below STREAMING_THRESHOLD are read into memory,
    // larger files use streaming two-pass (hash, then chunk) to bound memory.
    let file_meta = std::fs::metadata(local_path)
        .with_context(|| format!("stat for chunking: {}", local_path.display()))?;
    let file_size = file_meta.len();

    let use_streaming = file_size >= tcfs_chunks::STREAMING_THRESHOLD;

    // Pass 1: compute file hash
    let file_hash_hex = if use_streaming {
        let hash = tcfs_chunks::hash_file_streaming(local_path)
            .with_context(|| format!("streaming hash: {}", local_path.display()))?;
        tcfs_chunks::hash_to_hex(&hash)
    } else {
        // Small file: read fully, hash in memory
        let data = std::fs::read(local_path)
            .with_context(|| format!("reading for hash: {}", local_path.display()))?;
        tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data))
    };

    // Pass 2: chunk (deferred until after conflict check — may skip upload)

    // Build remote manifest path (using the file's content hash)
    let remote_manifest = format!("{remote_prefix}/manifests/{file_hash_hex}");

    // Get the local vclock from state (or start fresh)
    let mut local_vclock = state
        .get(local_path)
        .map(|s| s.vclock.clone())
        .unwrap_or_default();

    // Conflict detection: find the current remote manifest for this rel_path.
    // First try the index entry (covers different-content conflicts), then
    // fall back to checking the same-hash manifest path.
    let mut outcome = None;
    let mut remote_vclock_snapshot: Option<crate::conflict::VectorClock> = None;
    if !device_id.is_empty() {
        let remote_manifest_obj = if let Some(rp) = rel_path {
            // Look up the index entry to find what manifest is currently stored
            let index_key = format!("{}/index/{}", remote_prefix.trim_end_matches('/'), rp);
            let idx_manifest = if let Ok(idx_bytes) = op.read(&index_key).await {
                let idx_raw = idx_bytes.to_bytes();
                let idx_str = String::from_utf8_lossy(&idx_raw);
                // Parse "manifest_hash=<hash>\nsize=...\n"
                idx_str
                    .lines()
                    .find_map(|l| l.strip_prefix("manifest_hash="))
                    .map(|h| format!("{}/manifests/{}", remote_prefix.trim_end_matches('/'), h))
            } else {
                None
            };
            // Read the manifest pointed to by the index entry
            if let Some(ref manifest_path) = idx_manifest {
                if let Ok(remote_bytes) = op.read(manifest_path).await {
                    SyncManifest::from_bytes(&remote_bytes.to_bytes()).ok()
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            // No rel_path — fall back to checking the same-hash manifest
            if let Ok(true) = op.exists(&remote_manifest).await {
                if let Ok(remote_bytes) = op.read(&remote_manifest).await {
                    SyncManifest::from_bytes(&remote_bytes.to_bytes()).ok()
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Capture remote vclock for deferred merge (Issue #183)
        remote_vclock_snapshot = remote_manifest_obj.as_ref().map(|m| m.vclock.clone());

        if let Some(remote_manifest_obj) = remote_manifest_obj {
            let local_hash = &file_hash_hex;
            let remote_hash = &remote_manifest_obj.file_hash;
            let rp = rel_path.unwrap_or("");

            let sync_outcome = compare_clocks(
                &local_vclock,
                &remote_manifest_obj.vclock,
                local_hash,
                remote_hash,
                rp,
                device_id,
                &remote_manifest_obj.written_by,
            );

            match &sync_outcome {
                SyncOutcome::RemoteNewer => {
                    return Ok(UploadResult {
                        path: local_path.to_path_buf(),
                        remote_path: remote_manifest.clone(),
                        hash: file_hash_hex,
                        chunks: 0,
                        bytes: file_size,
                        skipped: true,
                        outcome: Some(sync_outcome),
                    });
                }
                SyncOutcome::Conflict(ref conflict_info) => {
                    // Record local state with conflict info so `tcfs resolve` can find it
                    let mut sync_state = make_sync_state_full(
                        local_path,
                        file_hash_hex.clone(),
                        0,
                        remote_manifest.clone(),
                        local_vclock,
                        device_id.to_string(),
                    )?;
                    sync_state.conflict = Some(conflict_info.clone());
                    state.set(local_path, sync_state);
                    return Ok(UploadResult {
                        path: local_path.to_path_buf(),
                        remote_path: remote_manifest.clone(),
                        hash: file_hash_hex,
                        chunks: 0,
                        bytes: file_size,
                        skipped: true,
                        outcome: Some(sync_outcome),
                    });
                }
                SyncOutcome::UpToDate => {
                    // Content dedup — already up to date
                    let sync_state = make_sync_state_full(
                        local_path,
                        file_hash_hex.clone(),
                        0,
                        remote_manifest.clone(),
                        local_vclock,
                        device_id.to_string(),
                    )?;
                    state.set(local_path, sync_state);
                    return Ok(UploadResult {
                        path: local_path.to_path_buf(),
                        remote_path: remote_manifest,
                        hash: file_hash_hex,
                        chunks: 0,
                        bytes: file_size,
                        skipped: true,
                        outcome: Some(sync_outcome),
                    });
                }
                SyncOutcome::LocalNewer => {
                    // Defer vclock merge until after successful manifest upload
                    // (prevents stale vclocks if upload fails)
                    outcome = Some(SyncOutcome::LocalNewer);
                }
            }
        }
    }

    // Check if this exact content is already stored (content-addressed dedup)
    // Only check when we haven't already done the remote manifest check above
    if outcome.is_none()
        && op.exists(&remote_manifest).await.unwrap_or(false)
        && device_id.is_empty()
    {
        debug!(hash = %file_hash_hex, "dedup: manifest already exists");
        let remote_path = remote_manifest.clone();
        let sync_state = make_sync_state_full(
            local_path,
            file_hash_hex.clone(),
            0,
            remote_path.clone(),
            local_vclock,
            device_id.to_string(),
        )?;
        state.set(local_path, sync_state);
        return Ok(UploadResult {
            path: local_path.to_path_buf(),
            remote_path,
            hash: file_hash_hex,
            chunks: 0,
            bytes: file_size,
            skipped: false,
            outcome: None,
        });
    }

    // Tick local vclock before writing
    if !device_id.is_empty() {
        local_vclock.tick(device_id);
    }

    // Chunk the file (deferred until after conflict/dedup checks).
    // Small files: read into memory. Large files: streaming chunker.
    let mut chunk_hashes = Vec::new();
    let mut bytes_uploaded = 0u64;
    let num_chunks;

    // Generate per-file encryption key if encryption is enabled
    #[cfg(feature = "crypto")]
    let (file_key, file_id) = if encryption.is_some() {
        let fk = tcfs_crypto::generate_file_key();
        let fid: [u8; 32] = {
            let hash = tcfs_chunks::hash_from_hex(&file_hash_hex)
                .context("parsing file hash for encryption file_id")?;
            *hash.as_bytes()
        };
        (Some(fk), Some(fid))
    } else {
        (None, None)
    };

    if use_streaming {
        // ── Streaming path: bounded memory for large files ─────────
        debug!(path = %local_path.display(), size = file_size, "using streaming chunker");

        // Verify file hasn't changed between hash and chunk passes
        let pre_chunk_meta = std::fs::metadata(local_path)
            .with_context(|| format!("re-stat before streaming chunk: {}", local_path.display()))?;
        if pre_chunk_meta.len() != file_size {
            anyhow::bail!(
                "file size changed between hash and chunk passes: {} ({} → {})",
                local_path.display(),
                file_size,
                pre_chunk_meta.len()
            );
        }

        let streaming_chunks = tcfs_chunks::chunk_file_streaming(local_path)
            .with_context(|| format!("streaming chunk: {}", local_path.display()))?;

        num_chunks = streaming_chunks.len();
        chunk_hashes.reserve(num_chunks);

        for (i, chunk) in streaming_chunks.iter().enumerate() {
            #[cfg(feature = "crypto")]
            let (upload_data, chunk_hash_hex) =
                if let (Some(ref fk), Some(ref fid)) = (&file_key, &file_id) {
                    let ciphertext = tcfs_crypto::encrypt_chunk(fk, i as u64, fid, &chunk.data)
                        .with_context(|| format!("encrypting chunk {i}"))?;
                    let ct_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
                    (ciphertext, ct_hash)
                } else {
                    let h = tcfs_chunks::hash_to_hex(&chunk.hash);
                    (chunk.data.clone(), h)
                };

            #[cfg(not(feature = "crypto"))]
            let (upload_data, chunk_hash_hex) = {
                let h = tcfs_chunks::hash_to_hex(&chunk.hash);
                (chunk.data.clone(), h)
            };

            let chunk_key = format!("{remote_prefix}/chunks/{chunk_hash_hex}");

            if !op.exists(&chunk_key).await.unwrap_or(false) {
                op.write(&chunk_key, upload_data)
                    .await
                    .with_context(|| format!("uploading chunk {i}: {chunk_key}"))?;
                bytes_uploaded += chunk.data.len() as u64;
            }

            chunk_hashes.push(chunk_hash_hex);

            if let Some(cb) = progress {
                cb(
                    (i + 1) as u64,
                    num_chunks as u64,
                    &format!("chunk {}/{num_chunks}", i + 1),
                );
            }
        }
    } else {
        // ── In-memory path: small files ───────────────────────────
        let (chunks, data) = tcfs_chunks::chunk_file(local_path)
            .with_context(|| format!("chunking: {}", local_path.display()))?;

        num_chunks = chunks.len();
        chunk_hashes.reserve(num_chunks);

        for (i, chunk) in chunks.iter().enumerate() {
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

            #[cfg(feature = "crypto")]
            let (upload_data, chunk_hash_hex) =
                if let (Some(ref fk), Some(ref fid)) = (&file_key, &file_id) {
                    let ciphertext = tcfs_crypto::encrypt_chunk(fk, i as u64, fid, chunk_data)
                        .with_context(|| format!("encrypting chunk {i}"))?;
                    let ct_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
                    (ciphertext, ct_hash)
                } else {
                    let h = tcfs_chunks::hash_to_hex(&chunk.hash);
                    (chunk_data.to_vec(), h)
                };

            #[cfg(not(feature = "crypto"))]
            let (upload_data, chunk_hash_hex) = {
                let h = tcfs_chunks::hash_to_hex(&chunk.hash);
                (chunk_data.to_vec(), h)
            };

            let chunk_key = format!("{remote_prefix}/chunks/{chunk_hash_hex}");

            if !op.exists(&chunk_key).await.unwrap_or(false) {
                op.write(&chunk_key, upload_data)
                    .await
                    .with_context(|| format!("uploading chunk {i}: {chunk_key}"))?;
                bytes_uploaded += chunk.length as u64;
            }

            chunk_hashes.push(chunk_hash_hex);

            if let Some(cb) = progress {
                cb(
                    (i + 1) as u64,
                    num_chunks as u64,
                    &format!("chunk {}/{num_chunks}", i + 1),
                );
            }
        }
    }

    // Wrap file key for manifest if encryption is enabled
    #[cfg(feature = "crypto")]
    let encrypted_file_key = if let (Some(ctx), Some(ref fk)) = (encryption, &file_key) {
        let wrapped = tcfs_crypto::wrap_key(&ctx.master_key, fk).context("wrapping file key")?;
        Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &wrapped,
        ))
    } else {
        None
    };

    #[cfg(not(feature = "crypto"))]
    let encrypted_file_key: Option<String> = None;

    // Capture Unix file permissions for cross-device preservation
    #[cfg(unix)]
    let file_mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(local_path)
            .ok()
            .map(|m| m.permissions().mode())
    };
    #[cfg(not(unix))]
    let file_mode: Option<u32> = None;

    // Build and upload SyncManifest v2
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let manifest = SyncManifest {
        version: 2,
        file_hash: file_hash_hex.clone(),
        file_size,
        chunks: chunk_hashes,
        vclock: local_vclock.clone(),
        written_by: device_id.to_string(),
        written_at: now,
        rel_path: rel_path.map(|s| s.to_string()),
        mode: file_mode,
        encrypted_file_key,
    };

    let manifest_bytes = manifest.to_bytes()?;
    op.write(&remote_manifest, manifest_bytes)
        .await
        .with_context(|| format!("uploading manifest: {remote_manifest}"))?;

    // Deferred vclock merge: only merge remote vclock after successful upload
    // to prevent stale vclocks if the upload had failed.
    if matches!(outcome, Some(SyncOutcome::LocalNewer)) {
        if let Some(ref remote_vc) = remote_vclock_snapshot {
            local_vclock.merge(remote_vc);
        }
    }

    info!(
        path = %local_path.display(),
        hash = %file_hash_hex,
        chunks = num_chunks,
        bytes = file_size,
        uploaded_bytes = bytes_uploaded,
        streaming = use_streaming,
        "uploaded"
    );

    // Update state cache
    let sync_state = make_sync_state_full(
        local_path,
        file_hash_hex.clone(),
        num_chunks,
        remote_manifest.clone(),
        local_vclock,
        device_id.to_string(),
    )?;
    state.set(local_path, sync_state);

    Ok(UploadResult {
        path: local_path.to_path_buf(),
        remote_path: remote_manifest,
        hash: file_hash_hex,
        chunks: num_chunks,
        bytes: file_size,
        skipped: false,
        outcome,
    })
}

/// Download a file from SeaweedFS using its manifest path.
///
/// Reads the manifest to get chunk hashes, fetches each chunk, reassembles
/// and writes to `local_path`. Supports both v1 (text) and v2 (JSON) manifests.
pub async fn download_file(
    op: &Operator,
    remote_manifest: &str,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
) -> Result<DownloadResult> {
    download_file_with_device(
        op,
        remote_manifest,
        local_path,
        remote_prefix,
        progress,
        "",
        None,
        None,
    )
    .await
}

/// Download with device identity, vector clock merge, and optional decryption.
#[allow(unused_variables)]
#[allow(clippy::too_many_arguments)]
pub async fn download_file_with_device(
    op: &Operator,
    remote_manifest: &str,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
    _device_id: &str,
    state: Option<&mut StateCache>,
    encryption: OptionalEncryption<'_>,
) -> Result<DownloadResult> {
    // Read manifest
    let manifest_bytes = op
        .read(remote_manifest)
        .await
        .with_context(|| format!("reading manifest: {remote_manifest}"))?;

    let manifest = SyncManifest::from_bytes(&manifest_bytes.to_bytes())
        .with_context(|| format!("parsing manifest: {remote_manifest}"))?;

    let chunk_hashes = manifest.chunk_hashes();

    // Empty file: no chunks to fetch — write an empty file directly
    if chunk_hashes.is_empty() {
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating dir: {}", parent.display()))?;
        }

        let tmp = local_path.with_extension("tcfs_tmp");
        tokio::fs::write(&tmp, &[])
            .await
            .with_context(|| format!("writing empty tmp: {}", tmp.display()))?;
        tokio::fs::rename(&tmp, local_path)
            .await
            .with_context(|| format!("renaming to: {}", local_path.display()))?;

        // Restore Unix file permissions from manifest
        #[cfg(unix)]
        if let Some(mode) = manifest.mode {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(mode);
            tokio::fs::set_permissions(local_path, perms)
                .await
                .with_context(|| format!("restoring permissions on: {}", local_path.display()))?;
        }

        // Merge remote vclock into local state
        if let Some(state) = state {
            if !_device_id.is_empty() {
                let mut local_vclock = state
                    .get(local_path)
                    .map(|s| s.vclock.clone())
                    .unwrap_or_default();
                local_vclock.merge(&manifest.vclock);

                let sync_state = make_sync_state_full(
                    local_path,
                    manifest.file_hash.clone(),
                    0,
                    remote_manifest.to_string(),
                    local_vclock,
                    _device_id.to_string(),
                )?;
                state.set(local_path, sync_state);
            }
        }

        info!(
            remote = %remote_manifest,
            local = %local_path.display(),
            bytes = 0u64,
            "downloaded (empty file)"
        );

        return Ok(DownloadResult {
            remote_path: remote_manifest.to_string(),
            local_path: local_path.to_path_buf(),
            bytes: 0,
        });
    }

    // Unwrap file key if manifest is encrypted
    #[cfg(feature = "crypto")]
    let file_key = if let Some(ref wrapped_b64) = manifest.encrypted_file_key {
        let ctx = encryption.ok_or_else(|| {
            anyhow::anyhow!(
                "manifest is encrypted but no encryption context provided for: {remote_manifest}"
            )
        })?;
        let wrapped =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, wrapped_b64)
                .context("decoding wrapped file key from manifest")?;
        Some(
            tcfs_crypto::unwrap_key(&ctx.master_key, &wrapped)
                .context("unwrapping file key from manifest")?,
        )
    } else {
        None
    };

    #[cfg(feature = "crypto")]
    let file_id: Option<[u8; 32]> = if file_key.is_some() {
        let hash = tcfs_chunks::hash_from_hex(&manifest.file_hash)
            .context("parsing manifest file_hash for decryption file_id")?;
        Some(*hash.as_bytes())
    } else {
        None
    };

    // Fetch and reassemble chunks, verifying each chunk's BLAKE3 hash
    let mut assembled = Vec::new();
    let total = chunk_hashes.len();

    for (i, hash) in chunk_hashes.iter().enumerate() {
        let chunk_key = format!("{remote_prefix}/chunks/{hash}");
        let chunk_data = op
            .read(&chunk_key)
            .await
            .with_context(|| format!("downloading chunk {i}: {chunk_key}"))?;

        let chunk_bytes = chunk_data.to_bytes();

        // Verify chunk integrity: BLAKE3 hash must match the manifest entry
        let actual_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
        if actual_hash != *hash {
            anyhow::bail!(
                "chunk integrity check failed for {chunk_key}: expected {hash}, got {actual_hash}"
            );
        }

        // Decrypt chunk if file key is present
        #[cfg(feature = "crypto")]
        let plaintext = if let (Some(ref fk), Some(ref fid)) = (&file_key, &file_id) {
            tcfs_crypto::decrypt_chunk(fk, i as u64, fid, &chunk_bytes)
                .with_context(|| format!("decrypting chunk {i}"))?
        } else {
            chunk_bytes.to_vec()
        };

        #[cfg(not(feature = "crypto"))]
        let plaintext = chunk_bytes.to_vec();

        assembled.extend_from_slice(&plaintext);

        if let Some(cb) = progress {
            cb(
                (i + 1) as u64,
                total as u64,
                &format!("chunk {}/{total}", i + 1),
            );
        }
    }

    let bytes = assembled.len() as u64;

    // Verify reassembled file hash matches the manifest (plaintext hash)
    let actual_file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&assembled));
    if actual_file_hash != manifest.file_hash {
        anyhow::bail!(
            "file integrity check failed for {remote_manifest}: expected {}, got {actual_file_hash}",
            manifest.file_hash
        );
    }

    // Atomic write to local path
    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating dir: {}", parent.display()))?;
    }

    let tmp = local_path.with_extension("tcfs_tmp");
    tokio::fs::write(&tmp, &assembled)
        .await
        .with_context(|| format!("writing tmp: {}", tmp.display()))?;
    tokio::fs::rename(&tmp, local_path)
        .await
        .with_context(|| format!("renaming to: {}", local_path.display()))?;

    // Restore Unix file permissions from manifest
    #[cfg(unix)]
    if let Some(mode) = manifest.mode {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        tokio::fs::set_permissions(local_path, perms)
            .await
            .with_context(|| format!("restoring permissions on: {}", local_path.display()))?;
    }

    // Merge remote vclock into local state if we have a state cache
    if let Some(state) = state {
        if !_device_id.is_empty() {
            let mut local_vclock = state
                .get(local_path)
                .map(|s| s.vclock.clone())
                .unwrap_or_default();
            local_vclock.merge(&manifest.vclock);

            let file_hash = tcfs_chunks::hash_bytes(&assembled);
            let file_hash_hex = tcfs_chunks::hash_to_hex(&file_hash);

            let sync_state = make_sync_state_full(
                local_path,
                file_hash_hex,
                total,
                remote_manifest.to_string(),
                local_vclock,
                _device_id.to_string(),
            )?;
            state.set(local_path, sync_state);
        }
    }

    info!(
        remote = %remote_manifest,
        local = %local_path.display(),
        bytes,
        "downloaded"
    );

    Ok(DownloadResult {
        remote_path: remote_manifest.to_string(),
        local_path: local_path.to_path_buf(),
        bytes,
    })
}

/// Walk a local directory and upload all changed files.
///
/// Returns stats: (files_uploaded, files_skipped, bytes_uploaded)
pub async fn push_tree(
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
) -> Result<(usize, usize, u64)> {
    push_tree_with_device(
        op,
        local_root,
        remote_prefix,
        state,
        progress,
        "",
        None,
        None,
    )
    .await
}

/// Push tree with device identity, optional collection config, and optional encryption.
#[allow(clippy::too_many_arguments)]
pub async fn push_tree_with_device(
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
    device_id: &str,
    collect_cfg: Option<&CollectConfig>,
    encryption: OptionalEncryption<'_>,
) -> Result<(usize, usize, u64)> {
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0u64;

    let cfg = collect_cfg.cloned().unwrap_or_default();
    let result = collect_files(local_root, &cfg)?;
    let total = result.files.len();

    for (i, path) in result.files.iter().enumerate() {
        let rel = path.strip_prefix(local_root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let msg = format!("[{}/{}] {}", i + 1, total, rel.display());
        if let Some(cb) = progress {
            cb(i as u64, total as u64, &msg);
        }

        match upload_file_with_device(
            op,
            path,
            &remote_path_prefix(remote_prefix),
            state,
            None,
            device_id,
            Some(&rel_str),
            encryption,
        )
        .await
        {
            Ok(result) => {
                if result.skipped {
                    skipped += 1;
                } else {
                    // Write index entry only when the manifest was actually uploaded.
                    // Skipped files (RemoteNewer, UpToDate, Conflict) already have
                    // a valid index entry, and writing one with the local hash would
                    // create an orphan pointing to a non-existent manifest.
                    let index_key =
                        format!("{}/index/{}", remote_path_prefix(remote_prefix), rel_str);
                    let index_entry = format!(
                        "manifest_hash={}\nsize={}\nchunks={}\n",
                        result.hash, result.bytes, result.chunks
                    );
                    if let Err(e) = op.write(&index_key, index_entry.into_bytes()).await {
                        warn!(path = %path.display(), "failed to write index entry: {e}");
                    }
                    uploaded += 1;
                    bytes += result.bytes;
                }
            }
            Err(e) => {
                warn!(path = %path.display(), "upload failed: {e}");
            }
        }
    }

    // Write `.tcfs_dir` markers for empty directories
    for dir in &result.empty_dirs {
        // Skip the root itself — it's never "empty" in the sync sense
        if dir == local_root {
            continue;
        }
        if let Ok(rel) = dir.strip_prefix(local_root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let marker_key = format!(
                "{}/index/{}/.tcfs_dir",
                remote_path_prefix(remote_prefix),
                rel_str
            );
            let marker_content = b"type=directory\n";
            if let Err(e) = op.write(&marker_key, marker_content.to_vec()).await {
                warn!(dir = %dir.display(), "failed to write empty dir marker: {e}");
            } else {
                debug!(dir = %rel_str, "wrote empty directory marker");
            }
        }
    }

    // Flush state cache after tree push
    state.flush()?;

    Ok((uploaded, skipped, bytes))
}

/// Collect all regular files under `root` recursively, respecting config.
///
/// When `config.sync_empty_dirs` is true, also collects directories that
/// contain no files (after exclusion rules) so callers can create `.tcfs_dir`
/// marker objects in the remote index.
pub fn collect_files(root: &Path, config: &CollectConfig) -> Result<CollectResult> {
    let mut files = Vec::new();
    let mut empty_dirs = Vec::new();
    let exclude_matchers: Vec<glob::Pattern> = config
        .exclude_patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();
    // Track visited canonical paths for symlink cycle detection
    let mut visited = std::collections::HashSet::new();
    if let Ok(canon) = std::fs::canonicalize(root) {
        visited.insert(canon);
    }
    collect_files_inner(
        root,
        &mut files,
        &mut empty_dirs,
        config,
        &exclude_matchers,
        &mut visited,
    )?;
    files.sort(); // deterministic order
    empty_dirs.sort();
    Ok(CollectResult { files, empty_dirs })
}

fn collect_files_inner(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    empty_dirs: &mut Vec<PathBuf>,
    config: &CollectConfig,
    excludes: &[glob::Pattern],
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let before = out.len();

    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading dir: {}", dir.display()))?
    {
        let entry = entry.context("reading dir entry")?;
        let path = entry.path();

        // Use file_type() (doesn't follow symlinks) for initial dispatch
        let ft = entry.file_type().context("file_type dir entry")?;

        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            // Check exclude patterns
            if excludes.iter().any(|p| p.matches(name)) {
                continue;
            }

            // Handle symlinks explicitly
            if ft.is_symlink() {
                if !config.follow_symlinks {
                    let target = std::fs::read_link(&path).unwrap_or_default();
                    warn!(
                        path = %path.display(),
                        target = %target.display(),
                        "skipping symlink (follow_symlinks=false)"
                    );
                    continue;
                }

                // Follow the symlink — resolve target and check for cycles
                match std::fs::canonicalize(&path) {
                    Ok(real) => {
                        if !visited.insert(real.clone()) {
                            warn!(
                                path = %path.display(),
                                target = %real.display(),
                                "skipping symlink: cycle detected"
                            );
                            continue;
                        }
                        // Check what the resolved target actually is
                        match std::fs::metadata(&real) {
                            Ok(meta) if meta.is_dir() => {
                                collect_files_inner(
                                    &path, out, empty_dirs, config, excludes, visited,
                                )?;
                            }
                            Ok(meta) if meta.is_file() => {
                                out.push(path);
                            }
                            Ok(_) => {} // special file, skip
                            Err(e) => {
                                warn!(
                                    path = %path.display(),
                                    target = %real.display(),
                                    "skipping symlink: stat target failed: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        // Broken symlink — canonicalize fails
                        warn!(
                            path = %path.display(),
                            "skipping broken symlink: {e}"
                        );
                    }
                }
                continue;
            }

            if ft.is_dir() {
                // Always skip these
                if name == "target" || name == "node_modules" || name == ".DS_Store" {
                    continue;
                }

                // Track visited directories — skip if already traversed
                // (prevents re-traversal when a symlink was followed first)
                if let Ok(canon) = std::fs::canonicalize(&path) {
                    if !visited.insert(canon) {
                        continue;
                    }
                }

                // Handle .git directories
                if name == ".git" {
                    if config.sync_git_dirs {
                        // Validate safety before including
                        let safety = crate::git_safety::git_is_safe(&path);
                        if !safety.blocking.is_empty() {
                            warn!(
                                path = %path.display(),
                                blocking = ?safety.blocking,
                                "skipping .git dir: active operations detected"
                            );
                            continue;
                        }
                        for w in &safety.warnings {
                            warn!(path = %path.display(), warning = %w, "git safety warning");
                        }
                        // In bundle mode, skip raw .git and handle at a higher level
                        if config.git_sync_mode == "bundle" {
                            continue;
                        }
                        // In raw mode, recurse into .git
                        collect_files_inner(&path, out, empty_dirs, config, excludes, visited)?;
                    }
                    continue;
                }

                // Handle other hidden directories
                if name.starts_with('.') && !config.sync_hidden_dirs {
                    continue;
                }

                collect_files_inner(&path, out, empty_dirs, config, excludes, visited)?;
            } else if ft.is_file() {
                out.push(path);
            }
        }
    }

    // If no files were collected from this directory (directly or via
    // subdirectories) and we're tracking empty dirs, record it as empty.
    if config.sync_empty_dirs && out.len() == before {
        empty_dirs.push(dir.to_path_buf());
    }

    Ok(())
}

/// Normalize a filesystem path into a stable S3 index key component.
///
/// - If `sync_root` is provided and the path is under it, returns the relative path.
/// - Otherwise strips the leading `/` from absolute paths, or returns relative paths as-is.
/// - Replaces `\` with `/` for cross-platform consistency.
pub fn normalize_rel_path(path: &Path, sync_root: Option<&Path>) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let rel = if let Some(root) = sync_root {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        canonical
            .strip_prefix(&canonical_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| {
                let s = canonical.to_string_lossy();
                PathBuf::from(s.trim_start_matches('/'))
            })
    } else if canonical.is_absolute() {
        let s = canonical.to_string_lossy();
        PathBuf::from(s.trim_start_matches('/'))
    } else {
        canonical
    };

    rel.to_string_lossy().replace('\\', "/")
}

/// Resolve a file path or manifest path to the actual S3 manifest path.
///
/// If the input contains `/manifests/`, it is returned as-is (assumed to be a manifest path).
/// Otherwise, treat it as a file path: normalize it, look up the index entry,
/// and construct the manifest path from the stored hash.
///
/// Falls back to searching the index prefix for a matching filename if the
/// normalized path doesn't match (e.g., pulling on a different host where
/// `canonicalize()` produces a different absolute path than the push host).
pub async fn resolve_manifest_path(
    op: &Operator,
    input: &str,
    remote_prefix: &str,
    sync_root: Option<&Path>,
) -> Result<String> {
    // If it already looks like a manifest path, use it directly
    if input.contains("/manifests/") {
        return Ok(input.to_string());
    }

    let prefix = remote_prefix.trim_end_matches('/');

    // Try 1: Normalize the input path to derive the index key
    let rel = normalize_rel_path(Path::new(input), sync_root);
    let index_key = format!("{prefix}/index/{rel}");

    if let Ok(idx_bytes) = op.read(&index_key).await {
        let idx_raw = idx_bytes.to_bytes();
        let idx_str = String::from_utf8_lossy(&idx_raw);
        if let Some(manifest_hash) = idx_str
            .lines()
            .find_map(|l| l.strip_prefix("manifest_hash="))
        {
            return Ok(format!("{prefix}/manifests/{manifest_hash}"));
        }
    }

    // Try 2: Search index entries for a matching filename.
    // This handles cross-host pull where the pushing host's canonicalized path
    // differs from the pulling host's (e.g., /tmp → /private/tmp on macOS).
    let filename = Path::new(input)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| input.to_string());

    let index_prefix = format!("{prefix}/index/");
    let entries = op
        .list(&index_prefix)
        .await
        .with_context(|| format!("listing index prefix: {index_prefix}"))?;

    for entry in entries {
        let entry_path = entry.path();
        if entry_path.ends_with(&format!("/{filename}")) || entry_path.ends_with(&filename) {
            if let Ok(idx_bytes) = op.read(entry_path).await {
                let idx_raw = idx_bytes.to_bytes();
                let idx_str = String::from_utf8_lossy(&idx_raw);
                if let Some(manifest_hash) = idx_str
                    .lines()
                    .find_map(|l| l.strip_prefix("manifest_hash="))
                {
                    return Ok(format!("{prefix}/manifests/{manifest_hash}"));
                }
            }
        }
    }

    anyhow::bail!(
        "no index entry found for '{}' (tried: {index_key}, filename search: {filename})",
        input
    )
}

/// Delete a file from remote storage (index entry + manifest + chunks).
///
/// Looks up the index entry for `rel_path`, reads the manifest to find chunk
/// hashes, then deletes the index entry and manifest. Chunks are left for GC
/// (they may be shared with other files via content-addressed dedup).
///
/// Also removes the file from the local state cache if present.
pub async fn delete_remote_file(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
    state: &mut StateCache,
    sync_root: Option<&Path>,
) -> Result<()> {
    let prefix = remote_prefix.trim_end_matches('/');
    let index_key = format!("{prefix}/index/{rel_path}");

    // Read index to find manifest hash
    let idx_raw = op
        .read(&index_key)
        .await
        .with_context(|| format!("reading index entry: {index_key}"))?
        .to_bytes();

    let idx_str = String::from_utf8_lossy(&idx_raw);
    let manifest_hash = idx_str
        .lines()
        .find_map(|l| l.strip_prefix("manifest_hash="))
        .ok_or_else(|| anyhow::anyhow!("index entry missing manifest_hash: {index_key}"))?
        .to_string();

    let manifest_key = format!("{prefix}/manifests/{manifest_hash}");

    // Delete index entry and manifest
    op.delete(&index_key)
        .await
        .with_context(|| format!("deleting index entry: {index_key}"))?;
    op.delete(&manifest_key)
        .await
        .with_context(|| format!("deleting manifest: {manifest_key}"))?;

    info!(rel_path = %rel_path, manifest = %manifest_hash, "deleted remote file");

    // Remove from state cache
    let local_path = sync_root
        .map(|r| r.join(rel_path))
        .unwrap_or_else(|| PathBuf::from(rel_path));
    state.remove(&local_path);

    // Also try to remove by searching the cache (handles path normalization mismatches)
    if let Some((key, _)) = state.get_by_rel_path(rel_path) {
        let key_owned = key.to_string();
        state.remove(Path::new(&key_owned));
    }

    state.flush()?;

    Ok(())
}

/// Normalize a remote prefix: ensure it doesn't have trailing slash
fn remote_path_prefix(prefix: &str) -> String {
    prefix.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> CollectConfig {
        CollectConfig::default()
    }

    fn no_empty_dirs_config() -> CollectConfig {
        CollectConfig {
            sync_empty_dirs: false,
            ..Default::default()
        }
    }

    #[test]
    fn collect_finds_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create structure: root/a/file.txt, root/empty/, root/nested/also_empty/
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/file.txt"), b"content").unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::create_dir_all(root.join("nested/also_empty")).unwrap();

        let result = collect_files(root, &default_config()).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].ends_with("a/file.txt"));

        // empty/ and nested/also_empty/ should be detected as empty dirs
        // nested/ itself also has no files (its only child is also_empty/ which is empty)
        let empty_names: Vec<String> = result
            .empty_dirs
            .iter()
            .map(|d| d.strip_prefix(root).unwrap().to_string_lossy().to_string())
            .collect();
        assert!(
            empty_names.contains(&"empty".to_string()),
            "should detect empty/ as empty dir, got: {:?}",
            empty_names
        );
        assert!(
            empty_names.contains(&"nested/also_empty".to_string()),
            "should detect nested/also_empty/ as empty dir, got: {:?}",
            empty_names
        );
    }

    #[test]
    fn collect_skips_empty_dirs_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("file.txt"), b"data").unwrap();

        let result = collect_files(root, &no_empty_dirs_config()).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(
            result.empty_dirs.is_empty(),
            "empty_dirs should be empty when sync_empty_dirs=false"
        );
    }

    #[test]
    fn collect_dir_with_file_not_marked_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("has_file")).unwrap();
        std::fs::write(root.join("has_file/doc.txt"), b"hello").unwrap();

        let result = collect_files(root, &default_config()).unwrap();

        assert_eq!(result.files.len(), 1);
        // has_file/ contains a file, so it should NOT appear in empty_dirs
        assert!(
            !result.empty_dirs.iter().any(|d| d.ends_with("has_file")),
            "directory with files should not be in empty_dirs"
        );
    }

    #[test]
    fn collect_root_not_marked_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Completely empty root
        let result = collect_files(root, &default_config()).unwrap();

        assert!(result.files.is_empty());
        // Root itself should be in empty_dirs (it's empty), but push_tree
        // skips it. The collector doesn't special-case root.
        // Actually root IS recorded — push_tree_with_device skips it.
    }

    #[test]
    fn collect_excluded_dir_not_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create structure: root/target/ (excluded by hardcoded rule)
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join("real_empty")).unwrap();
        std::fs::write(root.join("file.txt"), b"data").unwrap();

        let result = collect_files(root, &default_config()).unwrap();

        let empty_names: Vec<String> = result
            .empty_dirs
            .iter()
            .map(|d| d.strip_prefix(root).unwrap().to_string_lossy().to_string())
            .collect();

        // target/ is excluded entirely, so it shouldn't appear
        assert!(
            !empty_names.contains(&"target".to_string()),
            "excluded dirs should not appear in empty_dirs"
        );
        // real_empty/ should appear
        assert!(
            empty_names.contains(&"real_empty".to_string()),
            "real empty dir should be detected"
        );
    }
}
