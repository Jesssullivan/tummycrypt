//! MCP server implementation with tool definitions

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};

use tcfs_core::proto::{
    tcfs_daemon_client::TcfsDaemonClient, Empty, PullRequest, ResolveConflictRequest,
    StatusRequest, SyncStatusRequest,
};
use tonic::transport::Channel;

// ── Input schemas ────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SyncStatusInput {
    #[schemars(description = "File or directory path to check sync state for")]
    pub path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PullInput {
    #[schemars(description = "Remote path (S3 key) to download")]
    pub remote_path: String,
    #[schemars(description = "Local filesystem path to save the downloaded file")]
    pub local_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PushInput {
    #[schemars(description = "Local file path to upload to remote storage")]
    pub local_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ResolveConflictInput {
    #[schemars(description = "Relative path of the conflicting file")]
    pub rel_path: String,
    #[schemars(description = "Resolution: 'keep_local', 'keep_remote', 'keep_both', or 'defer'")]
    pub resolution: String,
}

// ── MCP Server ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TcfsMcp {
    socket_path: PathBuf,
    config_path: Option<PathBuf>,
    client: Arc<Mutex<Option<TcfsDaemonClient<Channel>>>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl TcfsMcp {
    pub fn new(socket_path: PathBuf, config_path: Option<PathBuf>) -> Self {
        Self {
            socket_path,
            config_path,
            client: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    /// Connect to the daemon, reusing existing connection if available
    async fn connect(&self) -> Result<TcfsDaemonClient<Channel>, String> {
        let mut guard = self.client.lock().await;
        if let Some(ref client) = *guard {
            return Ok(client.clone());
        }

        let path = self.socket_path.clone();
        let channel = tonic::transport::Endpoint::from_static("http://[::]:0")
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let path = path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(&path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .map_err(|e| {
                format!(
                    "failed to connect to daemon at {}: {e}",
                    self.socket_path.display()
                )
            })?;

        let client = TcfsDaemonClient::new(channel);
        *guard = Some(client.clone());
        Ok(client)
    }

    // ── Tools ────────────────────────────────────────────────────────────

    #[tool(
        description = "Get tcfs daemon status: version, storage connectivity, uptime, active mounts"
    )]
    async fn daemon_status(&self) -> String {
        match self.connect().await {
            Ok(mut client) => match client.status(StatusRequest {}).await {
                Ok(resp) => {
                    let s = resp.into_inner();
                    serde_json::json!({
                        "version": s.version,
                        "storage_endpoint": s.storage_endpoint,
                        "storage_ok": s.storage_ok,
                        "nats_ok": s.nats_ok,
                        "active_mounts": s.active_mounts,
                        "uptime_secs": s.uptime_secs,
                        "device_id": s.device_id,
                        "device_name": s.device_name,
                        "conflict_mode": s.conflict_mode,
                    })
                    .to_string()
                }
                Err(e) => format!("{{\"error\": \"status RPC failed: {e}\"}}"),
            },
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Get credential status: whether S3/storage credentials are loaded")]
    async fn credential_status(&self) -> String {
        match self.connect().await {
            Ok(mut client) => match client.credential_status(Empty {}).await {
                Ok(resp) => {
                    let c = resp.into_inner();
                    serde_json::json!({
                        "loaded": c.loaded,
                        "source": c.source,
                        "loaded_at": c.loaded_at,
                        "needs_reload": c.needs_reload,
                    })
                    .to_string()
                }
                Err(e) => format!("{{\"error\": \"credential_status RPC failed: {e}\"}}"),
            },
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Show tcfs configuration (daemon, storage, sync, fuse, crypto sections)")]
    async fn config_show(&self) -> String {
        let path = self
            .config_path
            .clone()
            .or_else(|| std::env::var("TCFS_CONFIG").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/etc/tcfs/config.toml"));

        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str::<tcfs_core::config::TcfsConfig>(&contents) {
                Ok(config) => match serde_json::to_string_pretty(&config) {
                    Ok(json) => json,
                    Err(e) => format!("{{\"error\": \"serialize config: {e}\"}}"),
                },
                Err(e) => {
                    format!("{{\"error\": \"parse config at {}: {e}\"}}", path.display())
                }
            },
            Err(e) => format!("{{\"error\": \"read config at {}: {e}\"}}", path.display()),
        }
    }

    #[tool(description = "Check sync status of a file: synced, pending, or unknown")]
    async fn sync_status(&self, Parameters(input): Parameters<SyncStatusInput>) -> String {
        match self.connect().await {
            Ok(mut client) => {
                match client
                    .sync_status(SyncStatusRequest { path: input.path })
                    .await
                {
                    Ok(resp) => {
                        let s = resp.into_inner();
                        serde_json::json!({
                            "path": s.path,
                            "state": s.state,
                            "blake3": s.blake3,
                            "size": s.size,
                            "last_synced": s.last_synced,
                        })
                        .to_string()
                    }
                    Err(e) => format!("{{\"error\": \"sync_status RPC failed: {e}\"}}"),
                }
            }
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Pull (download) a file from remote storage to a local path")]
    async fn pull(&self, Parameters(input): Parameters<PullInput>) -> String {
        match self.connect().await {
            Ok(mut client) => {
                match client
                    .pull(PullRequest {
                        remote_path: input.remote_path,
                        local_path: input.local_path,
                    })
                    .await
                {
                    Ok(resp) => {
                        use tokio_stream::StreamExt;
                        let mut stream = resp.into_inner();
                        let mut last_progress = None;
                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(p) => last_progress = Some(p),
                                Err(e) => {
                                    return format!("{{\"error\": \"pull stream error: {e}\"}}")
                                }
                            }
                        }
                        match last_progress {
                            Some(p) => serde_json::json!({
                                "bytes_received": p.bytes_received,
                                "total_bytes": p.total_bytes,
                                "done": p.done,
                                "error": if p.error.is_empty() { None } else { Some(&p.error) },
                            })
                            .to_string(),
                            None => "{\"error\": \"no progress received\"}".to_string(),
                        }
                    }
                    Err(e) => format!("{{\"error\": \"pull RPC failed: {e}\"}}"),
                }
            }
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(
        description = "Resolve a sync conflict by choosing a resolution strategy. Valid resolutions: keep_local, keep_remote, keep_both, defer"
    )]
    async fn resolve_conflict(
        &self,
        Parameters(input): Parameters<ResolveConflictInput>,
    ) -> String {
        match self.connect().await {
            Ok(mut client) => {
                match client
                    .resolve_conflict(ResolveConflictRequest {
                        path: input.rel_path.clone(),
                        resolution: input.resolution.clone(),
                    })
                    .await
                {
                    Ok(resp) => {
                        let r = resp.into_inner();
                        serde_json::json!({
                            "success": r.success,
                            "resolved_path": r.resolved_path,
                            "error": if r.error.is_empty() { None } else { Some(&r.error) },
                        })
                        .to_string()
                    }
                    Err(e) => format!("{{\"error\": \"resolve_conflict RPC failed: {e}\"}}"),
                }
            }
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Show all enrolled devices in the fleet and their sync status")]
    async fn device_status(&self) -> String {
        let registry_path = tcfs_secrets::device::default_registry_path();
        match tcfs_secrets::device::DeviceRegistry::load(&registry_path) {
            Ok(registry) => {
                let devices: Vec<serde_json::Value> = registry
                    .devices
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "name": d.name,
                            "device_id": d.device_id,
                            "public_key": d.public_key,
                            "enrolled_at": d.enrolled_at,
                            "revoked": d.revoked,
                            "last_nats_seq": d.last_nats_seq,
                            "description": d.description,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "devices": devices,
                    "total": registry.devices.len(),
                    "active": registry.active_devices().count(),
                })
                .to_string()
            }
            Err(e) => format!("{{\"error\": \"loading device registry: {e}\"}}"),
        }
    }

    #[tool(description = "Push (upload) a local file to remote storage")]
    async fn push(&self, Parameters(input): Parameters<PushInput>) -> String {
        let data = match std::fs::read(&input.local_path) {
            Ok(d) => d,
            Err(e) => return format!("{{\"error\": \"read file: {e}\"}}"),
        };

        let chunk = tcfs_core::proto::PushChunk {
            path: input.local_path.clone(),
            data,
            offset: 0,
            last: true,
        };

        match self.connect().await {
            Ok(mut client) => match client.push(tokio_stream::once(chunk)).await {
                Ok(resp) => {
                    use tokio_stream::StreamExt;
                    let mut stream = resp.into_inner();
                    let mut last_progress = None;
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(p) => last_progress = Some(p),
                            Err(e) => return format!("{{\"error\": \"push stream error: {e}\"}}"),
                        }
                    }
                    match last_progress {
                        Some(p) => serde_json::json!({
                            "bytes_sent": p.bytes_sent,
                            "total_bytes": p.total_bytes,
                            "chunk_hash": p.chunk_hash,
                            "done": p.done,
                            "error": if p.error.is_empty() { None } else { Some(&p.error) },
                        })
                        .to_string(),
                        None => "{\"error\": \"no progress received\"}".to_string(),
                    }
                }
                Err(e) => format!("{{\"error\": \"push RPC failed: {e}\"}}"),
            },
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }
}

#[tool_handler]
impl ServerHandler for TcfsMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "tcfs daemon control — query status, push/pull files, check sync state. \
                 Connects to tcfsd over Unix domain socket gRPC."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
