//! gRPC backend for the FileProvider FFI.
//!
//! Delegates all operations to the tcfsd daemon via gRPC.
//! This enables full fleet sync, NATS events, and conflict resolution —
//! the daemon handles E2EE, chunking, and storage internally.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;

use anyhow::Context;
use base64::Engine;
use secrecy::SecretString;
use tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::{to_c_string, TcfsChangeEvent, TcfsError, TcfsFileItem};

const FILE_PROVIDER_READ_ONLY_ERROR: &str =
    "TCFS FileProvider is read-only until exact version-token conditional publication is available";
const FILE_PROVIDER_VERSION_MISMATCH_PREFIX: &str = "file-provider version mismatch:";
const INCREMENTAL_WATCH_UNAVAILABLE_PREFIX: &str =
    "authoritative incremental Watch journal is unavailable;";

fn require_terminal_progress(completed: bool, operation: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        completed,
        "{operation} progress stream ended before terminal completion"
    );
    Ok(())
}

fn daemon_reported_version_mismatch(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::FailedPrecondition
        && status
            .message()
            .starts_with(FILE_PROVIDER_VERSION_MISMATCH_PREFIX)
}

fn daemon_reported_sync_anchor_expired(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::FailedPrecondition
        && status
            .message()
            .starts_with(INCREMENTAL_WATCH_UNAVAILABLE_PREFIX)
}

async fn daemon_exact_version(
    client: &mut TcfsDaemonClient<Channel>,
    remote_path: &str,
    requested_version: Option<String>,
) -> anyhow::Result<String> {
    if let Some(version) = requested_version.filter(|version| !version.is_empty()) {
        return Ok(version);
    }

    let parent = remote_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let listing = client
        .list_files(tonic::Request::new(tcfs_core::proto::ListFilesRequest {
            prefix: parent.to_string(),
        }))
        .await?
        .into_inner();
    let entry = listing
        .files
        .into_iter()
        .find(|entry| !entry.is_directory && entry.path == remote_path)
        .with_context(|| format!("no exact FileProvider item in daemon listing: {remote_path}"))?;
    anyhow::ensure!(
        !entry.version_token.is_empty(),
        "daemon listing omitted exact version token for {remote_path}"
    );
    Ok(entry.version_token)
}

fn validate_exact_pull_terminal(
    message: &tcfs_core::proto::PullProgress,
    expected_version: &str,
    bytes_received: u64,
) -> anyhow::Result<bool> {
    if !message.done {
        return Ok(false);
    }
    anyhow::ensure!(
        message.exact_content,
        "PullExact terminal response omitted the exact-content protocol marker"
    );
    anyhow::ensure!(
        !message.version_token.is_empty(),
        "PullExact terminal response omitted its selected version token"
    );
    crate::ensure_file_provider_version(Some(expected_version), &message.version_token)?;
    anyhow::ensure!(
        bytes_received == message.total_bytes,
        "PullExact terminal length mismatch: received {bytes_received}, expected {}",
        message.total_bytes
    );
    Ok(true)
}

async fn fetch_exact_from_daemon(
    target: &DaemonTarget,
    remote_path: String,
    requested_version: Option<String>,
    destination: PathBuf,
    progress: Option<tcfs_sync::engine::ProgressFn>,
) -> anyhow::Result<()> {
    use std::io::Write;

    let mut client = connect_once(target).await?;
    let expected_version =
        daemon_exact_version(&mut client, &remote_path, requested_version).await?;
    let mut stream = client
        .pull_exact(tonic::Request::new(tcfs_core::proto::PullExactRequest {
            remote_path: remote_path.clone(),
            expected_version: expected_version.clone(),
        }))
        .await?
        .into_inner();

    let destination_parent = destination
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut staging = tempfile::NamedTempFile::new_in(destination_parent).with_context(|| {
        format!(
            "creating FileProvider staging file beside {}",
            destination.display()
        )
    })?;
    let mut bytes_received = 0u64;
    let mut advertised_total: Option<u64> = None;
    let mut terminal_seen = false;

    while let Some(message) = stream.message().await? {
        anyhow::ensure!(
            !terminal_seen,
            "PullExact sent data after its terminal exact-content marker"
        );
        anyhow::ensure!(message.error.is_empty(), "{}", message.error);
        anyhow::ensure!(
            !message.exact_content || message.done,
            "PullExact exact-content marker appeared on a non-terminal message"
        );

        match advertised_total {
            Some(total) => anyhow::ensure!(
                message.total_bytes == total,
                "PullExact changed total bytes from {total} to {}",
                message.total_bytes
            ),
            None => advertised_total = Some(message.total_bytes),
        }

        if !message.data.is_empty() {
            staging
                .as_file_mut()
                .write_all(&message.data)
                .with_context(|| {
                    format!(
                        "writing staged FileProvider content for {}",
                        destination.display()
                    )
                })?;
            bytes_received = bytes_received
                .checked_add(message.data.len() as u64)
                .context("PullExact client byte counter overflow")?;
        }
        anyhow::ensure!(
            message.bytes_received == bytes_received,
            "PullExact cumulative byte mismatch: server {}, client {}",
            message.bytes_received,
            bytes_received
        );

        if let Some(callback) = progress.as_ref() {
            callback(bytes_received, message.total_bytes, "PullExact");
        }

        terminal_seen = validate_exact_pull_terminal(&message, &expected_version, bytes_received)?;
    }

    require_terminal_progress(terminal_seen, "PullExact exact-content")?;
    staging.as_file_mut().flush().with_context(|| {
        format!(
            "flushing staged FileProvider content for {}",
            destination.display()
        )
    })?;
    staging.persist(&destination).map_err(|error| {
        anyhow::anyhow!(
            "persisting staged FileProvider content to {}: {}",
            destination.display(),
            error.error
        )
    })?;
    Ok(())
}

/// Opaque provider handle wrapping a tokio runtime + gRPC client.
///
/// Created via `tcfs_provider_new`, freed via `tcfs_provider_free`.
pub struct TcfsProvider {
    runtime: tokio::runtime::Runtime,
    client: TcfsDaemonClient<Channel>,
    /// Remote prefix for path construction
    remote_prefix: String,
    device_id: String,
    /// Direct storage handle used for FileProvider content fetches. The daemon
    /// remains authoritative for enumeration and watch events, but it must not
    /// write into the extension's sandbox temp container.
    direct_operator: Option<opendal::Operator>,
    direct_master_key: Option<tcfs_crypto::MasterKey>,
    /// Per-device unwrap identity for reading `wrapped_file_keys` manifests
    /// (TIN-1417). `None` (the default) preserves master-only behavior.
    direct_device_identity: Option<tcfs_sync::engine::DeviceUnwrapIdentity>,
    /// Active-device recipients carried alongside the identity so the read
    /// context can be reconstructed per fetch. Empty in the default config.
    direct_device_recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
    /// Daemon connection target for lazy reconnection.
    target: DaemonTarget,
    last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DaemonTarget {
    Unix(PathBuf),
    Endpoint(String),
}

impl DaemonTarget {
    fn label(&self) -> String {
        match self {
            Self::Unix(path) => path.display().to_string(),
            Self::Endpoint(endpoint) => endpoint.clone(),
        }
    }
}

/// Connect to the daemon with retry.
///
/// Retries up to `max_retries` times with exponential backoff (200ms base).
/// This handles the case where the daemon hasn't started yet when the
/// FileProvider extension is loaded by fileproviderd.
async fn connect_with_retry(
    target: &DaemonTarget,
    max_retries: u32,
) -> Result<TcfsDaemonClient<Channel>, anyhow::Error> {
    let mut last_err = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = std::time::Duration::from_millis(200 * 2u64.pow(attempt - 1));
            tokio::time::sleep(backoff).await;
        }

        match connect_once(target).await {
            Ok(client) => return Ok(client),
            Err(e) => {
                tracing::warn!(
                    attempt = attempt + 1,
                    max = max_retries + 1,
                    "connect to tcfsd failed: {e}"
                );
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("connect failed")))
}

/// Single connection attempt to the daemon.
async fn connect_once(target: &DaemonTarget) -> Result<TcfsDaemonClient<Channel>, anyhow::Error> {
    match target {
        DaemonTarget::Unix(path) => {
            let path = path.clone();
            let channel = Endpoint::from_static("http://[::]:0")
                .connect_with_connector(service_fn(move |_: Uri| {
                    let path = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(&path).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                    }
                }))
                .await?;
            Ok(TcfsDaemonClient::new(channel))
        }
        DaemonTarget::Endpoint(endpoint) => {
            let channel = Endpoint::from_shared(endpoint.clone())?.connect().await?;
            Ok(TcfsDaemonClient::new(channel))
        }
    }
}

fn normalize_endpoint(endpoint: &str) -> String {
    if endpoint.contains("://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    }
}

fn endpoint_from_config(config: &serde_json::Value) -> Option<String> {
    config["daemon_endpoint"]
        .as_str()
        .map(normalize_endpoint)
        .or_else(|| {
            std::env::var("TCFS_ENDPOINT")
                .ok()
                .map(|endpoint| normalize_endpoint(&endpoint))
        })
}

fn default_socket_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let state_home =
        std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| format!("{home}/.local/state"));
    let xdg_path = PathBuf::from(format!("{state_home}/tcfsd/tcfsd.sock"));

    if xdg_path.exists() {
        return xdg_path;
    }

    // Sandboxed macOS extensions: try App Group container.
    let app_group = PathBuf::from(format!(
        "{home}/Library/Group Containers/group.io.tinyland.tcfs/tcfsd.sock"
    ));
    if app_group.exists() {
        return app_group;
    }

    xdg_path
}

fn target_from_config(config: &serde_json::Value) -> DaemonTarget {
    if let Some(endpoint) = endpoint_from_config(config) {
        return DaemonTarget::Endpoint(endpoint);
    }

    config["daemon_socket"]
        .as_str()
        .map(|path| DaemonTarget::Unix(PathBuf::from(path)))
        .or_else(|| {
            std::env::var("TCFS_SOCKET")
                .ok()
                .map(PathBuf::from)
                .map(DaemonTarget::Unix)
        })
        .unwrap_or_else(|| DaemonTarget::Unix(default_socket_path()))
}

fn build_direct_operator(config: &serde_json::Value) -> Option<opendal::Operator> {
    crate::storage_bounds::build_operator_from_json(config)
}

fn master_key_from_bytes(bytes: &[u8]) -> anyhow::Result<tcfs_crypto::MasterKey> {
    if bytes.len() != tcfs_crypto::KEY_SIZE {
        anyhow::bail!(
            "master key must be {} bytes, got {}",
            tcfs_crypto::KEY_SIZE,
            bytes.len()
        );
    }

    let mut key = [0u8; tcfs_crypto::KEY_SIZE];
    key.copy_from_slice(bytes);
    Ok(tcfs_crypto::MasterKey::from_bytes(key))
}

fn required_encryption_flag(config: &serde_json::Value) -> anyhow::Result<bool> {
    for key in [
        "encryption_required",
        "require_encryption",
        "crypto_required",
    ] {
        let Some(value) = config.get(key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let enabled = value
            .as_bool()
            .with_context(|| format!("{key} must be a boolean"))?;
        if enabled {
            return Ok(true);
        }
    }

    for section_name in ["crypto", "encryption"] {
        let Some(section) = config.get(section_name) else {
            continue;
        };
        match section {
            serde_json::Value::Null | serde_json::Value::Bool(false) => {}
            serde_json::Value::Bool(true) => return Ok(true),
            serde_json::Value::String(mode)
                if matches!(mode.as_str(), "plaintext" | "disabled" | "none") => {}
            serde_json::Value::String(mode) if matches!(mode.as_str(), "required" | "enabled") => {
                return Ok(true);
            }
            serde_json::Value::Object(values) => {
                for key in ["required", "enabled"] {
                    let Some(value) = values.get(key) else {
                        continue;
                    };
                    let enabled = value
                        .as_bool()
                        .with_context(|| format!("{section_name}.{key} must be a boolean"))?;
                    if enabled {
                        return Ok(true);
                    }
                }
            }
            _ => anyhow::bail!("{section_name} must be a boolean, a supported mode, or an object"),
        }
    }
    Ok(false)
}

fn configured_nonempty_string<'a>(
    config: &'a serde_json::Value,
    key: &str,
) -> anyhow::Result<Option<&'a str>> {
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .with_context(|| format!("{key} must be a string"))?;
    if value.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        !value.trim().is_empty(),
        "{key} must not be whitespace-only"
    );
    Ok(Some(value))
}

fn configured_u32(config: &serde_json::Value, key: &str, default: u32) -> anyhow::Result<u32> {
    let Some(value) = config.get(key) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(default);
    }
    let value = value
        .as_u64()
        .with_context(|| format!("{key} must be an unsigned integer"))?;
    u32::try_from(value).with_context(|| format!("{key} exceeds u32 range"))
}

fn derive_master_key_from_config(
    config: &serde_json::Value,
) -> anyhow::Result<Option<tcfs_crypto::MasterKey>> {
    let encryption_required = required_encryption_flag(config)?;

    if let Some(encoded) = configured_nonempty_string(config, "master_key_base64")? {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .context("decoding master_key_base64")?;
        return master_key_from_bytes(&decoded)
            .context("validating master_key_base64")
            .map(Some);
    }

    if let Some(path) = configured_nonempty_string(config, "master_key_file")? {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading configured master_key_file: {path}"))?;
        return master_key_from_bytes(&bytes)
            .with_context(|| format!("validating configured master_key_file: {path}"))
            .map(Some);
    }

    let Some(passphrase) = configured_nonempty_string(config, "encryption_passphrase")? else {
        anyhow::ensure!(
            !encryption_required,
            "encryption is required but no master key or passphrase is configured"
        );
        return Ok(None);
    };

    if passphrase.split_whitespace().count() >= 12 {
        return tcfs_crypto::mnemonic_to_master_key(passphrase)
            .context("deriving master key from configured mnemonic")
            .map(Some);
    }

    let salt_str = match config.get("encryption_salt") {
        None | Some(serde_json::Value::Null) => "tcfs-default-salt!",
        Some(value) => value.as_str().context("encryption_salt must be a string")?,
    };
    let mut salt = [0u8; 16];
    let salt_bytes = salt_str.as_bytes();
    let copy_len = salt_bytes.len().min(16);
    salt[..copy_len].copy_from_slice(&salt_bytes[..copy_len]);

    let params = tcfs_crypto::kdf::KdfParams {
        mem_cost_kib: configured_u32(config, "argon2_mem_cost_kib", 65536)?,
        time_cost: configured_u32(config, "argon2_time_cost", 3)?,
        parallelism: configured_u32(config, "argon2_parallelism", 4)?,
    };

    tcfs_crypto::derive_master_key(&SecretString::from(passphrase.to_string()), &salt, &params)
        .context("deriving master key from configured passphrase")
        .map(Some)
}

fn error_code_for_fetch_error(error: &anyhow::Error) -> TcfsError {
    if crate::is_file_provider_version_mismatch(error)
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<tonic::Status>()
                .is_some_and(daemon_reported_version_mismatch)
        })
    {
        TcfsError::TcfsErrorVersionMismatch
    } else if error.chain().any(|cause| {
        cause
            .downcast_ref::<opendal::Error>()
            .is_some_and(|e| e.kind() == opendal::ErrorKind::NotFound)
            || cause
                .downcast_ref::<tonic::Status>()
                .is_some_and(|status| status.code() == tonic::Code::NotFound)
    }) {
        TcfsError::TcfsErrorNotFound
    } else {
        TcfsError::TcfsErrorStorage
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_direct_to_file(
    operator: opendal::Operator,
    remote_prefix: String,
    device_id: String,
    master_key: Option<tcfs_crypto::MasterKey>,
    device_recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
    device_identity: Option<tcfs_sync::engine::DeviceUnwrapIdentity>,
    remote_path: String,
    requested_version: Option<String>,
    dest_path: PathBuf,
    progress: Option<tcfs_sync::engine::ProgressFn>,
) -> anyhow::Result<()> {
    if requested_version
        .as_deref()
        .is_some_and(|version| !version.is_empty())
    {
        let selected = tcfs_sync::engine::read_exact_visible_index_selection(
            &operator,
            &remote_path,
            &remote_prefix,
        )
        .await
        .with_context(|| format!("reading exact index version for: {remote_path}"))?
        .ok_or_else(|| {
            opendal::Error::new(
                opendal::ErrorKind::NotFound,
                "no exact visible index version for FileProvider item",
            )
            .with_context("path", remote_path.clone())
        })?;
        crate::ensure_file_provider_version(requested_version.as_deref(), &selected.manifest_hash)?;
    }

    let resolved_manifest = tcfs_sync::engine::resolve_exact_manifest_reference(
        &operator,
        &remote_path,
        &remote_prefix,
    )
    .await
    .with_context(|| format!("resolving exact manifest for: {remote_path}"))?
    .ok_or_else(|| {
        opendal::Error::new(
            opendal::ErrorKind::NotFound,
            "no exact index entry for FileProvider item",
        )
        .with_context("path", remote_path.clone())
    })?;

    let current_manifest_id = resolved_manifest
        .manifest_path()
        .rsplit('/')
        .next()
        .filter(|id| !id.is_empty())
        .context("resolved FileProvider manifest path has no object id")?;
    crate::ensure_file_provider_version(requested_version.as_deref(), current_manifest_id)?;

    // Build the read context with this device's unwrap identity attached
    // (TIN-1417). This is a READ path, so `wrap_mode` (which only drives the
    // write path) is immaterial here — the engine's read switch branches on the
    // manifest's own shape (`wrapped_file_keys` vs `encrypted_file_key`). With
    // the default config `device_recipients` is empty and `device_identity` is
    // None, so this equals `EncryptionContext::new(mk)` (byte-identical for
    // master-only manifests). When a per-device (`wrapped_file_keys`) manifest is
    // read, the engine's read switch fails CLOSED with a clear error if no
    // identity is attached — it never silently master-falls-back.
    let enc_ctx = master_key.as_ref().map(|mk| {
        let mode = if device_recipients.is_empty() {
            tcfs_sync::engine::WrapMode::Master
        } else {
            tcfs_sync::engine::WrapMode::PerDevice
        };
        tcfs_sync::engine::EncryptionContext::new(mk.clone()).with_wrap_mode(
            mode,
            device_recipients,
            device_identity,
        )
    });

    tcfs_sync::engine::download_resolved_file_with_device(
        &operator,
        &resolved_manifest,
        &dest_path,
        &remote_prefix,
        progress.as_ref(),
        &device_id,
        None,
        enc_ctx.as_ref(),
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
async fn delete_remote_entry(
    operator: &opendal::Operator,
    remote_prefix: &str,
    item_id: &str,
) -> anyhow::Result<()> {
    tcfs_sync::engine::delete_remote_index_entry(operator, item_id, remote_prefix).await
}

/// Create a new provider from a JSON configuration string.
///
/// The JSON should contain:
/// ```json
/// {
///   "daemon_socket": "/path/to/tcfsd.sock",
///   "daemon_endpoint": "http://127.0.0.1:19101",
///   "remote_prefix": "devices/mydevice"
/// }
/// ```
///
/// `daemon_endpoint` is preferred when present. Otherwise the provider falls
/// back to `daemon_socket`, `$TCFS_SOCKET`, and then the local default socket.
///
/// # Safety
///
/// `config_json` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_new(config_json: *const c_char) -> *mut TcfsProvider {
    if config_json.is_null() {
        return ptr::null_mut();
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let c_str = unsafe { CStr::from_ptr(config_json) };
        let json_str = match c_str.to_str() {
            Ok(s) => s,
            Err(_) => return ptr::null_mut(),
        };

        let config: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(_) => return ptr::null_mut(),
        };

        let target = target_from_config(&config);

        let prefix = config["remote_prefix"]
            .as_str()
            .unwrap_or("tcfs") // match StorageConfig::default().bucket
            .to_string();
        let device_id = config["device_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let direct_master_key = match derive_master_key_from_config(&config) {
            Ok(key) => key,
            Err(error) => {
                tracing::error!(
                    error = %format!("{error:#}"),
                    "refusing invalid FileProvider encryption configuration"
                );
                return ptr::null_mut();
            }
        };
        let direct_operator = build_direct_operator(&config);

        // Build the DEVICE-AWARE read context once (TIN-1417). With the default
        // config (`per_device_wrapping` absent/false) this yields an empty
        // recipient set and no identity — byte-identical to the prior
        // master-only behavior. When enabled, it loads this device's age secret
        // so per-device (`wrapped_file_keys`) manifests become readable.
        let (direct_device_recipients, direct_device_identity) = match &direct_master_key {
            Some(mk) => {
                let ctx = crate::device_ctx::build_encryption_context(&config, &device_id, mk);
                (ctx.device_recipients, ctx.device_identity)
            }
            None => (Vec::new(), None),
        };

        // Multi-threaded runtime with 2 workers — one for the background watch
        // stream, one for synchronous FFI calls (enumerate, fetch, upload).
        // The gRPC backend only does network I/O (no file coordination), so
        // worker threads don't conflict with fileproviderd's XPC locks.
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return ptr::null_mut(),
        };

        let client = match runtime.block_on(connect_with_retry(&target, 8)) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to connect to tcfsd at {}: {}", target.label(), e);
                return ptr::null_mut();
            }
        };

        Box::into_raw(Box::new(TcfsProvider {
            runtime,
            client,
            remote_prefix: prefix,
            device_id,
            direct_operator,
            direct_master_key,
            direct_device_identity,
            direct_device_recipients,
            target,
            last_error: Mutex::new(None),
        }))
    }));

    result.unwrap_or(ptr::null_mut())
}

impl TcfsProvider {
    fn clear_last_error(&self) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = None;
        }
    }

    fn set_last_error(&self, error: impl Into<String>) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error.into());
        }
    }

    fn reject_file_provider_mutation(&self) -> TcfsError {
        self.clear_last_error();
        self.set_last_error(FILE_PROVIDER_READ_ONLY_ERROR);
        TcfsError::TcfsErrorConflict
    }

    /// Attempt to reconnect if the daemon connection was lost.
    fn try_reconnect(&mut self) {
        match self.runtime.block_on(connect_once(&self.target)) {
            Ok(new_client) => {
                tracing::info!("reconnected to tcfsd at {}", self.target.label());
                self.client = new_client;
            }
            Err(e) => {
                tracing::warn!("reconnect to tcfsd failed: {e}");
            }
        }
    }
}

/// Return the last backend error message recorded on this provider.
///
/// The caller owns the returned string and must free it with `tcfs_string_free`.
///
/// # Safety
///
/// `provider` must be a valid pointer from `tcfs_provider_new`.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_last_error(provider: *mut TcfsProvider) -> *mut c_char {
    if provider.is_null() {
        return ptr::null_mut();
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &*provider };
        prov.last_error
            .lock()
            .ok()
            .and_then(|last_error| last_error.clone())
            .map(|message| to_c_string(&message))
            .unwrap_or(ptr::null_mut())
    }));

    result.unwrap_or(ptr::null_mut())
}

/// Enumerate files by querying daemon sync status.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `path` must be a valid null-terminated UTF-8 C string (use "" for root).
/// - `out_items` and `out_count` must be valid writable pointers.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_enumerate(
    provider: *mut TcfsProvider,
    path: *const c_char,
    out_items: *mut *mut TcfsFileItem,
    out_count: *mut usize,
) -> TcfsError {
    if provider.is_null() || path.is_null() || out_items.is_null() || out_count.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_path = unsafe { CStr::from_ptr(path) };
        let rel_path = match c_path.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let enumerate_result = prov.runtime.block_on(async {
            // Query daemon for all files matching the prefix
            let resp = prov
                .client
                .list_files(tonic::Request::new(tcfs_core::proto::ListFilesRequest {
                    prefix: rel_path.to_string(),
                }))
                .await?;

            let list = resp.into_inner();

            let items: Vec<TcfsFileItem> = list
                .files
                .into_iter()
                .map(|entry| TcfsFileItem {
                    item_id: to_c_string(&entry.path),
                    filename: to_c_string(&entry.filename),
                    file_size: entry.size,
                    modified_timestamp: entry.last_synced,
                    is_directory: entry.is_directory,
                    content_hash: to_c_string(&entry.version_token),
                    hydration_state: to_c_string(&entry.hydration_state),
                })
                .collect();

            Ok::<Vec<TcfsFileItem>, tonic::Status>(items)
        });

        match enumerate_result {
            Ok(items) => {
                let count = items.len();
                let boxed = items.into_boxed_slice();
                let ptr = Box::into_raw(boxed) as *mut TcfsFileItem;
                unsafe {
                    *out_items = ptr;
                    *out_count = count;
                }
                TcfsError::TcfsErrorNone
            }
            Err(e) => {
                tracing::error!("enumerate failed: {}, attempting reconnect", e);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Request changes through the daemon's authority-checked Watch RPC.
///
/// Positive timestamp cursors map the daemon's deliberate journal-unavailable
/// response to `TcfsErrorSyncAnchorExpired`; zero is live-only and never claims
/// historical completeness.
///
/// # Safety
///
/// - `provider` must be a valid `TcfsProvider` pointer.
/// - `path` must be a valid UTF-8 C string.
/// - `out_events` and `out_count` must be valid, non-null pointers.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_enumerate_changes(
    provider: *mut TcfsProvider,
    path: *const c_char,
    since_timestamp: i64,
    out_events: *mut *mut TcfsChangeEvent,
    out_count: *mut usize,
) -> TcfsError {
    if provider.is_null() || path.is_null() || out_events.is_null() || out_count.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_path = unsafe { CStr::from_ptr(path) };
        let rel_path = match c_path.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let changes_result = prov.runtime.block_on(async {
            use tokio::time::{timeout, Duration};

            // Positive cursors fail before a stream is returned until tcfsd
            // has an authoritative journal. Zero opens a live-only stream.
            let resp = prov
                .client
                .watch(tonic::Request::new(tcfs_core::proto::WatchRequest {
                    paths: vec![rel_path.to_string()],
                    since_timestamp,
                }))
                .await?;

            let mut stream = resp.into_inner();
            let mut events = Vec::new();

            // Collect only an immediately available live burst. This is never
            // treated as historical catch-up for a positive cursor.
            loop {
                match timeout(Duration::from_millis(500), stream.message()).await {
                    Ok(Ok(Some(watch_event))) => {
                        events.push(TcfsChangeEvent {
                            path: to_c_string(&watch_event.path),
                            filename: to_c_string(&watch_event.filename),
                            event_type: to_c_string(&watch_event.event_type),
                            timestamp: watch_event.timestamp,
                            file_size: watch_event.size,
                            content_hash: to_c_string(&watch_event.version_token),
                            is_directory: watch_event.is_directory,
                        });
                    }
                    Ok(Ok(None)) => break, // Stream ended
                    Ok(Err(e)) => return Err(e),
                    Err(_) => break, // Timeout — initial burst done
                }
            }

            Ok::<Vec<TcfsChangeEvent>, tonic::Status>(events)
        });

        match changes_result {
            Ok(events) => {
                let count = events.len();
                let boxed = events.into_boxed_slice();
                let ptr = Box::into_raw(boxed) as *mut TcfsChangeEvent;
                unsafe {
                    *out_events = ptr;
                    *out_count = count;
                }
                TcfsError::TcfsErrorNone
            }
            Err(e) => {
                if daemon_reported_sync_anchor_expired(&e) {
                    tracing::info!("enumerate_changes anchor expired: {e}");
                    return TcfsError::TcfsErrorSyncAnchorExpired;
                }
                tracing::error!("enumerate_changes failed: {}, attempting reconnect", e);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Fetch a file via the daemon's exact-path Pull RPC.
///
/// Uses the distinct `PullExact` method (not `Pull` or `Hydrate`). Daemons
/// without that RPC return `UNIMPLEMENTED`; the prior request shape decodes
/// field 2 as an empty local destination and rejects it before filesystem I/O.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` and `dest_path` must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
) -> TcfsError {
    unsafe { tcfs_provider_fetch_versioned_impl(provider, item_id, dest_path, ptr::null()) }
}

/// Fetch only the immutable manifest version exposed during enumeration.
///
/// # Safety
///
/// String pointers must be valid null-terminated UTF-8. `requested_version`
/// may be null or empty for an unconditional exact-current read.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch_versioned(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
) -> TcfsError {
    unsafe { tcfs_provider_fetch_versioned_impl(provider, item_id, dest_path, requested_version) }
}

unsafe fn tcfs_provider_fetch_versioned_impl(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        let c_requested_version = if requested_version.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(requested_version) })
        };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let requested_version = match c_requested_version.map(CStr::to_str).transpose() {
            Ok(version) => version
                .filter(|version| !version.is_empty())
                .map(str::to_string),
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let remote = item_str.to_string();
        let dest = PathBuf::from(dest_str);
        let direct_operator = prov.direct_operator.clone();
        let remote_prefix = prov.remote_prefix.clone();
        let device_id = prov.device_id.clone();
        let direct_master_key = prov.direct_master_key.clone();
        let direct_device_recipients = prov.direct_device_recipients.clone();
        let direct_device_identity = prov.direct_device_identity.clone();
        let target = prov.target.clone();

        let fetch_result = std::thread::spawn(move || {
            if let Some(operator) = direct_operator {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                return runtime.block_on(fetch_direct_to_file(
                    operator,
                    remote_prefix,
                    device_id,
                    direct_master_key,
                    direct_device_recipients,
                    direct_device_identity,
                    remote,
                    requested_version,
                    dest,
                    None,
                ));
            }

            // Fallback for minimal configs. PullExact streams bytes back to a
            // client-owned staging file; the extension never sends its local
            // destination to tcfsd.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(fetch_exact_from_daemon(
                &target,
                remote,
                requested_version,
                dest,
                None,
            ))
        })
        .join()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("fetch thread panicked")));

        match fetch_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::error!("fetch failed: {message}");
                prov.set_last_error(message);
                error_code_for_fetch_error(&e)
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Fetch a file via the daemon's exact-path Pull RPC with progress reporting.
///
/// Identical to `tcfs_provider_fetch` but invokes `callback` on each
/// `PullProgress` message so the caller (Swift/Finder) can drive a
/// progress bar.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` and `dest_path` must be valid null-terminated UTF-8 C strings.
/// - `callback_context` must remain valid until this function returns.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch_with_progress(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    callback: crate::TcfsProgressCallback,
    callback_context: *const std::ffi::c_void,
) -> TcfsError {
    unsafe {
        tcfs_provider_fetch_versioned_with_progress_impl(
            provider,
            item_id,
            dest_path,
            ptr::null(),
            callback,
            callback_context,
        )
    }
}

/// Progress-reporting conditional fetch; see [`tcfs_provider_fetch_versioned`].
///
/// # Safety
///
/// String pointers must be valid null-terminated UTF-8. `requested_version`
/// may be null. `callback_context` must remain valid until this call returns.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_fetch_versioned_with_progress(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
    callback: crate::TcfsProgressCallback,
    callback_context: *const std::ffi::c_void,
) -> TcfsError {
    unsafe {
        tcfs_provider_fetch_versioned_with_progress_impl(
            provider,
            item_id,
            dest_path,
            requested_version,
            callback,
            callback_context,
        )
    }
}

unsafe fn tcfs_provider_fetch_versioned_with_progress_impl(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
    dest_path: *const c_char,
    requested_version: *const c_char,
    callback: crate::TcfsProgressCallback,
    callback_context: *const std::ffi::c_void,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    // Store as usize so the closure is Send-safe (same pattern as direct.rs).
    let ctx = callback_context as usize;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        let c_requested_version = if requested_version.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(requested_version) })
        };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let requested_version = match c_requested_version.map(CStr::to_str).transpose() {
            Ok(version) => version
                .filter(|version| !version.is_empty())
                .map(str::to_string),
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let remote = item_str.to_string();
        let dest = PathBuf::from(dest_str);
        let direct_operator = prov.direct_operator.clone();
        let remote_prefix = prov.remote_prefix.clone();
        let device_id = prov.device_id.clone();
        let direct_master_key = prov.direct_master_key.clone();
        let direct_device_recipients = prov.direct_device_recipients.clone();
        let direct_device_identity = prov.direct_device_identity.clone();
        let target = prov.target.clone();

        let fetch_result = std::thread::spawn(move || {
            if let Some(cb) = callback {
                unsafe { cb(0, 0, ctx as *const std::ffi::c_void) };
            }

            if let Some(operator) = direct_operator {
                let progress: Option<tcfs_sync::engine::ProgressFn> = callback.map(|cb| {
                    Box::new(move |done: u64, total: u64, _message: &str| unsafe {
                        cb(done, total, ctx as *const std::ffi::c_void)
                    }) as tcfs_sync::engine::ProgressFn
                });
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                return runtime.block_on(fetch_direct_to_file(
                    operator,
                    remote_prefix,
                    device_id,
                    direct_master_key,
                    direct_device_recipients,
                    direct_device_identity,
                    remote,
                    requested_version,
                    dest,
                    progress,
                ));
            }

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let progress: Option<tcfs_sync::engine::ProgressFn> = callback.map(|cb| {
                Box::new(move |done: u64, total: u64, _message: &str| unsafe {
                    cb(done, total, ctx as *const std::ffi::c_void)
                }) as tcfs_sync::engine::ProgressFn
            });
            runtime.block_on(fetch_exact_from_daemon(
                &target,
                remote,
                requested_version,
                dest,
                progress,
            ))
        })
        .join()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("fetch thread panicked")));

        match fetch_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::error!("fetch_with_progress failed: {message}");
                prov.set_last_error(message);
                error_code_for_fetch_error(&e)
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Upload a local file via the daemon's Push RPC (streaming).
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `local_path` and `remote_rel` must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_upload(
    provider: *mut TcfsProvider,
    local_path: *const c_char,
    remote_rel: *const c_char,
) -> TcfsError {
    if provider.is_null() || local_path.is_null() || remote_rel.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }
    if unsafe { CStr::from_ptr(local_path) }.to_str().is_err()
        || unsafe { CStr::from_ptr(remote_rel) }.to_str().is_err()
    {
        return TcfsError::TcfsErrorInvalidArg;
    }

    unsafe { &*provider }.reject_file_provider_mutation()
}

#[cfg(test)]
#[allow(dead_code)]
unsafe fn tcfs_provider_upload_for_test(
    provider: *mut TcfsProvider,
    local_path: *const c_char,
    remote_rel: *const c_char,
) -> TcfsError {
    if provider.is_null() || local_path.is_null() || remote_rel.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_local = unsafe { CStr::from_ptr(local_path) };
        let c_remote = unsafe { CStr::from_ptr(remote_rel) };

        let local_str = match c_local.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let remote_str = match c_remote.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let upload_result = prov.runtime.block_on(async {
            let data = tokio::fs::read(local_str)
                .await
                .map_err(|e| tonic::Status::internal(format!("read local file: {e}")))?;

            // Pass only the relative path — the daemon's Push RPC applies the
            // remote_prefix when constructing the S3 index key. Prepending it
            // here caused double-prefixing: data/index/data/{file}.
            let remote_path = remote_str.trim_start_matches('/').to_string();

            // Send the file as a single PushChunk (daemon handles chunking internally)
            let chunk = tcfs_core::proto::PushChunk {
                path: remote_path,
                data: data.clone(),
                offset: 0,
                last: true,
            };

            let stream = tokio_stream::once(chunk);
            let mut resp_stream = prov
                .client
                .push(tonic::Request::new(stream))
                .await?
                .into_inner();

            // Drain progress stream
            let mut completed = false;
            while let Some(progress) = resp_stream.message().await? {
                if !progress.error.is_empty() {
                    return Err(tonic::Status::internal(progress.error));
                }
                if progress.done {
                    completed = true;
                    break;
                }
            }
            if !completed {
                return Err(tonic::Status::internal(
                    "Push progress stream ended before terminal completion",
                ));
            }

            Ok::<(), tonic::Status>(())
        });

        match upload_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                tracing::error!("upload failed: {}, attempting reconnect", e);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Delete a file from remote storage.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_delete(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }
    if unsafe { CStr::from_ptr(item_id) }.to_str().is_err() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    unsafe { &*provider }.reject_file_provider_mutation()
}

#[cfg(test)]
#[allow(dead_code)]
unsafe fn tcfs_provider_delete_for_test(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        prov.clear_last_error();

        let delete_result = prov.runtime.block_on(async {
            let operator = prov.direct_operator.clone().ok_or_else(|| {
                anyhow::anyhow!("remote delete requires direct storage credentials")
            })?;
            delete_remote_entry(&operator, &prov.remote_prefix, item_str).await?;

            // Best-effort local eviction after the authoritative remote delete.
            let _ = prov
                .client
                .unsync(tonic::Request::new(tcfs_core::proto::UnsyncRequest {
                    path: item_str.to_string(),
                    force: true,
                }))
                .await;

            Ok::<(), anyhow::Error>(())
        });

        match delete_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::error!("delete failed: {message}");
                prov.set_last_error(message);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Evict local materialization without deleting remote storage.
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `item_id` must be a valid null-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_unsync(
    provider: *mut TcfsProvider,
    item_id: *const c_char,
) -> TcfsError {
    if provider.is_null() || item_id.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        prov.clear_last_error();

        let unsync_result = prov.runtime.block_on(async {
            let resp = prov
                .client
                .unsync(tonic::Request::new(tcfs_core::proto::UnsyncRequest {
                    path: item_str.to_string(),
                    force: true,
                }))
                .await?
                .into_inner();

            if !resp.success && !resp.error.is_empty() {
                return Err(tonic::Status::internal(resp.error));
            }

            Ok::<(), tonic::Status>(())
        });

        match unsync_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                let message = format!("{e:#}");
                tracing::error!("unsync failed: {message}");
                prov.set_last_error(message);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Create a directory via the daemon's Push RPC (zero-byte marker).
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `parent_path` and `dir_name` must be valid null-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_create_dir(
    provider: *mut TcfsProvider,
    parent_path: *const c_char,
    dir_name: *const c_char,
) -> TcfsError {
    if provider.is_null() || parent_path.is_null() || dir_name.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }
    if unsafe { CStr::from_ptr(parent_path) }.to_str().is_err()
        || unsafe { CStr::from_ptr(dir_name) }.to_str().is_err()
    {
        return TcfsError::TcfsErrorInvalidArg;
    }

    unsafe { &*provider }.reject_file_provider_mutation()
}

#[cfg(test)]
#[allow(dead_code)]
unsafe fn tcfs_provider_create_dir_for_test(
    provider: *mut TcfsProvider,
    parent_path: *const c_char,
    dir_name: *const c_char,
) -> TcfsError {
    if provider.is_null() || parent_path.is_null() || dir_name.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_parent = unsafe { CStr::from_ptr(parent_path) };
        let c_name = unsafe { CStr::from_ptr(dir_name) };

        let parent_str = match c_parent.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let name_str = match c_name.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        // Relative path only — daemon applies the remote_prefix.
        let dir_path = if parent_str.is_empty() {
            format!("{}/", name_str.trim_matches('/'))
        } else {
            format!(
                "{}/{}/",
                parent_str.trim_matches('/'),
                name_str.trim_matches('/')
            )
        };

        let create_result = prov.runtime.block_on(async {
            let chunk = tcfs_core::proto::PushChunk {
                path: dir_path,
                data: vec![],
                offset: 0,
                last: true,
            };

            let stream = tokio_stream::once(chunk);
            let mut resp_stream = prov
                .client
                .push(tonic::Request::new(stream))
                .await?
                .into_inner();

            let mut completed = false;
            while let Some(progress) = resp_stream.message().await? {
                if !progress.error.is_empty() {
                    return Err(tonic::Status::internal(progress.error));
                }
                if progress.done {
                    completed = true;
                    break;
                }
            }
            if !completed {
                return Err(tonic::Status::internal(
                    "Push progress stream ended before terminal completion",
                ));
            }

            Ok::<(), tonic::Status>(())
        });

        match create_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                tracing::error!("create_dir failed: {}, attempting reconnect", e);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Start a persistent background Watch RPC stream.
///
/// Spawns a long-lived async task that keeps a Watch stream open to the daemon.
/// When any change event arrives, `callback` is invoked (debounced to at most
/// once per 500ms). The Swift side should call `signalEnumerator()` from the
/// callback to wake fileproviderd.
///
/// The task runs until the provider is freed (runtime dropped).
///
/// # Safety
///
/// - `provider` must be a valid pointer from `tcfs_provider_new`.
/// - `callback_context` must remain valid for the lifetime of the provider.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_start_watch(
    provider: *mut TcfsProvider,
    callback: crate::TcfsWatchCallback,
    callback_context: *const std::ffi::c_void,
) -> TcfsError {
    if provider.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let cb = match callback {
        Some(f) => f,
        None => return TcfsError::TcfsErrorInvalidArg,
    };

    let ctx = callback_context as usize;
    let prov = unsafe { &mut *provider };
    let target = prov.target.clone();

    // Create a SEPARATE gRPC client for the watch stream.
    // Sharing the main client's HTTP/2 channel can cause contention
    // with synchronous block_on() calls from fetchContents.
    let watch_client = match prov.runtime.block_on(connect_once(&target)) {
        Ok(c) => c,
        Err(_) => return TcfsError::TcfsErrorStorage,
    };
    let mut client = watch_client;

    prov.runtime.spawn(async move {
        loop {
            let watch_result = client
                .watch(tonic::Request::new(tcfs_core::proto::WatchRequest {
                    paths: vec![String::new()],
                    since_timestamp: 0,
                }))
                .await;

            match watch_result {
                Ok(resp) => {
                    let mut stream = resp.into_inner();
                    let mut last_signal = std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_millis(500))
                        .unwrap_or_else(std::time::Instant::now);

                    loop {
                        match stream.message().await {
                            Ok(Some(_event)) => {
                                // Debounce: signal at most once per 500ms.
                                if last_signal.elapsed() >= std::time::Duration::from_millis(500) {
                                    unsafe {
                                        cb(ctx as *const std::ffi::c_void);
                                    }
                                    last_signal = std::time::Instant::now();
                                }
                            }
                            Ok(None) => break,
                            Err(error) => {
                                tracing::warn!(
                                    "background watch stream failed authority checks: {error}"
                                );
                                // Wake FileProvider so its next incremental
                                // attempt observes the error without advancing
                                // its anchor and performs an authoritative
                                // re-enumeration.
                                unsafe {
                                    cb(ctx as *const std::ffi::c_void);
                                }
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("background watch failed: {e}, reconnecting in 5s");
                    unsafe {
                        cb(ctx as *const std::ffi::c_void);
                    }
                    // Try to reconnect the client
                    if let Ok(new_client) = connect_once(&target).await {
                        client = new_client;
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    TcfsError::TcfsErrorNone
}

/// Free a provider handle.
///
/// # Safety
///
/// `provider` must be a valid pointer from `tcfs_provider_new`, or null.
#[no_mangle]
pub unsafe extern "C" fn tcfs_provider_free(provider: *mut TcfsProvider) {
    if !provider.is_null() {
        unsafe {
            drop(Box::from_raw(provider));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn incomplete_progress_stream_is_an_error() {
        assert!(require_terminal_progress(true, "PullExact").is_ok());
        let error = require_terminal_progress(false, "PullExact").unwrap_err();
        assert!(error
            .to_string()
            .contains("ended before terminal completion"));
    }

    #[test]
    fn exact_pull_terminal_marker_is_mandatory_even_for_empty_files() {
        let legacy_terminal = tcfs_core::proto::PullProgress {
            done: true,
            ..Default::default()
        };
        let error = validate_exact_pull_terminal(&legacy_terminal, "manifest-v1", 0)
            .expect_err("legacy terminal response must not authorize destination replacement");
        assert!(error.to_string().contains("exact-content protocol marker"));

        let exact_terminal = tcfs_core::proto::PullProgress {
            done: true,
            exact_content: true,
            version_token: "manifest-v1".into(),
            ..Default::default()
        };
        assert!(validate_exact_pull_terminal(&exact_terminal, "manifest-v1", 0).unwrap());
    }

    #[test]
    fn exact_pull_terminal_rejects_a_different_server_version() {
        let terminal = tcfs_core::proto::PullProgress {
            done: true,
            exact_content: true,
            version_token: "manifest-v2".into(),
            ..Default::default()
        };
        let error = validate_exact_pull_terminal(&terminal, "manifest-v1", 0).unwrap_err();
        assert!(crate::is_file_provider_version_mismatch(&error));
    }

    #[test]
    fn grpc_file_provider_mutations_fail_closed_before_io() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let channel = {
            let _guard = runtime.enter();
            Endpoint::from_static("http://127.0.0.1:9").connect_lazy()
        };
        let provider = Box::into_raw(Box::new(TcfsProvider {
            runtime,
            client: TcfsDaemonClient::new(channel),
            remote_prefix: "data".into(),
            device_id: "file-provider-test".into(),
            direct_operator: None,
            direct_master_key: None,
            direct_device_identity: None,
            direct_device_recipients: Vec::new(),
            target: DaemonTarget::Endpoint("http://127.0.0.1:9".into()),
            last_error: Mutex::new(None),
        }));

        unsafe {
            let local = CString::new("/path/that/must/not/be-read").unwrap();
            let remote = CString::new("read-only.txt").unwrap();
            assert_eq!(
                tcfs_provider_upload(provider, local.as_ptr(), remote.as_ptr()),
                TcfsError::TcfsErrorConflict
            );
            assert_eq!(
                tcfs_provider_delete(provider, remote.as_ptr()),
                TcfsError::TcfsErrorConflict
            );
            let parent = CString::new("").unwrap();
            let child = CString::new("read-only-dir").unwrap();
            assert_eq!(
                tcfs_provider_create_dir(provider, parent.as_ptr(), child.as_ptr()),
                TcfsError::TcfsErrorConflict
            );
            assert!((*provider)
                .last_error
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("read-only"));
            tcfs_provider_free(provider);
        }
    }

    #[tokio::test]
    async fn direct_fetch_exact_miss_never_uses_same_filename_elsewhere() {
        let operator = opendal::Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&operator).unwrap();
        operator
            .write(
                "data/index/other/doc.txt",
                tcfs_sync::index_entry::RemoteIndexEntry::new("other-object", 0, 0)
                    .to_legacy_bytes(),
            )
            .await
            .unwrap();

        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();
        let error = fetch_direct_to_file(
            operator,
            "data".into(),
            "file-provider-test".into(),
            None,
            Vec::new(),
            None,
            "missing/doc.txt".into(),
            None,
            destination.clone(),
            None,
        )
        .await
        .expect_err("an exact FileProvider miss must not use basename fallback");

        assert!(
            format!("{error:#}").contains("no exact index entry for FileProvider item"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"keep-local-bytes");
    }

    #[tokio::test]
    async fn direct_fetch_rejects_stale_version_before_manifest_or_destination_io() {
        let operator = opendal::Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&operator).unwrap();
        operator
            .write(
                "data/index/docs/versioned.txt",
                tcfs_sync::index_entry::RemoteIndexEntry::new("current-manifest", 7, 1)
                    .to_legacy_bytes(),
            )
            .await
            .unwrap();

        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();
        let error = fetch_direct_to_file(
            operator,
            "data".into(),
            "file-provider-test".into(),
            None,
            Vec::new(),
            None,
            "docs/versioned.txt".into(),
            Some("stale-manifest".into()),
            destination.clone(),
            None,
        )
        .await
        .expect_err("a stale FileProvider version must fail before manifest I/O");

        assert!(crate::is_file_provider_version_mismatch(&error));
        assert_eq!(
            error_code_for_fetch_error(&error),
            TcfsError::TcfsErrorVersionMismatch
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"keep-local-bytes");
    }

    #[tokio::test]
    async fn direct_fetch_bound_version_does_not_recover_pending_only_entry() {
        let operator = opendal::Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&operator).unwrap();
        let preparing = tcfs_sync::index_entry::VersionedIndexEntry::preparing(
            None,
            tcfs_sync::index_entry::PendingIndexEntry::new(
                "pending-manifest",
                7,
                1,
                "data/staging/manifests/00000000-0000-4000-8000-000000000000-pending-manifest.json",
            ),
        )
        .to_json_bytes()
        .unwrap();
        operator
            .write("data/index/docs/pending.txt", preparing.clone())
            .await
            .unwrap();
        operator
            .write(
                "data/manifests/pending-manifest",
                b"must not be parsed by a bound pending-only read".to_vec(),
            )
            .await
            .unwrap();

        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();
        let error = fetch_direct_to_file(
            operator.clone(),
            "data".into(),
            "file-provider-test".into(),
            None,
            Vec::new(),
            None,
            "docs/pending.txt".into(),
            Some("pending-manifest".into()),
            destination.clone(),
            None,
        )
        .await
        .expect_err("a token that was never visible must not drive pending recovery");

        assert_eq!(
            error_code_for_fetch_error(&error),
            TcfsError::TcfsErrorNotFound
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"keep-local-bytes");
        assert_eq!(
            operator
                .read("data/index/docs/pending.txt")
                .await
                .unwrap()
                .to_vec(),
            preparing
        );
    }

    #[test]
    fn tonic_not_found_maps_to_file_provider_not_found() {
        let error = anyhow::Error::new(tonic::Status::not_found("missing exact item"));
        assert_eq!(
            error_code_for_fetch_error(&error),
            TcfsError::TcfsErrorNotFound
        );
    }

    #[test]
    fn daemon_exact_version_status_maps_only_the_dedicated_prefix() {
        let mismatch = anyhow::Error::new(tonic::Status::failed_precondition(format!(
            "{FILE_PROVIDER_VERSION_MISMATCH_PREFIX} requested old, current new"
        )));
        assert_eq!(
            error_code_for_fetch_error(&mismatch),
            TcfsError::TcfsErrorVersionMismatch
        );

        let unrelated = anyhow::Error::new(tonic::Status::failed_precondition("auth posture"));
        assert_eq!(
            error_code_for_fetch_error(&unrelated),
            TcfsError::TcfsErrorStorage
        );
    }

    #[test]
    fn incremental_watch_journal_failure_expires_the_file_provider_anchor() {
        let expired = tonic::Status::failed_precondition(format!(
            "{INCREMENTAL_WATCH_UNAVAILABLE_PREFIX} full refresh required"
        ));
        assert!(daemon_reported_sync_anchor_expired(&expired));
        assert!(!daemon_reported_sync_anchor_expired(
            &tonic::Status::failed_precondition("unrelated policy failure")
        ));
    }

    #[test]
    fn target_prefers_daemon_endpoint() {
        let config = serde_json::json!({
            "daemon_endpoint": "127.0.0.1:19101",
            "daemon_socket": "/tmp/ignored.sock"
        });

        assert_eq!(
            target_from_config(&config),
            DaemonTarget::Endpoint("http://127.0.0.1:19101".to_string())
        );
    }

    #[test]
    fn target_preserves_endpoint_scheme() {
        let config = serde_json::json!({
            "daemon_endpoint": "http://127.0.0.1:19101"
        });

        assert_eq!(
            target_from_config(&config),
            DaemonTarget::Endpoint("http://127.0.0.1:19101".to_string())
        );
    }

    #[test]
    fn target_uses_daemon_socket_without_endpoint() {
        let config = serde_json::json!({
            "daemon_socket": "/tmp/tcfsd.sock"
        });

        assert_eq!(
            target_from_config(&config),
            DaemonTarget::Unix(PathBuf::from("/tmp/tcfsd.sock"))
        );
    }

    #[test]
    fn direct_operator_requires_storage_credentials() {
        let config = serde_json::json!({
            "s3_endpoint": "https://example.invalid",
            "s3_bucket": "tcfs"
        });

        assert!(build_direct_operator(&config).is_none());
    }

    #[test]
    fn master_key_reads_base64_config() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let config = serde_json::json!({
            "master_key_base64": encoded
        });

        assert!(derive_master_key_from_config(&config).unwrap().is_some());
    }

    #[test]
    fn master_key_rejects_wrong_length_base64_config() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 31]);
        let config = serde_json::json!({
            "master_key_base64": encoded
        });

        let error = derive_master_key_from_config(&config).unwrap_err();
        assert!(format!("{error:#}").contains("master key must be 32 bytes"));
    }

    #[test]
    fn master_key_rejects_malformed_base64_config() {
        let config = serde_json::json!({
            "master_key_base64": "not base64!"
        });

        let error = derive_master_key_from_config(&config).unwrap_err();
        assert!(format!("{error:#}").contains("decoding master_key_base64"));
    }

    #[test]
    fn master_key_rejects_missing_configured_file() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-master-key");
        let config = serde_json::json!({
            "master_key_file": missing
        });

        let error = derive_master_key_from_config(&config).unwrap_err();
        assert!(format!("{error:#}").contains("reading configured master_key_file"));
    }

    #[test]
    fn master_key_rejects_required_encryption_without_key_material() {
        let config = serde_json::json!({
            "encryption_required": true
        });

        let error = derive_master_key_from_config(&config).unwrap_err();
        assert!(format!("{error:#}")
            .contains("encryption is required but no master key or passphrase is configured"));
    }

    #[test]
    fn master_key_allows_unconfigured_plaintext_posture() {
        assert!(derive_master_key_from_config(&serde_json::json!({}))
            .unwrap()
            .is_none());
    }
}
