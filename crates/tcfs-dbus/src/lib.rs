//! D-Bus interface for TCFS file sync status.
//!
//! Exposes the `io.tinyland.tcfs` D-Bus service so desktop integrations
//! (Nautilus, Dolphin, etc.) can query per-file sync state and trigger actions.

use std::sync::Arc;
use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{interface, Connection};

// ---------------------------------------------------------------------------
// Status backend trait (stub for now, will be wired to gRPC later)
// ---------------------------------------------------------------------------

/// Sync status for a single path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncStatus {
    Synced,
    Syncing,
    Placeholder,
    Conflict,
    Error,
    Unknown,
}

impl std::fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Synced => "synced",
            Self::Syncing => "syncing",
            Self::Placeholder => "placeholder",
            Self::Conflict => "conflict",
            Self::Error => "error",
            Self::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// Trait for querying and controlling sync status.
///
/// The default implementation returns `Unknown` for every path and no-ops for
/// sync/unsync. Replace with a real gRPC client in production.
pub trait StatusBackend: Send + Sync + 'static {
    fn get_status(&self, path: &str) -> impl std::future::Future<Output = SyncStatus> + Send;
    fn sync(&self, path: &str) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
    fn unsync(&self, path: &str) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}

/// Stub backend that always returns `Unknown`.
#[derive(Debug, Clone, Default)]
pub struct StubBackend;

impl StatusBackend for StubBackend {
    async fn get_status(&self, _path: &str) -> SyncStatus {
        SyncStatus::Unknown
    }
    async fn sync(&self, _path: &str) -> anyhow::Result<()> {
        tracing::warn!("sync requested but no backend is wired");
        Ok(())
    }
    async fn unsync(&self, _path: &str) -> anyhow::Result<()> {
        tracing::warn!("unsync requested but no backend is wired");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// gRPC backend (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "grpc")]
pub mod grpc_backend {
    use super::{StatusBackend, SyncStatus};
    use std::path::{Path, PathBuf};
    use tonic::transport::{Channel, Endpoint, Uri};
    use tower::service_fn;

    /// Backend that queries the tcfsd daemon over gRPC Unix socket.
    pub struct GrpcBackend {
        socket_path: PathBuf,
    }

    impl GrpcBackend {
        pub fn new(socket_path: &Path) -> Self {
            Self {
                socket_path: socket_path.to_path_buf(),
            }
        }

        async fn connect(
            &self,
        ) -> Result<tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient<Channel>, anyhow::Error>
        {
            let path = self.socket_path.clone();
            let channel = Endpoint::from_static("http://[::]:0")
                .connect_with_connector(service_fn(move |_: Uri| {
                    let path = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(&path).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                    }
                }))
                .await?;
            Ok(tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient::new(
                channel,
            ))
        }

        pub fn map_status(state: &str) -> SyncStatus {
            match state {
                "Synced" | "synced" => SyncStatus::Synced,
                "Active" | "active" | "Syncing" | "syncing" => SyncStatus::Syncing,
                "NotSynced" | "not_synced" => SyncStatus::Placeholder,
                "Conflict" | "conflict" => SyncStatus::Conflict,
                "Locked" | "locked" | "Error" | "error" => SyncStatus::Error,
                _ => SyncStatus::Unknown,
            }
        }
    }

    impl StatusBackend for GrpcBackend {
        async fn get_status(&self, path: &str) -> SyncStatus {
            match self.connect().await {
                Ok(mut client) => {
                    let req = tcfs_core::proto::SyncStatusRequest {
                        path: path.to_string(),
                    };
                    match client.sync_status(req).await {
                        Ok(resp) => Self::map_status(&resp.into_inner().state),
                        Err(e) => {
                            tracing::debug!(error = %e, "gRPC sync_status failed");
                            SyncStatus::Unknown
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "gRPC connect failed");
                    SyncStatus::Unknown
                }
            }
        }

        async fn sync(&self, path: &str) -> anyhow::Result<()> {
            let mut client = self.connect().await?;
            let req = tcfs_core::proto::PullRequest {
                remote_path: path.to_string(),
                local_path: path.to_string(),
            };
            client.pull(req).await?;
            Ok(())
        }

        async fn unsync(&self, path: &str) -> anyhow::Result<()> {
            let mut client = self.connect().await?;
            let req = tcfs_core::proto::UnsyncRequest {
                path: path.to_string(),
                force: false,
            };
            client.unsync(req).await?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// D-Bus interface object
// ---------------------------------------------------------------------------

/// D-Bus object published at `/io/tinyland/tcfs`.
pub struct TcfsFileStatus<B: StatusBackend = StubBackend> {
    backend: Arc<Mutex<B>>,
}

impl<B: StatusBackend> TcfsFileStatus<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
        }
    }
}

impl Default for TcfsFileStatus<StubBackend> {
    fn default() -> Self {
        Self::new(StubBackend)
    }
}

#[interface(name = "io.tinyland.tcfs")]
impl<B: StatusBackend> TcfsFileStatus<B> {
    /// Return the sync status string for `path`.
    async fn get_status(&self, path: &str) -> String {
        let backend = self.backend.lock().await;
        backend.get_status(path).await.to_string()
    }

    /// Request that `path` be synced locally.
    async fn sync(&self, path: &str) -> zbus::fdo::Result<()> {
        let backend = self.backend.lock().await;
        backend
            .sync(path)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("sync failed: {e}")))
    }

    /// Request that the local copy of `path` be removed (dehydrated).
    async fn unsync(&self, path: &str) -> zbus::fdo::Result<()> {
        let backend = self.backend.lock().await;
        backend
            .unsync(path)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("unsync failed: {e}")))
    }

    /// Signal emitted when a file's sync status changes.
    #[zbus(signal)]
    async fn status_changed(
        signal_emitter: &SignalEmitter<'_>,
        path: &str,
        status: &str,
    ) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------------
// Service entry point
// ---------------------------------------------------------------------------

/// Start the D-Bus service on the session bus.
///
/// Registers the `io.tinyland.tcfs` well-known name and serves requests until
/// the returned [`Connection`] is dropped.
pub async fn serve<B: StatusBackend>(backend: B) -> anyhow::Result<Connection> {
    let iface = TcfsFileStatus::new(backend);

    let conn = Connection::session().await?;

    conn.object_server().at("/io/tinyland/tcfs", iface).await?;

    conn.request_name("io.tinyland.tcfs").await?;

    tracing::info!("tcfs-dbus service registered on session bus");
    Ok(conn)
}

/// Convenience: start with the stub backend.
pub async fn serve_stub() -> anyhow::Result<Connection> {
    serve(StubBackend).await
}

/// Emit a `StatusChanged` signal on the D-Bus session bus.
///
/// Call this from the watcher/scheduler when a file's sync status changes
/// (e.g., conflict detected, sync completed, file evicted).
///
/// Uses raw D-Bus signal emission to avoid generic type constraints on
/// the registered interface.
pub async fn emit_status_changed(conn: &Connection, path: &str, status: &str) {
    if let Err(e) = conn
        .emit_signal(
            None::<zbus::names::BusName>,
            "/io/tinyland/tcfs",
            "io.tinyland.tcfs",
            "StatusChanged",
            &(path, status),
        )
        .await
    {
        tracing::debug!("failed to emit StatusChanged signal: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_display() {
        assert_eq!(SyncStatus::Synced.to_string(), "synced");
        assert_eq!(SyncStatus::Unknown.to_string(), "unknown");
        assert_eq!(SyncStatus::Conflict.to_string(), "conflict");
    }

    #[tokio::test]
    async fn stub_backend_returns_unknown() {
        let backend = StubBackend;
        assert_eq!(backend.get_status("/any/path").await, SyncStatus::Unknown);
    }

    #[cfg(feature = "grpc")]
    #[test]
    fn grpc_backend_status_mapping() {
        use grpc_backend::GrpcBackend;
        assert_eq!(GrpcBackend::map_status("Synced"), SyncStatus::Synced);
        assert_eq!(GrpcBackend::map_status("synced"), SyncStatus::Synced);
        assert_eq!(GrpcBackend::map_status("Active"), SyncStatus::Syncing);
        assert_eq!(
            GrpcBackend::map_status("NotSynced"),
            SyncStatus::Placeholder
        );
        assert_eq!(GrpcBackend::map_status("Conflict"), SyncStatus::Conflict);
        assert_eq!(GrpcBackend::map_status("Locked"), SyncStatus::Error);
        assert_eq!(GrpcBackend::map_status("garbage"), SyncStatus::Unknown);
    }
}
