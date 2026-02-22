//! tonic gRPC server over Unix domain socket

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex as TokioMutex;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tracing::info;

use crate::cred_store::SharedCredStore;

use tcfs_core::config::TcfsConfig;
use tcfs_core::proto::{
    tcfs_daemon_server::{TcfsDaemon, TcfsDaemonServer},
    *,
};

/// Implementation of the TcfsDaemon gRPC service
pub struct TcfsDaemonImpl {
    cred_store: SharedCredStore,
    config: Arc<TcfsConfig>,
    storage_ok: bool,
    storage_endpoint: String,
    start_time: std::time::Instant,
    state_cache: Arc<TokioMutex<tcfs_sync::state::StateCache>>,
    operator: Arc<TokioMutex<Option<opendal::Operator>>>,
}

impl TcfsDaemonImpl {
    pub fn new(
        cred_store: SharedCredStore,
        config: Arc<TcfsConfig>,
        storage_ok: bool,
        storage_endpoint: String,
        state_cache: tcfs_sync::state::StateCache,
        operator: Option<opendal::Operator>,
    ) -> Self {
        Self {
            cred_store,
            config,
            storage_ok,
            storage_endpoint,
            start_time: std::time::Instant::now(),
            state_cache: Arc::new(TokioMutex::new(state_cache)),
            operator: Arc::new(TokioMutex::new(operator)),
        }
    }
}

#[tonic::async_trait]
impl TcfsDaemon for TcfsDaemonImpl {
    async fn status(
        &self,
        _request: tonic::Request<StatusRequest>,
    ) -> Result<tonic::Response<StatusResponse>, tonic::Status> {
        let uptime = self.start_time.elapsed().as_secs() as i64;
        Ok(tonic::Response::new(StatusResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            storage_endpoint: self.storage_endpoint.clone(),
            storage_ok: self.storage_ok,
            nats_ok: false,
            active_mounts: 0,
            uptime_secs: uptime,
        }))
    }

    async fn credential_status(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<CredentialStatusResponse>, tonic::Status> {
        let store = self.cred_store.read().await;
        match store.as_ref() {
            Some(cs) => Ok(tonic::Response::new(CredentialStatusResponse {
                loaded: true,
                source: cs.source.clone(),
                loaded_at: 0,
                needs_reload: false,
            })),
            None => Ok(tonic::Response::new(CredentialStatusResponse {
                loaded: false,
                source: "none".into(),
                loaded_at: 0,
                needs_reload: true,
            })),
        }
    }

    async fn mount(
        &self,
        _request: tonic::Request<MountRequest>,
    ) -> Result<tonic::Response<MountResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "mount: not yet wired (use `tcfs mount` CLI directly)",
        ))
    }

    async fn unmount(
        &self,
        _request: tonic::Request<UnmountRequest>,
    ) -> Result<tonic::Response<UnmountResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "unmount: not yet wired (use `tcfs unmount` CLI directly)",
        ))
    }

    // ── Push: client-streaming upload ─────────────────────────────────────

    type PushStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<PushProgress, tonic::Status>> + Send>>;

    async fn push(
        &self,
        request: tonic::Request<tonic::Streaming<PushChunk>>,
    ) -> Result<tonic::Response<Self::PushStream>, tonic::Status> {
        use tokio_stream::StreamExt;

        let op = self.operator.lock().await;
        let op = op.as_ref().ok_or_else(|| {
            tonic::Status::unavailable("no storage operator — check credentials")
        })?;
        let op = op.clone();

        let state_cache = self.state_cache.clone();
        let prefix = self.config.storage.bucket.clone();

        let mut stream = request.into_inner();

        // Collect the streamed chunks into a file buffer
        let mut path = String::new();
        let mut data = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if path.is_empty() {
                path = chunk.path.clone();
            }
            data.extend_from_slice(&chunk.data);
        }

        if path.is_empty() {
            return Err(tonic::Status::invalid_argument("no path provided in push stream"));
        }

        // Write to a temp file and upload via sync engine
        let tmp_dir = tempfile::tempdir().map_err(|e| {
            tonic::Status::internal(format!("tempdir: {e}"))
        })?;
        let local_path = tmp_dir.path().join(&path);
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                tonic::Status::internal(format!("mkdir: {e}"))
            })?;
        }
        std::fs::write(&local_path, &data).map_err(|e| {
            tonic::Status::internal(format!("write temp: {e}"))
        })?;

        let total_bytes = data.len() as u64;

        let result = {
            let mut cache = state_cache.lock().await;
            tcfs_sync::engine::upload_file(&op, &local_path, &prefix, &mut cache, None).await
        };

        match result {
            Ok(upload) => {
                let progress = PushProgress {
                    bytes_sent: total_bytes,
                    total_bytes,
                    chunk_hash: upload.hash,
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(progress)))))
            }
            Err(e) => {
                let progress = PushProgress {
                    bytes_sent: 0,
                    total_bytes,
                    chunk_hash: String::new(),
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(progress)))))
            }
        }
    }

    // ── Pull: server-streaming download ───────────────────────────────────

    type PullStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<PullProgress, tonic::Status>> + Send>>;

    async fn pull(
        &self,
        request: tonic::Request<PullRequest>,
    ) -> Result<tonic::Response<Self::PullStream>, tonic::Status> {
        let req = request.into_inner();

        let op = self.operator.lock().await;
        let op = op.as_ref().ok_or_else(|| {
            tonic::Status::unavailable("no storage operator — check credentials")
        })?;
        let op = op.clone();

        let prefix = self.config.storage.bucket.clone();
        let local_path = std::path::PathBuf::from(&req.local_path);

        let result =
            tcfs_sync::engine::download_file(&op, &req.remote_path, &local_path, &prefix, None)
                .await;

        match result {
            Ok(dl) => {
                let progress = PullProgress {
                    bytes_received: dl.bytes,
                    total_bytes: dl.bytes,
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(progress)))))
            }
            Err(e) => {
                let progress = PullProgress {
                    bytes_received: 0,
                    total_bytes: 0,
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(progress)))))
            }
        }
    }

    // ── Hydrate ───────────────────────────────────────────────────────────

    type HydrateStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<HydrateProgress, tonic::Status>> + Send>>;

    async fn hydrate(
        &self,
        _request: tonic::Request<HydrateRequest>,
    ) -> Result<tonic::Response<Self::HydrateStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("hydrate: not yet wired"))
    }

    // ── Unsync ────────────────────────────────────────────────────────────

    async fn unsync(
        &self,
        _request: tonic::Request<UnsyncRequest>,
    ) -> Result<tonic::Response<UnsyncResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "unsync: not yet wired (use `tcfs unsync` CLI directly)",
        ))
    }

    // ── Sync Status ───────────────────────────────────────────────────────

    async fn sync_status(
        &self,
        request: tonic::Request<SyncStatusRequest>,
    ) -> Result<tonic::Response<SyncStatusResponse>, tonic::Status> {
        let req = request.into_inner();
        let path = std::path::PathBuf::from(&req.path);

        let cache = self.state_cache.lock().await;

        match cache.get(&path) {
            Some(entry) => Ok(tonic::Response::new(SyncStatusResponse {
                path: req.path,
                state: "synced".into(),
                blake3: entry.blake3.clone(),
                size: entry.size,
                last_synced: entry.last_synced as i64,
            })),
            None => {
                // Check if it needs sync
                let state = match cache.needs_sync(&path) {
                    Ok(None) => "unknown",
                    Ok(Some(_reason)) => "pending",
                    Err(_) => "unknown",
                };
                Ok(tonic::Response::new(SyncStatusResponse {
                    path: req.path,
                    state: state.into(),
                    blake3: String::new(),
                    size: 0,
                    last_synced: 0,
                }))
            }
        }
    }

    // ── Watch ─────────────────────────────────────────────────────────────

    type WatchStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<WatchEvent, tonic::Status>> + Send>>;

    async fn watch(
        &self,
        _request: tonic::Request<WatchRequest>,
    ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("watch: not yet wired"))
    }
}

/// Start the gRPC server on a Unix domain socket
pub async fn serve(socket_path: &Path, impl_: TcfsDaemonImpl) -> Result<()> {
    // Remove stale socket if it exists
    if socket_path.exists() {
        tokio::fs::remove_file(socket_path).await?;
    }

    // Create parent directory if needed
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let listener = UnixListener::bind(socket_path)?;
    let stream = UnixListenerStream::new(listener);

    info!(socket = %socket_path.display(), "gRPC server ready");

    Server::builder()
        .add_service(TcfsDaemonServer::new(impl_))
        .serve_with_incoming(stream)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))
}
