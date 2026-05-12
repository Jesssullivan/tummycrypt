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
use tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::{to_c_string, TcfsChangeEvent, TcfsError, TcfsFileItem};

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

fn master_key_from_config(config: &serde_json::Value) -> Option<tcfs_crypto::MasterKey> {
    if let Some(encoded) = config["master_key_base64"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        return master_key_from_bytes(&decoded).ok();
    }

    if let Some(path) = config["master_key_file"].as_str().filter(|s| !s.is_empty()) {
        let bytes = std::fs::read(path).ok()?;
        return master_key_from_bytes(&bytes).ok();
    }

    None
}

fn error_code_for_fetch_error(error: &anyhow::Error) -> TcfsError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<opendal::Error>()
            .is_some_and(|e| e.kind() == opendal::ErrorKind::NotFound)
    }) {
        TcfsError::TcfsErrorNotFound
    } else {
        TcfsError::TcfsErrorStorage
    }
}

async fn fetch_direct_to_file(
    operator: opendal::Operator,
    remote_prefix: String,
    device_id: String,
    master_key: Option<tcfs_crypto::MasterKey>,
    remote_path: String,
    dest_path: PathBuf,
    progress: Option<tcfs_sync::engine::ProgressFn>,
) -> anyhow::Result<()> {
    let resolved_manifest =
        tcfs_sync::engine::resolve_manifest_path(&operator, &remote_path, &remote_prefix, None)
            .await
            .with_context(|| format!("resolving manifest for: {remote_path}"))?;

    let enc_ctx = master_key
        .as_ref()
        .map(|mk| tcfs_sync::engine::EncryptionContext {
            master_key: mk.clone(),
        });

    tcfs_sync::engine::download_file_with_device(
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
        let direct_operator = build_direct_operator(&config);
        let direct_master_key = master_key_from_config(&config);

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
                    content_hash: to_c_string(&entry.blake3),
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

/// Enumerate changes since a timestamp anchor via the daemon's Watch RPC.
///
/// Returns only items that changed since `since_timestamp`, enabling
/// incremental `enumerateChanges` in FileProvider instead of full re-enumerate.
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

            // Call Watch RPC with since_timestamp to get catch-up events
            let resp = prov
                .client
                .watch(tonic::Request::new(tcfs_core::proto::WatchRequest {
                    paths: vec![rel_path.to_string()],
                    since_timestamp,
                }))
                .await?;

            let mut stream = resp.into_inner();
            let mut events = Vec::new();

            // Collect catch-up events (daemon sends them immediately).
            // Use a short timeout: after the initial burst, stop collecting.
            loop {
                match timeout(Duration::from_millis(500), stream.message()).await {
                    Ok(Ok(Some(watch_event))) => {
                        events.push(TcfsChangeEvent {
                            path: to_c_string(&watch_event.path),
                            filename: to_c_string(&watch_event.filename),
                            event_type: to_c_string(&watch_event.event_type),
                            timestamp: watch_event.timestamp,
                            file_size: watch_event.size,
                            content_hash: to_c_string(&watch_event.blake3),
                            is_directory: watch_event.is_directory,
                        });
                    }
                    Ok(Ok(None)) => break, // Stream ended
                    Ok(Err(e)) => {
                        tracing::warn!("enumerate_changes stream error: {e}");
                        break;
                    }
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
                tracing::error!("enumerate_changes failed: {}, attempting reconnect", e);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
            }
        }
    }));

    result.unwrap_or(TcfsError::TcfsErrorInternal)
}

/// Fetch a file via the daemon's Pull RPC.
///
/// Uses Pull (not Hydrate) because the daemon writes to `local_path` which
/// must be in the App Group container — the sandboxed extension cannot access
/// files on the daemon's filesystem.
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
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let remote = item_str.to_string();
        let dest = PathBuf::from(dest_str);
        let direct_operator = prov.direct_operator.clone();
        let remote_prefix = prov.remote_prefix.clone();
        let device_id = prov.device_id.clone();
        let direct_master_key = prov.direct_master_key.clone();
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
                    remote,
                    dest,
                    None,
                ));
            }

            // Fallback for minimal configs. Production FileProvider configs
            // should include direct storage credentials so the extension writes
            // its own FileProvider temp file instead of asking tcfsd to write
            // into the extension container.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let mut client = connect_once(&target).await?;
                let mut stream = client
                    .pull(tonic::Request::new(tcfs_core::proto::PullRequest {
                        remote_path: remote,
                        local_path: dest.display().to_string(),
                    }))
                    .await?
                    .into_inner();

                while let Some(progress) = stream.message().await? {
                    if !progress.error.is_empty() {
                        anyhow::bail!("{}", progress.error);
                    }
                    if progress.done {
                        break;
                    }
                }
                Ok::<(), anyhow::Error>(())
            })
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

/// Fetch a file via the daemon's Pull RPC with progress reporting.
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
    if provider.is_null() || item_id.is_null() || dest_path.is_null() {
        return TcfsError::TcfsErrorInvalidArg;
    }

    // Store as usize so the closure is Send-safe (same pattern as direct.rs).
    let ctx = callback_context as usize;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let c_dest = unsafe { CStr::from_ptr(dest_path) };
        prov.clear_last_error();

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let remote = item_str.to_string();
        let dest = PathBuf::from(dest_str);
        let direct_operator = prov.direct_operator.clone();
        let remote_prefix = prov.remote_prefix.clone();
        let device_id = prov.device_id.clone();
        let direct_master_key = prov.direct_master_key.clone();
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
                    remote,
                    dest,
                    progress,
                ));
            }

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let mut client = connect_once(&target).await?;
                let mut stream = client
                    .pull(tonic::Request::new(tcfs_core::proto::PullRequest {
                        remote_path: remote,
                        local_path: dest.display().to_string(),
                    }))
                    .await?
                    .into_inner();

                while let Some(progress) = stream.message().await? {
                    if !progress.error.is_empty() {
                        anyhow::bail!("{}", progress.error);
                    }
                    if let Some(cb) = callback {
                        unsafe {
                            cb(
                                progress.bytes_received,
                                progress.total_bytes,
                                ctx as *const std::ffi::c_void,
                            )
                        };
                    }
                    if progress.done {
                        break;
                    }
                }

                Ok::<(), anyhow::Error>(())
            })
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
            while let Some(progress) = resp_stream.message().await? {
                if !progress.error.is_empty() {
                    return Err(tonic::Status::internal(progress.error));
                }
                if progress.done {
                    break;
                }
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

/// Delete a file via the daemon (unsync then delete remote).
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

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prov = unsafe { &mut *provider };
        let c_item = unsafe { CStr::from_ptr(item_id) };
        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let delete_result = prov.runtime.block_on(async {
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

        match delete_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                tracing::error!("delete failed: {}, attempting reconnect", e);
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

            while let Some(progress) = resp_stream.message().await? {
                if progress.done {
                    break;
                }
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
                    let mut last_signal = std::time::Instant::now();

                    while let Ok(Some(_event)) = stream.message().await {
                        // Debounce: signal at most once per 500ms
                        if last_signal.elapsed() > std::time::Duration::from_millis(500) {
                            unsafe {
                                cb(ctx as *const std::ffi::c_void);
                            }
                            last_signal = std::time::Instant::now();
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("background watch failed: {e}, reconnecting in 5s");
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

        assert!(master_key_from_config(&config).is_some());
    }

    #[test]
    fn master_key_rejects_wrong_length_base64_config() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 31]);
        let config = serde_json::json!({
            "master_key_base64": encoded
        });

        assert!(master_key_from_config(&config).is_none());
    }
}
