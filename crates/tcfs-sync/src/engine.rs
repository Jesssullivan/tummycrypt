//! Sync engine: upload and download workers using OpenDAL + tcfs-chunks
//!
//! Phase 2 implementation covers:
//!   - `upload_file`: chunk → hash → skip if remote exists → upload via OpenDAL
//!   - `download_file`: fetch chunk objects → reassemble → write to local path
//!   - `push_tree`: walk a directory tree, upload changed files
//!   - `pull_file`: download a single remote path to local

use anyhow::{Context, Result};
use opendal::Operator;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::state::{make_sync_state, StateCache};

/// Progress callback type (bytes_done, bytes_total, message)
pub type ProgressFn = Box<dyn Fn(u64, u64, &str) + Send + Sync>;

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
pub async fn upload_file(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
) -> Result<UploadResult> {
    // Fast-path: check if file is already up-to-date
    match state.needs_sync(local_path)? {
        None => {
            let cached = state.get(local_path).unwrap();
            let result = UploadResult {
                path: local_path.to_path_buf(),
                remote_path: cached.remote_path.clone(),
                hash: cached.blake3.clone(),
                chunks: cached.chunk_count,
                bytes: cached.size,
                skipped: true,
            };
            debug!(path = %local_path.display(), "skip: unchanged since last sync");
            return Ok(result);
        }
        Some(reason) => {
            debug!(path = %local_path.display(), reason = %reason, "uploading");
        }
    }

    // Chunk the file
    let (chunks, data) = tcfs_chunks::chunk_file(local_path)
        .with_context(|| format!("chunking: {}", local_path.display()))?;

    let file_size = data.len() as u64;
    let file_hash = tcfs_chunks::hash_bytes(&data);
    let file_hash_hex = tcfs_chunks::hash_to_hex(&file_hash);

    // Build remote manifest path (using the file's content hash)
    let remote_manifest = format!("{remote_prefix}/manifests/{file_hash_hex}");

    // Check if this exact content is already stored (content-addressed dedup)
    if op.exists(&remote_manifest).await.unwrap_or(false) {
        debug!(hash = %file_hash_hex, "dedup: manifest already exists");
        let remote_path = remote_manifest.clone();
        let sync_state = make_sync_state(
            local_path,
            file_hash_hex.clone(),
            chunks.len(),
            remote_path.clone(),
        )?;
        state.set(local_path, sync_state);
        return Ok(UploadResult {
            path: local_path.to_path_buf(),
            remote_path,
            hash: file_hash_hex,
            chunks: chunks.len(),
            bytes: file_size,
            skipped: false,
        });
    }

    // Upload each chunk (skip if already present — dedup by chunk hash)
    let mut chunk_hashes = Vec::with_capacity(chunks.len());
    let mut bytes_uploaded = 0u64;

    for (i, chunk) in chunks.iter().enumerate() {
        let chunk_hash_hex = tcfs_chunks::hash_to_hex(&chunk.hash);
        let chunk_key = format!("{remote_prefix}/chunks/{chunk_hash_hex}");

        if !op.exists(&chunk_key).await.unwrap_or(false) {
            let chunk_data = &data[chunk.offset as usize..chunk.offset as usize + chunk.length];
            op.write(&chunk_key, chunk_data.to_vec())
                .await
                .with_context(|| format!("uploading chunk {i}: {chunk_key}"))?;
            bytes_uploaded += chunk.length as u64;
        }

        chunk_hashes.push(chunk_hash_hex);

        if let Some(cb) = progress {
            cb(
                (i + 1) as u64,
                chunks.len() as u64,
                &format!("chunk {}/{}", i + 1, chunks.len()),
            );
        }
    }

    // Upload manifest: newline-separated chunk hashes
    let manifest_content = chunk_hashes.join("\n");
    op.write(&remote_manifest, manifest_content.into_bytes())
        .await
        .with_context(|| format!("uploading manifest: {remote_manifest}"))?;

    info!(
        path = %local_path.display(),
        hash = %file_hash_hex,
        chunks = chunks.len(),
        bytes = file_size,
        uploaded_bytes = bytes_uploaded,
        "uploaded"
    );

    // Update state cache
    let sync_state = make_sync_state(
        local_path,
        file_hash_hex.clone(),
        chunks.len(),
        remote_manifest.clone(),
    )?;
    state.set(local_path, sync_state);

    Ok(UploadResult {
        path: local_path.to_path_buf(),
        remote_path: remote_manifest,
        hash: file_hash_hex,
        chunks: chunks.len(),
        bytes: file_size,
        skipped: false,
    })
}

/// Download a file from SeaweedFS using its manifest path.
///
/// Reads the manifest to get chunk hashes, fetches each chunk, reassembles
/// and writes to `local_path`.
pub async fn download_file(
    op: &Operator,
    remote_manifest: &str,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
) -> Result<DownloadResult> {
    // Read manifest
    let manifest_bytes = op
        .read(remote_manifest)
        .await
        .with_context(|| format!("reading manifest: {remote_manifest}"))?;

    let manifest_str = String::from_utf8(manifest_bytes.to_bytes().to_vec())
        .context("manifest is not valid UTF-8")?;

    let chunk_hashes: Vec<&str> = manifest_str.lines().filter(|l| !l.is_empty()).collect();

    if chunk_hashes.is_empty() {
        anyhow::bail!("manifest is empty: {remote_manifest}");
    }

    // Fetch and reassemble chunks
    let mut assembled = Vec::new();
    let total = chunk_hashes.len();

    for (i, hash) in chunk_hashes.iter().enumerate() {
        let chunk_key = format!("{remote_prefix}/chunks/{hash}");
        let chunk_data = op
            .read(&chunk_key)
            .await
            .with_context(|| format!("downloading chunk {i}: {chunk_key}"))?;

        assembled.extend_from_slice(&chunk_data.to_bytes());

        if let Some(cb) = progress {
            cb(
                (i + 1) as u64,
                total as u64,
                &format!("chunk {}/{total}", i + 1),
            );
        }
    }

    let bytes = assembled.len() as u64;

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
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0u64;

    let files = collect_files(local_root)?;
    let total = files.len();

    for (i, path) in files.iter().enumerate() {
        let rel = path.strip_prefix(local_root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let msg = format!("[{}/{}] {}", i + 1, total, rel.display());
        if let Some(cb) = progress {
            cb(i as u64, total as u64, &msg);
        }

        match upload_file(op, path, &remote_path_prefix(remote_prefix), state, None).await {
            Ok(result) => {
                // Write index entry: maps relative path → manifest hash + metadata.
                // This allows the FUSE driver to list files by original name.
                let index_key = format!("{}/index/{}", remote_path_prefix(remote_prefix), rel_str);
                let index_entry = format!(
                    "manifest_hash={}\nsize={}\nchunks={}\n",
                    result.hash, result.bytes, result.chunks
                );
                if let Err(e) = op.write(&index_key, index_entry.into_bytes()).await {
                    warn!(path = %path.display(), "failed to write index entry: {e}");
                }

                if result.skipped {
                    skipped += 1;
                } else {
                    uploaded += 1;
                    bytes += result.bytes;
                }
            }
            Err(e) => {
                warn!(path = %path.display(), "upload failed: {e}");
            }
        }
    }

    // Flush state cache after tree push
    state.flush()?;

    Ok((uploaded, skipped, bytes))
}

/// Collect all regular files under `root` recursively.
fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files_inner(root, &mut files)?;
    files.sort(); // deterministic order
    Ok(files)
}

fn collect_files_inner(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading dir: {}", dir.display()))?
    {
        let entry = entry.context("reading dir entry")?;
        let path = entry.path();
        let meta = entry.metadata().context("stat dir entry")?;

        if meta.is_dir() {
            // Skip hidden dirs and common noise
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
            }
            collect_files_inner(&path, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Normalize a remote prefix: ensure it doesn't have trailing slash
fn remote_path_prefix(prefix: &str) -> String {
    prefix.trim_end_matches('/').to_string()
}
