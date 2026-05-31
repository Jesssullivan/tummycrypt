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
    manifest_key, read_index_entry_record_from_store, resolve_visible_index_entry,
    write_committed_index_entry, write_preparing_index_entry, PendingIndexEntry, RemoteIndexEntry,
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
    assume_fresh_prefix: bool,
    write_timeout: Option<Duration>,
) -> Result<u64> {
    if assume_fresh_prefix || !op.exists(&chunk_key).await.unwrap_or(false) {
        write_chunk_with_retry(
            &op,
            &chunk_key,
            upload_data,
            chunk_idx,
            logical_len,
            write_timeout,
        )
        .await?;
        return Ok(logical_len);
    }

    Ok(0)
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

fn manifest_path_prefix(remote_prefix: &str) -> String {
    format!("{}/manifests", remote_prefix.trim_end_matches('/'))
}

async fn publish_index_reference(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    entry: RemoteIndexEntry,
) -> Result<()> {
    let prefix = remote_prefix.trim_end_matches('/');
    let index_key = format!("{prefix}/index/{rel_path}");
    let manifest_prefix = manifest_path_prefix(prefix);
    let manifest_path = manifest_key(&manifest_prefix, &entry.manifest_hash);

    anyhow::ensure!(
        op.exists(&manifest_path).await.unwrap_or(false),
        "cannot point index at missing manifest: {manifest_path}"
    );

    let _ = resolve_visible_index_entry(op, &index_key, &manifest_prefix).await?;
    write_committed_index_entry(op, &index_key, &entry).await
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

async fn publish_manifest_for_rel_path(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
) -> Result<()> {
    publish_manifest_for_rel_path_with_mode(
        op,
        remote_prefix,
        rel_path,
        manifest_bytes,
        entry,
        upload_assume_fresh_prefix(),
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
) -> Result<()> {
    if assume_fresh_prefix {
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
    let prefix = remote_prefix.trim_end_matches('/');
    let index_key = format!("{prefix}/index/{rel_path}");
    let manifest_prefix = manifest_path_prefix(prefix);
    let final_manifest_key = manifest_key(&manifest_prefix, &entry.manifest_hash);

    op.write(&final_manifest_key, manifest_bytes)
        .await
        .with_context(|| format!("uploading fresh-prefix manifest: {final_manifest_key}"))?;
    write_committed_index_entry(op, &index_key, &entry).await?;
    Ok(())
}

async fn publish_manifest_for_rel_path_with_hook<F>(
    op: &Operator,
    remote_prefix: &str,
    rel_path: &str,
    manifest_bytes: Vec<u8>,
    entry: RemoteIndexEntry,
    mut after_stage: F,
) -> Result<()>
where
    F: FnMut(PublishStage) -> Result<()>,
{
    let prefix = remote_prefix.trim_end_matches('/');
    let index_key = format!("{prefix}/index/{rel_path}");
    let manifest_prefix = manifest_path_prefix(prefix);
    let final_manifest_key = manifest_key(&manifest_prefix, &entry.manifest_hash);
    let staged_manifest_key = format!(
        "{prefix}/staging/manifests/{}-{}.json",
        Uuid::new_v4(),
        entry.manifest_hash
    );

    let current = resolve_visible_index_entry(op, &index_key, &manifest_prefix).await?;

    op.write(&staged_manifest_key, manifest_bytes.clone())
        .await
        .with_context(|| format!("writing staged manifest: {staged_manifest_key}"))?;
    after_stage(PublishStage::StagedManifest)?;

    write_preparing_index_entry(
        op,
        &index_key,
        current,
        PendingIndexEntry::from_remote_entry(&entry, staged_manifest_key.clone()),
    )
    .await?;
    after_stage(PublishStage::PreparingIndex)?;

    if !op.exists(&final_manifest_key).await.unwrap_or(false) {
        op.write(&final_manifest_key, manifest_bytes)
            .await
            .with_context(|| format!("uploading manifest: {final_manifest_key}"))?;
        after_stage(PublishStage::FinalManifest)?;
    }

    write_committed_index_entry(op, &index_key, &entry).await?;
    after_stage(PublishStage::CommittedIndex)?;
    let _ = op.delete(&staged_manifest_key).await;
    Ok(())
}

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
    let mut file_name = local_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(".tcfs"));
    file_name.push(format!(".{marker}.{}", Uuid::new_v4()));
    local_path.with_file_name(file_name)
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
) -> Result<(UploadResult, Option<SyncState>)> {
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

    // Build remote manifest path (using the file's content hash)
    let remote_manifest = format!("{remote_prefix}/manifests/{file_hash_hex}");
    let assume_fresh_prefix = runtime.assume_fresh_prefix;

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
        let remote_manifest_obj = if let Some(rp) = rel_path {
            // Look up the index entry to find what manifest is currently stored
            let index_key = format!("{}/index/{}", remote_prefix.trim_end_matches('/'), rp);
            let manifest_prefix = manifest_path_prefix(remote_prefix);
            let idx_manifest = resolve_visible_index_entry(op, &index_key, &manifest_prefix)
                .await
                .ok()
                .flatten()
                .map(|entry| manifest_key(&manifest_prefix, &entry.manifest_hash));
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
                    ensure_source_matches_snapshot(local_path, &snapshot, "remote-newer skip")?;
                    return Ok((
                        UploadResult {
                            path: local_path.to_path_buf(),
                            remote_path: remote_manifest.clone(),
                            hash: file_hash_hex,
                            chunks: 0,
                            bytes: file_size,
                            skipped: true,
                            outcome: Some(sync_outcome),
                        },
                        None,
                    ));
                }
                SyncOutcome::Conflict(ref conflict_info) => {
                    ensure_source_matches_snapshot(local_path, &snapshot, "conflict skip")?;
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
                    sync_state.status = FileSyncStatus::Conflict;
                    return Ok((
                        UploadResult {
                            path: local_path.to_path_buf(),
                            remote_path: remote_manifest.clone(),
                            hash: file_hash_hex,
                            chunks: 0,
                            bytes: file_size,
                            skipped: true,
                            outcome: Some(sync_outcome),
                        },
                        Some(sync_state),
                    ));
                }
                SyncOutcome::UpToDate => {
                    ensure_source_matches_snapshot(local_path, &snapshot, "up-to-date skip")?;
                    // Content dedup — already up to date
                    let sync_state = make_sync_state_full(
                        local_path,
                        file_hash_hex.clone(),
                        0,
                        remote_manifest.clone(),
                        local_vclock,
                        device_id.to_string(),
                    )?;
                    return Ok((
                        UploadResult {
                            path: local_path.to_path_buf(),
                            remote_path: remote_manifest,
                            hash: file_hash_hex,
                            chunks: 0,
                            bytes: file_size,
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
    if !assume_fresh_prefix
        && outcome.is_none()
        && op.exists(&remote_manifest).await.unwrap_or(false)
        && device_id.is_empty()
    {
        ensure_source_matches_snapshot(local_path, &snapshot, "dedup skip")?;
        debug!(hash = %file_hash_hex, "dedup: manifest already exists");
        let existing_manifest = op
            .read(&remote_manifest)
            .await
            .with_context(|| format!("reading existing manifest for dedup: {remote_manifest}"))?;
        let existing_manifest = SyncManifest::from_bytes(&existing_manifest.to_bytes())
            .with_context(|| format!("parsing existing manifest for dedup: {remote_manifest}"))?;
        let chunk_count = existing_manifest.chunk_hashes().len();

        if let Some(rp) = rel_path {
            publish_index_reference(
                op,
                remote_prefix,
                rp,
                RemoteIndexEntry::new(file_hash_hex.clone(), file_size, chunk_count),
            )
            .await?;
        }

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
            chunk_exists_check = !assume_fresh_prefix,
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
    if let Some(rp) = rel_path {
        publish_manifest_for_rel_path_with_mode(
            op,
            remote_prefix,
            rp,
            manifest_bytes,
            RemoteIndexEntry::new(file_hash_hex.clone(), file_size, num_chunks),
            assume_fresh_prefix,
        )
        .await?;
    } else {
        op.write(&remote_manifest, manifest_bytes)
            .await
            .with_context(|| format!("uploading manifest: {remote_manifest}"))?;
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
        chunk_exists_check = !assume_fresh_prefix,
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
            skipped: false,
            outcome,
        },
        Some(sync_state),
    ))
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
    let target = read_symlink_target_text(local_path)?;
    let symlink_hash = symlink_manifest_hash(&target);
    let remote_manifest = format!("{remote_prefix}/manifests/{symlink_hash}");

    let mut vclock = VectorClock::new();
    if !device_id.is_empty() {
        vclock.tick(device_id);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let manifest = SymlinkManifest::new(
        target.clone(),
        vclock,
        device_id.to_string(),
        now,
        Some(rel_path.to_string()),
    );
    publish_manifest_for_rel_path(
        op,
        remote_prefix,
        rel_path,
        manifest.to_bytes()?,
        RemoteIndexEntry::new_symlink(symlink_hash.clone(), target.clone()),
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
    state.set(local_path, sync_state);

    let assume_fresh_prefix = upload_assume_fresh_prefix();
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
        skipped: false,
        outcome: None,
    })
}

fn read_symlink_target_text(path: &Path) -> Result<String> {
    let target = std::fs::read_link(path)
        .with_context(|| format!("reading symlink target: {}", path.display()))?;
    target
        .to_str()
        .map(|s| s.to_string())
        .with_context(|| format!("symlink target is not valid UTF-8: {}", path.display()))
}

fn symlink_manifest_hash(target: &str) -> String {
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
    // Read manifest with retry so transient storage failures don't abort pull
    // paths immediately while chunk reads already back off.
    let manifest_bytes = read_with_retry(op, remote_manifest)
        .await
        .with_context(|| format!("reading manifest: {remote_manifest}"))?;

    if let Ok(manifest) = SymlinkManifest::from_bytes(&manifest_bytes) {
        create_local_symlink(local_path, &manifest.symlink_target).await?;
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

    let manifest = SyncManifest::from_bytes(&manifest_bytes)
        .with_context(|| format!("parsing manifest: {remote_manifest}"))?;

    let chunk_hashes = manifest.chunk_hashes();

    // Empty file: no chunks to fetch — write an empty file directly
    if chunk_hashes.is_empty() {
        if let Some(parent) = local_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating dir: {}", parent.display()))?;
        }

        let tmp = unique_tmp_path(local_path, "tcfs_tmp");
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

    // Fetch and reassemble chunks, verifying each chunk's BLAKE3 hash.
    // Write directly to a unique temp file so multi-GB files do not require a
    // second full in-memory copy before the atomic rename.
    let total = chunk_hashes.len();
    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating dir: {}", parent.display()))?;
    }

    let tmp = unique_tmp_path(local_path, "tcfs_tmp");
    let mut tmp_file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating tmp: {}", tmp.display()))?;
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

    // Verify reassembled file hash matches the manifest (plaintext hash)
    let actual_file_hash = tcfs_chunks::hash_to_hex(&hasher.finalize());
    if actual_file_hash != manifest.file_hash {
        let _ = tokio::fs::remove_file(&tmp).await;
        anyhow::bail!(
            "file integrity check failed for {remote_manifest}: expected {}, got {actual_file_hash}",
            manifest.file_hash
        );
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

async fn create_local_symlink(local_path: &Path, target: &str) -> Result<()> {
    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating dir: {}", parent.display()))?;
    }

    let tmp = unique_tmp_path(local_path, "tcfs_symlink_tmp");
    let _ = tokio::fs::remove_file(&tmp).await;

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

    tokio::fs::rename(&tmp, local_path)
        .await
        .with_context(|| format!("renaming symlink to: {}", local_path.display()))?;
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
            let marker_key = format!("{}/index/{}/.tcfs_dir", remote_prefix, rel_str);
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
            if let Some(reason) = blacklist.check_name(name, ft.is_dir()) {
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

    let manifest_prefix = manifest_path_prefix(prefix);
    if let Ok(Some(entry)) = resolve_visible_index_entry(op, &index_key, &manifest_prefix).await {
        return Ok(manifest_key(&manifest_prefix, &entry.manifest_hash));
    }

    // Try 2: Search index entries for a matching filename.
    // This handles cross-host pull where the pushing host's canonicalized path
    // differs from the pulling host's (e.g., /tmp → /private/tmp on macOS).
    let filename = Path::new(input)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| input.to_string());
    let filename = normalize_rel_path_text(&filename);

    let index_prefix = format!("{prefix}/index/");
    let entries = op
        .list(&index_prefix)
        .await
        .with_context(|| format!("listing index prefix: {index_prefix}"))?;

    for entry in entries {
        let entry_path = entry.path();
        if entry_path.ends_with(&format!("/{filename}")) || entry_path.ends_with(&filename) {
            if let Ok(Some(entry)) =
                resolve_visible_index_entry(op, entry_path, &manifest_prefix).await
            {
                return Ok(manifest_key(&manifest_prefix, &entry.manifest_hash));
            }
        }
    }

    anyhow::bail!(
        "no index entry found for '{}' (tried: {index_key}, filename search: {filename})",
        input
    )
}

async fn manifest_hash_referenced_by_index(
    op: &Operator,
    index_key: &str,
    manifest_hash: &str,
) -> Result<bool> {
    let Some(parsed) = read_index_entry_record_from_store(op, index_key).await? else {
        return Ok(false);
    };

    Ok(parsed
        .visible_entry()
        .map(|entry| entry.manifest_hash == manifest_hash)
        .unwrap_or(false)
        || parsed
            .pending_entry()
            .map(|entry| entry.manifest_hash == manifest_hash)
            .unwrap_or(false))
}

async fn manifest_hash_referenced_elsewhere(
    op: &Operator,
    index_prefix: &str,
    deleted_index_key: &str,
    manifest_hash: &str,
) -> Result<bool> {
    let entries = op
        .list(index_prefix)
        .await
        .with_context(|| format!("listing index prefix: {index_prefix}"))?;

    for entry in entries {
        let candidate = entry.path();
        if candidate == deleted_index_key {
            continue;
        }
        if manifest_hash_referenced_by_index(op, candidate, manifest_hash).await? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Delete a remote index entry and any now-unreferenced manifest objects.
///
/// Manifests are addressed by content hash, so multiple visible paths can
/// legitimately point at the same manifest. Delete removes the path's index
/// entry and preserves committed manifests still referenced by another index
/// entry. Chunks are left for GC.
pub async fn delete_remote_index_entry(
    op: &Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<()> {
    let rel_path = normalize_rel_path_text(rel_path.trim_start_matches('/'));
    let prefix = remote_prefix.trim_end_matches('/');
    let index_key = format!("{prefix}/index/{rel_path}");
    let index_prefix = format!("{prefix}/index/");
    let manifest_prefix = manifest_path_prefix(prefix);
    let parsed = read_index_entry_record_from_store(op, &index_key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing index entry: {index_key}"))?;

    let mut manifest_hashes = Vec::new();
    if let Some(entry) = parsed.visible_entry() {
        manifest_hashes.push(entry.manifest_hash.clone());
    }
    if let Some(entry) = parsed.pending_entry() {
        manifest_hashes.push(entry.manifest_hash.clone());
    }
    manifest_hashes.sort();
    manifest_hashes.dedup();

    let staged_manifest_keys: Vec<String> = parsed
        .pending_entry()
        .map(|entry| entry.staged_manifest_key.clone())
        .into_iter()
        .collect();

    op.delete(&index_key)
        .await
        .with_context(|| format!("deleting index entry: {index_key}"))?;

    for object_key in staged_manifest_keys {
        if let Err(e) = op.delete(&object_key).await {
            debug!(rel_path = %rel_path, object = %object_key, "best-effort staged manifest delete failed: {e}");
        }
    }

    for manifest_hash in manifest_hashes {
        let object_key = manifest_key(&manifest_prefix, &manifest_hash);
        match manifest_hash_referenced_elsewhere(op, &index_prefix, &index_key, &manifest_hash)
            .await
        {
            Ok(true) => {
                debug!(
                    rel_path = %rel_path,
                    object = %object_key,
                    "preserving manifest still referenced by another index entry"
                );
            }
            Ok(false) => {
                op.delete(&object_key)
                    .await
                    .with_context(|| format!("deleting manifest: {object_key}"))?;
            }
            Err(e) => {
                debug!(
                    rel_path = %rel_path,
                    object = %object_key,
                    "preserving manifest because reference scan failed: {e}"
                );
            }
        }
    }

    info!(rel_path = %rel_path, "deleted remote index entry");
    Ok(())
}

/// Delete a file from remote storage (index entry + unreferenced manifests).
///
/// Looks up the index entry for `rel_path`, deletes that visible path, and
/// removes manifest objects only when no other index entry still references
/// them. Chunks are left for GC.
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
        parse_index_entry_record, write_committed_index_entry, IndexEntryState, ParsedIndexEntry,
    };
    use opendal::services::Memory;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn memory_op() -> Operator {
        Operator::new(Memory::default()).unwrap().finish()
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

    async fn staging_manifest_keys(op: &Operator) -> Vec<String> {
        op.list("data/staging/manifests/")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path().to_string())
            .collect()
    }

    fn committed_manifest_hash(raw: &[u8]) -> String {
        match parse_index_entry_record(raw).unwrap() {
            ParsedIndexEntry::Legacy(entry) => entry.manifest_hash,
            ParsedIndexEntry::V2(entry) => entry.current.unwrap().manifest_hash,
        }
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
    async fn resolve_manifest_missing_errors() {
        let op = memory_op();
        let result = resolve_manifest_path(&op, "nonexistent.txt", "data", None).await;
        assert!(result.is_err());
    }

    // ── delete_remote_file ───────────────────────────────────────────────

    #[tokio::test]
    async fn delete_remote_file_removes_index_and_manifest() {
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

        // Both should be gone
        assert!(op.read("data/index/file.txt").await.is_err());
        assert!(op.read("data/manifests/abc123").await.is_err());
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

        assert!(op.read("data/index/a.txt").await.is_err());
        assert!(op.read("data/index/b.txt").await.is_ok());
        assert!(op.read("data/manifests/abc123").await.is_ok());
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
            encrypted_file_key: None,
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
                assert_eq!(current.manifest_hash, upload.hash);
                assert_eq!(current.size, upload.bytes);
                assert_eq!(current.chunks, upload.chunks);
            }
        }
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
            br#"{"version":2,"file_hash":"remotehash123","file_size":12,"chunks":[],"vclock":{"clocks":{"device-2":1}},"written_by":"device-2","written_at":1}"#.to_vec(),
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
        assert!(matches!(result.outcome, Some(SyncOutcome::Conflict(_))));

        let entry = state.get(&local).expect("conflicted state entry");
        assert_eq!(entry.status, FileSyncStatus::Conflict);
        assert!(
            entry.conflict.is_some(),
            "conflict payload should be preserved"
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
    async fn maybe_upload_chunk_assume_fresh_prefix_skips_exists_gate() {
        let op = memory_op();
        let key = "data/chunks/existing".to_string();
        op.write(&key, b"old".to_vec()).await.unwrap();

        let skipped =
            maybe_upload_chunk(op.clone(), key.clone(), b"new".to_vec(), 0, 3, false, None)
                .await
                .unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(op.read(&key).await.unwrap().to_bytes(), b"old".as_slice());

        let uploaded =
            maybe_upload_chunk(op.clone(), key.clone(), b"new".to_vec(), 0, 3, true, None)
                .await
                .unwrap();
        assert_eq!(uploaded, 3);
        assert_eq!(op.read(&key).await.unwrap().to_bytes(), b"new".as_slice());
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

        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            test_manifest_bytes("new456", 11),
            RemoteIndexEntry::new("new456", 11, 1),
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
        assert!(!op.exists("data/manifests/new456").await.unwrap());
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

        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            test_manifest_bytes("new456", 11),
            RemoteIndexEntry::new("new456", 11, 1),
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
        assert!(!op.exists("data/manifests/new456").await.unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected preparing v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Preparing);
                assert!(entry.current.is_none());
                assert_eq!(entry.pending.unwrap().manifest_hash, "new456");
            }
        }

        assert_eq!(
            resolve_manifest_path(&op, "doc.txt", "data", None)
                .await
                .unwrap(),
            "data/manifests/new456"
        );
        assert!(op.exists("data/manifests/new456").await.unwrap());
        assert!(staging_manifest_keys(&op).await.is_empty());

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, "new456");
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

        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            test_manifest_bytes("new456", 11),
            RemoteIndexEntry::new("new456", 11, 1),
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
        assert!(op.exists("data/manifests/new456").await.unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected preparing v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Preparing);
                assert_eq!(entry.current.unwrap().manifest_hash, "old123");
                assert_eq!(entry.pending.unwrap().manifest_hash, "new456");
            }
        }

        assert_eq!(
            resolve_manifest_path(&op, "doc.txt", "data", None)
                .await
                .unwrap(),
            "data/manifests/new456"
        );
        assert!(staging_manifest_keys(&op).await.is_empty());

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, "new456");
                assert!(entry.pending.is_none());
            }
        }
    }

    #[tokio::test]
    async fn publish_crash_after_committed_write_keeps_new_manifest_visible() {
        let op = memory_op();

        let err = publish_manifest_for_rel_path_with_hook(
            &op,
            "data",
            "doc.txt",
            test_manifest_bytes("new456", 11),
            RemoteIndexEntry::new("new456", 11, 1),
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
            "data/manifests/new456"
        );
        assert!(op.exists("data/manifests/new456").await.unwrap());
        assert_eq!(staging_manifest_keys(&op).await.len(), 1);

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, "new456");
                assert!(entry.pending.is_none());
            }
        }
    }

    #[tokio::test]
    async fn fresh_prefix_publish_writes_committed_index_without_staging() {
        let op = memory_op();
        publish_manifest_for_rel_path_fresh(
            &op,
            "data",
            "doc.txt",
            test_manifest_bytes("new456", 11),
            RemoteIndexEntry::new("new456", 11, 1),
        )
        .await
        .unwrap();

        assert!(op.exists("data/manifests/new456").await.unwrap());
        assert!(staging_manifest_keys(&op).await.is_empty());

        match parse_index_entry_record(&op.read("data/index/doc.txt").await.unwrap().to_vec())
            .unwrap()
        {
            ParsedIndexEntry::Legacy(_) => panic!("expected committed v2 index entry"),
            ParsedIndexEntry::V2(entry) => {
                assert_eq!(entry.state, IndexEntryState::Committed);
                assert_eq!(entry.current.unwrap().manifest_hash, "new456");
                assert!(entry.pending.is_none());
            }
        }
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
        assert_eq!(a_hash, b_hash);
        assert!(op
            .exists(&format!("data/manifests/{a_hash}"))
            .await
            .unwrap());
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
