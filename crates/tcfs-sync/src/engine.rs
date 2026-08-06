//! Sync engine: upload and download workers using OpenDAL + tcfs-chunks
//!
//! Phase 2 implementation covers:
//!   - `upload_file`: chunk → hash → refresh immutable chunk → upload via OpenDAL
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
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::blacklist::{Blacklist, BlacklistReason};
use crate::conflict::{compare_clocks, SyncOutcome, VectorClock};
use crate::index_entry::{
    admit_portable_namespace_entry, manifest_key, manifest_object_id,
    read_index_entry_record_from_store, require_absent_index_entry_for_update,
    resolve_visible_index_entry, resolve_visible_index_entry_for_update,
    validate_canonical_rel_path, validate_namespace_logical_path, validate_relative_storage_key,
    validate_remote_entry, write_committed_index_entry_conditionally,
    write_directory_marker_conditionally, write_immutable_manifest_object_if_absent,
    write_preparing_index_entry_conditionally, PendingIndexEntry, PortableNamespaceRole,
    RemoteEntryKind, RemoteIndexEntry, ResolvedIndexEntryForUpdate,
};
use crate::manifest::{SymlinkManifest, SyncManifest};
use crate::state::{make_sync_state_full, FileSyncStatus, StateCache, SyncState};

/// Default number of retry attempts for chunk upload/download operations.
const CHUNK_MAX_RETRIES: u32 = 3;

/// Default number of retry attempts for chunk downloads.
const DEFAULT_DOWNLOAD_CHUNK_RETRIES: u32 = 8;

/// Base delay between retries (doubles each attempt: 100ms, 200ms, 400ms).
const CHUNK_RETRY_BASE_MS: u64 = 100;

/// Hard cap for `TCFS_DOWNLOAD_CHUNK_RETRIES`.
const MAX_DOWNLOAD_CHUNK_RETRIES: u32 = 32;

/// Default bounded fanout for per-file chunk uploads.
const DEFAULT_UPLOAD_CHUNK_CONCURRENCY: usize = 4;

/// Hard cap for `TCFS_UPLOAD_CHUNK_CONCURRENCY`.
const MAX_UPLOAD_CHUNK_CONCURRENCY: usize = 64;

/// Default bounded fanout for fresh-prefix tree file uploads.
const DEFAULT_UPLOAD_FILE_CONCURRENCY: usize = 1;

/// Hard cap for `TCFS_UPLOAD_FILE_CONCURRENCY`.
const MAX_UPLOAD_FILE_CONCURRENCY: usize = 64;

/// Default per-attempt timeout for chunk uploads.
const DEFAULT_UPLOAD_CHUNK_TIMEOUT_SECS: u64 = 300;

/// Hard cap for `TCFS_UPLOAD_CHUNK_TIMEOUT_SECS`.
const MAX_UPLOAD_CHUNK_TIMEOUT_SECS: u64 = 3600;

/// Default per-attempt timeout for remote reads.
const DEFAULT_DOWNLOAD_READ_TIMEOUT_SECS: u64 = 300;

/// Hard cap for `TCFS_DOWNLOAD_READ_TIMEOUT_SECS`.
const MAX_DOWNLOAD_READ_TIMEOUT_SECS: u64 = 3600;

/// Hard cap for `TCFS_UPLOAD_PROGRESS_HEARTBEAT_SECS`.
const MAX_UPLOAD_PROGRESS_HEARTBEAT_SECS: u64 = 3600;

fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(CHUNK_RETRY_BASE_MS * 2u64.saturating_pow(attempt))
}

fn upload_chunk_concurrency_from_env_value(value: Option<&str>) -> usize {
    let Some(raw) = value else {
        return DEFAULT_UPLOAD_CHUNK_CONCURRENCY;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_UPLOAD_CHUNK_CONCURRENCY;
    }

    trimmed
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value.min(MAX_UPLOAD_CHUNK_CONCURRENCY))
        .unwrap_or(DEFAULT_UPLOAD_CHUNK_CONCURRENCY)
}

fn upload_chunk_concurrency() -> usize {
    upload_chunk_concurrency_from_env_value(
        std::env::var("TCFS_UPLOAD_CHUNK_CONCURRENCY")
            .ok()
            .as_deref(),
    )
}

fn download_chunk_retries_from_env_value(value: Option<&str>) -> u32 {
    let Some(raw) = value else {
        return DEFAULT_DOWNLOAD_CHUNK_RETRIES;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_DOWNLOAD_CHUNK_RETRIES;
    }

    trimmed
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value.min(MAX_DOWNLOAD_CHUNK_RETRIES))
        .unwrap_or(DEFAULT_DOWNLOAD_CHUNK_RETRIES)
}

fn download_chunk_retries() -> u32 {
    download_chunk_retries_from_env_value(
        std::env::var("TCFS_DOWNLOAD_CHUNK_RETRIES").ok().as_deref(),
    )
}

fn upload_file_concurrency_from_env_value(value: Option<&str>) -> usize {
    let Some(raw) = value else {
        return DEFAULT_UPLOAD_FILE_CONCURRENCY;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_UPLOAD_FILE_CONCURRENCY;
    }

    trimmed
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value.min(MAX_UPLOAD_FILE_CONCURRENCY))
        .unwrap_or(DEFAULT_UPLOAD_FILE_CONCURRENCY)
}

fn upload_file_concurrency() -> usize {
    upload_file_concurrency_from_env_value(
        std::env::var("TCFS_UPLOAD_FILE_CONCURRENCY")
            .ok()
            .as_deref(),
    )
}

fn upload_assume_fresh_prefix_from_env_value(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1" | "true" | "yes" | "on"))
}

fn upload_assume_fresh_prefix() -> bool {
    upload_assume_fresh_prefix_from_env_value(
        std::env::var("TCFS_UPLOAD_ASSUME_FRESH_PREFIX")
            .ok()
            .as_deref(),
    )
}

#[derive(Debug, Clone, Copy)]
struct UploadRuntimeOptions {
    assume_fresh_prefix: bool,
    file_upload_concurrency: usize,
}

impl UploadRuntimeOptions {
    fn from_env() -> Self {
        Self {
            assume_fresh_prefix: upload_assume_fresh_prefix(),
            file_upload_concurrency: upload_file_concurrency(),
        }
    }
}

fn should_upload_files_concurrently(
    runtime: UploadRuntimeOptions,
    encryption_present: bool,
) -> bool {
    runtime.assume_fresh_prefix && runtime.file_upload_concurrency > 1 && !encryption_present
}

fn upload_progress_every_chunks_from_env_value(value: Option<&str>) -> usize {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn upload_progress_every_chunks() -> usize {
    upload_progress_every_chunks_from_env_value(
        std::env::var("TCFS_UPLOAD_PROGRESS_EVERY_CHUNKS")
            .ok()
            .as_deref(),
    )
}

fn upload_progress_heartbeat_from_env_value(value: Option<&str>) -> Option<Duration> {
    let seconds = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);

    if seconds == 0 {
        return None;
    }

    Some(Duration::from_secs(
        seconds.min(MAX_UPLOAD_PROGRESS_HEARTBEAT_SECS),
    ))
}

fn upload_progress_heartbeat() -> Option<Duration> {
    upload_progress_heartbeat_from_env_value(
        std::env::var("TCFS_UPLOAD_PROGRESS_HEARTBEAT_SECS")
            .ok()
            .as_deref(),
    )
}

fn upload_chunk_timeout_from_env_value(value: Option<&str>) -> Option<Duration> {
    let seconds = match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => DEFAULT_UPLOAD_CHUNK_TIMEOUT_SECS,
        Some("0") => return None,
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|seconds| *seconds > 0)
            .unwrap_or(DEFAULT_UPLOAD_CHUNK_TIMEOUT_SECS),
    };

    Some(Duration::from_secs(
        seconds.min(MAX_UPLOAD_CHUNK_TIMEOUT_SECS),
    ))
}

fn upload_chunk_timeout() -> Option<Duration> {
    upload_chunk_timeout_from_env_value(
        std::env::var("TCFS_UPLOAD_CHUNK_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

fn download_read_timeout_from_env_value(value: Option<&str>) -> Option<Duration> {
    let seconds = match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => DEFAULT_DOWNLOAD_READ_TIMEOUT_SECS,
        Some("0") => return None,
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|seconds| *seconds > 0)
            .unwrap_or(DEFAULT_DOWNLOAD_READ_TIMEOUT_SECS),
    };

    Some(Duration::from_secs(
        seconds.min(MAX_DOWNLOAD_READ_TIMEOUT_SECS),
    ))
}

fn download_read_timeout() -> Option<Duration> {
    download_read_timeout_from_env_value(
        std::env::var("TCFS_DOWNLOAD_READ_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

fn should_record_chunk_upload_progress(
    completed_chunks: usize,
    num_chunks: usize,
    every_chunks: usize,
) -> bool {
    if every_chunks == 0 {
        return false;
    }

    completed_chunks.is_multiple_of(every_chunks)
        || (completed_chunks == num_chunks && num_chunks >= every_chunks)
}

fn rate_per_sec(units: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds <= f64::EPSILON {
        0.0
    } else {
        units as f64 / seconds
    }
}

async fn retry_with_backoff<T, E, Action, ActionFuture, Sleep, SleepFuture, OnRetry>(
    max_attempts: u32,
    mut action: Action,
    mut on_retry: OnRetry,
    mut sleep: Sleep,
) -> std::result::Result<T, E>
where
    Action: FnMut(u32) -> ActionFuture,
    ActionFuture: std::future::Future<Output = std::result::Result<T, E>>,
    Sleep: FnMut(std::time::Duration) -> SleepFuture,
    SleepFuture: std::future::Future<Output = ()>,
    OnRetry: FnMut(u32, std::time::Duration, &E),
{
    assert!(
        max_attempts > 0,
        "retry_with_backoff requires at least one attempt"
    );

    let mut last_err = None;
    for attempt in 0..max_attempts {
        match action(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt + 1 < max_attempts {
                    let delay = retry_delay(attempt);
                    on_retry(attempt + 1, delay, &err);
                    sleep(delay).await;
                }
                last_err = Some(err);
            }
        }
    }

    Err(last_err.expect("retry_with_backoff must capture a final error"))
}

async fn write_chunk_with_retry_inner<Write, WriteFuture, Sleep, SleepFuture>(
    key: &str,
    chunk_idx: usize,
    logical_len: u64,
    write_timeout: Option<Duration>,
    mut write: Write,
    sleep: Sleep,
) -> Result<()>
where
    Write: FnMut() -> WriteFuture,
    WriteFuture: std::future::Future<Output = Result<()>>,
    Sleep: FnMut(std::time::Duration) -> SleepFuture,
    SleepFuture: std::future::Future<Output = ()>,
{
    #[derive(Debug)]
    enum ChunkUploadError {
        Transport {
            source: anyhow::Error,
            elapsed: Duration,
        },
        Timeout {
            timeout: Duration,
            elapsed: Duration,
        },
    }

    impl ChunkUploadError {
        fn kind(&self) -> &'static str {
            match self {
                Self::Transport { .. } => "transport",
                Self::Timeout { .. } => "timeout",
            }
        }

        fn elapsed(&self) -> Duration {
            match self {
                Self::Transport { elapsed, .. } | Self::Timeout { elapsed, .. } => *elapsed,
            }
        }

        fn timeout_ms(&self) -> u128 {
            match self {
                Self::Transport { .. } => 0,
                Self::Timeout { timeout, .. } => timeout.as_millis(),
            }
        }
    }

    impl std::fmt::Display for ChunkUploadError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Transport { source, .. } => write!(f, "{source}"),
                Self::Timeout { timeout, .. } => {
                    write!(f, "chunk upload timed out after {} ms", timeout.as_millis())
                }
            }
        }
    }

    impl std::error::Error for ChunkUploadError {}

    retry_with_backoff(
        CHUNK_MAX_RETRIES,
        |_| {
            let fut = write();
            async move {
                let started = std::time::Instant::now();
                match write_timeout {
                    Some(limit) => match tokio::time::timeout(limit, fut).await {
                        Ok(result) => result.map_err(|source| ChunkUploadError::Transport {
                            source,
                            elapsed: started.elapsed(),
                        }),
                        Err(_) => Err(ChunkUploadError::Timeout {
                            timeout: limit,
                            elapsed: started.elapsed(),
                        }),
                    },
                    None => fut.await.map_err(|source| ChunkUploadError::Transport {
                        source,
                        elapsed: started.elapsed(),
                    }),
                }
            }
        },
        |attempt, delay, err: &ChunkUploadError| {
            warn!(
                key = key,
                chunk = chunk_idx,
                bytes = logical_len,
                attempt,
                max = CHUNK_MAX_RETRIES,
                kind = err.kind(),
                timeout_ms = err.timeout_ms(),
                elapsed_ms = err.elapsed().as_millis(),
                delay_ms = delay.as_millis(),
                error = %err,
                "chunk upload failed, retrying"
            );
        },
        sleep,
    )
    .await
    .map_err(|err| anyhow::Error::new(err).context(format!("uploading chunk {chunk_idx}: {key}")))
}

/// Write a chunk to remote storage with exponential backoff retry.
///
/// Retries up to the default chunk retry count on transient failures.
async fn write_chunk_with_retry(
    op: &Operator,
    key: &str,
    data: Vec<u8>,
    chunk_idx: usize,
    logical_len: u64,
    write_timeout: Option<Duration>,
) -> Result<()> {
    write_chunk_with_retry_inner(
        key,
        chunk_idx,
        logical_len,
        write_timeout,
        || {
            let data = data.clone();
            async move {
                op.write(key, data)
                    .await
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            }
        },
        tokio::time::sleep,
    )
    .await
}

async fn maybe_upload_chunk(
    op: Operator,
    chunk_key: String,
    upload_data: Vec<u8>,
    chunk_idx: usize,
    logical_len: u64,
    _assume_fresh_prefix: bool,
    write_timeout: Option<Duration>,
) -> Result<u64> {
    // Always refresh the content-addressed object. An exists-then-skip fast
    // path races a concurrent orphan sweep: an old unreferenced CAS object can
    // disappear between the check and staged-manifest publication. Rewriting
    // the identical bytes both repairs a corrupt object and gives GC's
    // quarantine clock a fresh lower bound.
    write_chunk_with_retry(
        &op,
        &chunk_key,
        upload_data,
        chunk_idx,
        logical_len,
        write_timeout,
    )
    .await?;
    Ok(logical_len)
}

async fn await_next_chunk_upload(pending: &mut JoinSet<Result<u64>>) -> Result<u64> {
    let joined = pending
        .join_next()
        .await
        .context("chunk upload task set unexpectedly empty")?;
    joined.context("chunk upload task panicked or was cancelled")?
}

struct ChunkUploadWaitContext<'a> {
    local_path: &'a Path,
    upload_started: Instant,
    completed_chunks: usize,
    num_chunks: usize,
    uploaded_bytes: u64,
    streaming: bool,
    chunk_upload_concurrency: usize,
    heartbeat: Option<Duration>,
}

async fn await_next_chunk_upload_with_heartbeat(
    pending: &mut JoinSet<Result<u64>>,
    context: ChunkUploadWaitContext<'_>,
) -> Result<u64> {
    let Some(heartbeat) = context.heartbeat else {
        return await_next_chunk_upload(pending).await;
    };

    let wait_started = Instant::now();
    loop {
        match tokio::time::timeout(heartbeat, await_next_chunk_upload(pending)).await {
            Ok(result) => return result,
            Err(_) => {
                let file_elapsed = context.upload_started.elapsed();
                info!(
                    path = %context.local_path.display(),
                    completed_chunks = context.completed_chunks,
                    chunks = context.num_chunks,
                    uploaded_bytes = context.uploaded_bytes,
                    file_elapsed_ms = file_elapsed.as_millis() as u64,
                    completed_chunks_per_sec = rate_per_sec(context.completed_chunks as u64, file_elapsed),
                    uploaded_bytes_per_sec = rate_per_sec(context.uploaded_bytes, file_elapsed),
                    streaming = context.streaming,
                    pending_uploads = pending.len(),
                    chunk_upload_concurrency = context.chunk_upload_concurrency,
                    wait_elapsed_ms = wait_started.elapsed().as_millis() as u64,
                    "chunk upload heartbeat"
                );
            }
        }
    }
}

fn record_chunk_upload_progress(
    local_path: &Path,
    completed_chunks: usize,
    num_chunks: usize,
    uploaded_bytes: u64,
    streaming: bool,
    every_chunks: usize,
) {
    if !should_record_chunk_upload_progress(completed_chunks, num_chunks, every_chunks) {
        return;
    }

    info!(
        path = %local_path.display(),
        completed_chunks,
        chunks = num_chunks,
        uploaded_bytes,
        streaming,
        "chunk upload progress"
    );
}

/// Read a key from remote storage with exponential backoff retry.
///
/// Used for manifest/index reads so transient storage errors behave the same as
/// chunk downloads instead of aborting the whole pull on the first failure.
async fn read_with_retry_inner<Read, ReadFuture, Sleep, SleepFuture>(
    key: &str,
    read_timeout: Option<Duration>,
    mut read: Read,
    sleep: Sleep,
) -> Result<Vec<u8>>
where
    Read: FnMut() -> ReadFuture,
    ReadFuture: std::future::Future<Output = Result<Vec<u8>>>,
    Sleep: FnMut(std::time::Duration) -> SleepFuture,
    SleepFuture: std::future::Future<Output = ()>,
{
    retry_with_backoff(
        CHUNK_MAX_RETRIES,
        |_| {
            let fut = read();
            async move {
                match read_timeout {
                    Some(limit) => match tokio::time::timeout(limit, fut).await {
                        Ok(result) => result,
                        Err(_) => {
                            anyhow::bail!("read timed out after {} ms", limit.as_millis())
                        }
                    },
                    None => fut.await,
                }
            }
        },
        |attempt, delay, err: &anyhow::Error| {
            warn!(
                key = key,
                attempt,
                max = CHUNK_MAX_RETRIES,
                delay_ms = delay.as_millis(),
                error = %err,
                "read failed, retrying"
            );
        },
        sleep,
    )
    .await
    .map_err(|err| err.context(format!("reading: {key}")))
}

async fn read_with_retry(op: &Operator, key: &str) -> Result<Vec<u8>> {
    read_with_retry_inner(
        key,
        download_read_timeout(),
        || async {
            op.read(key)
                .await
                .map(|data| data.to_vec())
                .map_err(anyhow::Error::from)
        },
        tokio::time::sleep,
    )
    .await
}

/// Read a chunk from remote storage with exponential backoff retry.
///
/// Retries up to `CHUNK_MAX_RETRIES` times on transient failures.
/// After successful read, verifies the BLAKE3 hash matches the expected value.
#[derive(Debug)]
enum ChunkReadError {
    Transport {
        source: anyhow::Error,
        elapsed: Duration,
    },
    Timeout {
        timeout: Duration,
        elapsed: Duration,
    },
    Integrity {
        expected: String,
        actual: String,
    },
}

impl std::fmt::Display for ChunkReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport { source, elapsed } => {
                write!(
                    f,
                    "chunk transport read failed after {} ms: {source}",
                    elapsed.as_millis()
                )
            }
            Self::Timeout { timeout, .. } => {
                write!(
                    f,
                    "chunk download timed out after {} ms",
                    timeout.as_millis()
                )
            }
            Self::Integrity { expected, actual } => {
                write!(
                    f,
                    "chunk integrity failed: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for ChunkReadError {}

#[cfg(test)]
async fn read_chunk_with_retry_inner<Read, ReadFuture, Sleep, SleepFuture>(
    key: &str,
    expected_hash: &str,
    chunk_idx: usize,
    read: Read,
    sleep: Sleep,
) -> Result<Vec<u8>>
where
    Read: FnMut() -> ReadFuture,
    ReadFuture: std::future::Future<Output = Result<Vec<u8>>>,
    Sleep: FnMut(std::time::Duration) -> SleepFuture,
    SleepFuture: std::future::Future<Output = ()>,
{
    read_chunk_with_retry_inner_with_attempts(
        key,
        expected_hash,
        chunk_idx,
        CHUNK_MAX_RETRIES,
        None,
        read,
        sleep,
    )
    .await
}

async fn read_chunk_with_retry_inner_with_attempts<Read, ReadFuture, Sleep, SleepFuture>(
    key: &str,
    expected_hash: &str,
    chunk_idx: usize,
    max_attempts: u32,
    read_timeout: Option<Duration>,
    mut read: Read,
    sleep: Sleep,
) -> Result<Vec<u8>>
where
    Read: FnMut() -> ReadFuture,
    ReadFuture: std::future::Future<Output = Result<Vec<u8>>>,
    Sleep: FnMut(std::time::Duration) -> SleepFuture,
    SleepFuture: std::future::Future<Output = ()>,
{
    let overall_started = Instant::now();
    retry_with_backoff(
        max_attempts,
        |_| {
            let read_attempt = read();
            async move {
                let started = Instant::now();
                let chunk_bytes = match read_timeout {
                    Some(limit) => match tokio::time::timeout(limit, read_attempt).await {
                        Ok(result) => result.map_err(|source| ChunkReadError::Transport {
                            source,
                            elapsed: started.elapsed(),
                        })?,
                        Err(_) => {
                            return Err(ChunkReadError::Timeout {
                                timeout: limit,
                                elapsed: started.elapsed(),
                            })
                        }
                    },
                    None => read_attempt
                        .await
                        .map_err(|source| ChunkReadError::Transport {
                            source,
                            elapsed: started.elapsed(),
                        })?,
                };
                let actual_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&chunk_bytes));
                if actual_hash == expected_hash {
                    Ok(chunk_bytes)
                } else {
                    Err(ChunkReadError::Integrity {
                        expected: expected_hash.to_string(),
                        actual: actual_hash,
                    })
                }
            }
        },
        |attempt, delay, err| match err {
            ChunkReadError::Transport { source, elapsed } => {
                warn!(
                    chunk = chunk_idx,
                    attempt,
                    max = max_attempts,
                    delay_ms = delay.as_millis(),
                    elapsed_ms = elapsed.as_millis(),
                    error = %source,
                    "chunk download failed, retrying"
                );
            }
            ChunkReadError::Timeout { timeout, elapsed } => {
                warn!(
                    chunk = chunk_idx,
                    attempt,
                    max = max_attempts,
                    timeout_ms = timeout.as_millis(),
                    elapsed_ms = elapsed.as_millis(),
                    delay_ms = delay.as_millis(),
                    "chunk download timed out, retrying"
                );
            }
            ChunkReadError::Integrity { actual, .. } => {
                warn!(
                    chunk = chunk_idx,
                    attempt,
                    expected = expected_hash,
                    actual = %actual,
                    max = max_attempts,
                    delay_ms = delay.as_millis(),
                    "chunk integrity mismatch, retrying"
                );
            }
        },
        sleep,
    )
    .await
    .map_err(|err| {
        anyhow::Error::new(err).context(format!(
            "downloading chunk {chunk_idx}: {key} after {max_attempts} attempts over {} ms",
            overall_started.elapsed().as_millis()
        ))
    })
}

async fn read_chunk_with_retry(
    op: &Operator,
    key: &str,
    expected_hash: &str,
    chunk_idx: usize,
) -> Result<Vec<u8>> {
    read_chunk_with_retry_inner_with_attempts(
        key,
        expected_hash,
        chunk_idx,
        download_chunk_retries(),
        download_read_timeout(),
        || async {
            op.read(key)
                .await
                .map(|data| data.to_vec())
                .map_err(anyhow::Error::from)
        },
        tokio::time::sleep,
    )
    .await
}

fn validated_remote_prefix(remote_prefix: &str) -> Result<&str> {
    anyhow::ensure!(
        remote_prefix == remote_prefix.trim_end_matches('/'),
        "remote prefix must be canonical without a trailing slash: {remote_prefix:?}"
    );
    validate_relative_storage_key(remote_prefix, "remote prefix")?;
    Ok(remote_prefix)
}

/// Fail closed unless this exact storage accessor and canonical remote prefix
/// have proven the conditional-write contract required by publication.
///
/// Call this before uploading chunks or creating any staging/index object.
/// The storage layer caches a successful live probe by accessor identity and
/// prefix, so repeated admission checks do not repeat network mutations.
pub async fn ensure_publish_storage_semantics(op: &Operator, remote_prefix: &str) -> Result<()> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    tcfs_storage::ensure_conditional_write_semantics(op, prefix)
        .await
        .with_context(|| {
            format!("verifying conditional-write semantics for remote prefix {prefix:?}")
        })
}

fn validated_index_rel_path(rel_path: &str) -> Result<&str> {
    validate_canonical_rel_path(rel_path)?;
    anyhow::ensure!(
        !rel_path
            .split('/')
            .any(|component| component.eq_ignore_ascii_case(".tcfs_dir")),
        "remote index path uses reserved TCFS directory-marker component: {rel_path:?}"
    );
    Ok(rel_path)
}

/// Publish the internal sentinel that preserves an otherwise empty directory.
///
/// Directory markers participate in the same portable namespace admission as
/// regular indexed files, while ordinary publishers are forbidden from using
/// the reserved `.tcfs_dir` component themselves.
pub async fn publish_directory_marker(
    op: &Operator,
    remote_prefix: &str,
    directory_rel_path: &str,
) -> Result<()> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(directory_rel_path)?;
    ensure_publish_storage_semantics(op, prefix).await?;
    admit_portable_namespace_entry(
        op,
        prefix,
        directory_rel_path,
        PortableNamespaceRole::Directory,
    )
    .await?;
    let marker_rel_path = format!("{directory_rel_path}/.tcfs_dir");
    let marker_key = format!("{prefix}/index/{marker_rel_path}");
    write_directory_marker_conditionally(op, prefix, &marker_key).await?;
    Ok(())
}

fn manifest_path_prefix(remote_prefix: &str) -> String {
    format!("{}/manifests", remote_prefix.trim_end_matches('/'))
}

fn path_text_has_reserved_git_component(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|component| component.eq_ignore_ascii_case(".git"))
}

fn filesystem_path_has_reserved_git_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            std::path::Component::Normal(name)
                if name.to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        )
    })
}

enum ParsedIndexedManifest {
    Regular(SyncManifest),
    Symlink(SymlinkManifest),
}

/// The causal fields needed to classify a local publish against one validated
/// remote manifest. `content_hash` remains the canonical local-state identity,
/// while `logical_identity` also binds the entry kind so a regular file and a
/// symlink can never compare as content-identical merely because their raw
/// identities happen to match.
struct ManifestCausality {
    content_hash: String,
    logical_identity: String,
    vclock: VectorClock,
    written_by: String,
}

fn logical_content_identity(kind: RemoteEntryKind, content_hash: &str) -> String {
    let kind = match kind {
        RemoteEntryKind::RegularFile => "regular_file",
        RemoteEntryKind::Symlink => "symlink",
    };
    format!("tcfs-logical-content-v1:{kind}:{content_hash}")
}

impl ManifestCausality {
    fn regular(manifest: SyncManifest) -> Self {
        let content_hash = manifest.file_hash;
        Self {
            logical_identity: logical_content_identity(RemoteEntryKind::RegularFile, &content_hash),
            content_hash,
            vclock: manifest.vclock,
            written_by: manifest.written_by,
        }
    }

    fn from_parsed(parsed: ParsedIndexedManifest) -> Self {
        match parsed {
            ParsedIndexedManifest::Regular(manifest) => Self::regular(manifest),
            ParsedIndexedManifest::Symlink(manifest) => {
                let content_hash = symlink_manifest_hash(&manifest.symlink_target);
                Self {
                    logical_identity: logical_content_identity(
                        RemoteEntryKind::Symlink,
                        &content_hash,
                    ),
                    content_hash,
                    vclock: manifest.vclock,
                    written_by: manifest.written_by,
                }
            }
        }
    }
}

/// Classify typed content identities without leaking the type tag into the
/// conflict payload. Conflict resolution and state-cache consumers still need
/// the canonical raw file/symlink hashes to identify either side.
#[allow(clippy::too_many_arguments)]
fn compare_typed_content_clocks(
    local: &VectorClock,
    remote: &VectorClock,
    local_kind: RemoteEntryKind,
    local_hash: &str,
    remote_identity: &str,
    remote_hash: &str,
    rel_path: &str,
    local_device: &str,
    remote_device: &str,
) -> SyncOutcome {
    let local_identity = logical_content_identity(local_kind, local_hash);
    let mut outcome = compare_clocks(
        local,
        remote,
        &local_identity,
        remote_identity,
        rel_path,
        local_device,
        remote_device,
    );
    if let SyncOutcome::Conflict(info) = &mut outcome {
        info.local_blake3 = local_hash.to_string();
        info.remote_blake3 = remote_hash.to_string();
    }
    outcome
}

/// Bind an existing legacy direct-manifest object to the content-hash key the
/// uploader selected. Direct manifests predate byte-addressed indexed objects,
/// so their embedded regular-file identity is the only safe compatibility
/// binding; v1 manifests and path-bound indexed manifests cannot prove it.
fn parse_direct_manifest_for_content_key(
    manifest_bytes: &[u8],
    expected_file_hash: &str,
    expected_file_size: u64,
) -> Result<SyncManifest> {
    let manifest = SyncManifest::from_bytes(manifest_bytes)?;
    anyhow::ensure!(
        matches!(manifest.version, 2 | 3),
        "direct content-addressed manifest requires JSON schema v2 or v3"
    );
    anyhow::ensure!(
        manifest.file_hash == expected_file_hash,
        "direct manifest content-key mismatch: key selects {expected_file_hash}, manifest declares {}",
        manifest.file_hash
    );
    anyhow::ensure!(
        manifest.file_size == expected_file_size,
        "direct manifest size mismatch for {expected_file_hash}: local {expected_file_size}, manifest {}",
        manifest.file_size
    );
    anyhow::ensure!(
        manifest.rel_path.is_none(),
        "direct manifest key contains a path-bound indexed manifest"
    );
    if manifest.version == 3 {
        anyhow::ensure!(
            !manifest.wrapped_file_keys.is_empty(),
            "direct regular manifest v3 is missing per-device wrapped keys"
        );
    }
    Ok(manifest)
}

fn validate_manifest_rel_path(actual: Option<&str>, expected: &str) -> Result<()> {
    anyhow::ensure!(
        actual == Some(expected),
        "manifest rel_path mismatch: expected {:?}, got {:?}",
        expected,
        actual
    );
    Ok(())
}

fn parse_manifest_for_index_entry(
    manifest_bytes: &[u8],
    expected_kind: RemoteEntryKind,
    expected_symlink_target: Option<&str>,
    rel_path: &str,
) -> Result<ParsedIndexedManifest> {
    match (expected_kind, expected_symlink_target) {
        (RemoteEntryKind::RegularFile, Some(_)) => {
            anyhow::bail!("regular-file index entry must not carry symlink_target")
        }
        (RemoteEntryKind::Symlink, None) => {
            anyhow::bail!("symlink index entry is missing symlink_target")
        }
        _ => {}
    }

    if expected_kind == RemoteEntryKind::Symlink && path_text_has_reserved_git_component(rel_path) {
        anyhow::bail!("refusing symlink index entry at reserved Git metadata path: {rel_path}");
    }

    match expected_kind {
        RemoteEntryKind::RegularFile => {
            if manifest_bytes
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                == Some(b'{')
            {
                let value: serde_json::Value = serde_json::from_slice(manifest_bytes)
                    .with_context(|| format!("parsing indexed manifest envelope for {rel_path}"))?;
                let object = value.as_object().with_context(|| {
                    format!("indexed regular manifest is not a JSON object for {rel_path}")
                })?;
                anyhow::ensure!(
                    !object.contains_key("kind") && !object.contains_key("symlink_target"),
                    "manifest kind mismatch for {rel_path}: regular-file index points at a symlink-discriminated envelope"
                );
            }
            if SymlinkManifest::from_bytes(manifest_bytes).is_ok() {
                anyhow::bail!(
                    "manifest kind mismatch for {rel_path}: index declares regular_file, manifest is symlink"
                );
            }
            let manifest = SyncManifest::from_bytes(manifest_bytes)
                .with_context(|| format!("parsing regular-file manifest for {rel_path}"))?;
            validate_manifest_rel_path(manifest.rel_path.as_deref(), rel_path)?;
            Ok(ParsedIndexedManifest::Regular(manifest))
        }
        RemoteEntryKind::Symlink => {
            let manifest = SymlinkManifest::from_bytes(manifest_bytes)
                .with_context(|| format!("parsing symlink manifest for {rel_path}"))?;
            let expected_target = expected_symlink_target
                .expect("symlink target presence checked before manifest parse");
            anyhow::ensure!(
                manifest.symlink_target == expected_target,
                "symlink target mismatch for {rel_path}: index declares {:?}, manifest declares {:?}",
                expected_target,
                manifest.symlink_target
            );
            validate_manifest_rel_path(manifest.rel_path.as_deref(), rel_path)?;
            Ok(ParsedIndexedManifest::Symlink(manifest))
        }
    }
}

fn validate_parsed_indexed_manifest_binding(
    manifest_bytes: &[u8],
    object_id: &str,
    parsed: &ParsedIndexedManifest,
) -> Result<()> {
    fn validate_regular_schema(manifest: &SyncManifest) -> Result<()> {
        anyhow::ensure!(
            matches!(manifest.version, 2 | 3),
            "indexed regular manifest must use JSON schema v2 or v3"
        );
        if manifest.version == 3 {
            anyhow::ensure!(
                !manifest.wrapped_file_keys.is_empty(),
                "indexed regular manifest v3 is missing per-device wrapped keys"
            );
        }
        crate::index_entry::validate_storage_key_component(
            &manifest.file_hash,
            "indexed regular manifest file_hash",
        )
    }

    crate::index_entry::validate_storage_key_component(object_id, "manifest object id")?;
    if let ParsedIndexedManifest::Regular(manifest) = parsed {
        validate_regular_schema(manifest)?;
    }
    let actual = manifest_object_id(manifest_bytes);
    if actual == object_id {
        return Ok(());
    }

    match parsed {
        ParsedIndexedManifest::Regular(manifest) => {
            anyhow::ensure!(
                manifest.file_hash == object_id,
                "indexed regular manifest object identity mismatch"
            );
        }
        ParsedIndexedManifest::Symlink(manifest) => {
            anyhow::ensure!(
                symlink_manifest_hash(&manifest.symlink_target) == object_id,
                "indexed symlink manifest object identity mismatch"
            );
        }
    }
    Ok(())
}

fn validate_parsed_indexed_manifest_metadata(
    parsed: &ParsedIndexedManifest,
    expected_size: u64,
    expected_chunks: usize,
    rel_path: &str,
) -> Result<()> {
    match parsed {
        ParsedIndexedManifest::Regular(manifest) => {
            anyhow::ensure!(
                manifest.file_size == expected_size,
                "index/manifest size mismatch for {rel_path}: index {expected_size}, manifest {}",
                manifest.file_size
            );
            anyhow::ensure!(
                manifest.chunks.len() == expected_chunks,
                "index/manifest chunk-count mismatch for {rel_path}: index {expected_chunks}, manifest {}",
                manifest.chunks.len()
            );
        }
        ParsedIndexedManifest::Symlink(manifest) => {
            anyhow::ensure!(
                expected_size == manifest.symlink_target.len() as u64 && expected_chunks == 0,
                "index/manifest symlink metadata mismatch for {rel_path}"
            );
        }
    }
    Ok(())
}

fn validate_fixed_manifest_ingress_path(parsed: &ParsedIndexedManifest) -> Result<()> {
    let rel_path = match parsed {
        ParsedIndexedManifest::Regular(manifest) => manifest.rel_path.as_deref(),
        ParsedIndexedManifest::Symlink(manifest) => manifest.rel_path.as_deref(),
    };
    let Some(rel_path) = rel_path else {
        return Ok(());
    };

    if let Some(reason) =
        Blacklist::default().check_fixed_ingress_path_components(Path::new(rel_path))
    {
        anyhow::bail!("refusing manifest-bound fixed-deny ingress path {rel_path:?}: {reason}");
    }
    Ok(())
}

/// Validate serialized manifest bytes against the index-selected object id,
/// kind, target, and path before planning or hydration uses remote metadata.
///
/// New objects are byte-addressed. The two compatibility exceptions are exact:
/// JSON regular manifests historically addressed by their embedded file hash,
/// and v3 symlink manifests historically addressed by the declared target
/// identity. Indexed v1 newline manifests have no embedded file identity and
/// therefore fail closed here; the legacy direct-manifest API remains readable.
pub fn validate_indexed_manifest_binding(
    manifest_bytes: &[u8],
    object_id: &str,
    expected_kind: RemoteEntryKind,
    expected_symlink_target: Option<&str>,
    rel_path: &str,
) -> Result<()> {
    validated_index_rel_path(rel_path)?;
    let parsed = parse_manifest_for_index_entry(
        manifest_bytes,
        expected_kind,
        expected_symlink_target,
        rel_path,
    )?;
    validate_parsed_indexed_manifest_binding(manifest_bytes, object_id, &parsed)
}

/// Validate a manifest against every piece of authority carried by its index
/// entry, including size and chunk count, before any local mutation.
pub fn validate_indexed_manifest_entry_binding(
    manifest_bytes: &[u8],
    object_id: &str,
    entry: &RemoteIndexEntry,
    rel_path: &str,
) -> Result<()> {
    validate_remote_entry(entry, "indexed manifest entry")?;
    validated_index_rel_path(rel_path)?;
    let parsed = parse_manifest_for_index_entry(
        manifest_bytes,
        entry.kind,
        entry.symlink_target.as_deref(),
        rel_path,
    )?;
    validate_parsed_indexed_manifest_binding(manifest_bytes, object_id, &parsed)?;
    validate_parsed_indexed_manifest_metadata(&parsed, entry.size, entry.chunks, rel_path)
}

fn validate_publish_manifest(
    manifest_bytes: &[u8],
    entry: &RemoteIndexEntry,
    rel_path: &str,
) -> Result<()> {
    validate_remote_entry(entry, "publish index entry")?;
    validated_index_rel_path(rel_path)?;
    if let (RemoteEntryKind::Symlink, Some(target)) = (entry.kind, entry.symlink_target.as_deref())
    {
        validate_indexed_symlink_target(Path::new(rel_path), target)?;
    }
    let parsed = parse_manifest_for_index_entry(
        manifest_bytes,
        entry.kind,
        entry.symlink_target.as_deref(),
        rel_path,
    )?;
    let actual = manifest_object_id(manifest_bytes);
    anyhow::ensure!(
        actual == entry.manifest_hash,
        "publish manifest content id mismatch for {rel_path}: index declares {}, bytes address as {actual}",
        entry.manifest_hash
    );
    validate_parsed_indexed_manifest_binding(manifest_bytes, &entry.manifest_hash, &parsed)?;
    validate_parsed_indexed_manifest_metadata(&parsed, entry.size, entry.chunks, rel_path)
}

/// Stages of the manifest/index publish pipeline.
///
/// Emitted via the `after_stage` hook in `publish_manifest_for_rel_path_with_hook`
/// so tests can inject failures between steps (see `engine` test module).
/// Each variant names the artifact that has **just been written**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishStage {
    StagedManifest,
    PreparingIndex,
    FinalManifest,
    CommittedIndex,
}

/// Exact index-object identity captured before an indexed publish decision.
///
/// The path and prefix binding prevents a caller from reusing a guard for a
/// different index object. Publication consumes the baseline, so a low-level
/// client cannot silently re-resolve a newer value immediately before CAS.
#[derive(Debug)]
pub struct IndexedPublishBaseline {
    remote_prefix: String,
    rel_path: String,
    resolved: ResolvedIndexEntryForUpdate,
}

impl IndexedPublishBaseline {
    pub fn current(&self) -> Option<&RemoteIndexEntry> {
        self.resolved.current()
    }
}

/// Capture the visible entry and its exact conditional-write identity for one
/// non-fresh indexed publication.
pub async fn bind_indexed_publish_baseline(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
) -> Result<IndexedPublishBaseline> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(rel_path)?;
    ensure_publish_storage_semantics(op, prefix).await?;
    let index_key = format!("{prefix}/index/{rel_path}");
    let manifest_prefix = manifest_path_prefix(prefix);
    let resolved = resolve_visible_index_entry_for_update(op, &index_key, &manifest_prefix)
        .await
        .with_context(|| format!("binding indexed publication baseline: {index_key}"))?;
    Ok(IndexedPublishBaseline {
        remote_prefix: prefix.to_string(),
        rel_path: rel_path.to_string(),
        resolved,
    })
}

/// Crash-consistent publish boundary for non-engine clients that already
/// produced manifest bytes and their byte-addressed index entry.
///
/// The manifest is staged, a preparing index record retains the last visible
/// entry, the immutable final object is installed, and only then is the new
/// entry committed. Callers must not write manifest and index objects
/// independently.
pub async fn publish_indexed_manifest(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
    baseline: IndexedPublishBaseline,
) -> Result<()> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(rel_path)?;
    anyhow::ensure!(
        baseline.remote_prefix == prefix && baseline.rel_path == rel_path,
        "indexed publish baseline is bound to {}/{}, not {prefix}/{rel_path}",
        baseline.remote_prefix,
        baseline.rel_path
    );
    publish_manifest_for_rel_path_with_mode(
        op,
        prefix,
        rel_path,
        manifest_bytes,
        entry,
        false,
        Some(baseline.resolved),
    )
    .await
}

async fn publish_manifest_for_rel_path_with_mode(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
    assume_fresh_prefix: bool,
    baseline: Option<ResolvedIndexEntryForUpdate>,
) -> Result<()> {
    ensure_publish_storage_semantics(op, remote_prefix).await?;
    if assume_fresh_prefix {
        anyhow::ensure!(
            baseline.is_none(),
            "fresh-prefix publication must not carry an existing index baseline"
        );
        return publish_manifest_for_rel_path_fresh(
            op,
            remote_prefix,
            rel_path,
            manifest_bytes,
            entry,
        )
        .await;
    }

    publish_manifest_for_rel_path_with_hook(
        op,
        remote_prefix,
        rel_path,
        manifest_bytes,
        entry,
        baseline,
        |_| Ok(()),
    )
    .await
}

async fn publish_manifest_for_rel_path_fresh(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
) -> Result<()> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validate_publish_manifest(&manifest_bytes, &entry, rel_path)?;
    let index_key = format!("{prefix}/index/{rel_path}");
    admit_portable_namespace_entry(op, prefix, rel_path, PortableNamespaceRole::File).await?;
    let manifest_prefix = manifest_path_prefix(prefix);
    let final_manifest_key = manifest_key(&manifest_prefix, &entry.manifest_hash);

    // `assume_fresh_prefix` is an exact ownership contract: this publisher is
    // the exclusive initializer of an empty remote prefix. Still bind the
    // index write to atomic absence so a violated contract can never turn into
    // an unconditional overwrite.
    let absent_guard = require_absent_index_entry_for_update(op, prefix, &index_key).await?;

    write_immutable_manifest_object_if_absent(op, prefix, &final_manifest_key, &manifest_bytes)
        .await?;
    write_committed_index_entry_conditionally(op, prefix, &index_key, &entry, absent_guard).await?;
    Ok(())
}

async fn publish_manifest_for_rel_path_with_hook<F>(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
    baseline: Option<ResolvedIndexEntryForUpdate>,
    mut after_stage: F,
) -> Result<()>
where
    F: FnMut(PublishStage) -> Result<()>,
{
    publish_manifest_for_rel_path_with_async_hook(
        op,
        remote_prefix,
        rel_path,
        manifest_bytes,
        entry,
        baseline,
        move |stage| std::future::ready(after_stage(stage)),
    )
    .await
}

async fn publish_manifest_for_rel_path_with_async_hook<F, Fut>(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
    baseline: Option<ResolvedIndexEntryForUpdate>,
    mut after_stage: F,
) -> Result<()>
where
    F: FnMut(PublishStage) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let prefix = validated_remote_prefix(remote_prefix)?;
    validate_publish_manifest(&manifest_bytes, &entry, rel_path)?;
    let index_key = format!("{prefix}/index/{rel_path}");
    admit_portable_namespace_entry(op, prefix, rel_path, PortableNamespaceRole::File).await?;
    let manifest_prefix = manifest_path_prefix(prefix);
    let final_manifest_key = manifest_key(&manifest_prefix, &entry.manifest_hash);
    let staged_manifest_key = format!(
        "{prefix}/staging/manifests/{}-{}.json",
        Uuid::new_v4(),
        entry.manifest_hash
    );

    let baseline = match baseline {
        Some(baseline) => baseline,
        None => {
            #[cfg(test)]
            {
                // Failure-injection tests exercise individual pipeline stages;
                // production entry points are required to prebind above.
                resolve_visible_index_entry_for_update(op, &index_key, &manifest_prefix).await?
            }
            #[cfg(not(test))]
            {
                anyhow::bail!("non-fresh indexed publication requires an exact prebound baseline")
            }
        }
    };
    let (current, index_guard) = baseline.into_parts();

    op.write(&staged_manifest_key, manifest_bytes.clone())
        .await
        .with_context(|| format!("writing staged manifest: {staged_manifest_key}"))?;
    after_stage(PublishStage::StagedManifest).await?;

    let preparing_guard = write_preparing_index_entry_conditionally(
        op,
        prefix,
        &index_key,
        current,
        PendingIndexEntry::from_remote_entry(&entry, staged_manifest_key.clone()),
        index_guard,
    )
    .await?;
    after_stage(PublishStage::PreparingIndex).await?;

    let created =
        write_immutable_manifest_object_if_absent(op, prefix, &final_manifest_key, &manifest_bytes)
            .await?;
    if created {
        after_stage(PublishStage::FinalManifest).await?;
    }

    write_committed_index_entry_conditionally(op, prefix, &index_key, &entry, preparing_guard)
        .await?;
    after_stage(PublishStage::CommittedIndex).await?;
    // Preserve the immutable staged object as recovery evidence. Physical
    // reclamation requires a separate reachability-safe GC snapshot.
    Ok(())
}

/// Optional encryption context for E2E encrypted push/pull.
///
/// When present, chunks are encrypted before upload and decrypted after download
/// using XChaCha20-Poly1305 with per-file keys wrapped by the master key.
#[cfg(feature = "crypto")]
pub use tcfs_core::config::WrapMode;

#[cfg(feature = "crypto")]
pub struct EncryptionContext {
    pub master_key: tcfs_crypto::MasterKey,
    /// File-key wrap mode (TIN-1417). Drives the write path:
    /// - [`WrapMode::Master`]: master-only wrap (`encrypted_file_key`), manifest v2.
    /// - [`WrapMode::Dual`]: BOTH master wrap + per-device wraps, manifest v2.
    /// - [`WrapMode::PerDevice`]: per-device wraps ONLY (drops master wrap),
    ///   manifest **v3**.
    ///
    /// Callers MUST satisfy the roll-call gate before selecting `PerDevice`
    /// (see `with_wrap_mode` / the daemon's `build_encryption_context`). When the
    /// gate is not satisfied callers fall back to `Dual` and warn — the engine
    /// itself trusts the mode it is handed.
    pub wrap_mode: WrapMode,
    /// Active-device recipients for per-device FileKey wrapping (TIN-1417).
    ///
    /// Required (non-empty) for `Dual` and `PerDevice`; ignored for `Master`.
    /// A device removed from this set (revoked) cannot decrypt content written
    /// after its removal in `PerDevice` mode.
    pub device_recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
    /// This device's age identity, used to unwrap per-device manifests on read.
    /// `None` relies on the master-key fallback (legacy / master / dual manifests).
    pub device_identity: Option<DeviceUnwrapIdentity>,
}

/// A local device's age X25519 identity for unwrapping per-device manifests.
#[cfg(feature = "crypto")]
#[derive(Clone)]
pub struct DeviceUnwrapIdentity {
    /// Stable TCFS device id this identity belongs to.
    pub device_id: String,
    /// Armored age X25519 secret key (`AGE-SECRET-KEY-1...`).
    pub secret: String,
}

#[cfg(feature = "crypto")]
impl EncryptionContext {
    /// Legacy shared-master context: master-only wrap, no per-device recipients
    /// or identity. [`WrapMode::Master`] — byte-identical to the historical
    /// default.
    pub fn new(master_key: tcfs_crypto::MasterKey) -> Self {
        Self {
            master_key,
            wrap_mode: WrapMode::Master,
            device_recipients: Vec::new(),
            device_identity: None,
        }
    }

    /// Attach per-device wrapping recipients and this device's unwrap identity,
    /// selecting [`WrapMode::PerDevice`] (per-device-only, manifest v3).
    ///
    /// Prefer [`Self::with_wrap_mode`] when the caller needs `Dual`. This method
    /// preserves the pre-TIN-1417-enum behavior (recipients present =>
    /// per-device-only writes) for existing call sites and tests. Callers MUST
    /// have satisfied the roll-call gate before reaching `PerDevice`.
    pub fn with_device_wrapping(
        self,
        recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
        identity: Option<DeviceUnwrapIdentity>,
    ) -> Self {
        self.with_wrap_mode(WrapMode::PerDevice, recipients, identity)
    }

    /// Attach an explicit wrap mode plus the per-device recipient set and this
    /// device's unwrap identity.
    ///
    /// For [`WrapMode::Master`] the recipients/identity are still recorded (so
    /// the same context can read per-device manifests it encounters) but the
    /// write path emits the master-only wrap. For `Dual`/`PerDevice` the
    /// recipients drive the per-device wraps.
    pub fn with_wrap_mode(
        mut self,
        wrap_mode: WrapMode,
        recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
        identity: Option<DeviceUnwrapIdentity>,
    ) -> Self {
        self.wrap_mode = wrap_mode;
        self.device_recipients = recipients;
        self.device_identity = identity;
        self
    }
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
    /// Whether to preserve symlinks as symlinks instead of skipping/following.
    pub preserve_symlinks: bool,
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
            preserve_symlinks: false,
            sync_empty_dirs: true,
        }
    }
}

/// Result of collecting files and empty directories from a local tree.
#[derive(Debug, Clone)]
pub struct CollectResult {
    /// Regular files to upload.
    pub files: Vec<PathBuf>,
    /// Symlinks to preserve as symlinks.
    pub symlinks: Vec<PathBuf>,
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
    /// Vector clock committed in the uploaded manifest/state entry.
    pub vclock: VectorClock,
    /// true if file was already up-to-date (skipped)
    pub skipped: bool,
    /// Sync outcome if conflict detection was performed
    pub outcome: Option<SyncOutcome>,
}

#[derive(Debug)]
enum UploadSourceSnapshot {
    InMemory(Vec<u8>),
    Streaming(Vec<tcfs_chunks::Chunk>),
}

#[derive(Debug)]
struct UploadSnapshot {
    file_hash_hex: String,
    file_size: u64,
    source: UploadSourceSnapshot,
}

fn prepare_upload_snapshot(local_path: &Path, use_streaming: bool) -> Result<UploadSnapshot> {
    if use_streaming {
        let (chunks, file_hash) = tcfs_chunks::chunk_file_streaming_metadata(local_path)
            .with_context(|| {
                format!(
                    "streaming chunk metadata for upload snapshot: {}",
                    local_path.display()
                )
            })?;
        let file_size = chunks.iter().map(|chunk| chunk.length as u64).sum();
        let file_hash_hex = tcfs_chunks::hash_to_hex(&file_hash);
        Ok(UploadSnapshot {
            file_hash_hex,
            file_size,
            source: UploadSourceSnapshot::Streaming(chunks),
        })
    } else {
        let data = std::fs::read(local_path)
            .with_context(|| format!("reading upload snapshot: {}", local_path.display()))?;
        let file_hash_hex = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data));
        Ok(UploadSnapshot {
            file_hash_hex,
            file_size: data.len() as u64,
            source: UploadSourceSnapshot::InMemory(data),
        })
    }
}

fn read_verified_snapshot_chunk_from(
    file: &mut std::fs::File,
    local_path: &Path,
    chunk: &tcfs_chunks::Chunk,
    chunk_idx: usize,
) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(chunk.offset))
        .with_context(|| format!("seeking chunk {chunk_idx}: {}", local_path.display()))?;

    let mut data = vec![0u8; chunk.length];
    file.read_exact(&mut data)
        .with_context(|| format!("reading chunk {chunk_idx}: {}", local_path.display()))?;

    let actual_hash = tcfs_chunks::hash_bytes(&data);
    if actual_hash != chunk.hash {
        anyhow::bail!(
            "file changed during streaming upload: chunk {chunk_idx} hash mismatch for {}",
            local_path.display()
        );
    }

    Ok(data)
}

fn ensure_source_matches_snapshot(
    local_path: &Path,
    snapshot: &UploadSnapshot,
    stage: &str,
) -> Result<()> {
    let current_meta = std::fs::metadata(local_path)
        .with_context(|| format!("stat during {stage}: {}", local_path.display()))?;
    if current_meta.len() != snapshot.file_size {
        anyhow::bail!(
            "file changed during {stage}: size mismatch for {} (snapshot={} current={})",
            local_path.display(),
            snapshot.file_size,
            current_meta.len()
        );
    }

    let current_hash_hex = match snapshot.source {
        UploadSourceSnapshot::InMemory(_) => {
            let data = std::fs::read(local_path)
                .with_context(|| format!("reading during {stage}: {}", local_path.display()))?;
            tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data))
        }
        UploadSourceSnapshot::Streaming(_) => {
            let hash = tcfs_chunks::hash_file_streaming(local_path).with_context(|| {
                format!("streaming hash during {stage}: {}", local_path.display())
            })?;
            tcfs_chunks::hash_to_hex(&hash)
        }
    };

    if current_hash_hex != snapshot.file_hash_hex {
        anyhow::bail!(
            "file changed during {stage}: hash mismatch for {} (snapshot={} current={})",
            local_path.display(),
            snapshot.file_hash_hex,
            current_hash_hex
        );
    }

    Ok(())
}

/// Result of downloading a single file
#[derive(Debug)]
pub struct DownloadResult {
    pub remote_path: String,
    pub local_path: PathBuf,
    pub bytes: u64,
    pub sync_state: Option<SyncState>,
}

fn unique_tmp_path(local_path: &Path, marker: &str) -> PathBuf {
    let target_name = local_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("tcfs"));
    let mut file_name = OsString::from(".");
    file_name.push(target_name);
    // `.tc` is a reserved physical-stub suffix in both the watcher and the
    // full-tree collector. Keeping it last fences in-progress and crash-leftover
    // plaintext, while the shared parent still permits an atomic final rename.
    file_name.push(format!(".{marker}.{}.tc", Uuid::new_v4()));
    local_path.with_file_name(file_name)
}

/// Create a same-directory download temp file without exposing plaintext or a
/// symlink-following window under a permissive daemon umask.
fn create_private_download_tmp(local_path: &Path) -> Result<(PathBuf, tokio::fs::File)> {
    let tmp = unique_tmp_path(local_path, "tcfs_tmp");
    let file = open_private_download_tmp(&tmp)?;
    Ok((tmp, tokio::fs::File::from_std(file)))
}

fn open_private_download_tmp(tmp: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(tmp)
        .with_context(|| format!("creating private download tmp: {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing private download tmp: {}", tmp.display()))?;
    }
    Ok(file)
}

/// Create any missing download parent directories with owner-only mode even
/// when the daemon inherited a permissive process umask. Existing directories
/// keep their operator-managed permissions.
fn create_private_download_parents(local_path: &Path) -> Result<()> {
    let Some(parent) = local_path.parent() else {
        return Ok(());
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .with_context(|| format!("creating private download parent: {}", parent.display()))
}

/// Convert a `SystemTime` into `(unix_secs, subsec_nanos)` for manifest storage.
///
/// Times before the Unix epoch are represented with a negative seconds component
/// and the matching positive sub-second remainder, mirroring `utimensat`'s
/// `timespec` convention so the round-trip is lossless.
fn systemtime_to_unix_parts(t: SystemTime) -> (i64, u32) {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            // Pre-epoch: duration is how far *before* the epoch we are.
            let d = e.duration();
            let nanos = d.subsec_nanos();
            if nanos == 0 {
                (-(d.as_secs() as i64), 0)
            } else {
                // Borrow one second so the nanos component stays in [0, 1e9).
                (-(d.as_secs() as i64) - 1, 1_000_000_000 - nanos)
            }
        }
    }
}

/// Apply a previously captured `(unix_secs, subsec_nanos)` mtime to `path`.
///
/// Only the modification time is set; the access time is left to the kernel's
/// default (`UTIME_OMIT`). On non-Unix targets this is a no-op — the manifest
/// still carries the value, and a future port can honor it. Best-effort: a
/// failure to restamp is logged but never aborts the restore, since the file
/// content is already correctly written.
#[cfg(unix)]
fn apply_manifest_mtime(path: &Path, mtime: (i64, u32)) {
    use std::os::unix::ffi::OsStrExt;
    let (secs, nanos) = mtime;
    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => {
            warn!(path = %path.display(), "skipping mtime restore: path contains NUL");
            return;
        }
    };
    // mtime carries the captured value; atime is omitted so we don't perturb it.
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: secs as libc::time_t,
            tv_nsec: nanos as _,
        },
    ];
    // SAFETY: `c_path` is a valid NUL-terminated C string for the duration of
    // the call, and `times` is a 2-element array of initialized `timespec`.
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        warn!(path = %path.display(), error = %err, "failed to restore mtime from manifest");
    }
}

#[cfg(not(unix))]
fn apply_manifest_mtime(_path: &Path, _mtime: (i64, u32)) {
    // No portable non-Unix mtime restore is wired yet; the value still round-trips
    // through the manifest so a future port can honor it.
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
    let tracked_state = state.get(local_path).cloned();
    let sync_reason = state.needs_sync(local_path)?;
    let (result, state_update) = upload_file_with_device_with_state(
        op,
        local_path,
        remote_prefix,
        progress,
        device_id,
        rel_path,
        encryption,
        tracked_state,
        sync_reason,
        UploadRuntimeOptions::from_env(),
        None,
    )
    .await?;

    if let Some(sync_state) = state_update {
        state.set(local_path, sync_state);
    }

    Ok(result)
}

/// Upload a file that a reconcile plan has ALREADY classified as needing a
/// push, bypassing the `needs_sync` stat quick-check.
///
/// `needs_sync` compares `(size, mtime-seconds)` against the cached state, so
/// it cannot see a same-second rewrite that keeps the size constant — exactly
/// what `git commit` does to a branch head ref (always 40-hex + newline). The
/// reconcile classifier content-hashes the local file before planning a
/// `Push`, so execution must honor that stronger decision rather than
/// re-derive staleness from stat and silently skip the push (which would leave
/// the remote head behind and break `.git` fast-forward convergence).
///
/// `git_ff_expected_manifest` marks a push that was reclassified from a `.git`
/// conflict by the fast-forward resolver: the plan proved the local git tip is
/// a strict descendant of the remote tip, so this push may dominate a
/// concurrent remote vector clock (merge + tick) instead of being veto-skipped
/// — but only while the remote index entry still carries exactly this manifest
/// hash (the one the ancestry proof was computed against). Ordinary pushes
/// pass `None` and keep the standard conflict-veto semantics.
#[allow(clippy::too_many_arguments)]
pub async fn upload_planned_push_with_device(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
    device_id: &str,
    rel_path: Option<&str>,
    encryption: OptionalEncryption<'_>,
    git_ff_expected_manifest: Option<&str>,
) -> Result<UploadResult> {
    let tracked_state = state.get(local_path).cloned();
    let (result, state_update) = upload_file_with_device_with_state(
        op,
        local_path,
        remote_prefix,
        progress,
        device_id,
        rel_path,
        encryption,
        tracked_state,
        Some("planned push (reconcile classified change)".into()),
        UploadRuntimeOptions::from_env(),
        git_ff_expected_manifest,
    )
    .await?;

    if let Some(sync_state) = state_update {
        state.set(local_path, sync_state);
    }

    Ok(result)
}

#[allow(unused_variables)]
#[allow(clippy::too_many_arguments)]
async fn upload_file_with_device_with_state(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
    device_id: &str,
    rel_path: Option<&str>,
    encryption: OptionalEncryption<'_>,
    tracked_state: Option<SyncState>,
    sync_reason: Option<String>,
    runtime: UploadRuntimeOptions,
    git_ff_expected_manifest: Option<&str>,
) -> Result<(UploadResult, Option<SyncState>)> {
    upload_file_with_device_with_state_and_publish_hook(
        op,
        local_path,
        remote_prefix,
        progress,
        device_id,
        rel_path,
        encryption,
        tracked_state,
        sync_reason,
        runtime,
        git_ff_expected_manifest,
        || std::future::ready(Ok(())),
    )
    .await
}

#[allow(unused_variables)]
#[allow(clippy::too_many_arguments)]
async fn upload_file_with_device_with_state_and_publish_hook<F, Fut>(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
    device_id: &str,
    rel_path: Option<&str>,
    encryption: OptionalEncryption<'_>,
    tracked_state: Option<SyncState>,
    sync_reason: Option<String>,
    runtime: UploadRuntimeOptions,
    git_ff_expected_manifest: Option<&str>,
    before_manifest_publish: F,
) -> Result<(UploadResult, Option<SyncState>)>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let remote_prefix = validated_remote_prefix(remote_prefix)?;
    if let Some(rel_path) = rel_path {
        validated_index_rel_path(rel_path)?;
    }

    // Fast-path: check if file is already up-to-date
    match sync_reason.as_deref() {
        None => {
            let cached = tracked_state.as_ref().ok_or_else(|| {
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
                vclock: cached.vclock.clone(),
                skipped: true,
                outcome: Some(SyncOutcome::UpToDate),
            };
            debug!(path = %local_path.display(), "skip: unchanged since last sync");
            return Ok((result, None));
        }
        Some(reason) => {
            debug!(path = %local_path.display(), reason = %reason, "uploading");
        }
    }

    // Probe the exact accessor/prefix before chunk, staging, manifest, or index
    // mutation. A backend that merely advertises conditional headers is not a
    // safe publication target until the live semantics have been verified.
    ensure_publish_storage_semantics(op, remote_prefix).await?;

    // Tiered chunking: files below STREAMING_THRESHOLD are read into memory,
    // larger files use streaming chunking. In both cases we derive the file
    // hash from the same snapshot bytes that will be uploaded.
    let file_meta = std::fs::metadata(local_path)
        .with_context(|| format!("stat for chunking: {}", local_path.display()))?;
    let use_streaming = file_meta.len() >= tcfs_chunks::STREAMING_THRESHOLD;
    let prepare_started = std::time::Instant::now();
    debug!(
        path = %local_path.display(),
        bytes = file_meta.len(),
        streaming = use_streaming,
        "preparing upload snapshot"
    );
    let snapshot = prepare_upload_snapshot(local_path, use_streaming)?;
    let snapshot_chunks = match &snapshot.source {
        UploadSourceSnapshot::InMemory(_) => 0,
        UploadSourceSnapshot::Streaming(chunks) => chunks.len(),
    };
    debug!(
        path = %local_path.display(),
        bytes = snapshot.file_size,
        streaming = use_streaming,
        chunks = snapshot_chunks,
        elapsed_ms = prepare_started.elapsed().as_millis(),
        "prepared upload snapshot"
    );
    let file_size = snapshot.file_size;
    let file_hash_hex = snapshot.file_hash_hex.clone();
    let verify_started = std::time::Instant::now();
    ensure_source_matches_snapshot(local_path, &snapshot, "upload preparation")?;
    debug!(
        path = %local_path.display(),
        bytes = file_size,
        streaming = use_streaming,
        elapsed_ms = verify_started.elapsed().as_millis(),
        "verified upload snapshot"
    );

    // Direct uploads stay content-addressed for compatibility. Path-indexed
    // uploads may carry path-specific metadata (mode, mtime, rel_path, vclock),
    // so their final manifest object id is derived after the manifest is built.
    let mut remote_manifest = format!(
        "{}/manifests/{file_hash_hex}",
        remote_prefix.trim_end_matches('/')
    );
    let assume_fresh_prefix = runtime.assume_fresh_prefix;
    let indexed_publish_baseline = if !assume_fresh_prefix {
        if let Some(rp) = rel_path {
            let index_key = format!("{remote_prefix}/index/{rp}");
            let manifest_prefix = manifest_path_prefix(remote_prefix);
            Some(
                resolve_visible_index_entry_for_update(op, &index_key, &manifest_prefix)
                    .await
                    .with_context(|| {
                        format!("resolving remote index before upload: {index_key}")
                    })?,
            )
        } else {
            None
        }
    } else {
        None
    };

    // Get the local vclock from state (or start fresh)
    let mut local_vclock = tracked_state
        .as_ref()
        .map(|s| s.vclock.clone())
        .unwrap_or_default();
    let local_edit_inferred = !device_id.is_empty() && tracked_state.is_some();
    if local_edit_inferred {
        // The file changed relative to tracked local state, so model the
        // pending upload as a descendant of the last synced version before
        // comparing against the current rel_path index entry.
        local_vclock.tick(device_id);
    }

    // Conflict detection: find the current remote manifest for this rel_path.
    // First try the index entry (covers different-content conflicts), then
    // fall back to checking the same-hash manifest path.
    let mut outcome = None;
    let mut remote_vclock_snapshot: Option<crate::conflict::VectorClock> = None;
    if !device_id.is_empty() && !assume_fresh_prefix {
        let (remote_manifest_obj, remote_manifest_path, remote_index_manifest_hash) =
            if let Some(rp) = rel_path {
                // Look up the index entry to find what manifest is currently stored
                let prefix = validated_remote_prefix(remote_prefix)?;
                validated_index_rel_path(rp)?;
                let manifest_prefix = manifest_path_prefix(prefix);
                let baseline = indexed_publish_baseline
                    .as_ref()
                    .context("indexed upload is missing its prebound publication baseline")?;
                let idx_entry = baseline.current().cloned();
                let idx_manifest_hash = idx_entry.as_ref().map(|e| e.manifest_hash.clone());
                let idx_manifest = idx_entry
                    .as_ref()
                    .map(|entry| manifest_key(&manifest_prefix, &entry.manifest_hash));
                // Read and validate the typed manifest pointed to by the index
                // entry. Kind transitions participate in ordinary vector-clock
                // classification instead of being rejected before causality is
                // considered.
                let manifest_obj = if let (Some(entry), Some(manifest_path)) =
                    (idx_entry.as_ref(), idx_manifest.as_ref())
                {
                    let remote_bytes = op.read(manifest_path).await.with_context(|| {
                        format!("reading remote manifest before upload: {manifest_path}")
                    })?;
                    let remote_bytes = remote_bytes.to_bytes();
                    validate_remote_entry(entry, "remote manifest before upload")?;
                    let parsed = parse_manifest_for_index_entry(
                        &remote_bytes,
                        entry.kind,
                        entry.symlink_target.as_deref(),
                        rp,
                    )
                    .with_context(|| {
                        format!("parsing remote manifest before upload: {manifest_path}")
                    })?;
                    validate_parsed_indexed_manifest_binding(
                        &remote_bytes,
                        &entry.manifest_hash,
                        &parsed,
                    )
                    .with_context(|| {
                        format!("validating remote manifest before upload: {manifest_path}")
                    })?;
                    validate_parsed_indexed_manifest_metadata(
                        &parsed,
                        entry.size,
                        entry.chunks,
                        rp,
                    )
                    .with_context(|| {
                        format!(
                            "validating remote manifest metadata before upload: {manifest_path}"
                        )
                    })?;
                    Some(ManifestCausality::from_parsed(parsed))
                } else {
                    None
                };
                (manifest_obj, idx_manifest, idx_manifest_hash)
            } else {
                // No rel_path — fall back to checking the same-hash manifest
                let manifest_path = remote_manifest.clone();
                let manifest_exists = op.exists(&manifest_path).await.with_context(|| {
                    format!("checking remote manifest before upload: {manifest_path}")
                })?;
                let manifest_obj = if manifest_exists {
                    let remote_bytes = op.read(&remote_manifest).await.with_context(|| {
                        format!("reading remote manifest before upload: {remote_manifest}")
                    })?;
                    Some(ManifestCausality::regular(
                        parse_direct_manifest_for_content_key(
                            &remote_bytes.to_bytes(),
                            &file_hash_hex,
                            file_size,
                        )
                        .with_context(|| {
                            format!(
                                "validating remote direct manifest before upload: {remote_manifest}"
                            )
                        })?,
                    ))
                } else {
                    None
                };
                (manifest_obj, Some(manifest_path), None)
            };

        // Capture remote vclock for deferred merge (Issue #183)
        remote_vclock_snapshot = remote_manifest_obj.as_ref().map(|m| m.vclock.clone());
        let current_remote_manifest_path = remote_manifest_path;

        if let Some(remote_manifest_obj) = remote_manifest_obj {
            let local_hash = &file_hash_hex;
            let remote_hash = &remote_manifest_obj.content_hash;
            let rp = rel_path.unwrap_or("");

            let mut sync_outcome = compare_typed_content_clocks(
                &local_vclock,
                &remote_manifest_obj.vclock,
                RemoteEntryKind::RegularFile,
                local_hash,
                &remote_manifest_obj.logical_identity,
                remote_hash,
                rp,
                device_id,
                &remote_manifest_obj.written_by,
            );

            // `.git` fast-forward push (reclassified by the reconcile plan):
            // the plan proved the local git tip is a strict descendant of the
            // remote tip, so pushing local cannot lose remote history even
            // though the vector clocks are concurrent (each device ticked its
            // own component — the bidirectional roam field case). Dominate the
            // remote clock — merge(local, remote) + tick(device) — and proceed
            // as LocalNewer instead of veto-skipping (which would replan the
            // same push forever). Guard: only while the remote index entry is
            // still exactly the manifest the ancestry proof was computed
            // against; if the remote moved since planning, keep the ordinary
            // conflict veto (fail closed, re-plan next cycle).
            if let (SyncOutcome::Conflict(_), Some(expected)) =
                (&sync_outcome, git_ff_expected_manifest)
            {
                if remote_index_manifest_hash.as_deref() == Some(expected) {
                    local_vclock.merge(&remote_manifest_obj.vclock);
                    local_vclock.tick(device_id);
                    info!(
                        path = %local_path.display(),
                        "git ff: ancestry-proven push dominating concurrent remote clock"
                    );
                    sync_outcome = SyncOutcome::LocalNewer;
                } else {
                    warn!(
                        path = %local_path.display(),
                        "git ff: remote manifest changed since plan; keeping conflict veto"
                    );
                }
            }

            match &sync_outcome {
                SyncOutcome::RemoteNewer => {
                    ensure_source_matches_snapshot(local_path, &snapshot, "remote-newer skip")?;
                    let remote_manifest_path = current_remote_manifest_path
                        .clone()
                        .unwrap_or_else(|| remote_manifest.clone());
                    return Ok((
                        UploadResult {
                            path: local_path.to_path_buf(),
                            remote_path: remote_manifest_path,
                            hash: file_hash_hex,
                            chunks: 0,
                            bytes: file_size,
                            vclock: remote_manifest_obj.vclock.clone(),
                            skipped: true,
                            outcome: Some(sync_outcome),
                        },
                        None,
                    ));
                }
                SyncOutcome::Conflict(ref conflict_info) => {
                    ensure_source_matches_snapshot(local_path, &snapshot, "conflict skip")?;
                    let remote_manifest_path = current_remote_manifest_path
                        .clone()
                        .unwrap_or_else(|| remote_manifest.clone());
                    let mut conflict_info = conflict_info.clone();
                    conflict_info.remote_manifest_key = Some(remote_manifest_path.clone());
                    // Record local state with conflict info so `tcfs resolve` can find it
                    let mut sync_state = make_sync_state_full(
                        local_path,
                        file_hash_hex.clone(),
                        0,
                        remote_manifest_path.clone(),
                        local_vclock,
                        device_id.to_string(),
                    )?;
                    sync_state.conflict = Some(conflict_info.clone());
                    sync_state.status = FileSyncStatus::Conflict;
                    return Ok((
                        UploadResult {
                            path: local_path.to_path_buf(),
                            remote_path: remote_manifest_path,
                            hash: file_hash_hex,
                            chunks: 0,
                            bytes: file_size,
                            vclock: sync_state.vclock.clone(),
                            skipped: true,
                            outcome: Some(SyncOutcome::Conflict(conflict_info)),
                        },
                        Some(sync_state),
                    ));
                }
                SyncOutcome::UpToDate => {
                    ensure_source_matches_snapshot(local_path, &snapshot, "up-to-date skip")?;
                    let remote_manifest_path = current_remote_manifest_path
                        .clone()
                        .unwrap_or_else(|| remote_manifest.clone());
                    // Content dedup — already up to date
                    let sync_state = make_sync_state_full(
                        local_path,
                        file_hash_hex.clone(),
                        0,
                        remote_manifest_path.clone(),
                        local_vclock,
                        device_id.to_string(),
                    )?;
                    return Ok((
                        UploadResult {
                            path: local_path.to_path_buf(),
                            remote_path: remote_manifest_path,
                            hash: file_hash_hex,
                            chunks: 0,
                            bytes: file_size,
                            vclock: sync_state.vclock.clone(),
                            skipped: true,
                            outcome: Some(sync_outcome),
                        },
                        Some(sync_state),
                    ));
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
    let direct_dedup_candidate =
        !assume_fresh_prefix && outcome.is_none() && rel_path.is_none() && device_id.is_empty();
    let direct_manifest_exists = if direct_dedup_candidate {
        op.exists(&remote_manifest)
            .await
            .with_context(|| format!("checking manifest for dedup: {remote_manifest}"))?
    } else {
        false
    };
    if direct_dedup_candidate && direct_manifest_exists {
        ensure_source_matches_snapshot(local_path, &snapshot, "dedup skip")?;
        debug!(hash = %file_hash_hex, "dedup: manifest already exists");
        let existing_manifest = op
            .read(&remote_manifest)
            .await
            .with_context(|| format!("reading existing manifest for dedup: {remote_manifest}"))?;
        let existing_manifest = parse_direct_manifest_for_content_key(
            &existing_manifest.to_bytes(),
            &file_hash_hex,
            file_size,
        )
        .with_context(|| format!("validating existing manifest for dedup: {remote_manifest}"))?;
        let chunk_count = existing_manifest.chunk_hashes().len();

        let remote_path = remote_manifest.clone();
        let sync_state = make_sync_state_full(
            local_path,
            file_hash_hex.clone(),
            chunk_count,
            remote_path.clone(),
            local_vclock,
            device_id.to_string(),
        )?;
        return Ok((
            UploadResult {
                path: local_path.to_path_buf(),
                remote_path,
                hash: file_hash_hex,
                chunks: chunk_count,
                bytes: file_size,
                vclock: sync_state.vclock.clone(),
                skipped: false,
                outcome: None,
            },
            Some(sync_state),
        ));
    }

    // Tick local vclock before writing
    if !device_id.is_empty() && !local_edit_inferred {
        local_vclock.tick(device_id);
    }

    // Upload the prepared snapshot bytes after conflict/dedup checks.
    let mut chunk_hashes = Vec::new();
    let mut bytes_uploaded = 0u64;
    let num_chunks;
    let chunk_upload_concurrency = upload_chunk_concurrency();
    let progress_every_chunks = upload_progress_every_chunks();
    let progress_heartbeat = upload_progress_heartbeat();
    let chunk_write_timeout = upload_chunk_timeout();
    let chunk_write_timeout_secs = chunk_write_timeout
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let upload_started = Instant::now();

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
        // ── Streaming path: prepared snapshot chunks ─────────
        debug!(
            path = %local_path.display(),
            size = file_size,
            chunk_upload_concurrency,
            chunk_cas_refresh = true,
            chunk_write_timeout_secs,
            "using streaming chunker"
        );
        let UploadSourceSnapshot::Streaming(streaming_chunks) = &snapshot.source else {
            unreachable!("streaming upload expected streaming snapshot")
        };

        num_chunks = streaming_chunks.len();
        chunk_hashes.reserve(num_chunks);
        let mut pending_uploads = JoinSet::new();
        let mut completed_chunks = 0usize;
        let mut snapshot_file = std::fs::File::open(local_path).with_context(|| {
            format!(
                "opening streaming upload source after snapshot: {}",
                local_path.display()
            )
        })?;

        for (i, chunk) in streaming_chunks.iter().enumerate() {
            let chunk_data =
                read_verified_snapshot_chunk_from(&mut snapshot_file, local_path, chunk, i)?;

            #[cfg(feature = "crypto")]
            let (upload_data, chunk_hash_hex) =
                if let (Some(ref fk), Some(ref fid)) = (&file_key, &file_id) {
                    let ciphertext = tcfs_crypto::encrypt_chunk(fk, i as u64, fid, &chunk_data)
                        .with_context(|| format!("encrypting chunk {i}"))?;
                    let ct_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
                    (ciphertext, ct_hash)
                } else {
                    let h = tcfs_chunks::hash_to_hex(&chunk.hash);
                    (chunk_data.clone(), h)
                };

            #[cfg(not(feature = "crypto"))]
            let (upload_data, chunk_hash_hex) = {
                let h = tcfs_chunks::hash_to_hex(&chunk.hash);
                (chunk_data.clone(), h)
            };

            let chunk_key = format!("{remote_prefix}/chunks/{chunk_hash_hex}");
            chunk_hashes.push(chunk_hash_hex);

            pending_uploads.spawn(maybe_upload_chunk(
                op.clone(),
                chunk_key,
                upload_data,
                i,
                chunk.length as u64,
                assume_fresh_prefix,
                chunk_write_timeout,
            ));

            while pending_uploads.len() >= chunk_upload_concurrency {
                bytes_uploaded += await_next_chunk_upload_with_heartbeat(
                    &mut pending_uploads,
                    ChunkUploadWaitContext {
                        local_path,
                        upload_started,
                        completed_chunks,
                        num_chunks,
                        uploaded_bytes: bytes_uploaded,
                        streaming: true,
                        chunk_upload_concurrency,
                        heartbeat: progress_heartbeat,
                    },
                )
                .await?;
                completed_chunks += 1;
                if let Some(cb) = progress {
                    cb(
                        completed_chunks as u64,
                        num_chunks as u64,
                        &format!("chunk {completed_chunks}/{num_chunks}"),
                    );
                }
                record_chunk_upload_progress(
                    local_path,
                    completed_chunks,
                    num_chunks,
                    bytes_uploaded,
                    true,
                    progress_every_chunks,
                );
            }
        }

        while !pending_uploads.is_empty() {
            bytes_uploaded += await_next_chunk_upload_with_heartbeat(
                &mut pending_uploads,
                ChunkUploadWaitContext {
                    local_path,
                    upload_started,
                    completed_chunks,
                    num_chunks,
                    uploaded_bytes: bytes_uploaded,
                    streaming: true,
                    chunk_upload_concurrency,
                    heartbeat: progress_heartbeat,
                },
            )
            .await?;
            completed_chunks += 1;
            if let Some(cb) = progress {
                cb(
                    completed_chunks as u64,
                    num_chunks as u64,
                    &format!("chunk {completed_chunks}/{num_chunks}"),
                );
            }
            record_chunk_upload_progress(
                local_path,
                completed_chunks,
                num_chunks,
                bytes_uploaded,
                true,
                progress_every_chunks,
            );
        }
    } else {
        // ── In-memory path: prepared snapshot bytes ───────────────
        let UploadSourceSnapshot::InMemory(data) = &snapshot.source else {
            unreachable!("in-memory upload expected in-memory snapshot")
        };
        let chunks = tcfs_chunks::chunk_data(data, tcfs_chunks::ChunkSizes::for_path(local_path));

        num_chunks = chunks.len();
        chunk_hashes.reserve(num_chunks);
        let mut pending_uploads = JoinSet::new();
        let mut completed_chunks = 0usize;

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
            chunk_hashes.push(chunk_hash_hex);

            pending_uploads.spawn(maybe_upload_chunk(
                op.clone(),
                chunk_key,
                upload_data,
                i,
                chunk.length as u64,
                assume_fresh_prefix,
                chunk_write_timeout,
            ));

            while pending_uploads.len() >= chunk_upload_concurrency {
                bytes_uploaded += await_next_chunk_upload_with_heartbeat(
                    &mut pending_uploads,
                    ChunkUploadWaitContext {
                        local_path,
                        upload_started,
                        completed_chunks,
                        num_chunks,
                        uploaded_bytes: bytes_uploaded,
                        streaming: false,
                        chunk_upload_concurrency,
                        heartbeat: progress_heartbeat,
                    },
                )
                .await?;
                completed_chunks += 1;
                if let Some(cb) = progress {
                    cb(
                        completed_chunks as u64,
                        num_chunks as u64,
                        &format!("chunk {completed_chunks}/{num_chunks}"),
                    );
                }
                record_chunk_upload_progress(
                    local_path,
                    completed_chunks,
                    num_chunks,
                    bytes_uploaded,
                    false,
                    progress_every_chunks,
                );
            }
        }

        while !pending_uploads.is_empty() {
            bytes_uploaded += await_next_chunk_upload_with_heartbeat(
                &mut pending_uploads,
                ChunkUploadWaitContext {
                    local_path,
                    upload_started,
                    completed_chunks,
                    num_chunks,
                    uploaded_bytes: bytes_uploaded,
                    streaming: false,
                    chunk_upload_concurrency,
                    heartbeat: progress_heartbeat,
                },
            )
            .await?;
            completed_chunks += 1;
            if let Some(cb) = progress {
                cb(
                    completed_chunks as u64,
                    num_chunks as u64,
                    &format!("chunk {completed_chunks}/{num_chunks}"),
                );
            }
            record_chunk_upload_progress(
                local_path,
                completed_chunks,
                num_chunks,
                bytes_uploaded,
                false,
                progress_every_chunks,
            );
        }
    }

    ensure_source_matches_snapshot(local_path, &snapshot, "manifest publish")?;

    // Wrap the file key for the manifest, branching on the wrap mode (TIN-1417):
    //
    // - `Master`  : master-only wrap (`encrypted_file_key`). Byte-identical to
    //               the legacy default. Manifest stays version 2.
    // - `Dual`    : EXPAND/transitional. Emit BOTH the master wrap (rollback +
    //               master/old-binary readers) AND per-device wraps. Version 2
    //               (back-compatible by construction).
    // - `PerDevice`: CONTRACT. Emit ONLY per-device wraps and DROP the master
    //               wrap (true revocation). Bumps the manifest to version 3 so
    //               pre-per-device binaries fail CLOSED.
    //
    // The roll-call gate (daemon/CLI/FP `build_encryption_context`) guarantees
    // `PerDevice`/`Dual` are only handed here with a real recipient set; we
    // still fail CLOSED below if `Dual`/`PerDevice` arrives with no recipients
    // rather than silently writing an unreadable or master-only manifest.
    #[cfg(feature = "crypto")]
    let wrap_age_recipients = |ctx: &EncryptionContext,
                               fk: &tcfs_crypto::FileKey|
     -> Result<Vec<crate::manifest::WrappedFileKey>> {
        Ok(
            tcfs_crypto::wrap_file_key_for_age_recipients(fk, &ctx.device_recipients)
                .context("wrapping file key for device recipients")?
                .into_iter()
                .map(|w| crate::manifest::WrappedFileKey {
                    recipient_device_id: w.recipient_device_id,
                    recipient: w.recipient,
                    algorithm: w.algorithm,
                    wrapped_key: w.wrapped_key,
                })
                .collect(),
        )
    };

    #[cfg(feature = "crypto")]
    let (encrypted_file_key, wrapped_file_keys, manifest_version) =
        if let (Some(ctx), Some(ref fk)) = (encryption, &file_key) {
            let master_wrap = |fk: &tcfs_crypto::FileKey| -> Result<String> {
                let wrapped =
                    tcfs_crypto::wrap_key(&ctx.master_key, fk).context("wrapping file key")?;
                Ok(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &wrapped,
                ))
            };
            match ctx.wrap_mode {
                WrapMode::Master => (Some(master_wrap(fk)?), Vec::new(), 2u32),
                WrapMode::Dual => {
                    if ctx.device_recipients.is_empty() {
                        anyhow::bail!(
                        "wrap_mode=Dual requires per-device recipients but none are configured; \
                             refusing to write (would silently degrade to master-only)"
                    );
                    }
                    (Some(master_wrap(fk)?), wrap_age_recipients(ctx, fk)?, 2u32)
                }
                WrapMode::PerDevice => {
                    if ctx.device_recipients.is_empty() {
                        // Fail CLOSED: a PerDevice write with no recipients would
                        // produce a keyless v3 manifest that nobody can read.
                        anyhow::bail!(
                            "wrap_mode=PerDevice requires per-device recipients but none are \
                             configured; refusing to drop the master wrap (fail-closed)"
                        );
                    }
                    (None, wrap_age_recipients(ctx, fk)?, 3u32)
                }
            }
        } else {
            (None, Vec::new(), 2u32)
        };

    #[cfg(not(feature = "crypto"))]
    let encrypted_file_key: Option<String> = None;
    #[cfg(not(feature = "crypto"))]
    let wrapped_file_keys: Vec<crate::manifest::WrappedFileKey> = Vec::new();
    #[cfg(not(feature = "crypto"))]
    let manifest_version: u32 = 2;

    // Capture Unix file permissions and the source mtime for cross-device
    // preservation, both from the SAME metadata read so they describe one stat
    // of the file (no TOCTOU drift between the two). The mtime keeps a restored
    // tree's timestamps intact so `git status` does not report spurious dirty
    // (TIN-1620 T13-Z).
    let source_metadata = std::fs::metadata(local_path).ok();
    #[cfg(unix)]
    let file_mode = {
        use std::os::unix::fs::PermissionsExt;
        source_metadata.as_ref().map(|m| m.permissions().mode())
    };
    #[cfg(not(unix))]
    let file_mode: Option<u32> = None;
    let file_mtime: Option<(i64, u32)> = source_metadata
        .as_ref()
        .and_then(|m| m.modified().ok())
        .map(systemtime_to_unix_parts);

    // Build and upload the manifest. Version is 2 for Master/Dual and 3 for
    // PerDevice (see the wrap-mode branch above) so pre-per-device binaries fail
    // CLOSED on a master-wrap-less v3 manifest instead of misreading it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let manifest = SyncManifest {
        version: manifest_version,
        file_hash: file_hash_hex.clone(),
        file_size,
        chunks: chunk_hashes,
        vclock: local_vclock.clone(),
        written_by: device_id.to_string(),
        written_at: now,
        rel_path: rel_path.map(|s| s.to_string()),
        mode: file_mode,
        mtime: file_mtime,
        encrypted_file_key,
        wrapped_file_keys,
    };

    let manifest_bytes = manifest.to_bytes()?;
    if let Some(rp) = rel_path {
        let manifest_hash = manifest_object_id(&manifest_bytes);
        remote_manifest = manifest_key(&manifest_path_prefix(remote_prefix), &manifest_hash);
        before_manifest_publish().await?;
        publish_manifest_for_rel_path_with_mode(
            op,
            remote_prefix,
            rp,
            manifest_bytes,
            RemoteIndexEntry::new(manifest_hash, file_size, num_chunks),
            assume_fresh_prefix,
            indexed_publish_baseline,
        )
        .await?;
    } else {
        // Legacy direct manifests are keyed by their embedded file hash rather
        // than by serialized bytes. Preserve that compatibility namespace, but
        // never overwrite a raced/collided object: creation is absent-only and
        // an existing object is accepted only when its bytes are exact.
        before_manifest_publish().await?;
        write_immutable_manifest_object_if_absent(
            op,
            remote_prefix,
            &remote_manifest,
            &manifest_bytes,
        )
        .await
        .with_context(|| format!("publishing direct immutable manifest: {remote_manifest}"))?;
    }

    // Deferred vclock merge: only merge remote vclock after successful upload
    // to prevent stale vclocks if the upload had failed.
    if matches!(outcome, Some(SyncOutcome::LocalNewer)) {
        if let Some(ref remote_vc) = remote_vclock_snapshot {
            local_vclock.merge(remote_vc);
        }
    }

    let upload_elapsed = upload_started.elapsed();
    info!(
        path = %local_path.display(),
        hash = %file_hash_hex,
        chunks = num_chunks,
        bytes = file_size,
        uploaded_bytes = bytes_uploaded,
        upload_elapsed_ms = upload_elapsed.as_millis() as u64,
        upload_chunks_per_sec = rate_per_sec(num_chunks as u64, upload_elapsed),
        upload_bytes_per_sec = rate_per_sec(bytes_uploaded, upload_elapsed),
        streaming = use_streaming,
        fresh_prefix_publish = assume_fresh_prefix,
        remote_conflict_check = !assume_fresh_prefix && !device_id.is_empty(),
        chunk_upload_concurrency,
        chunk_cas_refresh = true,
        chunk_write_timeout_secs,
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
    Ok((
        UploadResult {
            path: local_path.to_path_buf(),
            remote_path: remote_manifest,
            hash: file_hash_hex,
            chunks: num_chunks,
            bytes: file_size,
            vclock: sync_state.vclock.clone(),
            skipped: false,
            outcome,
        },
        Some(sync_state),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymlinkUploadSnapshot {
    target: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn capture_symlink_upload_snapshot(local_path: &Path) -> Result<SymlinkUploadSnapshot> {
    let before = std::fs::symlink_metadata(local_path)
        .with_context(|| format!("inspecting symlink upload source: {}", local_path.display()))?;
    anyhow::ensure!(
        before.file_type().is_symlink(),
        "symlink upload source is not a symbolic link: {}",
        local_path.display()
    );
    let target = read_symlink_target_text(local_path)?;
    let after = std::fs::symlink_metadata(local_path)
        .with_context(|| format!("rechecking symlink upload source: {}", local_path.display()))?;
    anyhow::ensure!(
        after.file_type().is_symlink(),
        "symlink upload source changed type while reading target: {}",
        local_path.display()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            before.dev() == after.dev() && before.ino() == after.ino(),
            "symlink upload source changed identity while reading target: {}",
            local_path.display()
        );
        Ok(SymlinkUploadSnapshot {
            target,
            device: before.dev(),
            inode: before.ino(),
        })
    }

    #[cfg(not(unix))]
    Ok(SymlinkUploadSnapshot { target })
}

fn ensure_symlink_upload_snapshot_matches(
    local_path: &Path,
    expected: &SymlinkUploadSnapshot,
) -> Result<()> {
    let current = capture_symlink_upload_snapshot(local_path)?;
    anyhow::ensure!(
        &current == expected,
        "symlink upload source changed before manifest publication: {}",
        local_path.display()
    );
    Ok(())
}

/// Upload a symbolic link as a first-class symlink entry.
pub async fn upload_symlink_with_device(
    op: &Operator,
    local_path: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    device_id: &str,
    rel_path: &str,
) -> Result<UploadResult> {
    let remote_prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(rel_path)?;
    ensure_publish_storage_semantics(op, remote_prefix).await?;
    let source_snapshot = capture_symlink_upload_snapshot(local_path)?;
    let target = source_snapshot.target.clone();

    // (TIN-1737) Self-defense on the upload side: refuse to *publish* a symlink
    // whose target is absolute, climbs out of its own directory, or resolves
    // onto the security deny-set. The egress collector already screens targets,
    // but this public API can be called directly, so we fail closed here too.
    if let Err(reason) = validate_restored_symlink_target(local_path, &target) {
        warn!(
            local = %local_path.display(),
            target = ?target,
            reason = %reason,
            "refusing to upload symlink: fail-closed egress guard"
        );
        anyhow::bail!(
            "refusing to upload symlink {}: {reason}",
            local_path.display()
        );
    }

    let symlink_hash = symlink_manifest_hash(&target);
    let tracked_state = state.get(local_path).cloned();
    let mut vclock = tracked_state
        .as_ref()
        .map(|entry| entry.vclock.clone())
        .unwrap_or_default();
    let local_changed = tracked_state
        .as_ref()
        .is_none_or(|entry| entry.blake3 != symlink_hash);
    if local_changed && !device_id.is_empty() {
        vclock.tick(device_id);
    }

    let assume_fresh_prefix = upload_assume_fresh_prefix();
    let prefix = remote_prefix;
    let manifest_prefix = manifest_path_prefix(prefix);
    let index_key = format!("{prefix}/index/{rel_path}");
    let mut outcome = None;
    let baseline = if assume_fresh_prefix {
        None
    } else {
        let baseline = resolve_visible_index_entry_for_update(op, &index_key, &manifest_prefix)
            .await
            .with_context(|| {
                format!("resolving remote symlink index before upload: {index_key}")
            })?;

        if let Some(remote_entry) = baseline.current().cloned() {
            let remote_manifest = manifest_key(&manifest_prefix, &remote_entry.manifest_hash);
            let remote_bytes = op
                .read(&remote_manifest)
                .await
                .with_context(|| {
                    format!("reading remote manifest before symlink upload: {remote_manifest}")
                })?
                .to_vec();
            let parsed = parse_manifest_for_index_entry(
                &remote_bytes,
                remote_entry.kind,
                remote_entry.symlink_target.as_deref(),
                rel_path,
            )?;
            validate_parsed_indexed_manifest_binding(
                &remote_bytes,
                &remote_entry.manifest_hash,
                &parsed,
            )?;
            validate_parsed_indexed_manifest_metadata(
                &parsed,
                remote_entry.size,
                remote_entry.chunks,
                rel_path,
            )?;
            let remote_causality = ManifestCausality::from_parsed(parsed);
            let remote_outcome = compare_typed_content_clocks(
                &vclock,
                &remote_causality.vclock,
                RemoteEntryKind::Symlink,
                &symlink_hash,
                &remote_causality.logical_identity,
                &remote_causality.content_hash,
                rel_path,
                device_id,
                &remote_causality.written_by,
            );

            match remote_outcome {
                SyncOutcome::RemoteNewer => {
                    ensure_symlink_upload_snapshot_matches(local_path, &source_snapshot)?;
                    return Ok(UploadResult {
                        path: local_path.to_path_buf(),
                        remote_path: remote_manifest,
                        hash: symlink_hash,
                        chunks: 0,
                        bytes: target.len() as u64,
                        vclock: remote_causality.vclock,
                        skipped: true,
                        outcome: Some(SyncOutcome::RemoteNewer),
                    });
                }
                SyncOutcome::Conflict(mut conflict) => {
                    ensure_symlink_upload_snapshot_matches(local_path, &source_snapshot)?;
                    conflict.remote_manifest_key = Some(remote_manifest.clone());
                    let mut sync_state = make_symlink_sync_state(
                        local_path,
                        symlink_hash.clone(),
                        remote_manifest.clone(),
                        vclock.clone(),
                        device_id.to_string(),
                        target.len() as u64,
                    )?;
                    sync_state.conflict = Some(conflict.clone());
                    sync_state.status = FileSyncStatus::Conflict;
                    state.set(local_path, sync_state);
                    return Ok(UploadResult {
                        path: local_path.to_path_buf(),
                        remote_path: remote_manifest,
                        hash: symlink_hash,
                        chunks: 0,
                        bytes: target.len() as u64,
                        vclock,
                        skipped: true,
                        outcome: Some(SyncOutcome::Conflict(conflict)),
                    });
                }
                SyncOutcome::UpToDate => {
                    ensure_symlink_upload_snapshot_matches(local_path, &source_snapshot)?;
                    vclock.merge(&remote_causality.vclock);
                    let sync_state = make_symlink_sync_state(
                        local_path,
                        symlink_hash.clone(),
                        remote_manifest.clone(),
                        vclock.clone(),
                        device_id.to_string(),
                        target.len() as u64,
                    )?;
                    state.set(local_path, sync_state);
                    return Ok(UploadResult {
                        path: local_path.to_path_buf(),
                        remote_path: remote_manifest,
                        hash: symlink_hash,
                        chunks: 0,
                        bytes: target.len() as u64,
                        vclock,
                        skipped: true,
                        outcome: Some(SyncOutcome::UpToDate),
                    });
                }
                SyncOutcome::LocalNewer => {
                    vclock.merge(&remote_causality.vclock);
                    outcome = Some(SyncOutcome::LocalNewer);
                }
            }
        } else if !local_changed && !device_id.is_empty() {
            // Republishing a tracked link after its remote index disappeared is
            // a new causal event even when the local target itself is unchanged.
            vclock.tick(device_id);
        }

        Some(baseline)
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let manifest = SymlinkManifest::new(
        target.clone(),
        vclock.clone(),
        device_id.to_string(),
        now,
        Some(rel_path.to_string()),
    );
    let manifest_bytes = manifest.to_bytes()?;
    let manifest_hash = manifest_object_id(&manifest_bytes);
    let remote_manifest = manifest_key(&manifest_path_prefix(remote_prefix), &manifest_hash);
    ensure_symlink_upload_snapshot_matches(local_path, &source_snapshot)?;
    publish_manifest_for_rel_path_with_mode(
        op,
        prefix,
        rel_path,
        manifest_bytes,
        RemoteIndexEntry::new_symlink(manifest_hash, target.clone()),
        assume_fresh_prefix,
        baseline,
    )
    .await?;

    let sync_state = make_symlink_sync_state(
        local_path,
        symlink_hash.clone(),
        remote_manifest.clone(),
        manifest.vclock.clone(),
        device_id.to_string(),
        target.len() as u64,
    )?;
    let result_vclock = sync_state.vclock.clone();
    state.set(local_path, sync_state);

    info!(
        path = %local_path.display(),
        target = %target,
        hash = %symlink_hash,
        fresh_prefix_publish = assume_fresh_prefix,
        "uploaded symlink"
    );

    Ok(UploadResult {
        path: local_path.to_path_buf(),
        remote_path: remote_manifest,
        hash: symlink_hash,
        chunks: 0,
        bytes: target.len() as u64,
        vclock: result_vclock,
        skipped: false,
        outcome,
    })
}

pub(crate) fn read_symlink_target_text(path: &Path) -> Result<String> {
    let target = std::fs::read_link(path)
        .with_context(|| format!("reading symlink target: {}", path.display()))?;
    target
        .to_str()
        .map(|s| s.to_string())
        .with_context(|| format!("symlink target is not valid UTF-8: {}", path.display()))
}

/// Stable identity hash for a symlink, keyed only on its target text.
///
/// This is the single source of truth shared by the symlink push path
/// (`upload_symlink_with_device`), the pull path, and the reconcile compare
/// path so that all three agree on when two symlinks are "the same".
pub(crate) fn symlink_manifest_hash(target: &str) -> String {
    let mut data = b"tcfs-symlink-v1\0".to_vec();
    data.extend_from_slice(target.as_bytes());
    tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data))
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

/// Legacy direct-manifest download with device identity, vector-clock merge,
/// and optional decryption.
///
/// This API intentionally preserves manifest auto-detection for callers that do
/// not resolve through the path index. Reconcile must use
/// [`download_file_with_device_checked`] so remote index kind and target
/// metadata are bound before any local mutation.
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
    download_file_with_device_inner(
        op,
        remote_manifest,
        local_path,
        remote_prefix,
        progress,
        _device_id,
        state,
        encryption,
        None,
    )
    .await
}

/// The exact local object identity a guarded indexed hydration is allowed to
/// replace.
///
/// `Tracked` binds both the object type and its canonical content identity:
/// regular files use their BLAKE3 content hash, while symlinks use
/// [`symlink_manifest_hash`] over the link target text. Directories and other
/// filesystem object types are never valid replacement preconditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedLocalFingerprint {
    Absent,
    Tracked {
        kind: RemoteEntryKind,
        blake3: String,
    },
}

fn hash_open_regular_file(path: &Path) -> Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).with_context(|| {
        format!(
            "opening local file for guarded fingerprint: {}",
            path.display()
        )
    })?;
    let opened = file.metadata().with_context(|| {
        format!(
            "inspecting local file for guarded fingerprint: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        opened.is_file(),
        "guarded local path is not a regular file: {}",
        path.display()
    );

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hashing guarded local file: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    // Refuse a pathname swap that happened while the opened inode was being
    // hashed. The final replacement guard calls this immediately before rename.
    let current = std::fs::symlink_metadata(path)
        .with_context(|| format!("rechecking guarded local file: {}", path.display()))?;
    anyhow::ensure!(
        current.is_file(),
        "guarded local path changed type while hashing: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            opened.dev() == current.dev() && opened.ino() == current.ino(),
            "guarded local path changed identity while hashing: {}",
            path.display()
        );
    }

    Ok(tcfs_chunks::hash_to_hex(&hasher.finalize()))
}

/// Capture the current local identity for use as an indexed-hydration
/// precondition. The returned value is safe to compare with tracked state and
/// later pass to [`hydrate_indexed_snapshot_with_device`].
pub fn capture_local_fingerprint(path: &Path) -> Result<ExpectedLocalFingerprint> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExpectedLocalFingerprint::Absent)
        }
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("inspecting guarded local path: {}", path.display())))
        }
    };

    if metadata.file_type().is_symlink() {
        let target = read_symlink_target_text(path)?;
        let current = std::fs::symlink_metadata(path)
            .with_context(|| format!("rechecking guarded local symlink: {}", path.display()))?;
        anyhow::ensure!(
            current.file_type().is_symlink(),
            "guarded local path changed type while reading symlink: {}",
            path.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            anyhow::ensure!(
                metadata.dev() == current.dev() && metadata.ino() == current.ino(),
                "guarded local symlink changed identity while reading target: {}",
                path.display()
            );
        }
        return Ok(ExpectedLocalFingerprint::Tracked {
            kind: RemoteEntryKind::Symlink,
            blake3: symlink_manifest_hash(&target),
        });
    }

    if metadata.is_file() {
        return Ok(ExpectedLocalFingerprint::Tracked {
            kind: RemoteEntryKind::RegularFile,
            blake3: hash_open_regular_file(path)?,
        });
    }

    anyhow::bail!(
        "guarded local path has unsupported object type: {}",
        path.display()
    )
}

#[derive(Clone, Copy)]
struct ReplacementGuard<'a> {
    op: &'a Operator,
    snapshot: &'a IndexedManifestSnapshot,
    expected_local: &'a ExpectedLocalFingerprint,
}

#[derive(Clone, Copy)]
struct ExpectedDownloadEntry<'a> {
    rel_path: &'a str,
    kind: RemoteEntryKind,
    symlink_target: Option<&'a str>,
    size: u64,
    chunks: usize,
    manifest_bytes: Option<&'a [u8]>,
    replacement_guard: Option<ReplacementGuard<'a>>,
}

async fn verify_replacement_guard(guard: ReplacementGuard<'_>, local_path: &Path) -> Result<()> {
    anyhow::ensure!(
        indexed_manifest_snapshot_is_current(guard.op, guard.snapshot).await?,
        "indexed snapshot authority changed before replacing {}",
        local_path.display()
    );

    // Do the potentially slower remote authority check first, then fingerprint
    // the local pathname as the final operation immediately before rename.
    let actual = capture_local_fingerprint(local_path)?;
    anyhow::ensure!(
        &actual == guard.expected_local,
        "local fingerprint changed before indexed hydration replaced {}: expected {:?}, got {:?}",
        local_path.display(),
        guard.expected_local,
        actual
    );
    Ok(())
}

/// Download a path-indexed object only when its manifest agrees exactly with
/// the index metadata carried by the reconcile plan.
#[allow(clippy::too_many_arguments)]
pub async fn download_file_with_device_checked(
    op: &Operator,
    remote_manifest: &str,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
    device_id: &str,
    state: Option<&mut StateCache>,
    encryption: OptionalEncryption<'_>,
    expected_rel_path: &str,
    expected_kind: RemoteEntryKind,
    expected_symlink_target: Option<&str>,
    expected_size: u64,
    expected_chunks: usize,
) -> Result<DownloadResult> {
    download_file_with_device_inner(
        op,
        remote_manifest,
        local_path,
        remote_prefix,
        progress,
        device_id,
        state,
        encryption,
        Some(ExpectedDownloadEntry {
            rel_path: expected_rel_path,
            kind: expected_kind,
            symlink_target: expected_symlink_target,
            size: expected_size,
            chunks: expected_chunks,
            manifest_bytes: None,
            replacement_guard: None,
        }),
    )
    .await
}

/// Hydrate the exact immutable manifest bytes captured by
/// [`resolve_exact_indexed_manifest_snapshot`].
///
/// Before replacing the local path, the download rechecks that the exact path
/// index still selects the snapshot's full entry authority and that the local
/// object still matches `expected_local`. Both checks happen at every rename
/// boundary (regular, empty, and symlink), after all remote content has been
/// staged and verified.
#[allow(clippy::too_many_arguments)]
pub async fn hydrate_indexed_snapshot_with_device(
    op: &Operator,
    snapshot: &IndexedManifestSnapshot,
    local_path: &Path,
    progress: Option<&ProgressFn>,
    device_id: &str,
    state: Option<&mut StateCache>,
    encryption: OptionalEncryption<'_>,
    expected_local: &ExpectedLocalFingerprint,
) -> Result<DownloadResult> {
    download_file_with_device_inner(
        op,
        snapshot.manifest_path(),
        local_path,
        snapshot.remote_prefix(),
        progress,
        device_id,
        state,
        encryption,
        Some(ExpectedDownloadEntry {
            rel_path: snapshot.rel_path(),
            kind: snapshot.kind(),
            symlink_target: snapshot.symlink_target(),
            size: snapshot.size(),
            chunks: snapshot.chunks(),
            manifest_bytes: Some(snapshot.manifest_bytes()),
            replacement_guard: Some(ReplacementGuard {
                op,
                snapshot,
                expected_local,
            }),
        }),
    )
    .await
}

/// Download a resolved reference without discarding path-index authority.
/// Indexed references use the checked object/kind/target/path boundary;
/// explicit manifest references retain the legacy direct-manifest behavior.
#[allow(clippy::too_many_arguments)]
pub async fn download_resolved_file_with_device(
    op: &Operator,
    resolved: &ResolvedManifest,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
    device_id: &str,
    state: Option<&mut StateCache>,
    encryption: OptionalEncryption<'_>,
) -> Result<DownloadResult> {
    match (&resolved.rel_path, &resolved.index_entry) {
        (Some(rel_path), Some(entry)) => {
            let prefix = validated_remote_prefix(remote_prefix)?;
            let expected_manifest_path =
                manifest_key(&manifest_path_prefix(prefix), &entry.manifest_hash);
            anyhow::ensure!(
                resolved.manifest_path == expected_manifest_path,
                "resolved manifest path does not match its index authority"
            );
            download_file_with_device_checked(
                op,
                &resolved.manifest_path,
                local_path,
                remote_prefix,
                progress,
                device_id,
                state,
                encryption,
                rel_path,
                entry.kind,
                entry.symlink_target.as_deref(),
                entry.size,
                entry.chunks,
            )
            .await
        }
        (None, None) => {
            download_file_with_device(
                op,
                &resolved.manifest_path,
                local_path,
                remote_prefix,
                progress,
                device_id,
                state,
                encryption,
            )
            .await
        }
        _ => anyhow::bail!("resolved manifest carries incomplete index authority"),
    }
}

#[allow(unused_variables)]
#[allow(clippy::too_many_arguments)]
async fn download_file_with_device_inner(
    op: &Operator,
    remote_manifest: &str,
    local_path: &Path,
    remote_prefix: &str,
    progress: Option<&ProgressFn>,
    _device_id: &str,
    state: Option<&mut StateCache>,
    encryption: OptionalEncryption<'_>,
    expected: Option<ExpectedDownloadEntry<'_>>,
) -> Result<DownloadResult> {
    let expected_object_id = if let Some(expected) = expected {
        let prefix = validated_remote_prefix(remote_prefix)?;
        validated_index_rel_path(expected.rel_path)?;
        validate_relative_storage_key(remote_manifest, "remote manifest key")?;
        let expected_manifest_prefix = format!("{prefix}/manifests/");
        let object_id = remote_manifest
            .strip_prefix(&expected_manifest_prefix)
            .filter(|object_id| !object_id.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("remote manifest key is outside the selected manifest namespace")
            })?;
        crate::index_entry::validate_storage_key_component(object_id, "remote manifest object id")?;
        match (expected.kind, expected.symlink_target) {
            (RemoteEntryKind::RegularFile, Some(_)) => {
                anyhow::bail!("regular-file index entry must not carry symlink_target")
            }
            (RemoteEntryKind::Symlink, None) => {
                anyhow::bail!("symlink index entry is missing symlink_target")
            }
            (RemoteEntryKind::Symlink, Some(target))
                if target.is_empty() || target.chars().any(char::is_control) =>
            {
                anyhow::bail!("symlink index target is empty or contains a control character")
            }
            _ => {}
        }
        if expected.kind == RemoteEntryKind::Symlink
            && path_text_has_reserved_git_component(expected.rel_path)
        {
            anyhow::bail!("refusing symlink index entry at reserved Git metadata path");
        }
        Some(object_id.to_string())
    } else {
        None
    };

    // Snapshot hydration consumes the exact bytes captured during indexed
    // resolution. Legacy/checked download APIs retain their remote read here.
    let manifest_bytes = if let Some(bytes) = expected.and_then(|entry| entry.manifest_bytes) {
        bytes.to_vec()
    } else {
        // Read manifest with retry so transient storage failures don't abort
        // pull paths immediately while chunk reads already back off.
        read_with_retry(op, remote_manifest)
            .await
            .with_context(|| format!("reading manifest: {remote_manifest}"))?
    };

    let parsed = if let Some(expected) = expected {
        let parsed = parse_manifest_for_index_entry(
            &manifest_bytes,
            expected.kind,
            expected.symlink_target,
            expected.rel_path,
        )?;
        validate_parsed_indexed_manifest_binding(
            &manifest_bytes,
            expected_object_id
                .as_deref()
                .expect("checked download captured manifest object id"),
            &parsed,
        )?;
        validate_parsed_indexed_manifest_metadata(
            &parsed,
            expected.size,
            expected.chunks,
            expected.rel_path,
        )?;
        parsed
    } else if let Ok(manifest) = SymlinkManifest::from_bytes(&manifest_bytes) {
        ParsedIndexedManifest::Symlink(manifest)
    } else {
        ParsedIndexedManifest::Regular(
            SyncManifest::from_bytes(&manifest_bytes)
                .with_context(|| format!("parsing manifest: {remote_manifest}"))?,
        )
    };

    // Direct immutable-manifest references have no logical path authority until
    // their bytes are parsed. Enforce the non-configurable ingress denylist here,
    // before symlink creation, parent/temp creation, cache writes, or any other
    // local mutation, so every CLI/gRPC/direct caller shares this boundary.
    validate_fixed_manifest_ingress_path(&parsed)?;

    if let ParsedIndexedManifest::Symlink(ref manifest) = parsed {
        let reserved_git_path = expected
            .is_some_and(|expected| path_text_has_reserved_git_component(expected.rel_path))
            || filesystem_path_has_reserved_git_component(local_path);
        anyhow::ensure!(
            !reserved_git_path,
            "refusing symlink manifest at reserved Git metadata path"
        );
        create_local_symlink(
            local_path,
            &manifest.symlink_target,
            expected.and_then(|entry| entry.replacement_guard),
        )
        .await?;
        let mut sync_state_for_result = None;
        if !_device_id.is_empty() {
            let mut local_vclock = state
                .as_ref()
                .and_then(|state| state.get(local_path))
                .map(|s| s.vclock.clone())
                .unwrap_or_default();
            local_vclock.merge(&manifest.vclock);

            let sync_state = make_symlink_sync_state(
                local_path,
                symlink_manifest_hash(&manifest.symlink_target),
                remote_manifest.to_string(),
                local_vclock,
                _device_id.to_string(),
                manifest.symlink_target.len() as u64,
            )?;
            if let Some(state) = state {
                state.set(local_path, sync_state.clone());
            }
            sync_state_for_result = Some(sync_state);
        }

        info!(
            remote = %remote_manifest,
            local = %local_path.display(),
            target = %manifest.symlink_target,
            "downloaded symlink"
        );
        return Ok(DownloadResult {
            remote_path: remote_manifest.to_string(),
            local_path: local_path.to_path_buf(),
            bytes: manifest.symlink_target.len() as u64,
            sync_state: sync_state_for_result,
        });
    }

    let ParsedIndexedManifest::Regular(manifest) = parsed else {
        unreachable!("symlink manifest returned above")
    };

    // Manifest version gate (TIN-1417). v1 (legacy text) and v2 (master/dual)
    // are always readable. v3 is the per-device-only (CONTRACT) shape that DROPS
    // the master wrap; a binary without per-device read support MUST fail CLOSED
    // rather than misread a master-wrap-less manifest as keyless. Any version
    // beyond what we understand is also rejected.
    //
    // With the `crypto` feature, v3 is supported; the per-device unwrap branch
    // below independently fails CLOSED when no device identity is available. The
    // `wrapped_file_keys` shape check guards against a v3 claim with no per-device
    // wraps (which we could not decrypt). Without `crypto`, v3 is never readable.
    #[cfg(feature = "crypto")]
    {
        if manifest.version > 3 {
            anyhow::bail!(
                "manifest version {} is newer than this binary supports (max 3) for: {remote_manifest}; refusing (fail-closed)",
                manifest.version
            );
        }
        if manifest.version == 3 && manifest.wrapped_file_keys.is_empty() {
            anyhow::bail!(
                "manifest claims per-device (v3) but carries no wrapped_file_keys for: {remote_manifest}; refusing (fail-closed)"
            );
        }
    }
    #[cfg(not(feature = "crypto"))]
    {
        if manifest.version >= 3 {
            anyhow::bail!(
                "manifest version {} requires per-device crypto support not built into this binary for: {remote_manifest}; refusing (fail-closed)",
                manifest.version
            );
        }
    }

    let chunk_hashes = manifest.chunk_hashes();

    // Empty file: no chunks to fetch — write an empty file directly
    if chunk_hashes.is_empty() {
        anyhow::ensure!(
            manifest.file_size == 0,
            "empty manifest declares non-zero file_size {} for: {remote_manifest}",
            manifest.file_size
        );
        let empty_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b""));
        anyhow::ensure!(
            manifest.file_hash == empty_hash,
            "empty file integrity check failed for {remote_manifest}: expected {}, got {empty_hash}",
            manifest.file_hash
        );
        create_private_download_parents(local_path)?;

        let (tmp, tmp_file) = create_private_download_tmp(local_path)?;
        drop(tmp_file);
        if let Some(guard) = expected.and_then(|entry| entry.replacement_guard) {
            if let Err(error) = verify_replacement_guard(guard, local_path).await {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(error);
            }
        }
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

        // Restore the source mtime (TIN-1620 T13-Z) BEFORE re-stat below, so the
        // state cache and `git status` see the original timestamp, not "now".
        // Old manifests carry `mtime: None`, leaving current behavior unchanged.
        if let Some(mtime) = manifest.mtime {
            apply_manifest_mtime(local_path, mtime);
        }

        let mut sync_state_for_result = None;
        if !_device_id.is_empty() {
            let mut local_vclock = state
                .as_ref()
                .and_then(|state| state.get(local_path))
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
            if let Some(state) = state {
                state.set(local_path, sync_state.clone());
            }
            sync_state_for_result = Some(sync_state);
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
            sync_state: sync_state_for_result,
        });
    }

    // Unwrap the file key if the manifest is encrypted. Prefer per-device wraps
    // (TIN-1417): when present, the file key is unwrapped with this device's age
    // identity. Manifests carrying only the legacy master-wrapped key fall back
    // to master-key unwrap.
    //
    // Dual manifests (v2) carry BOTH `wrapped_file_keys` AND a master
    // `encrypted_file_key`. A device that has NO usable per-device wrap (no
    // encryption context, no age identity, or no stanza addressing it) MUST fall
    // back to the master wrap when one is present — this is the whole point of
    // Dual's rollback/recovery rationale, and keeps a Master-mode/no-identity
    // device able to read peer-written Dual content. PerDevice manifests (v3)
    // carry NO master wrap (`encrypted_file_key == None`); for those the
    // per-device path is the only path and we stay strictly fail-closed.
    #[cfg(feature = "crypto")]
    let unwrap_master = |wrapped_b64: &str| -> Result<tcfs_crypto::FileKey> {
        let ctx = encryption.ok_or_else(|| {
            anyhow::anyhow!(
                "manifest is encrypted but no encryption context provided for: {remote_manifest}"
            )
        })?;
        let wrapped =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, wrapped_b64)
                .context("decoding wrapped file key from manifest")?;
        tcfs_crypto::unwrap_key(&ctx.master_key, &wrapped)
            .context("unwrapping file key from manifest")
    };

    #[cfg(feature = "crypto")]
    let file_key = if !manifest.wrapped_file_keys.is_empty() {
        // Attempt the per-device unwrap first (preferred). Capture the failure
        // instead of propagating so we can fall back to the master wrap when the
        // manifest carries one (Dual/v2).
        let per_device: Result<tcfs_crypto::FileKey> = (|| {
            let ctx = encryption.ok_or_else(|| {
                anyhow::anyhow!(
                    "manifest is per-device encrypted but no encryption context provided for: {remote_manifest}"
                )
            })?;
            let identity = ctx.device_identity.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "manifest is per-device encrypted but this device has no age identity for: {remote_manifest}"
                )
            })?;
            let age_wraps: Vec<tcfs_crypto::AgeWrappedFileKey> = manifest
                .wrapped_file_keys
                .iter()
                .map(|w| tcfs_crypto::AgeWrappedFileKey {
                    recipient_device_id: w.recipient_device_id.clone(),
                    recipient: w.recipient.clone(),
                    algorithm: w.algorithm.clone(),
                    wrapped_key: w.wrapped_key.clone(),
                })
                .collect();
            tcfs_crypto::unwrap_file_key_with_age_identity(
                &age_wraps,
                &identity.secret,
                Some(&identity.device_id),
            )
            .context("unwrapping per-device file key from manifest")
        })();

        match per_device {
            Ok(fk) => Some(fk),
            Err(per_device_err) => {
                // Fall back to the master wrap ONLY when one is actually present
                // (Dual/v2). A v3 (PerDevice) manifest has no master wrap and
                // MUST stay strictly fail-closed — surface the per-device error.
                if let Some(ref wrapped_b64) = manifest.encrypted_file_key {
                    debug!(
                        remote = %remote_manifest,
                        error = %per_device_err,
                        "per-device unwrap unavailable; falling back to master wrap (Dual manifest)"
                    );
                    Some(unwrap_master(wrapped_b64)?)
                } else {
                    return Err(per_device_err);
                }
            }
        }
    } else if let Some(ref wrapped_b64) = manifest.encrypted_file_key {
        Some(unwrap_master(wrapped_b64)?)
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

    // Fetch and reassemble chunks, verifying each chunk's BLAKE3 hash.
    // Write directly to a unique temp file so multi-GB files do not require a
    // second full in-memory copy before the atomic rename.
    let total = chunk_hashes.len();
    create_private_download_parents(local_path)?;

    let (tmp, mut tmp_file) = create_private_download_tmp(local_path)?;
    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0u64;

    for (i, hash) in chunk_hashes.iter().enumerate() {
        let chunk_key = format!("{remote_prefix}/chunks/{hash}");

        // Download with retry + integrity verification
        let chunk_bytes: Vec<u8> = match read_chunk_with_retry(op, &chunk_key, hash, i).await {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        };

        // Decrypt chunk if file key is present
        #[cfg(feature = "crypto")]
        let plaintext = if let (Some(ref fk), Some(ref fid)) = (&file_key, &file_id) {
            match tcfs_crypto::decrypt_chunk(fk, i as u64, fid, &chunk_bytes)
                .with_context(|| format!("decrypting chunk {i}"))
            {
                Ok(plaintext) => plaintext,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(e);
                }
            }
        } else {
            chunk_bytes
        };

        #[cfg(not(feature = "crypto"))]
        let plaintext = chunk_bytes;

        if let Err(e) = tmp_file.write_all(&plaintext).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(anyhow::Error::new(e).context(format!("writing tmp: {}", tmp.display())));
        }
        hasher.update(&plaintext);
        bytes += plaintext.len() as u64;

        if let Some(cb) = progress {
            cb(
                (i + 1) as u64,
                total as u64,
                &format!("chunk {}/{total}", i + 1),
            );
        }
    }

    if let Err(e) = tmp_file.flush().await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(anyhow::Error::new(e).context(format!("flushing tmp: {}", tmp.display())));
    }
    drop(tmp_file);

    if bytes != manifest.file_size {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!(
            "file size check failed for {remote_manifest}: expected {}, got {bytes}",
            manifest.file_size
        );
    }

    // Verify reassembled file hash matches the manifest (plaintext hash)
    let actual_file_hash = tcfs_chunks::hash_to_hex(&hasher.finalize());
    if actual_file_hash != manifest.file_hash {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!(
            "file integrity check failed for {remote_manifest}: expected {}, got {actual_file_hash}",
            manifest.file_hash
        );
    }

    if let Some(guard) = expected.and_then(|entry| entry.replacement_guard) {
        if let Err(error) = verify_replacement_guard(guard, local_path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(error);
        }
    }

    // Atomic write to local path
    if let Err(e) = tokio::fs::rename(&tmp, local_path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(anyhow::Error::new(e).context(format!("renaming to: {}", local_path.display())));
    }

    // Restore Unix file permissions from manifest
    #[cfg(unix)]
    if let Some(mode) = manifest.mode {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        tokio::fs::set_permissions(local_path, perms)
            .await
            .with_context(|| format!("restoring permissions on: {}", local_path.display()))?;
    }

    // Restore the source mtime (TIN-1620 T13-Z) BEFORE the re-stat below, so the
    // state cache and `git status` see the original timestamp, not "now". Old
    // manifests carry `mtime: None`, leaving current behavior unchanged.
    if let Some(mtime) = manifest.mtime {
        apply_manifest_mtime(local_path, mtime);
    }

    let mut sync_state_for_result = None;
    if !_device_id.is_empty() {
        let mut local_vclock = state
            .as_ref()
            .and_then(|state| state.get(local_path))
            .map(|s| s.vclock.clone())
            .unwrap_or_default();
        local_vclock.merge(&manifest.vclock);

        let sync_state = make_sync_state_full(
            local_path,
            actual_file_hash,
            total,
            remote_manifest.to_string(),
            local_vclock,
            _device_id.to_string(),
        )?;
        if let Some(state) = state {
            state.set(local_path, sync_state.clone());
        }
        sync_state_for_result = Some(sync_state);
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
        sync_state: sync_state_for_result,
    })
}

/// Why a restored symlink target was refused. Used for structured warn logging.
///
/// (TIN-1737) Symlink restore is an *ingress* path: a hostile peer can publish a
/// `SymlinkManifest` whose target points outside the sync root or at a local
/// secret store, and every pulling host would otherwise materialize it
/// verbatim. We fail closed here, before the link is ever created.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SymlinkRejection {
    /// Target string was empty.
    Empty,
    /// Target contains a control character that could inject logs or metadata.
    ControlCharacter,
    /// Target uses a backslash, whose path meaning differs across platforms.
    NonPortableSeparator,
    /// Target contains a normal component that has no single portable spelling.
    NonPortableComponent,
    /// Target is an absolute path (`/etc/passwd`, `C:\...`, UNC).
    Absolute,
    /// Target uses `..` to cross the admitted logical-root boundary (or the
    /// link-parent boundary when an absolute caller cannot identify that root).
    Traversal,
    /// Resolved target lands on the fail-closed security deny-set
    /// (`.ssh`, `.gnupg`, dotenv, credential files, live DBs, ...).
    DenySet(BlacklistReason),
    /// Link or target crosses a reserved Git metadata directory.
    ReservedGit,
}

impl std::fmt::Display for SymlinkRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymlinkRejection::Empty => write!(f, "empty target"),
            SymlinkRejection::ControlCharacter => write!(f, "control character in target"),
            SymlinkRejection::NonPortableSeparator => {
                write!(f, "non-portable backslash separator in target")
            }
            SymlinkRejection::NonPortableComponent => {
                write!(f, "non-portable path component in target")
            }
            SymlinkRejection::Absolute => write!(f, "absolute target"),
            SymlinkRejection::Traversal => write!(f, "`..` crosses admitted root boundary"),
            SymlinkRejection::DenySet(reason) => write!(f, "deny-set: {reason}"),
            SymlinkRejection::ReservedGit => write!(f, "reserved Git metadata path"),
        }
    }
}

/// Whether `target` begins with a Windows path prefix.
///
/// This is deliberately textual rather than `Path`-based: a Linux build must
/// reject the same drive, rooted-current-drive, UNC, and device spellings that
/// a Windows build would reject.
fn has_windows_path_prefix(target: &str) -> bool {
    let bytes = target.as_bytes();
    target.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

#[derive(Clone, Copy)]
enum SymlinkLinkPathKind {
    Physical,
    IndexedLogical,
}

/// Lexically resolve a portable `/`-separated symlink body without touching the
/// filesystem.
///
/// A root-relative logical link path (the form used by indexed plans) supplies
/// its parent depth, so `../sibling` is accepted only when it remains under that
/// logical root. An absolute filesystem link path does not identify its sync
/// root; those legacy restore/upload callers therefore get a zero ascent budget
/// and retain the stricter "do not leave the link's own parent" behavior.
fn lexical_resolve_portable_target(
    local_path: &Path,
    target: &str,
    link_path_kind: SymlinkLinkPathKind,
) -> std::result::Result<PathBuf, SymlinkRejection> {
    use std::path::Component;

    let base = local_path.parent().unwrap_or_else(|| Path::new("."));
    let mut resolved = base.to_path_buf();

    let root_relative_parent_depth = match link_path_kind {
        SymlinkLinkPathKind::Physical => None,
        SymlinkLinkPathKind::IndexedLogical => {
            let mut depth = 0usize;
            let mut is_root_relative = true;
            for component in base.components() {
                match component {
                    Component::CurDir => {}
                    Component::Normal(_) => depth += 1,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                        is_root_relative = false;
                        break;
                    }
                }
            }
            is_root_relative.then_some(depth)
        }
    };

    let mut parent_ascent_budget = root_relative_parent_depth.unwrap_or(0);
    let mut target_descent_depth = 0usize;
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if target_descent_depth > 0 {
                    target_descent_depth -= 1;
                    resolved.pop();
                } else if parent_ascent_budget > 0 {
                    parent_ascent_budget -= 1;
                    resolved.pop();
                } else {
                    return Err(SymlinkRejection::Traversal);
                }
            }
            name => {
                target_descent_depth += 1;
                resolved.push(name);
            }
        }
    }

    Ok(resolved)
}

/// Fail-closed validation for a restored symlink target (TIN-1737).
///
/// `local_path` is the would-be link location; `target` is the attacker-supplied
/// link body. Rejects: empty, controls, non-portable separators or prefixes,
/// `..`-escape above the logical root, or any resolved component hitting the
/// security deny-set. Returns `Ok(())` for a benign in-root relative target.
///
/// NOTE: `create_local_symlink` does not receive the sync root, so the link's
/// own parent directory is used as the escape boundary for absolute filesystem
/// paths. This is *more* strict than "must stay within the sync root" (the link
/// is always created inside the root), so it can refuse an otherwise-legitimate
/// `../sibling/file` target. Root-relative indexed paths do carry their logical
/// parent depth and may ascend within that root.
fn validate_restored_symlink_target(
    local_path: &Path,
    target: &str,
) -> std::result::Result<(), SymlinkRejection> {
    validate_symlink_target(local_path, target, SymlinkLinkPathKind::Physical)
}

pub(crate) fn validate_restored_symlink_target_for_physical_path(
    local_path: &Path,
    target: &str,
) -> Result<()> {
    validate_restored_symlink_target(local_path, target).map_err(|reason| {
        anyhow::anyhow!(
            "refusing symlink target at {}: {reason}",
            local_path.display()
        )
    })
}

fn validate_symlink_target(
    local_path: &Path,
    target: &str,
    link_path_kind: SymlinkLinkPathKind,
) -> std::result::Result<(), SymlinkRejection> {
    if target.is_empty() {
        return Err(SymlinkRejection::Empty);
    }
    if target.chars().any(char::is_control) {
        return Err(SymlinkRejection::ControlCharacter);
    }

    // Parse the serialized target using one portable grammar. Host-native
    // `Path::is_absolute` / `Path::components` would interpret `C:\...` and
    // backslashes differently on Linux, macOS, and Windows.
    if target.starts_with('/') || has_windows_path_prefix(target) {
        return Err(SymlinkRejection::Absolute);
    }
    if target.contains('\\') {
        return Err(SymlinkRejection::NonPortableSeparator);
    }
    for component in target.split('/') {
        if component.is_empty() {
            return Err(SymlinkRejection::NonPortableComponent);
        }
        if component != ".git" && component.eq_ignore_ascii_case(".git") {
            return Err(SymlinkRejection::ReservedGit);
        }
        if component != "."
            && component != ".."
            && validate_namespace_logical_path(component).is_err()
        {
            return Err(SymlinkRejection::NonPortableComponent);
        }
    }

    let target_path = Path::new(target);
    let resolved = lexical_resolve_portable_target(local_path, target, link_path_kind)?;

    if filesystem_path_has_reserved_git_component(local_path)
        || filesystem_path_has_reserved_git_component(&resolved)
        || filesystem_path_has_reserved_git_component(target_path)
        || path_text_has_reserved_git_component(target)
    {
        return Err(SymlinkRejection::ReservedGit);
    }

    // (c) Resolved target hits the fail-closed security deny-set. We reuse the
    // egress-side `check_security_path_components`, which is config-independent.
    // A default Blacklist is sufficient because the security deny-set is fixed.
    let blacklist = Blacklist::default();
    if let Some(reason) = blacklist.check_security_path_components(&resolved) {
        return Err(SymlinkRejection::DenySet(reason));
    }
    // Also screen the raw target components directly, in case the resolved form
    // (anchored at `base`) somehow masked a security name. Belt and suspenders.
    if let Some(reason) = blacklist.check_security_path_components(target_path) {
        return Err(SymlinkRejection::DenySet(reason));
    }

    Ok(())
}

/// Planning-safe wrapper for the symlink ingress/egress target policy. The
/// attacker-controlled target is deliberately omitted from the error text.
pub fn validate_indexed_symlink_target(local_path: &Path, target: &str) -> Result<()> {
    let logical_path = local_path
        .to_str()
        .context("indexed symlink path must be valid UTF-8")?;
    validate_namespace_logical_path(logical_path)
        .context("indexed symlink path must be a canonical logical relative path")?;
    validate_symlink_target(local_path, target, SymlinkLinkPathKind::IndexedLogical).map_err(
        |reason| {
            anyhow::anyhow!(
                "refusing symlink target at {}: {reason}",
                local_path.display()
            )
        },
    )
}

/// Create a restored symlink after fail-closed target validation (TIN-1737).
///
/// A refused target is an explicit error so reconcile cannot count a hostile
/// no-op as a successful pull or persist sync state for it.
async fn create_local_symlink(
    local_path: &Path,
    target: &str,
    replacement_guard: Option<ReplacementGuard<'_>>,
) -> Result<()> {
    if let Err(reason) = validate_restored_symlink_target(local_path, target) {
        warn!(
            local = %local_path.display(),
            target = ?target,
            reason = %reason,
            "refusing to restore symlink: fail-closed ingress guard"
        );
        anyhow::bail!(
            "refusing to restore symlink at {}: {reason}",
            local_path.display()
        );
    }

    create_private_download_parents(local_path)?;

    let tmp = unique_tmp_path(local_path, "tcfs_symlink_tmp");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &tmp)
            .with_context(|| format!("creating symlink: {} -> {target}", tmp.display()))?;
    }

    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, &tmp)
            .with_context(|| format!("creating symlink: {} -> {target}", tmp.display()))?;
    }

    if let Some(guard) = replacement_guard {
        if let Err(error) = verify_replacement_guard(guard, local_path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(error);
        }
    }

    if let Err(error) = tokio::fs::rename(&tmp, local_path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(anyhow::Error::new(error)
            .context(format!("renaming symlink to: {}", local_path.display())));
    }
    Ok(())
}

fn make_symlink_sync_state(
    local_path: &Path,
    hash_hex: String,
    remote_path: String,
    vclock: VectorClock,
    device_id: String,
    size: u64,
) -> Result<SyncState> {
    let meta = std::fs::symlink_metadata(local_path)
        .with_context(|| format!("stat symlink for sync state: {}", local_path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(SyncState {
        blake3: hash_hex,
        size,
        mtime,
        chunk_count: 0,
        remote_path,
        last_synced: now,
        vclock,
        device_id,
        conflict: None,
        status: FileSyncStatus::Synced,
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
    push_tree_with_device_with_runtime(
        op,
        local_root,
        remote_prefix,
        state,
        progress,
        device_id,
        collect_cfg,
        encryption,
        UploadRuntimeOptions::from_env(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn push_tree_with_device_with_runtime(
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
    device_id: &str,
    collect_cfg: Option<&CollectConfig>,
    encryption: OptionalEncryption<'_>,
    runtime: UploadRuntimeOptions,
) -> Result<(usize, usize, u64)> {
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0u64;

    let cfg = collect_cfg.cloned().unwrap_or_default();
    let result = collect_files(local_root, &cfg)?;
    let total = result.files.len() + result.symlinks.len();
    let remote_prefix = remote_path_prefix(remote_prefix);

    if should_upload_files_concurrently(runtime, encryption.is_some()) {
        let stats = push_regular_files_concurrently(
            op,
            local_root,
            &remote_prefix,
            state,
            progress,
            device_id,
            &result.files,
            total,
            runtime,
        )
        .await?;
        uploaded += stats.0;
        skipped += stats.1;
        bytes += stats.2;
    } else {
        for (i, path) in result.files.iter().enumerate() {
            let rel = path.strip_prefix(local_root).unwrap_or(path);
            let rel_str = normalize_rel_path_text(&rel.to_string_lossy());

            let msg = format!("[{}/{}] {}", i + 1, total, rel.display());
            if let Some(cb) = progress {
                cb(i as u64, total as u64, &msg);
            }

            let tracked_state = state.get(path).cloned();
            let sync_reason = match state.needs_sync(path) {
                Ok(reason) => reason,
                Err(e) => {
                    warn!(path = %path.display(), "upload preflight failed: {e}");
                    continue;
                }
            };

            match upload_file_with_device_with_state(
                op,
                path,
                &remote_prefix,
                None,
                device_id,
                Some(&rel_str),
                encryption,
                tracked_state,
                sync_reason,
                runtime,
                None,
            )
            .await
            {
                Ok((result, state_update)) => {
                    if let Some(sync_state) = state_update {
                        state.set(&result.path, sync_state);
                    }
                    if result.skipped {
                        skipped += 1;
                    } else {
                        // Path publication is owned by upload_file_with_device so
                        // the manifest and index sequence stays crash-aware.
                        uploaded += 1;
                        bytes += result.bytes;
                    }
                }
                Err(e) => {
                    warn!(path = %path.display(), "upload failed: {e}");
                }
            }
        }
    }

    for (i, path) in result.symlinks.iter().enumerate() {
        let rel = path.strip_prefix(local_root).unwrap_or(path);
        let rel_str = normalize_rel_path_text(&rel.to_string_lossy());
        let ordinal = result.files.len() + i + 1;

        let msg = format!("[{ordinal}/{total}] {} -> symlink", rel.display());
        if let Some(cb) = progress {
            cb((ordinal - 1) as u64, total as u64, &msg);
        }

        match upload_symlink_with_device(op, path, &remote_prefix, state, device_id, &rel_str).await
        {
            Ok(result) => {
                if result.skipped {
                    skipped += 1;
                } else {
                    uploaded += 1;
                    bytes += result.bytes;
                }
            }
            Err(e) => {
                warn!(path = %path.display(), "symlink upload failed: {e}");
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
            let rel_str = normalize_rel_path_text(&rel.to_string_lossy());
            if let Err(e) = publish_directory_marker(op, &remote_prefix, &rel_str).await {
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

type TreeUploadTaskResult = Result<(UploadResult, Option<SyncState>)>;

async fn await_next_tree_upload(
    pending: &mut JoinSet<(PathBuf, TreeUploadTaskResult)>,
) -> Option<(PathBuf, TreeUploadTaskResult)> {
    match pending.join_next().await {
        Some(Ok(result)) => Some(result),
        Some(Err(e)) => {
            warn!("file upload task panicked or was cancelled: {e}");
            None
        }
        None => None,
    }
}

fn apply_tree_upload_result(
    path: PathBuf,
    result: TreeUploadTaskResult,
    state: &mut StateCache,
    uploaded: &mut usize,
    skipped: &mut usize,
    bytes: &mut u64,
) {
    match result {
        Ok((result, state_update)) => {
            if let Some(sync_state) = state_update {
                state.set(&result.path, sync_state);
            }
            if result.skipped {
                *skipped += 1;
            } else {
                *uploaded += 1;
                *bytes += result.bytes;
            }
        }
        Err(e) => {
            warn!(path = %path.display(), "upload failed: {e}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn push_regular_files_concurrently(
    op: &Operator,
    local_root: &Path,
    remote_prefix: &str,
    state: &mut StateCache,
    progress: Option<&ProgressFn>,
    device_id: &str,
    files: &[PathBuf],
    total: usize,
    runtime: UploadRuntimeOptions,
) -> Result<(usize, usize, u64)> {
    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0u64;
    let mut pending = JoinSet::new();
    let concurrency = runtime.file_upload_concurrency;

    info!(
        files = files.len(),
        file_upload_concurrency = concurrency,
        fresh_prefix_publish = runtime.assume_fresh_prefix,
        "uploading tree files with bounded file concurrency"
    );

    for (i, path) in files.iter().enumerate() {
        let rel = path.strip_prefix(local_root).unwrap_or(path);
        let rel_str = normalize_rel_path_text(&rel.to_string_lossy());

        let msg = format!("[{}/{}] {}", i + 1, total, rel.display());
        if let Some(cb) = progress {
            cb(i as u64, total as u64, &msg);
        }

        let tracked_state = state.get(path).cloned();
        let sync_reason = match state.needs_sync(path) {
            Ok(reason) => reason,
            Err(e) => {
                warn!(path = %path.display(), "upload preflight failed: {e}");
                continue;
            }
        };

        let op = op.clone();
        let path = path.clone();
        let remote_prefix = remote_prefix.to_string();
        let device_id = device_id.to_string();
        pending.spawn(async move {
            let result = upload_file_with_device_with_state(
                &op,
                &path,
                &remote_prefix,
                None,
                &device_id,
                Some(&rel_str),
                None,
                tracked_state,
                sync_reason,
                runtime,
                None,
            )
            .await;
            (path, result)
        });

        while pending.len() >= concurrency {
            if let Some((path, result)) = await_next_tree_upload(&mut pending).await {
                apply_tree_upload_result(
                    path,
                    result,
                    state,
                    &mut uploaded,
                    &mut skipped,
                    &mut bytes,
                );
            }
        }
    }

    while !pending.is_empty() {
        if let Some((path, result)) = await_next_tree_upload(&mut pending).await {
            apply_tree_upload_result(path, result, state, &mut uploaded, &mut skipped, &mut bytes);
        }
    }

    Ok((uploaded, skipped, bytes))
}

/// Collect all regular files under `root` recursively, respecting config.
///
/// When `config.sync_empty_dirs` is true, also collects directories that
/// contain no files (after exclusion rules) so callers can create `.tcfs_dir`
/// marker objects in the remote index.
pub fn collect_files(root: &Path, config: &CollectConfig) -> Result<CollectResult> {
    let mut files = Vec::new();
    let mut symlinks = Vec::new();
    let mut empty_dirs = Vec::new();
    let blacklist = Blacklist::new(
        &config.exclude_patterns,
        config.sync_hidden_dirs,
        config.sync_git_dirs,
        &config.git_sync_mode,
    );
    if let Some(reason) = blacklist.check_security_path_components(root) {
        warn!(
            path = %root.display(),
            reason = %reason,
            "skipping collection root: security deny-set"
        );
        return Ok(CollectResult {
            files,
            symlinks,
            empty_dirs,
        });
    }
    // Track visited canonical paths for symlink cycle detection
    let mut visited = std::collections::HashSet::new();
    if let Ok(canon) = std::fs::canonicalize(root) {
        visited.insert(canon);
    }
    collect_files_inner(
        root,
        &mut files,
        &mut symlinks,
        &mut empty_dirs,
        config,
        &blacklist,
        &mut visited,
    )?;

    // Bundle mode: for every enrolled git repo, capture `.git` as a single
    // `git bundle` and add the bundle to the upload set as a normal object.
    // The raw `.git/*` internals were skipped by `collect_files_inner`.
    if config.sync_git_dirs && config.git_sync_mode == "bundle" {
        collect_git_bundles(root, &mut files);
    }

    files.sort(); // deterministic order
    symlinks.sort();
    empty_dirs.sort();
    Ok(CollectResult {
        files,
        symlinks,
        empty_dirs,
    })
}

fn collect_files_inner(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    symlinks: &mut Vec<PathBuf>,
    empty_dirs: &mut Vec<PathBuf>,
    config: &CollectConfig,
    blacklist: &Blacklist,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let before = out.len();
    let before_symlinks = symlinks.len();

    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading dir: {}", dir.display()))?
    {
        let entry = entry.context("reading dir entry")?;
        let path = entry.path();

        // Use file_type() (doesn't follow symlinks) for initial dispatch
        let ft = entry.file_type().context("file_type dir entry")?;

        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            // Full-path check (not just the name): the fail-closed
            // `.git/worktrees/` fence needs path context so the per-worktree
            // admin area is never collected even in raw mode (G5 / TIN-1620).
            if let Some(reason) = blacklist.check(&path, ft.is_dir()) {
                match &reason {
                    BlacklistReason::Security(_) => warn!(
                        path = %path.display(),
                        reason = %reason,
                        "skipping path: security deny-set"
                    ),
                    _ => debug!(
                        path = %path.display(),
                        reason = %reason,
                        "skipping path: blacklist"
                    ),
                }
                continue;
            }

            // Handle symlinks explicitly
            if ft.is_symlink() {
                let target = std::fs::read_link(&path).unwrap_or_default();
                if let Some(reason) = blacklist.check_security_path_components(&target) {
                    warn!(
                        path = %path.display(),
                        target = %target.display(),
                        reason = %reason,
                        "skipping symlink: target matches security deny-set"
                    );
                    continue;
                }

                if config.preserve_symlinks {
                    symlinks.push(path);
                    continue;
                }

                if !config.follow_symlinks {
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
                        if let Some(reason) = blacklist.check_security_path_components(&real) {
                            warn!(
                                path = %path.display(),
                                target = %real.display(),
                                reason = %reason,
                                "skipping symlink: resolved target matches security deny-set"
                            );
                            continue;
                        }
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
                                    &path, out, symlinks, empty_dirs, config, blacklist, visited,
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
                        collect_files_inner(
                            &path, out, symlinks, empty_dirs, config, blacklist, visited,
                        )?;
                    }
                    continue;
                }

                collect_files_inner(&path, out, symlinks, empty_dirs, config, blacklist, visited)?;
            } else if ft.is_file() {
                out.push(path);
            }
        }
    }

    // If no files were collected from this directory (directly or via
    // subdirectories) and we're tracking empty dirs, record it as empty.
    if config.sync_empty_dirs && out.len() == before && symlinks.len() == before_symlinks {
        empty_dirs.push(dir.to_path_buf());
    }

    Ok(())
}

/// Walk `root` for git working trees (directories containing a `.git`
/// directory) and, for each one that is safe to snapshot, create a git bundle
/// and append its path to `files` so it is uploaded as a normal TCFS object.
///
/// Repos with in-progress operations (rebase, merge, lockfiles) are skipped
/// this cycle and will be retried on the next sync once the operation settles.
/// Bundle staleness is handled implicitly: `git bundle create --all` always
/// reflects current refs, and the resulting object only re-uploads chunks that
/// actually changed (content-addressed dedup).
fn collect_git_bundles(root: &Path, files: &mut Vec<PathBuf>) {
    let mut repos = Vec::new();
    find_git_repos(root, &mut repos);
    for repo_root in repos {
        let git_dir = repo_root.join(".git");
        let safety = crate::git_safety::git_is_safe(&git_dir);
        if !safety.blocking.is_empty() {
            warn!(
                repo = %repo_root.display(),
                blocking = ?safety.blocking,
                "skipping git bundle: active git operation in progress"
            );
            continue;
        }
        match crate::git_safety::snapshot_git_for_sync(&repo_root) {
            Ok(bundle_path) => {
                debug!(repo = %repo_root.display(), bundle = %bundle_path.display(), "captured git bundle");
                files.push(bundle_path);
            }
            Err(e) => {
                warn!(repo = %repo_root.display(), "git bundle failed: {e}");
            }
        }
    }
}

/// Recursively find directories under `root` that contain a `.git` directory
/// (i.e. git working-tree roots). Does not descend into `.git` itself.
fn find_git_repos(dir: &Path, out: &mut Vec<PathBuf>) {
    if dir.join(".git").is_dir() {
        out.push(dir.to_path_buf());
        // Still descend to catch nested submodule/worktree repos.
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".git" {
            continue;
        }
        find_git_repos(&path, out);
    }
}

/// Restore git history from any TCFS bundles materialized under `root`.
///
/// Call this after a pull/rehydrate completes: for every
/// `.git-tcfs-bundle` file found under `root`, restore the repo's `.git`
/// metadata in place so `git log` / `git status` / `git fetch` work on the
/// peer. The synced working-tree files are left untouched.
///
/// Returns the number of repos successfully restored.
pub fn restore_git_bundles_under(root: &Path) -> usize {
    let mut bundles = Vec::new();
    find_git_bundles(root, &mut bundles);
    let mut restored = 0usize;
    for bundle in bundles {
        let Some(repo_root) = bundle.parent() else {
            continue;
        };
        match crate::git_safety::restore_git_bundle_into(&bundle, repo_root) {
            Ok(()) => {
                info!(repo = %repo_root.display(), "restored git history from bundle");
                restored += 1;
            }
            Err(e) => {
                warn!(repo = %repo_root.display(), "git bundle restore failed: {e}");
            }
        }
    }
    restored
}

/// Recursively find `.git-tcfs-bundle` files under `dir`.
fn find_git_bundles(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name == crate::git_safety::GIT_BUNDLE_REL_PATH {
                out.push(path.clone());
            }
        }
        if ft.is_dir() {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == ".git" {
                continue;
            }
            find_git_bundles(&path, out);
        }
    }
}

/// Normalize a filesystem path into a stable S3 index key component.
///
/// - If `sync_root` is provided and the path is under it, returns the relative path.
/// - Otherwise strips the leading `/` from absolute paths, or returns relative paths as-is.
/// - Replaces `\` with `/` for cross-platform consistency.
pub(crate) fn normalize_rel_path_text(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .map(|component| component.nfc().collect::<String>())
        .collect::<Vec<_>>()
        .join("/")
}

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

    normalize_rel_path_text(&rel.to_string_lossy())
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedManifest {
    manifest_path: String,
    rel_path: Option<String>,
    index_entry: Option<RemoteIndexEntry>,
}

impl ResolvedManifest {
    fn direct(manifest_path: String) -> Self {
        Self {
            manifest_path,
            rel_path: None,
            index_entry: None,
        }
    }

    fn indexed(manifest_path: String, rel_path: String, index_entry: RemoteIndexEntry) -> Self {
        Self {
            manifest_path,
            rel_path: Some(rel_path),
            index_entry: Some(index_entry),
        }
    }

    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }

    /// Canonical logical path retained when resolution came through the index.
    /// Direct immutable-manifest references return `None` until their bytes are
    /// parsed by the common download boundary.
    pub fn rel_path(&self) -> Option<&str> {
        self.rel_path.as_deref()
    }
}

/// One immutable, path-index-authorized manifest observation.
///
/// All metadata exposed here comes from the manifest bytes selected and
/// validated at resolution time, rather than from an eventually-consistent
/// notification. The selected index entry and bytes remain private so callers
/// cannot detach hydration from this authority.
#[derive(Debug, Clone)]
pub struct IndexedManifestSnapshot {
    remote_prefix: String,
    rel_path: String,
    manifest_path: String,
    manifest_object_id: String,
    index_entry: RemoteIndexEntry,
    manifest_bytes: Vec<u8>,
    kind: RemoteEntryKind,
    content_hash: String,
    size: u64,
    vclock: VectorClock,
    written_by: String,
    symlink_target: Option<String>,
}

impl IndexedManifestSnapshot {
    fn remote_prefix(&self) -> &str {
        &self.remote_prefix
    }

    fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }

    pub fn manifest_object_id(&self) -> &str {
        &self.manifest_object_id
    }

    pub fn kind(&self) -> RemoteEntryKind {
        self.kind
    }

    /// Canonical local identity: BLAKE3 for regular files, or the stable
    /// target-derived TCFS symlink identity for symlinks.
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn chunks(&self) -> usize {
        self.index_entry.chunks
    }

    pub fn vclock(&self) -> &VectorClock {
        &self.vclock
    }

    pub fn written_by(&self) -> &str {
        &self.written_by
    }

    pub fn symlink_target(&self) -> Option<&str> {
        self.symlink_target.as_deref()
    }
}

/// Resolve only one exact canonical path-index key while retaining the index
/// authority required by [`download_resolved_file_with_device`].
///
/// This API never accepts direct manifest keys, normalizes host paths, or falls
/// back to another entry with the same final filename. Exact absence is
/// returned as `Ok(None)` so ingress callers can map it to their own not-found
/// status without weakening path identity.
pub async fn resolve_exact_manifest_reference(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<Option<ResolvedManifest>> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(rel_path)?;
    let manifest_prefix = manifest_path_prefix(prefix);
    let index_key = format!("{prefix}/index/{rel_path}");
    let Some(entry) = resolve_visible_index_entry(op, &index_key, &manifest_prefix)
        .await
        .with_context(|| format!("resolving exact index entry: {index_key}"))?
    else {
        return Ok(None);
    };
    validate_remote_entry(&entry, "exact manifest reference")?;
    Ok(Some(ResolvedManifest::indexed(
        manifest_key(&manifest_prefix, &entry.manifest_hash),
        rel_path.to_owned(),
        entry,
    )))
}

/// Read only the current pointer from one exact path-index record.
///
/// This deliberately does not probe, parse, or recover a manifest. It exists
/// so conditional readers can reject an already-stale immutable version before
/// unrelated manifest or chunk I/O. The observation is not a content snapshot:
/// callers that continue must still use an authoritative resolver below and
/// compare the selected version again before consuming content.
pub async fn read_exact_visible_index_selection(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<Option<RemoteIndexEntry>> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(rel_path)?;
    let index_key = format!("{prefix}/index/{rel_path}");
    let Some(record) = read_index_entry_record_from_store(op, &index_key)
        .await
        .with_context(|| format!("reading exact index selection: {index_key}"))?
    else {
        return Ok(None);
    };
    let Some(entry) = record.visible_entry().cloned() else {
        return Ok(None);
    };
    validate_remote_entry(&entry, "exact visible index selection")?;
    Ok(Some(entry))
}

/// Resolve only the exact canonical `rel_path` index key and capture its
/// selected, validated manifest bytes. Unlike [`resolve_manifest_reference`],
/// this never performs filename fallback and never accepts a direct manifest
/// key.
pub async fn resolve_exact_indexed_manifest_snapshot(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<Option<IndexedManifestSnapshot>> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    validated_index_rel_path(rel_path)?;
    let manifest_prefix = manifest_path_prefix(prefix);
    let index_key = format!("{prefix}/index/{rel_path}");
    let Some(entry) = resolve_visible_index_entry(op, &index_key, &manifest_prefix)
        .await
        .with_context(|| format!("resolving exact index entry: {index_key}"))?
    else {
        return Ok(None);
    };
    validate_remote_entry(&entry, "exact indexed snapshot")?;

    let manifest_object_id = entry.manifest_hash.clone();
    let manifest_path = manifest_key(&manifest_prefix, &manifest_object_id);
    let manifest_bytes = read_with_retry(op, &manifest_path)
        .await
        .with_context(|| format!("reading exact indexed manifest: {manifest_path}"))?;
    let parsed = parse_manifest_for_index_entry(
        &manifest_bytes,
        entry.kind,
        entry.symlink_target.as_deref(),
        rel_path,
    )?;
    validate_parsed_indexed_manifest_binding(&manifest_bytes, &manifest_object_id, &parsed)?;

    let (content_hash, size, vclock, written_by, symlink_target) = match parsed {
        ParsedIndexedManifest::Regular(manifest) => {
            anyhow::ensure!(
                entry.size == manifest.file_size,
                "index/manifest size mismatch for {rel_path}: index {}, manifest {}",
                entry.size,
                manifest.file_size
            );
            anyhow::ensure!(
                entry.chunks == manifest.chunks.len(),
                "index/manifest chunk-count mismatch for {rel_path}: index {}, manifest {}",
                entry.chunks,
                manifest.chunks.len()
            );
            (
                manifest.file_hash,
                manifest.file_size,
                manifest.vclock,
                manifest.written_by,
                None,
            )
        }
        ParsedIndexedManifest::Symlink(manifest) => {
            let size = manifest.symlink_target.len() as u64;
            anyhow::ensure!(
                entry.size == size && entry.chunks == 0,
                "index/manifest symlink metadata mismatch for {rel_path}"
            );
            (
                symlink_manifest_hash(&manifest.symlink_target),
                size,
                manifest.vclock,
                manifest.written_by,
                Some(manifest.symlink_target),
            )
        }
    };

    Ok(Some(IndexedManifestSnapshot {
        remote_prefix: prefix.to_string(),
        rel_path: rel_path.to_string(),
        manifest_path,
        manifest_object_id,
        index_entry: entry.clone(),
        manifest_bytes,
        kind: entry.kind,
        content_hash,
        size,
        vclock,
        written_by,
        symlink_target,
    }))
}

/// Return whether the exact path index still selects the snapshot's complete
/// entry authority. This performs no filename fallback and does not reread the
/// captured manifest bytes.
pub async fn indexed_manifest_snapshot_is_current(
    op: &Operator,
    snapshot: &IndexedManifestSnapshot,
) -> Result<bool> {
    let manifest_prefix = manifest_path_prefix(snapshot.remote_prefix());
    let index_key = format!("{}/index/{}", snapshot.remote_prefix(), snapshot.rel_path());
    let current = resolve_visible_index_entry(op, &index_key, &manifest_prefix)
        .await
        .with_context(|| format!("rechecking indexed snapshot authority: {index_key}"))?;
    Ok(current.as_ref() == Some(&snapshot.index_entry))
}

/// Resolve a path while retaining the index authority needed to validate the
/// selected manifest before local mutation. Explicit manifest references are
/// represented as legacy direct reads with no index authority.
pub async fn resolve_manifest_reference(
    op: &Operator,
    input: &str,
    remote_prefix: &str,
    sync_root: Option<&Path>,
) -> Result<ResolvedManifest> {
    let prefix = validated_remote_prefix(remote_prefix)?;
    let manifest_prefix = manifest_path_prefix(prefix);

    // Explicit manifest reads remain supported, but only inside the selected
    // root's manifest namespace and with one safe object-id component.
    if input.contains("/manifests/") {
        validate_relative_storage_key(input, "explicit manifest key")?;
        let object_id = input
            .strip_prefix(&format!("{manifest_prefix}/"))
            .filter(|object_id| !object_id.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("explicit manifest key is outside the selected root namespace")
            })?;
        crate::index_entry::validate_storage_key_component(
            object_id,
            "explicit manifest object id",
        )?;
        return Ok(ResolvedManifest::direct(input.to_string()));
    }

    // Try 1: Normalize the input path to derive the index key
    let rel = normalize_rel_path(Path::new(input), sync_root);
    validated_index_rel_path(&rel)?;
    let index_key = format!("{prefix}/index/{rel}");

    match resolve_visible_index_entry(op, &index_key, &manifest_prefix).await {
        Ok(Some(entry)) => {
            return Ok(ResolvedManifest::indexed(
                manifest_key(&manifest_prefix, &entry.manifest_hash),
                rel,
                entry,
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(error).with_context(|| format!("resolving index entry: {index_key}"));
        }
    }

    // Try 2: Search index entries for a matching filename.
    // This handles cross-host pull where the pushing host's canonicalized path
    // differs from the pulling host's (e.g., /tmp → /private/tmp on macOS).
    let filename = Path::new(input)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| input.to_string());
    let filename = normalize_rel_path_text(&filename);
    crate::index_entry::validate_storage_key_component(&filename, "fallback index filename")?;

    let index_prefix = format!("{prefix}/index/");
    let entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("listing index prefix: {index_prefix}"))?;

    let mut matches = Vec::new();
    for entry in entries {
        let entry_path = entry.path();
        if entry_path.ends_with('/') {
            continue;
        }
        validate_relative_storage_key(entry_path, "listed index key")?;
        let listed_rel_path = entry_path
            .strip_prefix(&index_prefix)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| anyhow::anyhow!("listed index key escaped requested prefix"))?;
        validated_index_rel_path(listed_rel_path)?;
        if listed_rel_path.rsplit('/').next() == Some(filename.as_str()) {
            match resolve_visible_index_entry(op, entry_path, &manifest_prefix).await {
                Ok(Some(entry)) => {
                    matches.push(ResolvedManifest::indexed(
                        manifest_key(&manifest_prefix, &entry.manifest_hash),
                        listed_rel_path.to_string(),
                        entry,
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("resolving listed index entry: {entry_path}"));
                }
            }
        }
    }

    match matches.len() {
        1 => return Ok(matches.pop().expect("one fallback manifest match")),
        count if count > 1 => {
            let mut paths: Vec<&str> = matches
                .iter()
                .filter_map(|candidate| candidate.rel_path.as_deref())
                .collect();
            paths.sort_unstable();
            anyhow::bail!(
                "ambiguous filename fallback for {filename:?}: matched {} index entries: {}",
                paths.len(),
                paths.join(", ")
            );
        }
        _ => {}
    }

    anyhow::bail!(
        "no index entry found for '{}' (tried: {index_key}, filename search: {filename})",
        input
    )
}

/// Compatibility wrapper for direct-manifest callers. New path-indexed
/// download code must retain [`ResolvedManifest`] and use
/// [`download_resolved_file_with_device`] instead of discarding its authority.
pub async fn resolve_manifest_path(
    op: &Operator,
    input: &str,
    remote_prefix: &str,
    sync_root: Option<&Path>,
) -> Result<String> {
    Ok(
        resolve_manifest_reference(op, input, remote_prefix, sync_root)
            .await?
            .manifest_path,
    )
}

/// Logically delete a remote index entry with compare-and-swap.
///
/// Manifest, staging, and chunk objects remain immutable recovery evidence.
/// Physical reclamation belongs to a separate GC protocol that can prove a
/// stable reachability snapshot; delete must never race a new reference.
pub async fn delete_remote_index_entry(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<()> {
    let rel_path = normalize_rel_path_text(rel_path);
    validated_index_rel_path(&rel_path)?;
    let prefix = validated_remote_prefix(remote_prefix)?;
    let index_key = format!("{prefix}/index/{rel_path}");
    let index_bytes = op
        .read(&index_key)
        .await
        .with_context(|| format!("reading index entry before logical delete: {index_key}"))?
        .to_vec();
    let parsed = crate::index_entry::parse_index_entry_record(&index_bytes)
        .with_context(|| format!("parsing index entry before logical delete: {index_key}"))?;
    if let Some(pending) = parsed.pending_entry() {
        crate::index_entry::validate_staged_manifest_key(&manifest_path_prefix(prefix), pending)?;
    }
    anyhow::ensure!(
        parsed.visible_entry().is_some() || parsed.pending_entry().is_some(),
        "index entry is already logically deleted: {index_key}"
    );
    crate::index_entry::tombstone_index_entry_if_exact(op, prefix, &index_key, &index_bytes)
        .await?;

    info!(rel_path = %rel_path, "logically deleted remote index entry");
    Ok(())
}

/// Logically delete a file from remote storage.
///
/// Looks up the index entry for `rel_path`, deletes that visible path, and
/// preserves immutable object evidence for a later reachability-safe GC.
///
/// Also removes the file from the local state cache if present.
pub async fn delete_remote_file(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
    state: &mut StateCache,
    sync_root: Option<&Path>,
) -> Result<()> {
    let rel_path = normalize_rel_path_text(rel_path.trim_start_matches('/'));
    delete_remote_index_entry(op, &rel_path, remote_prefix).await?;

    // Remove from state cache
    let local_path = sync_root
        .map(|r| r.join(&rel_path))
        .unwrap_or_else(|| PathBuf::from(&rel_path));
    state.remove(&local_path);

    // Also try to remove by searching the cache (handles path normalization mismatches)
    if let Some((key, _)) = state.get_by_rel_path(&rel_path) {
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
    use crate::index_entry::{
        parse_index_entry_record, IndexEntryState, ParsedIndexEntry, PendingIndexEntry,
        VersionedIndexEntry,
    };
    use opendal::services::Memory;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const PRIVATE_TMP_CHILD_MARKER: &str = "TCFS_PRIVATE_TMP_UMASK_CHILD";
    const PRIVATE_TMP_CHILD_DIR: &str = "TCFS_PRIVATE_TMP_UMASK_DIR";

    fn memory_op() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        crate::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    async fn write_committed_index_entry(
        op: &Operator,
        index_key: &str,
        entry: &RemoteIndexEntry,
    ) -> Result<()> {
        op.write(
            index_key,
            VersionedIndexEntry::committed(entry.clone()).to_json_bytes()?,
        )
        .await?;
        Ok(())
    }

    async fn write_preparing_index_entry(
        op: &Operator,
        index_key: &str,
        current: Option<RemoteIndexEntry>,
        pending: PendingIndexEntry,
    ) -> Result<()> {
        op.write(
            index_key,
            VersionedIndexEntry::preparing(current, pending).to_json_bytes()?,
        )
        .await?;
        Ok(())
    }

    fn default_config() -> CollectConfig {
        CollectConfig::default()
    }

    fn no_empty_dirs_config() -> CollectConfig {
        CollectConfig {
            sync_empty_dirs: false,
            ..Default::default()
        }
    }

    fn rel_names(paths: &[PathBuf], root: &Path) -> Vec<String> {
        paths
            .iter()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect()
    }

    fn test_manifest_bytes(file_hash: &str, file_size: u64) -> Vec<u8> {
        format!(
            r#"{{"version":2,"file_hash":"{file_hash}","file_size":{file_size},"chunks":[],"vclock":{{"clocks":{{}}}},"written_by":"tester","written_at":0}}"#
        )
        .into_bytes()
    }

    fn bound_test_manifest_bytes(file_hash: &str, file_size: u64, rel_path: &str) -> Vec<u8> {
        format!(
            r#"{{"version":2,"file_hash":"{file_hash}","file_size":{file_size},"chunks":[],"vclock":{{"clocks":{{}}}},"written_by":"tester","written_at":0,"rel_path":"{rel_path}"}}"#
        )
        .into_bytes()
    }

    fn indexed_test_manifest_for_rel(
        file_hash: &str,
        file_size: u64,
        rel_path: &str,
    ) -> (Vec<u8>, RemoteIndexEntry) {
        let bytes = bound_test_manifest_bytes(file_hash, file_size, rel_path);
        let object_id = manifest_object_id(&bytes);
        let entry = RemoteIndexEntry::new(object_id, file_size, 0);
        (bytes, entry)
    }

    fn indexed_test_manifest(file_hash: &str, file_size: u64) -> (Vec<u8>, RemoteIndexEntry) {
        indexed_test_manifest_for_rel(file_hash, file_size, "doc.txt")
    }

    async fn publish_indexed_test_manifest(
        op: &Operator,
        remote_prefix: &str,
        rel_path: &str,
        bytes: Vec<u8>,
        entry: RemoteIndexEntry,
    ) -> Result<()> {
        let baseline = bind_indexed_publish_baseline(op, remote_prefix, rel_path).await?;
        publish_indexed_manifest(op, remote_prefix, rel_path, bytes, entry, baseline).await
    }

    async fn staging_manifest_keys(op: &Operator) -> Vec<String> {
        op.list("data/staging/manifests/")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path().to_string())
            .collect()
    }

    #[tokio::test]
    async fn direct_manifest_fixed_deny_rel_path_fails_before_local_mutation() {
        for (case, rel_path) in ["master.key", "nested/.env"].into_iter().enumerate() {
            let op = memory_op();
            let tmp = tempfile::tempdir().unwrap();
            let local = tmp.path().join(format!("case-{case}/downloaded"));
            let remote = format!("data/manifests/direct-deny-{case}");
            op.write(
                &remote,
                bound_test_manifest_bytes(&format!("deny-{case}"), 0, rel_path),
            )
            .await
            .unwrap();

            let error = download_file(&op, &remote, &local, "data", None)
                .await
                .unwrap_err();
            let error_text = format!("{error:#}");
            assert!(error_text.contains("fixed-deny ingress path"));
            assert!(error_text.contains(rel_path));
            assert!(!local.parent().unwrap().exists());
        }
    }

    #[tokio::test]
    async fn direct_manifest_allows_ordinary_bound_rel_path_and_object_id() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("download/output.txt");
        let remote = "data/manifests/0123456789abcdef";
        let empty_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b""));
        op.write(
            remote,
            bound_test_manifest_bytes(&empty_hash, 0, "docs/output.txt"),
        )
        .await
        .unwrap();

        download_file(&op, remote, &local, "data", None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"");
    }

    fn committed_manifest_hash(raw: &[u8]) -> String {
        match parse_index_entry_record(raw).unwrap() {
            ParsedIndexEntry::Legacy(entry) => entry.manifest_hash,
            ParsedIndexEntry::V2(entry) => entry.current.unwrap().manifest_hash,
        }
    }

    #[test]
    fn indexed_manifest_binding_rejects_v1_and_incomplete_regular_v3() {
        let v1 = b"legacy-chunk\n";
        let v1_id = manifest_object_id(v1);
        assert!(validate_indexed_manifest_binding(
            v1,
            &v1_id,
            RemoteEntryKind::RegularFile,
            None,
            "file"
        )
        .is_err());

        let v3 = br#"{"version":3,"file_hash":"file","file_size":0,"chunks":[],"vclock":{"clocks":{}},"written_by":"peer","written_at":0,"rel_path":"file"}"#;
        let v3_id = manifest_object_id(v3);
        assert!(validate_indexed_manifest_binding(
            v3,
            &v3_id,
            RemoteEntryKind::RegularFile,
            None,
            "file"
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_download_tmp_and_parents_ignore_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;

        if std::env::var_os(PRIVATE_TMP_CHILD_MARKER).is_some() {
            let dir = PathBuf::from(std::env::var_os(PRIVATE_TMP_CHILD_DIR).unwrap());
            // SAFETY: this branch runs in a dedicated one-test child process.
            unsafe { libc::umask(0o000) };
            let target = dir.join("new/deep/file.txt");
            create_private_download_parents(&target).unwrap();
            let (tmp, file) = create_private_download_tmp(&target).unwrap();
            drop(file);

            let parent_mode = std::fs::metadata(target.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777;
            assert_eq!(parent_mode, 0o700);
            assert_eq!(file_mode, 0o600);
            std::fs::remove_file(tmp).unwrap();
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("engine::tests::private_download_tmp_and_parents_ignore_permissive_umask")
            .arg("--nocapture")
            .env(PRIVATE_TMP_CHILD_MARKER, "1")
            .env(PRIVATE_TMP_CHILD_DIR, dir.path())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "private temp umask child failed: {status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_download_tmp_refuses_preexisting_symlink_without_touching_victim() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        let tmp = dir.path().join("chosen.tmp");
        let original = b"do not truncate";
        std::fs::write(&victim, original).unwrap();
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();

        let error = open_private_download_tmp(&tmp).unwrap_err();

        assert!(format!("{error:#}").contains("creating private download tmp"));
        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert_eq!(std::fs::read_link(&tmp).unwrap(), victim);
    }

    #[test]
    fn hydration_temp_names_are_reserved_until_atomic_final_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("report.txt");
        for marker in ["tcfs_tmp", "tcfs_symlink_tmp"] {
            let tmp = unique_tmp_path(&target, marker);

            assert_eq!(tmp.parent(), target.parent(), "rename must stay atomic");
            let tmp_name = tmp.file_name().unwrap().to_string_lossy();
            assert!(
                tmp_name.starts_with('.'),
                "temp should be hidden: {tmp_name}"
            );
            assert_eq!(tmp.extension().and_then(|ext| ext.to_str()), Some("tc"));
            assert_eq!(
                Blacklist::default().check(&tmp, false),
                Some(BlacklistReason::StubExtension),
                "watchers and scans must fence the crash-leftover temp"
            );
        }

        let tmp = unique_tmp_path(&target, "tcfs_tmp");

        std::fs::write(&tmp, b"private plaintext").unwrap();
        let while_downloading = collect_files(
            dir.path(),
            &CollectConfig {
                sync_hidden_dirs: true,
                sync_empty_dirs: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            while_downloading.files.is_empty(),
            "temp leaked into full scan: {:?}",
            while_downloading.files
        );

        std::fs::rename(&tmp, &target).unwrap();
        let after_rename = collect_files(dir.path(), &no_empty_dirs_config()).unwrap();
        assert_eq!(after_rename.files, vec![target.clone()]);
        assert_eq!(std::fs::read(target).unwrap(), b"private plaintext");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_hydration_temp_is_reserved_until_atomic_final_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("link");
        let tmp = unique_tmp_path(&target, "tcfs_symlink_tmp");
        std::os::unix::fs::symlink("sibling.txt", &tmp).unwrap();

        let collect_config = CollectConfig {
            sync_hidden_dirs: true,
            preserve_symlinks: true,
            sync_empty_dirs: false,
            ..Default::default()
        };
        let while_downloading = collect_files(dir.path(), &collect_config).unwrap();
        assert!(
            while_downloading.symlinks.is_empty(),
            "symlink temp leaked into full scan: {:?}",
            while_downloading.symlinks
        );

        std::fs::rename(&tmp, &target).unwrap();
        let after_rename = collect_files(dir.path(), &collect_config).unwrap();
        assert_eq!(after_rename.symlinks, vec![target.clone()]);
        assert_eq!(
            std::fs::read_link(target).unwrap(),
            Path::new("sibling.txt")
        );
    }

    #[test]
    fn streaming_upload_snapshot_keeps_chunk_metadata() {
        let data: Vec<u8> = (0u64..524288)
            .map(|i| (i.wrapping_mul(19) ^ (i >> 8)) as u8)
            .collect();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &data).unwrap();

        let snapshot = prepare_upload_snapshot(tmp.path(), true).unwrap();

        assert_eq!(snapshot.file_size, data.len() as u64);
        assert_eq!(
            snapshot.file_hash_hex,
            tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data))
        );
        let UploadSourceSnapshot::Streaming(chunks) = snapshot.source else {
            panic!("streaming snapshot should keep chunk metadata");
        };
        assert_eq!(
            chunks.iter().map(|chunk| chunk.length).sum::<usize>(),
            data.len()
        );
    }

    #[test]
    fn verified_snapshot_chunk_refuses_mutated_source_bytes() {
        let data: Vec<u8> = (0u64..524288)
            .map(|i| (i.wrapping_mul(23) ^ (i >> 6)) as u8)
            .collect();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &data).unwrap();

        let snapshot = prepare_upload_snapshot(tmp.path(), true).unwrap();
        let UploadSourceSnapshot::Streaming(chunks) = snapshot.source else {
            panic!("streaming snapshot should keep chunk metadata");
        };
        assert!(!chunks.is_empty());

        let mut mutated = data;
        mutated[0] ^= 0xff;
        std::fs::write(tmp.path(), mutated).unwrap();

        let mut file = std::fs::File::open(tmp.path()).unwrap();
        let err =
            read_verified_snapshot_chunk_from(&mut file, tmp.path(), &chunks[0], 0).unwrap_err();
        assert!(
            err.to_string()
                .contains("file changed during streaming upload"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn upload_chunk_concurrency_env_is_bounded() {
        assert_eq!(
            upload_chunk_concurrency_from_env_value(None),
            DEFAULT_UPLOAD_CHUNK_CONCURRENCY
        );
        assert_eq!(
            upload_chunk_concurrency_from_env_value(Some("")),
            DEFAULT_UPLOAD_CHUNK_CONCURRENCY
        );
        assert_eq!(
            upload_chunk_concurrency_from_env_value(Some("not-a-number")),
            DEFAULT_UPLOAD_CHUNK_CONCURRENCY
        );
        assert_eq!(
            upload_chunk_concurrency_from_env_value(Some("0")),
            DEFAULT_UPLOAD_CHUNK_CONCURRENCY
        );
        assert_eq!(upload_chunk_concurrency_from_env_value(Some("7")), 7);
        assert_eq!(
            upload_chunk_concurrency_from_env_value(Some("999")),
            MAX_UPLOAD_CHUNK_CONCURRENCY
        );
    }

    #[test]
    fn download_chunk_retries_env_is_bounded() {
        assert_eq!(
            download_chunk_retries_from_env_value(None),
            DEFAULT_DOWNLOAD_CHUNK_RETRIES
        );
        assert_eq!(
            download_chunk_retries_from_env_value(Some("")),
            DEFAULT_DOWNLOAD_CHUNK_RETRIES
        );
        assert_eq!(
            download_chunk_retries_from_env_value(Some("not-a-number")),
            DEFAULT_DOWNLOAD_CHUNK_RETRIES
        );
        assert_eq!(
            download_chunk_retries_from_env_value(Some("0")),
            DEFAULT_DOWNLOAD_CHUNK_RETRIES
        );
        assert_eq!(download_chunk_retries_from_env_value(Some("7")), 7);
        assert_eq!(
            download_chunk_retries_from_env_value(Some("999")),
            MAX_DOWNLOAD_CHUNK_RETRIES
        );
    }

    #[test]
    fn upload_file_concurrency_env_is_bounded() {
        assert_eq!(
            upload_file_concurrency_from_env_value(None),
            DEFAULT_UPLOAD_FILE_CONCURRENCY
        );
        assert_eq!(
            upload_file_concurrency_from_env_value(Some("")),
            DEFAULT_UPLOAD_FILE_CONCURRENCY
        );
        assert_eq!(
            upload_file_concurrency_from_env_value(Some("not-a-number")),
            DEFAULT_UPLOAD_FILE_CONCURRENCY
        );
        assert_eq!(
            upload_file_concurrency_from_env_value(Some("0")),
            DEFAULT_UPLOAD_FILE_CONCURRENCY
        );
        assert_eq!(upload_file_concurrency_from_env_value(Some("7")), 7);
        assert_eq!(
            upload_file_concurrency_from_env_value(Some("999")),
            MAX_UPLOAD_FILE_CONCURRENCY
        );
    }

    #[test]
    fn file_concurrency_requires_fresh_prefix_and_plaintext_uploads() {
        assert!(!should_upload_files_concurrently(
            UploadRuntimeOptions {
                assume_fresh_prefix: false,
                file_upload_concurrency: 8,
            },
            false,
        ));
        assert!(!should_upload_files_concurrently(
            UploadRuntimeOptions {
                assume_fresh_prefix: true,
                file_upload_concurrency: 1,
            },
            false,
        ));
        assert!(!should_upload_files_concurrently(
            UploadRuntimeOptions {
                assume_fresh_prefix: true,
                file_upload_concurrency: 8,
            },
            true,
        ));
        assert!(should_upload_files_concurrently(
            UploadRuntimeOptions {
                assume_fresh_prefix: true,
                file_upload_concurrency: 8,
            },
            false,
        ));
    }

    #[test]
    fn upload_assume_fresh_prefix_env_is_strictly_opt_in() {
        assert!(!upload_assume_fresh_prefix_from_env_value(None));
        assert!(!upload_assume_fresh_prefix_from_env_value(Some("")));
        assert!(!upload_assume_fresh_prefix_from_env_value(Some("0")));
        assert!(!upload_assume_fresh_prefix_from_env_value(Some("false")));
        assert!(!upload_assume_fresh_prefix_from_env_value(Some("TRUE")));
        assert!(upload_assume_fresh_prefix_from_env_value(Some("1")));
        assert!(upload_assume_fresh_prefix_from_env_value(Some("true")));
        assert!(upload_assume_fresh_prefix_from_env_value(Some(" yes ")));
        assert!(upload_assume_fresh_prefix_from_env_value(Some("on")));
    }

    #[test]
    fn upload_progress_every_chunks_env_defaults_to_disabled() {
        assert_eq!(upload_progress_every_chunks_from_env_value(None), 0);
        assert_eq!(upload_progress_every_chunks_from_env_value(Some("")), 0);
        assert_eq!(
            upload_progress_every_chunks_from_env_value(Some("not-a-number")),
            0
        );
        assert_eq!(upload_progress_every_chunks_from_env_value(Some("0")), 0);
        assert_eq!(
            upload_progress_every_chunks_from_env_value(Some("5000")),
            5000
        );
    }

    #[test]
    fn upload_progress_heartbeat_env_defaults_to_disabled() {
        assert_eq!(upload_progress_heartbeat_from_env_value(None), None);
        assert_eq!(upload_progress_heartbeat_from_env_value(Some("")), None);
        assert_eq!(
            upload_progress_heartbeat_from_env_value(Some("not-a-number")),
            None
        );
        assert_eq!(upload_progress_heartbeat_from_env_value(Some("0")), None);
        assert_eq!(
            upload_progress_heartbeat_from_env_value(Some("15")),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            upload_progress_heartbeat_from_env_value(Some("999999")),
            Some(Duration::from_secs(MAX_UPLOAD_PROGRESS_HEARTBEAT_SECS))
        );
    }

    #[test]
    fn chunk_upload_progress_records_interval_and_large_final_rows() {
        assert!(!should_record_chunk_upload_progress(1, 1, 0));
        assert!(!should_record_chunk_upload_progress(1, 4999, 5000));
        assert!(!should_record_chunk_upload_progress(4999, 4999, 5000));
        assert!(should_record_chunk_upload_progress(5000, 10001, 5000));
        assert!(should_record_chunk_upload_progress(10000, 10001, 5000));
        assert!(should_record_chunk_upload_progress(10001, 10001, 5000));
    }

    // ── collect_files (empty dir detection) ──────────────────────────────
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

    #[test]
    fn collect_applies_security_deny_set_with_hidden_dirs_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join(".claude/projects/demo")).unwrap();
        std::fs::create_dir_all(root.join(".claude/projects/demo/.ssh")).unwrap();
        std::fs::create_dir_all(root.join(".claude/projects/demo/.config/sops-nix/secrets"))
            .unwrap();
        std::fs::write(root.join(".claude/projects/demo/session.jsonl"), b"ok").unwrap();
        std::fs::write(root.join(".claude/projects/demo/notes.md"), b"ok").unwrap();
        std::fs::write(root.join(".claude/projects/demo/.env"), b"secret").unwrap();
        std::fs::write(root.join(".claude/projects/demo/logs_2.sqlite"), b"db").unwrap();
        std::fs::write(root.join(".claude/projects/demo/opencode.db-wal"), b"db").unwrap();
        std::fs::write(
            root.join(".claude/projects/demo/.credentials.json"),
            b"secret",
        )
        .unwrap();
        std::fs::write(root.join(".claude/projects/demo/auth.json"), b"secret").unwrap();
        std::fs::write(
            root.join(".claude/projects/demo/.ssh/id_ed25519"),
            b"secret",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/projects/demo/.config/sops-nix/secrets/database"),
            b"secret",
        )
        .unwrap();

        let result = collect_files(
            root,
            &CollectConfig {
                sync_hidden_dirs: true,
                sync_git_dirs: true,
                git_sync_mode: "raw".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let files = rel_names(&result.files, root);
        assert_eq!(
            files,
            vec![
                ".claude/projects/demo/notes.md".to_string(),
                ".claude/projects/demo/session.jsonl".to_string(),
            ]
        );
    }

    #[test]
    fn collect_raw_git_fences_worktree_admin_and_gitfile_pointers() {
        // G5 / TIN-1620 worktree fence: raw .git roam must never collect the
        // per-worktree admin area or gitfile pointers, while regular .git
        // internals still roam.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Main repo with a linked-worktree admin area.
        std::fs::create_dir_all(root.join("repo/.git/objects/ab")).unwrap();
        std::fs::create_dir_all(root.join("repo/.git/refs/heads")).unwrap();
        std::fs::create_dir_all(root.join("repo/.git/worktrees/wt-fence")).unwrap();
        std::fs::write(root.join("repo/.git/HEAD"), b"ref: refs/heads/main").unwrap();
        std::fs::write(root.join("repo/.git/objects/ab/cdef"), b"obj").unwrap();
        std::fs::write(root.join("repo/.git/refs/heads/main"), b"abc").unwrap();
        std::fs::write(
            root.join("repo/.git/worktrees/wt-fence/gitdir"),
            b"/abs/path/wt-linked/.git\n",
        )
        .unwrap();
        std::fs::write(
            root.join("repo/.git/worktrees/wt-fence/HEAD"),
            b"ref: refs/heads/fence",
        )
        .unwrap();
        std::fs::write(root.join("repo/src.rs"), b"fn main() {}").unwrap();

        // Linked worktree: `.git` is a FILE containing `gitdir: <abs path>`.
        std::fs::create_dir_all(root.join("wt-linked")).unwrap();
        std::fs::write(
            root.join("wt-linked/.git"),
            b"gitdir: /abs/path/repo/.git/worktrees/wt-fence\n",
        )
        .unwrap();
        std::fs::write(root.join("wt-linked/notes.md"), b"work").unwrap();

        // Submodule-shaped gitfile pointer in a subdir.
        std::fs::create_dir_all(root.join("repo/vendor/dep")).unwrap();
        std::fs::write(
            root.join("repo/vendor/dep/.git"),
            b"gitdir: ../../.git/modules/dep\n",
        )
        .unwrap();

        let result = collect_files(
            root,
            &CollectConfig {
                sync_hidden_dirs: true,
                sync_git_dirs: true,
                git_sync_mode: "raw".into(),
                sync_empty_dirs: false,
                ..Default::default()
            },
        )
        .unwrap();

        let files = rel_names(&result.files, root);
        // Regular .git internals + working files still roam (no raw regression).
        assert!(files.contains(&"repo/.git/HEAD".to_string()), "{files:?}");
        assert!(files.contains(&"repo/.git/objects/ab/cdef".to_string()));
        assert!(files.contains(&"repo/.git/refs/heads/main".to_string()));
        assert!(files.contains(&"repo/src.rs".to_string()));
        assert!(files.contains(&"wt-linked/notes.md".to_string()));
        // Fenced: nothing under .git/worktrees/, and no gitfile pointers.
        assert!(
            files.iter().all(|f| !f.contains(".git/worktrees")),
            "worktrees admin area must never be collected: {files:?}"
        );
        assert!(
            files
                .iter()
                .all(|f| f != "wt-linked/.git" && f != "repo/vendor/dep/.git"),
            "gitfile pointers must never be collected: {files:?}"
        );
    }

    #[test]
    fn collect_refuses_security_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".ssh");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("id_ed25519"), b"secret").unwrap();

        let result = collect_files(
            &root,
            &CollectConfig {
                sync_hidden_dirs: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(result.files.is_empty());
        assert!(result.symlinks.is_empty());
        assert!(result.empty_dirs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn collect_refuses_security_symlink_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(root.join(".ssh/id_ed25519"), b"secret").unwrap();
        std::fs::write(root.join("session.jsonl"), b"ok").unwrap();
        std::os::unix::fs::symlink("session.jsonl", root.join("session-link")).unwrap();
        std::os::unix::fs::symlink(".ssh/id_ed25519", root.join("key-link")).unwrap();

        let result = collect_files(
            root,
            &CollectConfig {
                sync_hidden_dirs: true,
                preserve_symlinks: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(rel_names(&result.files, root), vec!["session.jsonl"]);
        assert_eq!(rel_names(&result.symlinks, root), vec!["session-link"]);
    }

    // ── symlink restore ingress guard (TIN-1737) ─────────────────────────

    /// Publish a `SymlinkManifest` for `target` into a memory operator at a
    /// deterministic manifest path and return that path.
    async fn publish_symlink_manifest(op: &Operator, hash: &str, target: &str) -> String {
        publish_symlink_manifest_for_rel(op, hash, target, "link").await
    }

    async fn publish_symlink_manifest_for_rel(
        op: &Operator,
        hash: &str,
        target: &str,
        rel_path: &str,
    ) -> String {
        let manifest = SymlinkManifest::new(
            target.to_string(),
            VectorClock::new(),
            "hostile-peer".to_string(),
            0,
            Some(rel_path.to_string()),
        );
        let manifest_path = format!("data/manifests/{hash}");
        op.write(&manifest_path, manifest.to_bytes().unwrap())
            .await
            .unwrap();
        manifest_path
    }

    async fn publish_bound_symlink_manifest_for_rel(
        op: &Operator,
        target: &str,
        rel_path: &str,
    ) -> String {
        let manifest = SymlinkManifest::new(
            target.to_string(),
            VectorClock::new(),
            "peer".to_string(),
            0,
            Some(rel_path.to_string()),
        );
        let bytes = manifest.to_bytes().unwrap();
        let manifest_path = manifest_key("data/manifests", &manifest_object_id(&bytes));
        op.write(&manifest_path, bytes).await.unwrap();
        manifest_path
    }

    /// Drive the legacy direct-manifest wrapper used outside reconcile.
    async fn restore_symlink(
        op: &Operator,
        manifest_path: &str,
        local_path: &Path,
    ) -> Result<DownloadResult> {
        download_file_with_device(op, manifest_path, local_path, "data", None, "", None, None).await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_restore_refuses_parent_traversal_target() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/sub/link");
        let mp = publish_symlink_manifest(&op, "trav", "../../.ssh/authorized_keys").await;

        let error = restore_symlink(&op, &mp, &local).await.unwrap_err();
        assert!(format!("{error:#}").contains("refusing to restore symlink"));
        assert!(!local.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_restore_refuses_absolute_target() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        let mp = publish_symlink_manifest(&op, "abs", "/etc/passwd").await;

        let error = restore_symlink(&op, &mp, &local).await.unwrap_err();
        assert!(format!("{error:#}").contains("refusing to restore symlink"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_restore_refuses_deny_set_target() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        // In-root relative target that lands inside a security deny-set dir.
        let mp = publish_symlink_manifest(&op, "deny", ".gnupg/x").await;

        let error = restore_symlink(&op, &mp, &local).await.unwrap_err();
        assert!(format!("{error:#}").contains("refusing to restore symlink"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_restore_allows_benign_in_root_target() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/sub/link");
        // Sibling file within the same directory — legitimate relative target.
        let mp = publish_symlink_manifest(&op, "ok", "sibling.txt").await;

        restore_symlink(&op, &mp, &local).await.unwrap();
        let read_back = std::fs::read_link(&local).unwrap();
        assert_eq!(read_back, Path::new("sibling.txt"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checked_regular_index_rejects_symlink_manifest_without_mutation() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        let manifest = publish_symlink_manifest(&op, "kind-symlink", "sibling.txt").await;

        let error = download_file_with_device_checked(
            &op,
            &manifest,
            &local,
            "data",
            None,
            "",
            None,
            None,
            "link",
            RemoteEntryKind::RegularFile,
            None,
            0,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("manifest kind mismatch"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[tokio::test]
    async fn checked_regular_index_rejects_hybrid_symlink_envelope() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/file");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "kind": "symlink",
            "symlink_target": "sibling.txt",
            "file_hash": "file",
            "file_size": 0,
            "chunks": [],
            "vclock": { "clocks": {} },
            "written_by": "peer",
            "written_at": 0,
            "rel_path": "file"
        }))
        .unwrap();
        let object_id = manifest_object_id(&bytes);
        let manifest = manifest_key("data/manifests", &object_id);
        op.write(&manifest, bytes).await.unwrap();

        let error = download_file_with_device_checked(
            &op,
            &manifest,
            &local,
            "data",
            None,
            "",
            None,
            None,
            "file",
            RemoteEntryKind::RegularFile,
            None,
            0,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("symlink-discriminated envelope"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checked_symlink_index_rejects_regular_manifest_without_mutation() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        op.write(
            "data/manifests/kind-regular",
            test_manifest_bytes("file", 0),
        )
        .await
        .unwrap();

        let error = download_file_with_device_checked(
            &op,
            "data/manifests/kind-regular",
            &local,
            "data",
            None,
            "",
            None,
            None,
            "link",
            RemoteEntryKind::Symlink,
            Some("sibling.txt"),
            11,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("parsing symlink manifest"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checked_symlink_target_mismatch_does_not_mutate_local_path() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        let manifest = publish_symlink_manifest(&op, "target-mismatch", "actual.txt").await;
        let mut state = StateCache::open(&tmp.path().join("state.json")).unwrap();

        let error = download_file_with_device_checked(
            &op,
            &manifest,
            &local,
            "data",
            None,
            "neo",
            Some(&mut state),
            None,
            "link",
            RemoteEntryKind::Symlink,
            Some("declared.txt"),
            12,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("symlink target mismatch"));
        assert!(std::fs::symlink_metadata(&local).is_err());
        assert_eq!(state.len(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checked_non_git_symlink_hydrates_when_kind_and_target_match() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        let manifest = publish_bound_symlink_manifest_for_rel(&op, "sibling.txt", "link").await;

        download_file_with_device_checked(
            &op,
            &manifest,
            &local,
            "data",
            None,
            "",
            None,
            None,
            "link",
            RemoteEntryKind::Symlink,
            Some("sibling.txt"),
            11,
            0,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_link(&local).unwrap(),
            Path::new("sibling.txt")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checked_symlink_refuses_ascii_case_git_alias_without_mutation() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/.GIT/HEAD");
        let manifest = publish_symlink_manifest(&op, "git-alias-symlink", "../outside").await;

        let error = download_file_with_device_checked(
            &op,
            &manifest,
            &local,
            "data",
            None,
            "",
            None,
            None,
            ".GIT/HEAD",
            RemoteEntryKind::Symlink,
            Some("../outside"),
            10,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("non-canonical alias of reserved .git"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[tokio::test]
    async fn checked_regular_download_rejects_wrong_bytes_under_new_object_key() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/file");
        let good_bytes = bound_test_manifest_bytes("good-file", 0, "file");
        let object_id = manifest_object_id(&good_bytes);
        let object_key = manifest_key("data/manifests", &object_id);
        let wrong_bytes = bound_test_manifest_bytes("different-file", 0, "file");
        op.write(&object_key, wrong_bytes).await.unwrap();

        let error = download_file_with_device_checked(
            &op,
            &object_key,
            &local,
            "data",
            None,
            "",
            None,
            None,
            "file",
            RemoteEntryKind::RegularFile,
            None,
            0,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("object identity mismatch"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[tokio::test]
    async fn forged_empty_manifest_preserves_existing_bytes_and_state_cache() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/file");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, b"keep-local-bytes").unwrap();

        let mut state = StateCache::open(&tmp.path().join("state.json")).unwrap();
        let mut clock = VectorClock::new();
        clock.tick("neo");
        let tracked = make_sync_state_full(
            &local,
            "tracked-hash".into(),
            1,
            "data/manifests/tracked".into(),
            clock,
            "neo".into(),
        )
        .unwrap();
        state.set(&local, tracked);

        let forged = bound_test_manifest_bytes("not-the-empty-hash", 0, "file");
        let object_id = manifest_object_id(&forged);
        let object_key = manifest_key("data/manifests", &object_id);
        op.write(&object_key, forged).await.unwrap();

        let error = download_file_with_device_checked(
            &op,
            &object_key,
            &local,
            "data",
            None,
            "neo",
            Some(&mut state),
            None,
            "file",
            RemoteEntryKind::RegularFile,
            None,
            0,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("empty file integrity check failed"));
        assert_eq!(std::fs::read(&local).unwrap(), b"keep-local-bytes");
        let still_tracked = state.get(&local).unwrap();
        assert_eq!(still_tracked.blake3, "tracked-hash");
        assert_eq!(still_tracked.chunk_count, 1);
        assert_eq!(still_tracked.remote_path, "data/manifests/tracked");
        assert_eq!(still_tracked.vclock.get("neo"), 1);

        let size_error = download_file_with_device_checked(
            &op,
            &object_key,
            &local,
            "data",
            None,
            "neo",
            Some(&mut state),
            None,
            "file",
            RemoteEntryKind::RegularFile,
            None,
            1,
            0,
        )
        .await
        .unwrap_err();
        assert!(format!("{size_error:#}").contains("index/manifest size mismatch"));

        let chunk_error = download_file_with_device_checked(
            &op,
            &object_key,
            &local,
            "data",
            None,
            "neo",
            Some(&mut state),
            None,
            "file",
            RemoteEntryKind::RegularFile,
            None,
            0,
            1,
        )
        .await
        .unwrap_err();
        assert!(format!("{chunk_error:#}").contains("index/manifest chunk-count mismatch"));
        assert_eq!(std::fs::read(&local).unwrap(), b"keep-local-bytes");
        assert_eq!(state.get(&local).unwrap().blake3, "tracked-hash");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checked_symlink_download_rejects_wrong_bytes_under_new_object_key() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("root/link");
        let good = SymlinkManifest::new(
            "sibling.txt",
            VectorClock::new(),
            "peer".into(),
            0,
            Some("link".into()),
        )
        .to_bytes()
        .unwrap();
        let object_id = manifest_object_id(&good);
        let object_key = manifest_key("data/manifests", &object_id);
        let wrong = SymlinkManifest::new(
            "sibling.txt",
            VectorClock::new(),
            "peer".into(),
            1,
            Some("link".into()),
        )
        .to_bytes()
        .unwrap();
        op.write(&object_key, wrong).await.unwrap();

        let error = download_file_with_device_checked(
            &op,
            &object_key,
            &local,
            "data",
            None,
            "",
            None,
            None,
            "link",
            RemoteEntryKind::Symlink,
            Some("sibling.txt"),
            11,
            0,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("object identity mismatch"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_writer_addresses_manifest_bytes_but_keeps_target_state_identity() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("link");
        std::os::unix::fs::symlink("sibling.txt", &local).unwrap();
        let mut state = StateCache::open(&tmp.path().join("state.json")).unwrap();

        let result = upload_symlink_with_device(&op, &local, "data", &mut state, "neo", "link")
            .await
            .unwrap();
        let parsed =
            parse_index_entry_record(&op.read("data/index/link").await.unwrap().to_vec()).unwrap();
        let entry = parsed.visible_entry().unwrap();
        let bytes = op
            .read(&manifest_key("data/manifests", &entry.manifest_hash))
            .await
            .unwrap()
            .to_vec();

        assert_eq!(entry.manifest_hash, manifest_object_id(&bytes));
        assert_eq!(result.hash, symlink_manifest_hash("sibling.txt"));
        assert_eq!(
            result.remote_path,
            manifest_key("data/manifests", &entry.manifest_hash)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_upload_rejects_unbound_logical_parent_target() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("target.txt"), b"target").unwrap();
        let local = nested.join("link");
        std::os::unix::fs::symlink("../target.txt", &local).unwrap();
        let mut state = StateCache::open(&tmp.path().join("state.json")).unwrap();

        let error =
            upload_symlink_with_device(&op, &local, "data", &mut state, "neo", "nested/link")
                .await
                .unwrap_err();

        assert!(
            format!("{error:#}").contains("crosses admitted root boundary"),
            "unexpected rejection: {error:#}"
        );
        assert!(!op.exists("data/index/nested/link").await.unwrap());
    }

    #[test]
    fn typed_content_identity_never_equates_regular_file_and_symlink() {
        let clock = VectorClock::new();
        let raw_identity = "same-raw-identity";
        let outcome = compare_typed_content_clocks(
            &clock,
            &clock,
            RemoteEntryKind::RegularFile,
            raw_identity,
            &logical_content_identity(RemoteEntryKind::Symlink, raw_identity),
            raw_identity,
            "entry",
            "local",
            "remote",
        );

        let SyncOutcome::Conflict(conflict) = outcome else {
            panic!("different object kinds with equal raw identities must conflict");
        };
        assert_eq!(conflict.local_blake3, raw_identity);
        assert_eq!(conflict.remote_blake3, raw_identity);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sequential_symlink_to_regular_transition_is_local_newer() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("entry");
        let mut state = StateCache::open(&tmp.path().join("state.json")).unwrap();

        std::os::unix::fs::symlink("sibling.txt", &local).unwrap();
        upload_symlink_with_device(&op, &local, "data", &mut state, "device-a", "entry")
            .await
            .unwrap();

        std::fs::remove_file(&local).unwrap();
        std::fs::write(&local, b"causally newer regular replacement").unwrap();
        let result = upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "device-a",
            Some("entry"),
            None,
        )
        .await
        .unwrap();

        assert!(!result.skipped);
        assert!(matches!(result.outcome, Some(SyncOutcome::LocalNewer)));
        let entry = parse_index_entry_record(&op.read("data/index/entry").await.unwrap().to_vec())
            .unwrap()
            .visible_entry()
            .unwrap()
            .clone();
        assert_eq!(entry.kind, RemoteEntryKind::RegularFile);
        let manifest = SyncManifest::from_bytes(
            &op.read(&manifest_key("data/manifests", &entry.manifest_hash))
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(manifest.vclock.get("device-a"), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sequential_regular_to_symlink_transition_is_local_newer() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("entry");
        let mut state = StateCache::open(&tmp.path().join("state.json")).unwrap();

        std::fs::write(&local, b"regular base").unwrap();
        upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "device-a",
            Some("entry"),
            None,
        )
        .await
        .unwrap();

        std::fs::remove_file(&local).unwrap();
        std::os::unix::fs::symlink("sibling.txt", &local).unwrap();
        let result =
            upload_symlink_with_device(&op, &local, "data", &mut state, "device-a", "entry")
                .await
                .unwrap();

        assert!(!result.skipped);
        assert!(matches!(result.outcome, Some(SyncOutcome::LocalNewer)));
        let entry = parse_index_entry_record(&op.read("data/index/entry").await.unwrap().to_vec())
            .unwrap()
            .visible_entry()
            .unwrap()
            .clone();
        assert_eq!(entry.kind, RemoteEntryKind::Symlink);
        let manifest = SymlinkManifest::from_bytes(
            &op.read(&manifest_key("data/manifests", &entry.manifest_hash))
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(manifest.vclock.get("device-a"), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_regular_against_symlink_transition_conflicts_without_overwrite() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let seed = tmp.path().join("seed/entry");
        let device_a = tmp.path().join("device-a/entry");
        let device_b = tmp.path().join("device-b/entry");
        for path in [&seed, &device_a, &device_b] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"shared regular base").unwrap();
        }

        let mut seed_state = StateCache::open(&tmp.path().join("seed-state.json")).unwrap();
        upload_file_with_device(
            &op,
            &seed,
            "data",
            &mut seed_state,
            None,
            "seed",
            Some("entry"),
            None,
        )
        .await
        .unwrap();
        let shared_base = seed_state.get(&seed).unwrap().clone();

        let mut state_a = StateCache::open(&tmp.path().join("a-state.json")).unwrap();
        let mut state_b = StateCache::open(&tmp.path().join("b-state.json")).unwrap();
        state_a.set(&device_a, shared_base.clone());
        state_b.set(&device_b, shared_base);

        std::fs::remove_file(&device_a).unwrap();
        std::os::unix::fs::symlink("winner.txt", &device_a).unwrap();
        let winner =
            upload_symlink_with_device(&op, &device_a, "data", &mut state_a, "device-a", "entry")
                .await
                .unwrap();
        assert!(matches!(winner.outcome, Some(SyncOutcome::LocalNewer)));
        let winner_entry =
            parse_index_entry_record(&op.read("data/index/entry").await.unwrap().to_vec())
                .unwrap()
                .visible_entry()
                .unwrap()
                .clone();

        std::fs::write(&device_b, b"concurrent regular edit from device b").unwrap();
        let result = upload_file_with_device(
            &op,
            &device_b,
            "data",
            &mut state_b,
            None,
            "device-b",
            Some("entry"),
            None,
        )
        .await
        .unwrap();

        assert!(result.skipped);
        let SyncOutcome::Conflict(conflict) = result.outcome.unwrap() else {
            panic!("concurrent cross-kind edit must conflict");
        };
        assert_eq!(conflict.remote_blake3, symlink_manifest_hash("winner.txt"));
        assert_eq!(
            state_b.get(&device_b).unwrap().status,
            FileSyncStatus::Conflict
        );
        let still_current =
            parse_index_entry_record(&op.read("data/index/entry").await.unwrap().to_vec())
                .unwrap()
                .visible_entry()
                .unwrap()
                .clone();
        assert_eq!(still_current, winner_entry);
        assert_eq!(still_current.kind, RemoteEntryKind::Symlink);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sequential_two_device_symlink_edits_preserve_concurrent_winner() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let seed_link = tmp.path().join("seed/link");
        let link_a = tmp.path().join("device-a/link");
        let link_b = tmp.path().join("device-b/link");
        for link in [&seed_link, &link_a, &link_b] {
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink("base.txt", link).unwrap();
        }

        let mut seed_state = StateCache::open(&tmp.path().join("seed-state.json")).unwrap();
        upload_symlink_with_device(&op, &seed_link, "data", &mut seed_state, "seed", "link")
            .await
            .unwrap();
        let shared_base = seed_state.get(&seed_link).unwrap().clone();

        let mut state_a = StateCache::open(&tmp.path().join("a-state.json")).unwrap();
        let mut state_b = StateCache::open(&tmp.path().join("b-state.json")).unwrap();
        state_a.set(&link_a, shared_base.clone());
        state_b.set(&link_b, shared_base);

        std::fs::remove_file(&link_a).unwrap();
        std::os::unix::fs::symlink("from-a.txt", &link_a).unwrap();
        std::fs::remove_file(&link_b).unwrap();
        std::os::unix::fs::symlink("from-b.txt", &link_b).unwrap();

        let result_a =
            upload_symlink_with_device(&op, &link_a, "data", &mut state_a, "device-a", "link")
                .await
                .unwrap();
        assert!(matches!(result_a.outcome, Some(SyncOutcome::LocalNewer)));
        let winner = parse_index_entry_record(&op.read("data/index/link").await.unwrap().to_vec())
            .unwrap()
            .visible_entry()
            .unwrap()
            .clone();

        let result_b =
            upload_symlink_with_device(&op, &link_b, "data", &mut state_b, "device-b", "link")
                .await
                .unwrap();
        assert!(result_b.skipped);
        assert!(matches!(result_b.outcome, Some(SyncOutcome::Conflict(_))));
        assert_eq!(
            state_b.get(&link_b).unwrap().status,
            FileSyncStatus::Conflict
        );

        let still_current =
            parse_index_entry_record(&op.read("data/index/link").await.unwrap().to_vec())
                .unwrap()
                .visible_entry()
                .unwrap()
                .clone();
        assert_eq!(still_current, winner);
        let winner_bytes = op
            .read(&manifest_key(
                "data/manifests",
                &still_current.manifest_hash,
            ))
            .await
            .unwrap()
            .to_vec();
        let winner_manifest = SymlinkManifest::from_bytes(&winner_bytes).unwrap();
        assert_eq!(winner_manifest.symlink_target, "from-a.txt");
        assert_eq!(winner_manifest.vclock.get("seed"), 1);
        assert_eq!(winner_manifest.vclock.get("device-a"), 1);
        assert_eq!(winner_manifest.vclock.get("device-b"), 0);
    }

    #[test]
    fn validate_restored_symlink_target_rules() {
        let link = Path::new("/sync/root/sub/link");
        // Refusals.
        assert!(matches!(
            validate_restored_symlink_target(link, ""),
            Err(SymlinkRejection::Empty)
        ));
        assert!(matches!(
            validate_restored_symlink_target(link, "/etc/passwd"),
            Err(SymlinkRejection::Absolute)
        ));
        assert!(matches!(
            validate_restored_symlink_target(link, "../../.ssh/authorized_keys"),
            // `..`-escape is detected before the deny-set check.
            Err(SymlinkRejection::Traversal)
        ));
        assert!(matches!(
            validate_restored_symlink_target(link, ".gnupg/x"),
            Err(SymlinkRejection::DenySet(_))
        ));
        assert!(matches!(
            validate_restored_symlink_target(link, "nested/.ssh/id_ed25519"),
            Err(SymlinkRejection::DenySet(_))
        ));
        assert!(matches!(
            validate_restored_symlink_target(link, "nested/.SSH/id_ed25519"),
            Err(SymlinkRejection::DenySet(_))
        ));
        assert!(matches!(
            validate_restored_symlink_target(link, "repo/.GIT/config"),
            Err(SymlinkRejection::ReservedGit)
        ));
        // Allowed: benign in-root relative targets.
        assert!(validate_restored_symlink_target(link, "sibling.txt").is_ok());
        assert!(validate_restored_symlink_target(link, "./nested/file").is_ok());
        assert!(validate_restored_symlink_target(link, "a/b/c").is_ok());
        assert!(validate_restored_symlink_target(link, "missing/broken-target").is_ok());
    }

    #[test]
    fn validate_restored_symlink_target_uses_portable_prefix_and_separator_rules() {
        let link = Path::new("/sync/root/sub/link");

        for target in [
            r"C:\outside",
            "C:/outside",
            "C:outside",
            r"\\server\share",
            r"\\?\C:\outside",
            r"\\.\PhysicalDrive0",
            r"\??\C:\outside",
            "//server/share",
            "//?/C:/outside",
        ] {
            assert!(
                matches!(
                    validate_restored_symlink_target(link, target),
                    Err(SymlinkRejection::Absolute)
                ),
                "Windows absolute/prefix form was accepted: {target:?}"
            );
        }

        for target in [r"dir\..\outside", r"nested\file", r"safe/inner\file"] {
            assert!(
                matches!(
                    validate_restored_symlink_target(link, target),
                    Err(SymlinkRejection::NonPortableSeparator)
                ),
                "backslash-bearing target was accepted: {target:?}"
            );
        }
    }

    #[test]
    fn validate_restored_symlink_target_rejects_nonportable_components() {
        let link = Path::new("/sync/root/sub/link");

        for target in [
            "nested/a:b",
            "nested/NUL",
            "nested/COM1.txt",
            "nested/git~1/config",
            "nested/.tcfs_dir/entry",
            "nested/.TCFS_DIR/entry",
            "nested/trailing.",
            "nested/trailing ",
            "nested/question?",
            "nested/re\u{301}sume\u{301}.txt",
            "nested//file",
            "nested/",
        ] {
            assert!(
                matches!(
                    validate_restored_symlink_target(link, target),
                    Err(SymlinkRejection::NonPortableComponent)
                ),
                "non-portable target component was accepted: {target:?}"
            );
        }

        for target in [
            "nested/file",
            "nested/Résumé.txt",
            "./nested/file",
            "../sibling",
        ] {
            assert!(
                !matches!(
                    validate_restored_symlink_target(link, target),
                    Err(SymlinkRejection::NonPortableComponent)
                ),
                "portable target component was rejected: {target:?}"
            );
        }
    }

    #[test]
    fn validate_restored_symlink_target_rejects_controls_on_every_host() {
        let link = Path::new("/sync/root/sub/link");

        for target in [
            "safe\0other",
            "safe\nother",
            "safe\u{1f}other",
            "safe\u{7f}other",
        ] {
            assert!(
                matches!(
                    validate_restored_symlink_target(link, target),
                    Err(SymlinkRejection::ControlCharacter)
                ),
                "control-bearing target was accepted: {target:?}"
            );
        }
    }

    #[test]
    fn validate_indexed_symlink_target_resolves_against_logical_parent_depth() {
        let nested_link = Path::new("a/b/link");

        assert!(validate_indexed_symlink_target(nested_link, "../../sibling").is_ok());
        assert!(validate_indexed_symlink_target(nested_link, "../missing/target").is_ok());
        assert!(matches!(
            validate_symlink_target(
                nested_link,
                "../../../outside",
                SymlinkLinkPathKind::IndexedLogical
            ),
            Err(SymlinkRejection::Traversal)
        ));
        assert!(matches!(
            validate_symlink_target(
                nested_link,
                "../../.git/config",
                SymlinkLinkPathKind::IndexedLogical
            ),
            Err(SymlinkRejection::ReservedGit)
        ));

        // Physical destination paths do not disclose the logical sync root, so
        // their conservative boundary remains the link's own parent.
        assert!(matches!(
            validate_restored_symlink_target(Path::new("/sync/root/a/b/link"), "../sibling"),
            Err(SymlinkRejection::Traversal)
        ));
        assert!(matches!(
            validate_restored_symlink_target(Path::new("sandbox/link"), "../outside"),
            Err(SymlinkRejection::Traversal)
        ));
        assert!(
            validate_indexed_symlink_target(Path::new("/sync/root/a/b/link"), "nested/file")
                .is_err()
        );
        for reserved_link_path in [".tcfs_dir", "nested/.TCFS_DIR/link"] {
            assert!(
                validate_indexed_symlink_target(Path::new(reserved_link_path), "nested/file")
                    .is_err(),
                "reserved indexed link path was accepted: {reserved_link_path:?}"
            );
        }
    }

    // ── normalize_rel_path ───────────────────────────────────────────────

    #[test]
    fn normalize_rel_path_relative_passthrough() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.txt");
        std::fs::write(&file, b"x").unwrap();

        // With sync_root set, file under root → relative
        let result = normalize_rel_path(&file, Some(dir.path()));
        assert_eq!(result, "doc.txt");
    }

    #[test]
    fn normalize_rel_path_nested() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let file = dir.path().join("a/b/deep.txt");
        std::fs::write(&file, b"x").unwrap();

        let result = normalize_rel_path(&file, Some(dir.path()));
        assert_eq!(result, "a/b/deep.txt");
    }

    #[test]
    fn normalize_rel_path_no_sync_root_strips_leading_slash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();

        let result = normalize_rel_path(&file, None);
        // Absolute path should have leading / stripped
        assert!(!result.starts_with('/'), "should strip leading /: {result}");
        assert!(result.ends_with("file.txt"));
    }

    #[test]
    fn normalize_rel_path_normalizes_decomposed_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cafe\u{301}.txt");
        std::fs::write(&file, b"x").unwrap();

        let result = normalize_rel_path(&file, Some(dir.path()));
        assert_eq!(result, "caf\u{e9}.txt");
    }

    // ── resolve_manifest_path ─────────────────────────────────────────��──

    #[tokio::test]
    async fn resolve_manifest_passthrough() {
        let op = memory_op();
        let result = resolve_manifest_path(&op, "data/manifests/abc123", "data", None)
            .await
            .unwrap();
        assert_eq!(result, "data/manifests/abc123");
    }

    #[tokio::test]
    async fn resolve_manifest_rejects_cross_root_or_traversing_explicit_key() {
        let op = memory_op();

        assert!(
            resolve_manifest_path(&op, "other/manifests/abc123", "data", None)
                .await
                .is_err()
        );
        assert!(
            resolve_manifest_path(&op, "data/manifests/../victim", "data", None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn resolve_manifest_from_index() {
        let op = memory_op();
        // Write an index entry
        op.write(
            "data/index/doc.txt",
            RemoteIndexEntry::new("abc123", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/abc123",
            br#"{"version":2,"file_hash":"abc123","file_size":100,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        let result = resolve_manifest_path(&op, "doc.txt", "data", None)
            .await
            .unwrap();
        assert_eq!(result, "data/manifests/abc123");
    }

    #[tokio::test]
    async fn resolve_manifest_filename_search_normalizes_unicode() {
        let op = memory_op();
        op.write(
            "data/index/caf\u{e9}.txt",
            RemoteIndexEntry::new("abc123", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/abc123",
            br#"{"version":2,"file_hash":"abc123","file_size":100,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        let host_a = tempfile::tempdir().unwrap();
        let host_b = tempfile::tempdir().unwrap();
        let input = host_a.path().join("cafe\u{301}.txt");

        let result =
            resolve_manifest_path(&op, &input.to_string_lossy(), "data", Some(host_b.path()))
                .await
                .unwrap();
        assert_eq!(result, "data/manifests/abc123");
    }

    #[tokio::test]
    async fn filename_fallback_matches_exact_final_component_not_suffix() {
        let op = memory_op();
        op.write(
            "data/index/nested/foobar",
            RemoteIndexEntry::new("suffix-object", 0, 0).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write("data/manifests/suffix-object", b"present".to_vec())
            .await
            .unwrap();

        let error = resolve_manifest_reference(&op, "bar", "data", None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("no index entry found"));
    }

    #[tokio::test]
    async fn filename_fallback_rejects_ambiguous_duplicate_final_components() {
        let op = memory_op();
        for (rel_path, object_id) in [("a/foo", "object-a"), ("b/foo", "object-b")] {
            op.write(
                &format!("data/index/{rel_path}"),
                RemoteIndexEntry::new(object_id, 0, 0).to_legacy_bytes(),
            )
            .await
            .unwrap();
            op.write(
                &manifest_key("data/manifests", object_id),
                b"present".to_vec(),
            )
            .await
            .unwrap();
        }

        let error = resolve_manifest_reference(&op, "foo", "data", None)
            .await
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("ambiguous filename fallback"));
        assert!(text.contains("a/foo"));
        assert!(text.contains("b/foo"));
    }

    #[tokio::test]
    async fn resolve_manifest_missing_errors() {
        let op = memory_op();
        let result = resolve_manifest_path(&op, "nonexistent.txt", "data", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn exact_manifest_reference_miss_never_falls_back_by_filename() {
        let op = memory_op();
        let entry = RemoteIndexEntry::new("nested-object", 0, 0);
        op.write("data/manifests/nested-object", b"present".to_vec())
            .await
            .unwrap();
        write_committed_index_entry(&op, "data/index/nested/doc.txt", &entry)
            .await
            .unwrap();

        assert!(resolve_exact_manifest_reference(&op, "doc.txt", "data")
            .await
            .unwrap()
            .is_none());
        let resolved = resolve_exact_manifest_reference(&op, "nested/doc.txt", "data")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.rel_path(), Some("nested/doc.txt"));
        assert_eq!(resolved.manifest_path(), "data/manifests/nested-object");
    }

    #[tokio::test]
    async fn exact_visible_index_selection_does_not_require_manifest_io() {
        let op = memory_op();
        let entry = RemoteIndexEntry::new("missing-manifest", 7, 1);
        write_committed_index_entry(&op, "data/index/docs/versioned.txt", &entry)
            .await
            .unwrap();

        let selected = read_exact_visible_index_selection(&op, "docs/versioned.txt", "data")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected, entry);

        let error = resolve_exact_manifest_reference(&op, "docs/versioned.txt", "data")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("missing manifest"),
            "{error:#}"
        );

        let preparing = crate::index_entry::VersionedIndexEntry::preparing(
            None,
            PendingIndexEntry::new(
                "pending-manifest",
                8,
                1,
                "data/staging/manifests/00000000-0000-4000-8000-000000000000-pending-manifest.json",
            ),
        )
        .to_json_bytes()
        .unwrap();
        op.write("data/index/docs/versioned.txt", preparing.clone())
            .await
            .unwrap();
        assert!(
            read_exact_visible_index_selection(&op, "docs/versioned.txt", "data")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            op.read("data/index/docs/versioned.txt")
                .await
                .unwrap()
                .to_vec(),
            preparing
        );
        assert!(!op.exists("data/manifests/pending-manifest").await.unwrap());
    }

    #[tokio::test]
    async fn exact_indexed_snapshot_miss_never_falls_back_by_filename() {
        let op = memory_op();
        let (bytes, entry) = indexed_test_manifest_for_rel("remote", 0, "nested/doc.txt");
        let manifest_path = manifest_key(&manifest_path_prefix("data"), &entry.manifest_hash);
        op.write(&manifest_path, bytes).await.unwrap();
        write_committed_index_entry(&op, "data/index/nested/doc.txt", &entry)
            .await
            .unwrap();

        assert!(
            resolve_exact_indexed_manifest_snapshot(&op, "doc.txt", "data")
                .await
                .unwrap()
                .is_none()
        );

        let snapshot = resolve_exact_indexed_manifest_snapshot(&op, "nested/doc.txt", "data")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.rel_path(), "nested/doc.txt");
        assert_eq!(snapshot.manifest_path(), manifest_path);
        assert_eq!(snapshot.manifest_object_id(), entry.manifest_hash);
        assert_eq!(snapshot.kind(), RemoteEntryKind::RegularFile);
        assert_eq!(snapshot.content_hash(), "remote");
        assert_eq!(snapshot.size(), 0);
        assert_eq!(snapshot.written_by(), "tester");
        assert!(snapshot.vclock().clocks.is_empty());
    }

    #[tokio::test]
    async fn indexed_snapshot_hydration_uses_captured_manifest_bytes() {
        let op = memory_op();
        let empty_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b""));
        let (bytes, entry) = indexed_test_manifest_for_rel(&empty_hash, 0, "doc.txt");
        let manifest_path = manifest_key(&manifest_path_prefix("data"), &entry.manifest_hash);
        op.write(&manifest_path, bytes).await.unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &entry)
            .await
            .unwrap();
        let snapshot = resolve_exact_indexed_manifest_snapshot(&op, "doc.txt", "data")
            .await
            .unwrap()
            .unwrap();

        // Simulate an object-store violation after resolution. Hydration must
        // neither reread nor silently switch away from the captured bytes.
        op.write(&manifest_path, b"corrupt replacement".to_vec())
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("doc.txt");
        let expected = capture_local_fingerprint(&local).unwrap();

        hydrate_indexed_snapshot_with_device(
            &op, &snapshot, &local, None, "", None, None, &expected,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"");
    }

    #[tokio::test]
    async fn indexed_snapshot_hydration_refuses_local_edit_before_replace() {
        let op = memory_op();
        let empty_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b""));
        let (bytes, entry) = indexed_test_manifest_for_rel(&empty_hash, 0, "doc.txt");
        let manifest_path = manifest_key(&manifest_path_prefix("data"), &entry.manifest_hash);
        op.write(&manifest_path, bytes).await.unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &entry)
            .await
            .unwrap();
        let snapshot = resolve_exact_indexed_manifest_snapshot(&op, "doc.txt", "data")
            .await
            .unwrap()
            .unwrap();

        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("doc.txt");
        std::fs::write(&local, b"tracked").unwrap();
        let expected = capture_local_fingerprint(&local).unwrap();
        std::fs::write(&local, b"edited outside tcfs").unwrap();

        let error = hydrate_indexed_snapshot_with_device(
            &op, &snapshot, &local, None, "", None, None, &expected,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("local fingerprint changed"));
        assert_eq!(std::fs::read(&local).unwrap(), b"edited outside tcfs");
    }

    #[tokio::test]
    async fn indexed_snapshot_hydration_refuses_index_advance_before_replace() {
        let op = memory_op();
        let empty_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b""));
        let (bytes_b, entry_b) = indexed_test_manifest_for_rel(&empty_hash, 0, "doc.txt");
        let manifest_b = manifest_key(&manifest_path_prefix("data"), &entry_b.manifest_hash);
        op.write(&manifest_b, bytes_b).await.unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &entry_b)
            .await
            .unwrap();
        let snapshot = resolve_exact_indexed_manifest_snapshot(&op, "doc.txt", "data")
            .await
            .unwrap()
            .unwrap();

        let (bytes_c, entry_c) = indexed_test_manifest_for_rel("next", 0, "doc.txt");
        let manifest_c = manifest_key(&manifest_path_prefix("data"), &entry_c.manifest_hash);
        op.write(&manifest_c, bytes_c).await.unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &entry_c)
            .await
            .unwrap();

        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("doc.txt");
        let expected = ExpectedLocalFingerprint::Absent;
        let error = hydrate_indexed_snapshot_with_device(
            &op, &snapshot, &local, None, "", None, None, &expected,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("snapshot authority changed"));
        assert!(std::fs::symlink_metadata(&local).is_err());
    }

    // ── delete_remote_file ───────────────────────────────────────────────

    #[tokio::test]
    async fn delete_remote_file_tombstones_index_and_preserves_manifest_evidence() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        // Write index and manifest
        op.write(
            "data/index/file.txt",
            RemoteIndexEntry::new("abc123", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/abc123",
            br#"{"version":2,"file_hash":"abc123","file_size":100,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        delete_remote_file(&op, "file.txt", "data", &mut state, None)
            .await
            .unwrap();

        let deleted =
            crate::index_entry::read_index_entry_record_from_store(&op, "data/index/file.txt")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            deleted.state(),
            crate::index_entry::IndexEntryState::Deleted
        );
        assert!(deleted.visible_entry().is_none());
        assert!(op.read("data/manifests/abc123").await.is_ok());
    }

    #[tokio::test]
    async fn delete_remote_file_preserves_manifest_shared_by_another_index() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        op.write(
            "data/index/a.txt",
            RemoteIndexEntry::new("abc123", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/index/b.txt",
            RemoteIndexEntry::new("abc123", 100, 1).to_legacy_bytes(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/abc123",
            br#"{"version":2,"file_hash":"abc123","file_size":100,"chunks":[],"vclock":{"clocks":{}},"written_by":"neo","written_at":0}"#.to_vec(),
        )
        .await
        .unwrap();

        delete_remote_file(&op, "a.txt", "data", &mut state, None)
            .await
            .unwrap();

        let deleted =
            crate::index_entry::read_index_entry_record_from_store(&op, "data/index/a.txt")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            deleted.state(),
            crate::index_entry::IndexEntryState::Deleted
        );
        assert!(deleted.visible_entry().is_none());
        assert!(op.read("data/index/b.txt").await.is_ok());
        assert!(op.read("data/manifests/abc123").await.is_ok());
    }

    #[tokio::test]
    async fn delete_rejects_cross_root_staged_key_without_touching_victim_or_index() {
        let op = memory_op();
        let index_key = "data/index/doc.txt";
        let victim_key = "other/staging/manifests/00000000-0000-4000-8000-000000000011-new456.json";
        let victim_bytes = b"other root private object".to_vec();
        op.write(victim_key, victim_bytes.clone()).await.unwrap();
        write_preparing_index_entry(
            &op,
            index_key,
            Some(RemoteIndexEntry::new("old123", 10, 1)),
            PendingIndexEntry::new("new456", 11, 1, victim_key),
        )
        .await
        .unwrap();
        let original_index = op.read(index_key).await.unwrap().to_vec();

        let error = delete_remote_index_entry(&op, "doc.txt", "data")
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("escapes its root staging namespace"));
        assert_eq!(op.read(victim_key).await.unwrap().to_vec(), victim_bytes);
        assert_eq!(op.read(index_key).await.unwrap().to_vec(), original_index);
    }

    #[tokio::test]
    async fn delete_rejects_unsafe_relative_path_before_remote_mutation() {
        let op = memory_op();
        op.write(
            "data/index/safe.txt",
            RemoteIndexEntry::new("abc123", 1, 0).to_legacy_bytes(),
        )
        .await
        .unwrap();
        let original = op.read("data/index/safe.txt").await.unwrap().to_vec();

        let error = delete_remote_index_entry(&op, "../safe.txt", "data")
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("unsafe empty/dot component"));
        assert_eq!(
            op.read("data/index/safe.txt").await.unwrap().to_vec(),
            original
        );
    }

    // ── upload + download roundtrip (memory operator) ────────────────────

    #[tokio::test]
    async fn upload_download_roundtrip_small_file() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        // Write a small local file
        let local = dir.path().join("hello.txt");
        std::fs::write(&local, b"hello world").unwrap();

        // Upload
        let up = upload_file(&op, &local, "data", &mut state, None)
            .await
            .unwrap();
        assert!(!up.skipped);
        assert_eq!(up.bytes, 11);
        assert!(!up.hash.is_empty());

        // Download to a different location
        let dl_path = dir.path().join("downloaded.txt");
        let dl = download_file(&op, &up.remote_path, &dl_path, "data", None)
            .await
            .unwrap();
        assert_eq!(dl.bytes, 11);

        // Verify content matches
        let content = std::fs::read_to_string(&dl_path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn upload_result_vclock_matches_committed_manifest_and_state() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let local = dir.path().join("todo.txt");
        std::fs::write(&local, b"ship the watcher event clock").unwrap();

        let up = upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "neo",
            Some("notes/todo.txt"),
            None,
        )
        .await
        .unwrap();
        assert!(!up.skipped);
        assert_eq!(up.vclock.get("neo"), 1);

        let manifest_bytes = op.read(&up.remote_path).await.unwrap();
        let manifest = SyncManifest::from_bytes(&manifest_bytes.to_bytes()).unwrap();
        let cached = state.get(&local).unwrap();

        assert_eq!(up.vclock, manifest.vclock);
        assert_eq!(up.vclock, cached.vclock);
    }

    #[test]
    fn systemtime_to_unix_parts_roundtrips_post_epoch() {
        // A representative post-epoch instant with sub-second precision.
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        assert_eq!(systemtime_to_unix_parts(t), (1_700_000_000, 123_456_789));
    }

    #[test]
    fn systemtime_to_unix_parts_handles_pre_epoch() {
        // 0.5s before the epoch: seconds borrow down, nanos stay in [0, 1e9).
        let t = UNIX_EPOCH - Duration::new(0, 500_000_000);
        let (secs, nanos) = systemtime_to_unix_parts(t);
        assert_eq!(secs, -1);
        assert_eq!(nanos, 500_000_000);
    }

    #[cfg(unix)]
    fn mtime_of(path: &Path) -> (i64, u32) {
        let meta = std::fs::metadata(path).unwrap();
        systemtime_to_unix_parts(meta.modified().unwrap())
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// (a) Chunked-file path: a file uploaded with a known source mtime restores
    /// into a fresh dir with that exact mtime, not "now". This is the input to a
    /// clean `git status` (TIN-1620 T13-Z).
    #[cfg(unix)]
    #[tokio::test]
    async fn mtime_round_trips_for_chunked_file() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let local = dir.path().join("src.txt");
        // Larger than a chunk boundary is unnecessary; non-empty exercises the
        // chunked restore path (empty is handled by a separate test).
        std::fs::write(&local, b"content with a known timestamp").unwrap();

        // Stamp a known, distinctly-old mtime on the source.
        let known = (1_600_000_000_i64, 250_000_000_u32);
        apply_manifest_mtime(&local, known);
        assert_eq!(mtime_of(&local), known, "test setup: source mtime not set");

        let up = upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "device-1",
            Some("src.txt"),
            None,
        )
        .await
        .unwrap();

        // Restore into a fresh location (no pre-existing file => fresh mtime risk).
        let restore_dir = tempfile::tempdir().unwrap();
        let dl_path = restore_dir.path().join("restored.txt");
        let mut restore_state = StateCache::open(&restore_dir.path().join("s2.json")).unwrap();
        download_file_with_device(
            &op,
            &up.remote_path,
            &dl_path,
            "data",
            None,
            "device-2",
            Some(&mut restore_state),
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(&dl_path).unwrap(),
            b"content with a known timestamp"
        );
        let restored = mtime_of(&dl_path);
        // Seconds must match exactly; nanos within filesystem precision (1us).
        assert_eq!(restored.0, known.0, "restored mtime seconds drifted");
        assert!(
            (restored.1 as i64 - known.1 as i64).abs() <= 1_000,
            "restored mtime nanos drifted: got {} want {}",
            restored.1,
            known.1
        );
    }

    /// Path-indexed manifests must not be keyed only by content hash. Two files
    /// can have identical bytes but different git-relevant metadata.
    #[cfg(unix)]
    #[tokio::test]
    async fn same_content_files_keep_distinct_path_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        let body = b"same bytes, different metadata";
        std::fs::write(&a, body).unwrap();
        std::fs::write(&b, body).unwrap();

        let a_mtime = (1_600_000_100_i64, 100_000_000_u32);
        let b_mtime = (1_600_000_200_i64, 200_000_000_u32);
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755)).unwrap();
        apply_manifest_mtime(&a, a_mtime);
        apply_manifest_mtime(&b, b_mtime);

        let up_a = upload_file_with_device(
            &op,
            &a,
            "data",
            &mut state,
            None,
            "device-1",
            Some("a.txt"),
            None,
        )
        .await
        .unwrap();
        let up_b = upload_file_with_device(
            &op,
            &b,
            "data",
            &mut state,
            None,
            "device-1",
            Some("b.txt"),
            None,
        )
        .await
        .unwrap();

        assert_eq!(up_a.hash, up_b.hash, "content hash should dedupe bytes");
        assert_ne!(
            up_a.remote_path, up_b.remote_path,
            "path-specific metadata must not collide on a content-hash manifest"
        );
        let chunks = op.list("data/chunks/").await.unwrap();
        assert_eq!(
            chunks.len(),
            up_a.chunks,
            "same content should still dedupe chunk objects"
        );

        let index_a = op.read("data/index/a.txt").await.unwrap().to_vec();
        let index_b = op.read("data/index/b.txt").await.unwrap().to_vec();
        assert_eq!(
            manifest_key(
                &manifest_path_prefix("data"),
                &committed_manifest_hash(&index_a)
            ),
            up_a.remote_path
        );
        assert_eq!(
            manifest_key(
                &manifest_path_prefix("data"),
                &committed_manifest_hash(&index_b)
            ),
            up_b.remote_path
        );

        let restore_dir = tempfile::tempdir().unwrap();
        let mut restore_state = StateCache::open(&restore_dir.path().join("restore.json")).unwrap();
        let restore_a = restore_dir.path().join("a.txt");
        let restore_b = restore_dir.path().join("b.txt");
        download_file_with_device(
            &op,
            &up_a.remote_path,
            &restore_a,
            "data",
            None,
            "device-2",
            Some(&mut restore_state),
            None,
        )
        .await
        .unwrap();
        download_file_with_device(
            &op,
            &up_b.remote_path,
            &restore_b,
            "data",
            None,
            "device-2",
            Some(&mut restore_state),
            None,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&restore_a).unwrap(), body);
        assert_eq!(std::fs::read(&restore_b).unwrap(), body);
        assert_eq!(mode_of(&restore_a), 0o644);
        assert_eq!(mode_of(&restore_b), 0o755);
        assert_eq!(mtime_of(&restore_a).0, a_mtime.0);
        assert_eq!(mtime_of(&restore_b).0, b_mtime.0);
    }

    /// (a) Empty-file path: the zero-byte restore branch also restamps mtime.
    #[cfg(unix)]
    #[tokio::test]
    async fn mtime_round_trips_for_empty_file() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let mut state = StateCache::open(&dir.path().join("state.json")).unwrap();

        let local = dir.path().join("empty.txt");
        std::fs::write(&local, b"").unwrap();
        let known = (1_555_000_000_i64, 0_u32);
        apply_manifest_mtime(&local, known);

        let up = upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "device-1",
            Some("empty.txt"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(up.chunks, 0, "empty file must take the chunkless path");

        let restore_dir = tempfile::tempdir().unwrap();
        let dl_path = restore_dir.path().join("restored_empty.txt");
        let mut restore_state = StateCache::open(&restore_dir.path().join("s2.json")).unwrap();
        download_file_with_device(
            &op,
            &up.remote_path,
            &dl_path,
            "data",
            None,
            "device-2",
            Some(&mut restore_state),
            None,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::metadata(&dl_path).unwrap().len(), 0);
        assert_eq!(
            mtime_of(&dl_path).0,
            known.0,
            "empty-file mtime not restored"
        );
    }

    /// (b) Back-compat: a manifest serialized WITHOUT an mtime field (old fleet)
    /// deserializes to `mtime: None` and restores with today's behavior — no
    /// panic, no addressing change, mtime left to "now".
    #[cfg(unix)]
    #[tokio::test]
    async fn old_manifest_without_mtime_restores_with_current_behavior() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();

        // A pre-mtime v2 manifest: note no `mtime` key at all.
        let body = b"legacy body bytes";
        let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(body));
        let chunk_hash = file_hash.clone();
        op.write(&format!("data/chunks/{chunk_hash}"), body.to_vec())
            .await
            .unwrap();
        let legacy_json = format!(
            r#"{{"version":2,"file_hash":"{file_hash}","file_size":{},"chunks":["{chunk_hash}"],"vclock":{{"clocks":{{}}}},"written_by":"old","written_at":1,"rel_path":"legacy.txt"}}"#,
            body.len()
        );
        // Sanity: this JSON deserializes with mtime None and never panics.
        let parsed = SyncManifest::from_bytes(legacy_json.as_bytes()).unwrap();
        assert!(parsed.mtime.is_none(), "old manifest must yield mtime None");

        let manifest_path = format!("data/manifests/{file_hash}");
        op.write(&manifest_path, legacy_json.into_bytes())
            .await
            .unwrap();

        let dl_path = dir.path().join("legacy_restored.txt");
        let before = SystemTime::now();
        let dl = download_file(&op, &manifest_path, &dl_path, "data", None)
            .await
            .unwrap();
        assert_eq!(dl.bytes, body.len() as u64);
        assert_eq!(std::fs::read(&dl_path).unwrap(), body);

        // Current behavior: mtime is whatever the OS stamped at write (~now), not
        // some restored value — we never errored and never set an old time.
        let restored_secs = mtime_of(&dl_path).0;
        let before_secs = systemtime_to_unix_parts(before).0;
        assert!(
            restored_secs + 5 >= before_secs,
            "restore with no manifest mtime must keep fresh-write timestamp"
        );
    }

    #[tokio::test]
    async fn download_file_cleans_streaming_tmp_after_chunk_failure() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let download_dir = dir.path().join("downloads");
        let dl_path = download_dir.join("large.bin");

        let first = b"first chunk";
        let missing = b"missing chunk";
        let first_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(first));
        let missing_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(missing));
        let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(
            &[first.as_slice(), missing.as_slice()].concat(),
        ));

        op.write(&format!("data/chunks/{first_hash}"), first.to_vec())
            .await
            .unwrap();
        let manifest = SyncManifest {
            version: 2,
            file_hash: file_hash.clone(),
            file_size: (first.len() + missing.len()) as u64,
            chunks: vec![first_hash, missing_hash],
            vclock: VectorClock::new(),
            written_by: "tester".into(),
            written_at: 0,
            rel_path: Some("large.bin".into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        let manifest_path = format!("data/manifests/{file_hash}");
        op.write(&manifest_path, manifest.to_bytes().unwrap())
            .await
            .unwrap();

        let err = download_file(&op, &manifest_path, &dl_path, "data", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("downloading chunk"),
            "unexpected error: {err:#}"
        );
        assert!(
            !dl_path.exists(),
            "failed download must not leave the target path"
        );
        let leftovers = std::fs::read_dir(&download_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "failed download left temp files behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn upload_file_with_device_publishes_committed_v2_index() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let local = dir.path().join("hello.txt");
        std::fs::write(&local, b"hello index").unwrap();

        let upload = upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "device-1",
            Some("hello.txt"),
            None,
        )
        .await
        .unwrap();

        let index_bytes = op.read("data/index/hello.txt").await.unwrap().to_vec();
        match crate::index_entry::parse_index_entry_record(&index_bytes).unwrap() {
            crate::index_entry::ParsedIndexEntry::Legacy(_) => {
                panic!("expected committed v2 index entry")
            }
            crate::index_entry::ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, crate::index_entry::IndexEntryState::Committed);
                let current = entry.current.expect("current committed entry");
                assert_eq!(current.size, upload.bytes);
                assert_eq!(current.chunks, upload.chunks);
                let manifest_path =
                    manifest_key(&manifest_path_prefix("data"), &current.manifest_hash);
                assert_eq!(manifest_path, upload.remote_path);
                let manifest_bytes = op.read(&manifest_path).await.unwrap().to_vec();
                let manifest = SyncManifest::from_bytes(&manifest_bytes).unwrap();
                assert_eq!(manifest.file_hash, upload.hash);
            }
        }
    }

    #[tokio::test]
    async fn direct_upload_rejects_manifest_bound_to_another_content_key() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("direct.txt");
        let local_bytes = b"direct content";
        std::fs::write(&local, local_bytes).unwrap();
        let local_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(local_bytes.as_slice()));
        let manifest_key = format!("data/manifests/{local_hash}");
        let collided = SyncManifest {
            version: 2,
            file_hash: "different-content-hash".into(),
            file_size: local_bytes.len() as u64,
            chunks: Vec::new(),
            vclock: VectorClock::new(),
            written_by: String::new(),
            written_at: 0,
            rel_path: None,
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        }
        .to_bytes()
        .unwrap();
        op.write(&manifest_key, collided.clone()).await.unwrap();
        let mut state = StateCache::open(&dir.path().join("state.json")).unwrap();

        let error = upload_file(&op, &local, "data", &mut state, None)
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("direct manifest content-key mismatch"));
        assert_eq!(op.read(&manifest_key).await.unwrap().to_vec(), collided);
        assert!(state.get(&local).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_direct_uploads_cannot_overwrite_collided_manifest() {
        use std::os::unix::fs::PermissionsExt;

        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let local_a = dir.path().join("a.txt");
        let local_b = dir.path().join("b.txt");
        let bytes = b"same direct content, distinct metadata";
        std::fs::write(&local_a, bytes).unwrap();
        std::fs::write(&local_b, bytes).unwrap();
        std::fs::set_permissions(&local_a, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&local_b, std::fs::Permissions::from_mode(0o644)).unwrap();

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let barrier_a = barrier.clone();
        let upload_a = upload_file_with_device_with_state_and_publish_hook(
            &op,
            &local_a,
            "data",
            None,
            "",
            None,
            None,
            None,
            Some("forced direct publish".into()),
            UploadRuntimeOptions {
                assume_fresh_prefix: false,
                file_upload_concurrency: 1,
            },
            None,
            move || async move {
                barrier_a.wait().await;
                Ok(())
            },
        );
        let barrier_b = barrier.clone();
        let upload_b = upload_file_with_device_with_state_and_publish_hook(
            &op,
            &local_b,
            "data",
            None,
            "",
            None,
            None,
            None,
            Some("forced direct publish".into()),
            UploadRuntimeOptions {
                assume_fresh_prefix: false,
                file_upload_concurrency: 1,
            },
            None,
            move || async move {
                barrier_b.wait().await;
                Ok(())
            },
        );

        let (result_a, result_b) = tokio::join!(upload_a, upload_b);
        assert_eq!(result_a.is_ok() as usize + result_b.is_ok() as usize, 1);
        let error = result_a.err().or_else(|| result_b.err()).unwrap();
        assert!(format!("{error:#}")
            .contains("existing immutable manifest object does not match its byte address"));

        let content_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(bytes.as_slice()));
        let manifest_key = format!("data/manifests/{content_hash}");
        let stored = op.read(&manifest_key).await.unwrap().to_vec();
        let manifest =
            parse_direct_manifest_for_content_key(&stored, &content_hash, bytes.len() as u64)
                .unwrap();
        assert!(matches!(
            manifest.mode.map(|mode| mode & 0o777),
            Some(0o600) | Some(0o644)
        ));
    }

    #[tokio::test]
    async fn upload_file_with_device_marks_conflict_status() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let local = dir.path().join("hello.txt");
        std::fs::write(&local, b"hello base").unwrap();

        let mut local_vclock = crate::conflict::VectorClock::new();
        local_vclock.tick("device-1");
        state.set(
            &local,
            crate::state::SyncState {
                blake3: "basehash123".into(),
                size: 10,
                mtime: 0,
                chunk_count: 0,
                remote_path: "data/manifests/basehash123".into(),
                last_synced: 0,
                vclock: local_vclock,
                device_id: "device-1".into(),
                conflict: None,
                status: FileSyncStatus::Synced,
            },
        );

        std::fs::write(&local, b"hello local").unwrap();

        let remote_manifest_hash = "remotehash123";
        op.write(
            &format!("data/manifests/{remote_manifest_hash}"),
            br#"{"version":2,"file_hash":"remotehash123","file_size":12,"chunks":[],"vclock":{"clocks":{"device-2":1}},"written_by":"device-2","written_at":1,"rel_path":"hello.txt"}"#.to_vec(),
        )
        .await
        .unwrap();
        write_committed_index_entry(
            &op,
            "data/index/hello.txt",
            &crate::index_entry::RemoteIndexEntry::new(remote_manifest_hash, 12, 0),
        )
        .await
        .unwrap();

        let result = upload_file_with_device(
            &op,
            &local,
            "data",
            &mut state,
            None,
            "device-1",
            Some("hello.txt"),
            None,
        )
        .await
        .unwrap();

        assert!(result.skipped);
        let result_conflict = match result.outcome {
            Some(SyncOutcome::Conflict(info)) => info,
            other => panic!("expected conflict outcome, got: {other:?}"),
        };
        assert_eq!(
            result_conflict.remote_manifest_key.as_deref(),
            Some("data/manifests/remotehash123")
        );

        let entry = state.get(&local).expect("conflicted state entry");
        assert_eq!(entry.status, FileSyncStatus::Conflict);
        assert_eq!(
            entry
                .conflict
                .as_ref()
                .expect("conflict payload should be preserved")
                .remote_manifest_key
                .as_deref(),
            Some("data/manifests/remotehash123")
        );
    }

    #[test]
    fn upload_chunk_timeout_env_is_bounded() {
        assert_eq!(
            upload_chunk_timeout_from_env_value(None),
            Some(Duration::from_secs(DEFAULT_UPLOAD_CHUNK_TIMEOUT_SECS))
        );
        assert_eq!(upload_chunk_timeout_from_env_value(Some("0")), None);
        assert_eq!(
            upload_chunk_timeout_from_env_value(Some("15")),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            upload_chunk_timeout_from_env_value(Some("not-a-number")),
            Some(Duration::from_secs(DEFAULT_UPLOAD_CHUNK_TIMEOUT_SECS))
        );
        assert_eq!(
            upload_chunk_timeout_from_env_value(Some("999999")),
            Some(Duration::from_secs(MAX_UPLOAD_CHUNK_TIMEOUT_SECS))
        );
    }

    #[test]
    fn download_read_timeout_env_is_bounded() {
        assert_eq!(
            download_read_timeout_from_env_value(None),
            Some(Duration::from_secs(DEFAULT_DOWNLOAD_READ_TIMEOUT_SECS))
        );
        assert_eq!(download_read_timeout_from_env_value(Some("0")), None);
        assert_eq!(
            download_read_timeout_from_env_value(Some("15")),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            download_read_timeout_from_env_value(Some("not-a-number")),
            Some(Duration::from_secs(DEFAULT_DOWNLOAD_READ_TIMEOUT_SECS))
        );
        assert_eq!(
            download_read_timeout_from_env_value(Some("999999")),
            Some(Duration::from_secs(MAX_DOWNLOAD_READ_TIMEOUT_SECS))
        );
    }

    #[tokio::test]
    async fn chunk_upload_retry_succeeds_after_transient_failure() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        write_chunk_with_retry_inner(
            "data/chunks/abc123",
            0,
            3,
            None,
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            anyhow::bail!("transient write failure");
                        }
                        Ok(())
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![std::time::Duration::from_millis(100)]
        );
    }

    #[tokio::test]
    async fn maybe_upload_chunk_refreshes_existing_cas_object() {
        let op = memory_op();
        let key = "data/chunks/existing".to_string();
        op.write(&key, b"old".to_vec()).await.unwrap();

        let refreshed =
            maybe_upload_chunk(op.clone(), key.clone(), b"new".to_vec(), 0, 3, false, None)
                .await
                .unwrap();
        assert_eq!(refreshed, 3);
        assert_eq!(op.read(&key).await.unwrap().to_bytes(), b"new".as_slice());

        let uploaded =
            maybe_upload_chunk(op.clone(), key.clone(), b"newer".to_vec(), 0, 3, true, None)
                .await
                .unwrap();
        assert_eq!(uploaded, 3);
        assert_eq!(op.read(&key).await.unwrap().to_bytes(), b"newer".as_slice());
    }

    #[tokio::test]
    async fn chunk_upload_timeout_retries_then_succeeds() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        write_chunk_with_retry_inner(
            "data/chunks/slow-once",
            7,
            1024,
            Some(Duration::from_millis(1)),
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        Ok(())
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![std::time::Duration::from_millis(100)]
        );
    }

    #[tokio::test]
    async fn chunk_upload_retry_exhausts_without_sleeping_after_last_failure() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        let err = write_chunk_with_retry_inner(
            "data/chunks/abc123",
            0,
            3,
            None,
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        anyhow::bail!("persistent write failure");
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("uploading chunk 0: data/chunks/abc123"));
        assert_eq!(attempts.load(Ordering::SeqCst), CHUNK_MAX_RETRIES as usize);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(200),
            ]
        );
    }

    #[tokio::test]
    async fn chunk_upload_timeout_exhausts_with_context() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        let err = write_chunk_with_retry_inner(
            "data/chunks/never-finishes",
            9,
            2048,
            Some(Duration::from_millis(1)),
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        std::future::pending::<Result<()>>().await
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap_err();

        let message = format!("{err:#}");
        assert!(message.contains("uploading chunk 9: data/chunks/never-finishes"));
        assert!(message.contains("chunk upload timed out"));
        assert_eq!(attempts.load(Ordering::SeqCst), CHUNK_MAX_RETRIES as usize);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(200),
            ]
        );
    }

    #[tokio::test]
    async fn manifest_read_retry_succeeds_after_transient_failure() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        let bytes = read_with_retry_inner(
            "data/manifests/doc.json",
            None,
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            anyhow::bail!("transient read failure");
                        }
                        Ok(b"manifest".to_vec())
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(bytes, b"manifest".to_vec());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![std::time::Duration::from_millis(100)]
        );
    }

    #[tokio::test]
    async fn manifest_read_retry_exhausts_after_expected_attempts() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        let err = read_with_retry_inner(
            "data/manifests/doc.json",
            None,
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        anyhow::bail!("persistent read failure");
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("reading: data/manifests/doc.json"));
        assert_eq!(attempts.load(Ordering::SeqCst), CHUNK_MAX_RETRIES as usize);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(200),
            ]
        );
    }

    #[tokio::test]
    async fn manifest_read_timeout_exhausts_after_expected_attempts() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));

        let err = read_with_retry_inner(
            "data/manifests/never-finishes.json",
            Some(Duration::from_millis(1)),
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        std::future::pending::<Result<Vec<u8>>>().await
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap_err();

        let message = format!("{err:#}");
        assert!(message.contains("reading: data/manifests/never-finishes.json"));
        assert!(message.contains("read timed out"));
        assert_eq!(attempts.load(Ordering::SeqCst), CHUNK_MAX_RETRIES as usize);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(200),
            ]
        );
    }

    #[tokio::test]
    async fn chunk_download_retry_succeeds_after_transient_transport_failure() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));
        let payload = b"hello retry".to_vec();
        let expected_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&payload));

        let bytes = read_chunk_with_retry_inner(
            "data/chunks/abc123",
            &expected_hash,
            0,
            {
                let attempts = attempts.clone();
                let payload = payload.clone();
                move || {
                    let attempts = attempts.clone();
                    let payload = payload.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            anyhow::bail!("transient transport failure");
                        }
                        Ok(payload)
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(bytes, payload);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![std::time::Duration::from_millis(100)]
        );
    }

    #[tokio::test]
    async fn chunk_download_timeout_exhausts_with_context() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));
        let expected_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(b"never"));

        let err = read_chunk_with_retry_inner_with_attempts(
            "data/chunks/never-finishes",
            &expected_hash,
            7,
            CHUNK_MAX_RETRIES,
            Some(Duration::from_millis(1)),
            {
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        std::future::pending::<Result<Vec<u8>>>().await
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap_err();

        let message = format!("{err:#}");
        assert!(message.contains("downloading chunk 7: data/chunks/never-finishes"));
        assert!(message.contains("chunk download timed out"));
        assert_eq!(attempts.load(Ordering::SeqCst), CHUNK_MAX_RETRIES as usize);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(200),
            ]
        );
    }

    #[tokio::test]
    async fn chunk_download_retry_recovers_after_integrity_mismatch() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));
        let good = b"hello integrity".to_vec();
        let expected_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&good));

        let bytes = read_chunk_with_retry_inner(
            "data/chunks/abc123",
            &expected_hash,
            0,
            {
                let attempts = attempts.clone();
                let good = good.clone();
                move || {
                    let attempts = attempts.clone();
                    let good = good.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            return Ok(b"corrupted".to_vec());
                        }
                        Ok(good)
                    }
                }
            },
            {
                let delays = delays.clone();
                move |delay| {
                    delays.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(bytes, good);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            *delays.lock().unwrap(),
            vec![std::time::Duration::from_millis(100)]
        );
    }

    #[tokio::test]
    async fn publish_preflight_rejects_wrong_content_id_before_any_write() {
        let op = memory_op();
        let bytes = bound_test_manifest_bytes("file", 0, "doc.txt");

        let error = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            bytes,
            RemoteIndexEntry::new("wrong-object-id", 0, 0),
            None,
            |_| Ok(()),
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("content id mismatch"));
        assert!(op.read("data/index/doc.txt").await.is_err());
        assert!(op.list("data/staging/manifests/").await.unwrap().is_empty());
        assert!(op.list("data/manifests/").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn publish_preflight_rejects_kind_mismatch_before_any_write() {
        let op = memory_op();
        let bytes = bound_test_manifest_bytes("file", 0, "link");
        let object_id = manifest_object_id(&bytes);

        let error = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "link",
            bytes,
            RemoteIndexEntry::new_symlink(object_id, "sibling.txt"),
            None,
            |_| Ok(()),
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("parsing symlink manifest"));
        assert!(op.read("data/index/link").await.is_err());
        assert!(op.list("data/staging/manifests/").await.unwrap().is_empty());
        assert!(op.list("data/manifests/").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fresh_publish_never_overwrites_collided_immutable_manifest() {
        let op = memory_op();
        let (bytes, entry) = indexed_test_manifest_for_rel("fresh", 0, "doc.txt");
        let manifest_path = manifest_key("data/manifests", &entry.manifest_hash);
        op.write(&manifest_path, b"unrelated bytes".to_vec())
            .await
            .unwrap();

        let error = publish_manifest_for_rel_path_fresh(&op, "data", "doc.txt", bytes, entry)
            .await
            .unwrap_err();

        assert!(format!("{error:#}")
            .contains("existing immutable manifest object does not match its byte address"));
        assert_eq!(
            op.read(&manifest_path).await.unwrap().to_vec(),
            b"unrelated bytes"
        );
        assert!(!op.exists("data/index/doc.txt").await.unwrap());
    }

    #[tokio::test]
    async fn staged_publish_never_overwrites_collided_immutable_manifest() {
        let op = memory_op();
        let (bytes, entry) = indexed_test_manifest_for_rel("staged", 0, "doc.txt");
        let manifest_path = manifest_key("data/manifests", &entry.manifest_hash);
        op.write(&manifest_path, b"unrelated bytes".to_vec())
            .await
            .unwrap();

        let error = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            bytes,
            entry,
            None,
            |_| Ok(()),
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}")
            .contains("existing immutable manifest object does not match its byte address"));
        assert_eq!(
            op.read(&manifest_path).await.unwrap().to_vec(),
            b"unrelated bytes"
        );
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);
        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::V2(record) => {
                assert_eq!(record.state, IndexEntryState::Preparing);
                assert!(record.pending.is_some());
            }
            ParsedIndexEntry::Legacy(_) => panic!("expected preparing index entry"),
        }
    }

    #[tokio::test]
    async fn publish_boundary_rejects_unicode_casefold_alias() {
        let op = memory_op();
        let (first_bytes, first_entry) =
            indexed_test_manifest_for_rel("first", 0, "Straße/file.txt");
        publish_indexed_test_manifest(&op, "data", "Straße/file.txt", first_bytes, first_entry)
            .await
            .unwrap();

        let (alias_bytes, alias_entry) =
            indexed_test_manifest_for_rel("alias", 0, "STRASSE/other.txt");
        let error = publish_indexed_test_manifest(
            &op,
            "data",
            "STRASSE/other.txt",
            alias_bytes,
            alias_entry,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("namespace spelling collision"));
        assert!(op.read("data/index/STRASSE/other.txt").await.is_err());
    }

    #[tokio::test]
    async fn publish_boundary_rejects_file_ancestor_collision_but_allows_siblings() {
        let op = memory_op();
        let (ancestor_bytes, ancestor_entry) =
            indexed_test_manifest_for_rel("ancestor", 0, "folder");
        publish_indexed_test_manifest(&op, "data", "folder", ancestor_bytes, ancestor_entry)
            .await
            .unwrap();

        let (child_bytes, child_entry) =
            indexed_test_manifest_for_rel("child", 0, "folder/file.txt");
        let error =
            publish_indexed_test_manifest(&op, "data", "folder/file.txt", child_bytes, child_entry)
                .await
                .unwrap_err();
        assert!(format!("{error:#}").contains("file/ancestor collision"));

        let sibling_op = memory_op();
        for (hash, rel_path) in [("one", "folder/one.txt"), ("two", "folder/two.txt")] {
            let (bytes, entry) = indexed_test_manifest_for_rel(hash, 0, rel_path);
            publish_indexed_test_manifest(&sibling_op, "data", rel_path, bytes, entry)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn directory_markers_share_namespace_admission_and_are_reserved() {
        let op = memory_op();
        publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap();
        publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap();
        assert!(op.exists("data/index/empty/.tcfs_dir").await.unwrap());

        let (child_bytes, child_entry) =
            indexed_test_manifest_for_rel("child", 0, "empty/file.txt");
        publish_indexed_test_manifest(&op, "data", "empty/file.txt", child_bytes, child_entry)
            .await
            .unwrap();

        let (reserved_bytes, reserved_entry) =
            indexed_test_manifest_for_rel("reserved", 0, "other/.tcfs_dir");
        let error = publish_indexed_test_manifest(
            &op,
            "data",
            "other/.tcfs_dir",
            reserved_bytes,
            reserved_entry,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("reserved TCFS directory-marker"));

        let alias_error = publish_directory_marker(&op, "data", "EMPTY")
            .await
            .unwrap_err();
        assert!(format!("{alias_error:#}").contains("namespace spelling collision"));

        op.write("data/index/empty/.tcfs_dir", b"corrupt".to_vec())
            .await
            .unwrap();
        let corrupt_error = publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap_err();
        assert!(format!("{corrupt_error:#}").contains("publishing canonical directory marker"));
        assert_eq!(
            op.read("data/index/empty/.tcfs_dir")
                .await
                .unwrap()
                .to_vec(),
            b"corrupt"
        );
    }

    #[tokio::test]
    async fn directory_marker_mkdir_rmdir_mkdir_roundtrip_resurrects_tombstone() {
        let op = memory_op();
        let marker_key = "data/index/empty/.tcfs_dir";

        publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap();
        crate::index_entry::tombstone_directory_marker_if_exact(
            &op,
            "data",
            marker_key,
            crate::index_entry::DIRECTORY_MARKER_BYTES,
        )
        .await
        .unwrap();
        assert!(
            !crate::index_entry::directory_marker_is_visible(&op, marker_key)
                .await
                .unwrap()
        );

        publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap();
        publish_directory_marker(&op, "data", "empty")
            .await
            .unwrap();

        assert!(
            crate::index_entry::directory_marker_is_visible(&op, marker_key)
                .await
                .unwrap()
        );
        assert_eq!(
            op.read(marker_key).await.unwrap().to_vec(),
            crate::index_entry::DIRECTORY_MARKER_BYTES
        );
    }

    #[tokio::test]
    async fn publish_crash_after_staged_write_preserves_existing_visible_manifest() {
        let op = memory_op();
        let old = RemoteIndexEntry::new("old123", 10, 1);
        let old_manifest_key = manifest_key("data/manifests", &old.manifest_hash);
        op.write(
            &old_manifest_key,
            test_manifest_bytes(&old.manifest_hash, old.size),
        )
        .await
        .unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &old)
            .await
            .unwrap();

        let (new_bytes, new_entry) = indexed_test_manifest("new456", 11);
        let new_id = new_entry.manifest_hash.clone();
        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            new_bytes,
            new_entry,
            None,
            |stage| {
                if stage == PublishStage::StagedManifest {
                    return Err(anyhow::anyhow!("injected crash after staged manifest"));
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("injected crash"));

        assert_eq!(
            resolve_manifest_path(&op, "doc.txt", "data", None)
                .await
                .unwrap(),
            "data/manifests/old123"
        );
        assert!(!op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, "old123");
                assert!(entry.pending.is_none());
            }
        }
    }

    #[tokio::test]
    async fn publish_crash_after_preparing_write_rolls_forward_new_path_on_read() {
        let op = memory_op();

        let (new_bytes, new_entry) = indexed_test_manifest("new456", 11);
        let new_id = new_entry.manifest_hash.clone();
        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            new_bytes,
            new_entry,
            None,
            |stage| {
                if stage == PublishStage::PreparingIndex {
                    return Err(anyhow::anyhow!("injected crash after preparing index"));
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("injected crash"));
        assert!(!op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected preparing v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Preparing);
                assert!(entry.current.is_none());
                assert_eq!(entry.pending.unwrap().manifest_hash, new_id);
            }
        }

        assert_eq!(
            resolve_manifest_path(&op, "doc.txt", "data", None)
                .await
                .unwrap(),
            manifest_key("data/manifests", &new_id)
        );
        assert!(op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert_eq!(
            staging_manifest_keys(&op).await.len(),
            1,
            "recovery keeps immutable staging evidence for reachability-safe GC"
        );

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, new_id);
                assert!(entry.pending.is_none());
            }
        }
    }

    #[tokio::test]
    async fn publish_crash_after_final_manifest_write_commits_pending_on_read() {
        let op = memory_op();
        let old = RemoteIndexEntry::new("old123", 10, 1);
        let old_manifest_key = manifest_key("data/manifests", &old.manifest_hash);
        op.write(
            &old_manifest_key,
            test_manifest_bytes(&old.manifest_hash, old.size),
        )
        .await
        .unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &old)
            .await
            .unwrap();

        let (new_bytes, new_entry) = indexed_test_manifest("new456", 11);
        let new_id = new_entry.manifest_hash.clone();
        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            new_bytes,
            new_entry,
            None,
            |stage| {
                if stage == PublishStage::FinalManifest {
                    return Err(anyhow::anyhow!("injected crash after final manifest"));
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("injected crash"));
        assert!(op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected preparing v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Preparing);
                assert_eq!(entry.current.unwrap().manifest_hash, "old123");
                assert_eq!(entry.pending.unwrap().manifest_hash, new_id);
            }
        }

        assert_eq!(
            resolve_manifest_path(&op, "doc.txt", "data", None)
                .await
                .unwrap(),
            manifest_key("data/manifests", &new_id)
        );
        assert_eq!(
            staging_manifest_keys(&op).await.len(),
            1,
            "recovery keeps immutable staging evidence for reachability-safe GC"
        );

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, new_id);
                assert!(entry.pending.is_none());
            }
        }
    }

    #[tokio::test]
    async fn publish_crash_after_committed_write_keeps_new_manifest_visible() {
        let op = memory_op();

        let (new_bytes, new_entry) = indexed_test_manifest("new456", 11);
        let new_id = new_entry.manifest_hash.clone();
        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            new_bytes,
            new_entry,
            None,
            |stage| {
                if stage == PublishStage::CommittedIndex {
                    return Err(anyhow::anyhow!("injected crash after committed index"));
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("injected crash"));

        assert_eq!(
            resolve_manifest_path(&op, "doc.txt", "data", None)
                .await
                .unwrap(),
            manifest_key("data/manifests", &new_id)
        );
        assert!(op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, new_id);
                assert!(entry.pending.is_none());
            }
        }
    }

    async fn assert_two_publishers_preserve_loser_evidence(existing: bool) {
        let op = memory_op();
        if existing {
            let old = RemoteIndexEntry::new("old123", 10, 0);
            op.write(
                &manifest_key("data/manifests", &old.manifest_hash),
                test_manifest_bytes(&old.manifest_hash, old.size),
            )
            .await
            .unwrap();
            write_committed_index_entry(&op, "data/index/doc.txt", &old)
                .await
                .unwrap();
        }

        let (bytes_a, entry_a) = indexed_test_manifest("publisher-a", 0);
        let (bytes_b, entry_b) = indexed_test_manifest("publisher-b", 0);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let barrier_a = barrier.clone();
        let barrier_b = barrier.clone();

        let publish_a = publish_manifest_for_rel_path_with_async_hook(
            &op,
            "data",
            "doc.txt",
            bytes_a.clone(),
            entry_a.clone(),
            None,
            move |stage| {
                let barrier = barrier_a.clone();
                async move {
                    if stage == PublishStage::StagedManifest {
                        barrier.wait().await;
                    }
                    Ok(())
                }
            },
        );
        let publish_b = publish_manifest_for_rel_path_with_async_hook(
            &op,
            "data",
            "doc.txt",
            bytes_b.clone(),
            entry_b.clone(),
            None,
            move |stage| {
                let barrier = barrier_b.clone();
                async move {
                    if stage == PublishStage::StagedManifest {
                        barrier.wait().await;
                    }
                    Ok(())
                }
            },
        );

        let (result_a, result_b) = tokio::join!(publish_a, publish_b);
        let (winner, loser_bytes, loser_error) = match (result_a, result_b) {
            (Ok(()), Err(error)) => (&entry_a, bytes_b, error),
            (Err(error), Ok(())) => (&entry_b, bytes_a, error),
            (result_a, result_b) => {
                panic!(
                    "expected exactly one conditional publisher to win: {result_a:?}, {result_b:?}"
                )
            }
        };
        let error_text = format!("{loser_error:#}");
        assert!(
            error_text.contains("index entry changed")
                || error_text.contains("index entry appeared"),
            "unexpected loser error: {error_text}"
        );

        let index_bytes = op.read("data/index/doc.txt").await.unwrap().to_vec();
        assert_eq!(committed_manifest_hash(&index_bytes), winner.manifest_hash);
        assert!(op
            .exists(&manifest_key("data/manifests", &winner.manifest_hash))
            .await
            .unwrap());

        let staged = staging_manifest_keys(&op).await;
        assert_eq!(
            staged.len(),
            2,
            "both publishers must retain immutable staging evidence"
        );
        let mut staged_bytes = Vec::new();
        for key in staged {
            staged_bytes.push(op.read(&key).await.unwrap().to_vec());
        }
        let winner_bytes = op
            .read(&manifest_key("data/manifests", &winner.manifest_hash))
            .await
            .unwrap()
            .to_vec();
        assert!(staged_bytes.contains(&loser_bytes));
        assert!(staged_bytes.contains(&winner_bytes));
        if existing {
            assert!(op.exists("data/manifests/old123").await.unwrap());
        }
    }

    #[tokio::test]
    async fn concurrent_publishers_cannot_overwrite_existing_index_baseline() {
        assert_two_publishers_preserve_loser_evidence(true).await;
    }

    #[tokio::test]
    async fn concurrent_publishers_atomically_create_absent_index_baseline() {
        assert_two_publishers_preserve_loser_evidence(false).await;
    }

    #[tokio::test]
    async fn prebound_upload_decision_never_rebinds_after_remote_index_advance() {
        let op = memory_op();
        let (old_bytes, old_entry) = indexed_test_manifest("old", 0);
        op.write(
            &manifest_key("data/manifests", &old_entry.manifest_hash),
            old_bytes,
        )
        .await
        .unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &old_entry)
            .await
            .unwrap();

        let baseline =
            resolve_visible_index_entry_for_update(&op, "data/index/doc.txt", "data/manifests")
                .await
                .unwrap();
        assert_eq!(baseline.current(), Some(&old_entry));

        // The remote advances after conflict resolution but before publication.
        // The publisher must CAS against the original decision, not adopt this
        // newer entry as a fresh baseline.
        let (newer_bytes, newer_entry) = indexed_test_manifest("newer", 0);
        op.write(
            &manifest_key("data/manifests", &newer_entry.manifest_hash),
            newer_bytes,
        )
        .await
        .unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &newer_entry)
            .await
            .unwrap();

        let (publishing_bytes, publishing_entry) = indexed_test_manifest("publishing", 0);
        let publishing_id = publishing_entry.manifest_hash.clone();
        let error = publish_manifest_for_rel_path_with_async_hook(
            &op,
            "data",
            "doc.txt",
            publishing_bytes.clone(),
            publishing_entry,
            Some(baseline),
            |_| std::future::ready(Ok(())),
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("changed during preparing publish"));
        assert_eq!(
            committed_manifest_hash(&op.read("data/index/doc.txt").await.unwrap().to_vec()),
            newer_entry.manifest_hash
        );
        assert!(!op
            .exists(&manifest_key("data/manifests", &publishing_id))
            .await
            .unwrap());
        let staged = staging_manifest_keys(&op).await;
        assert_eq!(staged.len(), 1);
        assert_eq!(
            op.read(&staged[0]).await.unwrap().to_vec(),
            publishing_bytes
        );
    }

    #[tokio::test]
    async fn upload_path_carries_conflict_baseline_through_remote_advance() {
        let op = memory_op();
        let tmp = tempfile::tempdir().unwrap();
        let local_path = tmp.path().join("doc.txt");
        std::fs::write(&local_path, b"local edit").unwrap();

        let (old_bytes, old_entry) = indexed_test_manifest("old", 0);
        let old_manifest_key = manifest_key("data/manifests", &old_entry.manifest_hash);
        op.write(&old_manifest_key, old_bytes).await.unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &old_entry)
            .await
            .unwrap();

        let tracked_state = make_sync_state_full(
            &local_path,
            "old".to_string(),
            0,
            old_manifest_key,
            VectorClock::new(),
            "device-a".to_string(),
        )
        .unwrap();

        let (newer_bytes, newer_entry) = indexed_test_manifest("newer", 0);
        let newer_manifest_key = manifest_key("data/manifests", &newer_entry.manifest_hash);
        let hook_op = op.clone();
        let hook_entry = newer_entry.clone();
        let hook = move || {
            let op = hook_op.clone();
            let entry = hook_entry.clone();
            let manifest_key = newer_manifest_key.clone();
            let bytes = newer_bytes.clone();
            async move {
                op.write(&manifest_key, bytes).await?;
                write_committed_index_entry(&op, "data/index/doc.txt", &entry).await
            }
        };

        let error = upload_file_with_device_with_state_and_publish_hook(
            &op,
            &local_path,
            "data",
            None,
            "device-a",
            Some("doc.txt"),
            None,
            Some(tracked_state),
            Some("local content changed".to_string()),
            UploadRuntimeOptions {
                assume_fresh_prefix: false,
                file_upload_concurrency: 1,
            },
            None,
            hook,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("changed during preparing publish"));
        assert_eq!(
            committed_manifest_hash(&op.read("data/index/doc.txt").await.unwrap().to_vec()),
            newer_entry.manifest_hash
        );
        let staged = staging_manifest_keys(&op).await;
        assert_eq!(
            staged.len(),
            1,
            "failed upload must retain staging evidence"
        );
    }

    #[tokio::test]
    async fn publish_commit_cas_preserves_concurrent_newer_index_and_evidence() {
        let op = memory_op();
        let (publishing_bytes, publishing_entry) = indexed_test_manifest("publishing", 0);
        let publishing_id = publishing_entry.manifest_hash.clone();
        let (newer_bytes, newer_entry) = indexed_test_manifest("newer", 0);
        op.write(
            &manifest_key("data/manifests", &newer_entry.manifest_hash),
            newer_bytes,
        )
        .await
        .unwrap();

        let hook_op = op.clone();
        let hook_entry = newer_entry.clone();
        let error = publish_manifest_for_rel_path_with_async_hook(
            &op,
            "data",
            "doc.txt",
            publishing_bytes,
            publishing_entry,
            None,
            move |stage| {
                let op = hook_op.clone();
                let entry = hook_entry.clone();
                async move {
                    if stage == PublishStage::FinalManifest {
                        write_committed_index_entry(&op, "data/index/doc.txt", &entry).await?;
                    }
                    Ok(())
                }
            },
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("index entry changed during committing publish"));
        assert_eq!(
            committed_manifest_hash(&op.read("data/index/doc.txt").await.unwrap().to_vec()),
            newer_entry.manifest_hash
        );
        assert!(op
            .exists(&manifest_key("data/manifests", &publishing_id))
            .await
            .unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);
    }

    #[tokio::test]
    async fn fresh_prefix_publish_writes_committed_index_without_staging() {
        let op = memory_op();
        let (new_bytes, new_entry) = indexed_test_manifest("new456", 11);
        let new_id = new_entry.manifest_hash.clone();
        publish_manifest_for_rel_path_fresh(&op, "data", "doc.txt", new_bytes, new_entry)
            .await
            .unwrap();

        assert!(op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert!(staging_manifest_keys(&op).await.is_empty());

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, new_id);
                assert!(entry.pending.is_none());
            }
        }
    }

    #[tokio::test]
    async fn fresh_prefix_contract_violation_never_overwrites_existing_index() {
        let op = memory_op();
        let old = RemoteIndexEntry::new("old123", 10, 0);
        op.write(
            &manifest_key("data/manifests", &old.manifest_hash),
            test_manifest_bytes(&old.manifest_hash, old.size),
        )
        .await
        .unwrap();
        write_committed_index_entry(&op, "data/index/doc.txt", &old)
            .await
            .unwrap();

        let (new_bytes, new_entry) = indexed_test_manifest("new456", 0);
        let new_id = new_entry.manifest_hash.clone();
        let error =
            publish_manifest_for_rel_path_fresh(&op, "data", "doc.txt", new_bytes, new_entry)
                .await
                .unwrap_err();
        assert!(format!("{error:#}").contains("index entry appeared"));
        assert_eq!(
            committed_manifest_hash(&op.read("data/index/doc.txt").await.unwrap().to_vec()),
            old.manifest_hash
        );
        assert!(op
            .exists(&manifest_key("data/manifests", &new_id))
            .await
            .unwrap());
        assert!(staging_manifest_keys(&op).await.is_empty());
    }

    #[tokio::test]
    async fn push_tree_fresh_prefix_file_concurrency_uploads_many_files_and_flushes_state() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let mut expected_bytes = 0u64;
        for i in 0..24 {
            let data = format!("parallel file {i:02}\n").into_bytes();
            expected_bytes += data.len() as u64;
            std::fs::write(root.join("docs").join(format!("file-{i:02}.txt")), data).unwrap();
        }

        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();
        let runtime = UploadRuntimeOptions {
            assume_fresh_prefix: true,
            file_upload_concurrency: 6,
        };

        let (uploaded, skipped, bytes) = push_tree_with_device_with_runtime(
            &op,
            &root,
            "data",
            &mut state,
            None,
            "",
            Some(&no_empty_dirs_config()),
            None,
            runtime,
        )
        .await
        .unwrap();

        assert_eq!(uploaded, 24);
        assert_eq!(skipped, 0);
        assert_eq!(bytes, expected_bytes);
        assert_eq!(state.len(), 24);
        assert!(state_path.exists(), "tree push should flush state to disk");
        assert!(staging_manifest_keys(&op).await.is_empty());
        assert!(op.exists("data/index/docs/file-00.txt").await.unwrap());
        assert!(op.exists("data/index/docs/file-23.txt").await.unwrap());
    }

    #[tokio::test]
    async fn push_tree_concurrent_fresh_prefix_preserves_duplicate_content_index_entries() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let content = b"same bytes at two paths";
        std::fs::write(root.join("docs/a.txt"), content).unwrap();
        std::fs::write(root.join("docs/b.txt"), content).unwrap();

        let mut state = StateCache::open(&dir.path().join("state.json")).unwrap();
        let runtime = UploadRuntimeOptions {
            assume_fresh_prefix: true,
            file_upload_concurrency: 2,
        };

        let (uploaded, skipped, bytes) = push_tree_with_device_with_runtime(
            &op,
            &root,
            "data",
            &mut state,
            None,
            "",
            Some(&no_empty_dirs_config()),
            None,
            runtime,
        )
        .await
        .unwrap();

        assert_eq!(uploaded, 2);
        assert_eq!(skipped, 0);
        assert_eq!(bytes, (content.len() * 2) as u64);
        let a_raw = op.read("data/index/docs/a.txt").await.unwrap().to_vec();
        let b_raw = op.read("data/index/docs/b.txt").await.unwrap().to_vec();
        let a_hash = committed_manifest_hash(&a_raw);
        let b_hash = committed_manifest_hash(&b_raw);
        assert_ne!(
            a_hash, b_hash,
            "duplicate content at different paths must keep path-scoped manifests"
        );
        let a_manifest_raw = op
            .read(&format!("data/manifests/{a_hash}"))
            .await
            .unwrap()
            .to_vec();
        let b_manifest_raw = op
            .read(&format!("data/manifests/{b_hash}"))
            .await
            .unwrap()
            .to_vec();
        let a_manifest = SyncManifest::from_bytes(&a_manifest_raw).unwrap();
        let b_manifest = SyncManifest::from_bytes(&b_manifest_raw).unwrap();
        assert_eq!(
            a_manifest.file_hash, b_manifest.file_hash,
            "content hash should still dedupe identical bytes"
        );
        assert_eq!(a_manifest.rel_path.as_deref(), Some("docs/a.txt"));
        assert_eq!(b_manifest.rel_path.as_deref(), Some("docs/b.txt"));
    }

    #[tokio::test]
    async fn push_tree_concurrent_file_error_keeps_successful_state_updates() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        let ok_a = root.join("ok-a.txt");
        let missing = root.join("missing.txt");
        let ok_b = root.join("ok-b.txt");
        std::fs::write(&ok_a, b"first good file").unwrap();
        std::fs::write(&ok_b, b"second good file").unwrap();

        let mut state = StateCache::open(&dir.path().join("state.json")).unwrap();
        let runtime = UploadRuntimeOptions {
            assume_fresh_prefix: true,
            file_upload_concurrency: 4,
        };
        let files = vec![ok_a.clone(), missing, ok_b.clone()];

        let (uploaded, skipped, bytes) = push_regular_files_concurrently(
            &op,
            &root,
            "data",
            &mut state,
            None,
            "",
            &files,
            files.len(),
            runtime,
        )
        .await
        .unwrap();

        assert_eq!(uploaded, 2);
        assert_eq!(skipped, 0);
        assert_eq!(
            bytes,
            b"first good file".len() as u64 + b"second good file".len() as u64
        );
        assert!(state.get(&ok_a).is_some());
        assert!(state.get(&ok_b).is_some());
        assert!(op.exists("data/index/ok-a.txt").await.unwrap());
        assert!(op.exists("data/index/ok-b.txt").await.unwrap());
    }

    #[tokio::test]
    async fn upload_skips_when_already_synced() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let local = dir.path().join("file.txt");
        std::fs::write(&local, b"content").unwrap();

        // First upload
        let up1 = upload_file(&op, &local, "data", &mut state, None)
            .await
            .unwrap();
        assert!(!up1.skipped);

        // Second upload of same file — should skip (dedup)
        let up2 = upload_file(&op, &local, "data", &mut state, None)
            .await
            .unwrap();
        assert!(up2.skipped, "second upload of unchanged file should skip");
    }

    #[tokio::test]
    async fn upload_fails_if_file_changes_during_chunk_upload() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = StateCache::open(&state_path).unwrap();

        let original = b"hello world";
        let local = dir.path().join("file.txt");
        std::fs::write(&local, original).unwrap();

        let mutated = b"jello world";
        let expected_manifest = format!(
            "data/manifests/{}",
            tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(original))
        );
        let local_for_progress = local.clone();
        let mutated_once = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mutated_once_for_progress = mutated_once.clone();
        let progress: ProgressFn = Box::new(move |current, _total, _message| {
            if current == 1
                && !mutated_once_for_progress.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                std::fs::write(&local_for_progress, mutated).unwrap();
            }
        });

        let err = upload_file(&op, &local, "data", &mut state, Some(&progress))
            .await
            .unwrap_err();
        let err_text = format!("{err:#}");

        assert!(
            err_text.contains("file changed during manifest publish"),
            "unexpected error: {err_text}"
        );
        assert!(
            op.read(&expected_manifest).await.is_err(),
            "manifest must not be published after a detected write race"
        );
        assert!(
            state.get(&local).is_none(),
            "state cache must not be updated after a detected write race"
        );
    }

    // ── remote_path_prefix ───────────────────────────────────────────────

    #[test]
    fn remote_path_prefix_strips_trailing_slash() {
        assert_eq!(remote_path_prefix("data/"), "data");
        assert_eq!(remote_path_prefix("data"), "data");
        assert_eq!(remote_path_prefix("a/b/c/"), "a/b/c");
    }
}

#[cfg(test)]
mod proptest_suite {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// normalize_rel_path must never panic on arbitrary path strings.
        #[test]
        fn normalize_never_panics(input in ".*") {
            let _ = normalize_rel_path(Path::new(&input), None);
        }

        /// Output never contains backslashes (Windows path separators).
        #[test]
        fn normalize_no_backslash(input in ".*") {
            let result = normalize_rel_path(Path::new(&input), None);
            prop_assert!(!result.contains('\\'), "backslash in output: {result}");
        }

        /// With a real tempdir as sync_root, file paths under it are relativized.
        #[test]
        fn normalize_under_root_is_relative(filename in "[a-zA-Z][a-zA-Z0-9._-]{0,63}") {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join(&filename);
            std::fs::write(&file, b"x").unwrap();

            let result = normalize_rel_path(&file, Some(dir.path()));
            prop_assert_eq!(result, filename);
        }
    }
}
