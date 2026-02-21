//! tonic gRPC server over Unix domain socket

use anyhow::Result;
use std::path::Path;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tracing::info;

use crate::cred_store::SharedCredStore;

use tcfs_core::proto::{
    tcfs_daemon_server::{TcfsDaemon, TcfsDaemonServer},
    *,
};

/// Implementation of the TcfsDaemon gRPC service
pub struct TcfsDaemonImpl {
    cred_store: SharedCredStore,
    storage_ok: bool,
    storage_endpoint: String,
    start_time: std::time::Instant,
}

impl TcfsDaemonImpl {
    pub fn new(cred_store: SharedCredStore, storage_ok: bool, storage_endpoint: String) -> Self {
        Self {
            cred_store,
            storage_ok,
            storage_endpoint,
            start_time: std::time::Instant::now(),
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
            nats_ok: false,   // Phase 2
            active_mounts: 0, // Phase 3
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
                loaded_at: 0, // TODO: track load time
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
        Err(tonic::Status::unimplemented("mount: Phase 3"))
    }

    async fn unmount(
        &self,
        _request: tonic::Request<UnmountRequest>,
    ) -> Result<tonic::Response<UnmountResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("unmount: Phase 3"))
    }

    type PushStream = tokio_stream::Once<Result<PushProgress, tonic::Status>>;
    async fn push(
        &self,
        _request: tonic::Request<tonic::Streaming<PushChunk>>,
    ) -> Result<tonic::Response<Self::PushStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("push: Phase 2"))
    }

    type PullStream = tokio_stream::Once<Result<PullProgress, tonic::Status>>;
    async fn pull(
        &self,
        _request: tonic::Request<PullRequest>,
    ) -> Result<tonic::Response<Self::PullStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("pull: Phase 2"))
    }

    type HydrateStream = tokio_stream::Once<Result<HydrateProgress, tonic::Status>>;
    async fn hydrate(
        &self,
        _request: tonic::Request<HydrateRequest>,
    ) -> Result<tonic::Response<Self::HydrateStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("hydrate: Phase 3"))
    }

    async fn unsync(
        &self,
        _request: tonic::Request<UnsyncRequest>,
    ) -> Result<tonic::Response<UnsyncResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("unsync: Phase 3"))
    }

    async fn sync_status(
        &self,
        _request: tonic::Request<SyncStatusRequest>,
    ) -> Result<tonic::Response<SyncStatusResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("sync_status: Phase 2"))
    }

    type WatchStream = tokio_stream::Once<Result<WatchEvent, tonic::Status>>;
    async fn watch(
        &self,
        _request: tonic::Request<WatchRequest>,
    ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("watch: Phase 2"))
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
