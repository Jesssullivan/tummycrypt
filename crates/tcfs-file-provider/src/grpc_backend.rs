//! gRPC backend for the FileProvider FFI.
//!
//! Delegates all operations to the tcfsd daemon via Unix domain socket gRPC.
//! This enables full fleet sync, NATS events, and conflict resolution —
//! the daemon handles E2EE, chunking, and storage internally.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::ptr;

use tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::{to_c_string, TcfsError, TcfsFileItem};

/// Opaque provider handle wrapping a tokio runtime + gRPC client.
///
/// Created via `tcfs_provider_new`, freed via `tcfs_provider_free`.
pub struct TcfsProvider {
    runtime: tokio::runtime::Runtime,
    client: TcfsDaemonClient<Channel>,
    /// Remote prefix for path construction
    remote_prefix: String,
    /// Socket path for lazy reconnection
    socket_path: String,
}

/// Connect to the daemon over a Unix domain socket with retry.
///
/// Retries up to `max_retries` times with exponential backoff (200ms base).
/// This handles the case where the daemon hasn't started yet when the
/// FileProvider extension is loaded by fileproviderd.
async fn connect_with_retry(
    socket_path: &str,
    max_retries: u32,
) -> Result<TcfsDaemonClient<Channel>, anyhow::Error> {
    let mut last_err = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = std::time::Duration::from_millis(200 * 2u64.pow(attempt - 1));
            tokio::time::sleep(backoff).await;
        }

        match connect_once(socket_path).await {
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

/// Single connection attempt to the daemon over a Unix domain socket.
async fn connect_once(socket_path: &str) -> Result<TcfsDaemonClient<Channel>, anyhow::Error> {
    let path = PathBuf::from(socket_path);

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

/// Create a new provider from a JSON configuration string.
///
/// The JSON should contain:
/// ```json
/// {
///   "daemon_socket": "/path/to/tcfsd.sock",
///   "remote_prefix": "devices/mydevice"
/// }
/// ```
///
/// Falls back to `$TCFS_SOCKET` env var, then
/// `$XDG_STATE_HOME/tcfsd/tcfsd.sock` if `daemon_socket` is not set.
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

        let socket_path = config["daemon_socket"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| std::env::var("TCFS_SOCKET").ok())
            .unwrap_or_else(|| {
                let state_home = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                    format!("{home}/.local/state")
                });
                format!("{state_home}/tcfsd/tcfsd.sock")
            });

        let prefix = config["remote_prefix"]
            .as_str()
            .unwrap_or("default")
            .to_string();

        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(_) => return ptr::null_mut(),
        };

        let client = match runtime.block_on(connect_with_retry(&socket_path, 4)) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to connect to tcfsd at {}: {}", socket_path, e);
                return ptr::null_mut();
            }
        };

        Box::into_raw(Box::new(TcfsProvider {
            runtime,
            client,
            remote_prefix: prefix,
            socket_path,
        }))
    }));

    result.unwrap_or(ptr::null_mut())
}

impl TcfsProvider {
    /// Attempt to reconnect if the daemon connection was lost.
    fn try_reconnect(&mut self) {
        match self.runtime.block_on(connect_once(&self.socket_path)) {
            Ok(new_client) => {
                tracing::info!("reconnected to tcfsd at {}", self.socket_path);
                self.client = new_client;
            }
            Err(e) => {
                tracing::warn!("reconnect to tcfsd failed: {e}");
            }
        }
    }
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

/// Fetch (hydrate) a file via the daemon's Hydrate RPC.
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

        let item_str = match c_item.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };
        let dest_str = match c_dest.to_str() {
            Ok(s) => s,
            Err(_) => return TcfsError::TcfsErrorInvalidArg,
        };

        let fetch_result = prov.runtime.block_on(async {
            // Use Hydrate RPC — daemon handles download, decryption, assembly
            let mut stream = prov
                .client
                .hydrate(tonic::Request::new(tcfs_core::proto::HydrateRequest {
                    stub_path: item_str.to_string(),
                    partial_ok: false,
                }))
                .await?
                .into_inner();

            let mut local_path = String::new();
            while let Some(progress) = stream.message().await? {
                if !progress.error.is_empty() {
                    return Err(tonic::Status::internal(progress.error));
                }
                if progress.done {
                    local_path = progress.local_path;
                    break;
                }
            }

            if local_path.is_empty() {
                return Err(tonic::Status::internal(
                    "hydrate completed without local_path",
                ));
            }

            // Copy the daemon's hydrated file to the FileProvider's destination
            tokio::fs::copy(&local_path, dest_str)
                .await
                .map_err(|e| tonic::Status::internal(format!("copy to dest: {e}")))?;

            Ok::<(), tonic::Status>(())
        });

        match fetch_result {
            Ok(()) => TcfsError::TcfsErrorNone,
            Err(e) => {
                tracing::error!("fetch failed: {}, attempting reconnect", e);
                prov.try_reconnect();
                TcfsError::TcfsErrorStorage
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

            let remote_path = format!(
                "{}/{}",
                prov.remote_prefix.trim_end_matches('/'),
                remote_str.trim_start_matches('/')
            );

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

        let dir_path = format!(
            "{}/{}{}/",
            prov.remote_prefix.trim_end_matches('/'),
            if parent_str.is_empty() {
                String::new()
            } else {
                format!("{}/", parent_str.trim_matches('/'))
            },
            name_str.trim_matches('/')
        );

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
