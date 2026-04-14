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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::pin::Pin;
    use std::sync::Arc;

    use rmcp::handler::server::wrapper::Parameters;
    use tcfs_core::proto::tcfs_daemon_server::{TcfsDaemon, TcfsDaemonServer};
    use tcfs_core::proto::*;
    use tempfile::TempDir;
    use tokio::net::UnixListener;
    use tokio::sync::{Mutex as TokioMutex, Notify};
    use tokio::task::JoinHandle;
    use tokio_stream::wrappers::UnixListenerStream;
    use tokio_stream::{Stream, StreamExt};
    use tonic::{Request, Response, Status};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[derive(Debug, Default)]
    struct FakeDaemonCalls {
        status_calls: usize,
        credential_status_calls: usize,
        sync_status_paths: Vec<String>,
        pull_requests: Vec<PullRequest>,
        resolve_requests: Vec<ResolveConflictRequest>,
        push_chunks: Vec<PushChunk>,
    }

    #[derive(Clone, Default)]
    struct FakeDaemon {
        calls: Arc<TokioMutex<FakeDaemonCalls>>,
    }

    struct McpHarness {
        mcp: TcfsMcp,
        calls: Arc<TokioMutex<FakeDaemonCalls>>,
        _socket_dir: TempDir,
        shutdown: Arc<Notify>,
        handle: JoinHandle<Result<(), tonic::transport::Error>>,
    }

    impl McpHarness {
        async fn shutdown(self) {
            self.shutdown.notify_waiters();
            self.handle.await.unwrap().unwrap();
        }
    }

    impl FakeDaemon {
        fn new() -> Self {
            Self::default()
        }
    }

    fn parse_json(output: String) -> serde_json::Value {
        serde_json::from_str(&output).unwrap_or_else(|e| {
            panic!("expected JSON output, got {output:?}: {e}");
        })
    }

    fn assert_object_keys(value: &serde_json::Value, keys: &[&str]) {
        let actual: BTreeSet<&str> = value
            .as_object()
            .expect("expected JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(actual, expected);
    }

    fn assert_daemon_error(output: String, socket_path: &std::path::Path) {
        let value = parse_json(output);
        assert_object_keys(&value, &["error"]);
        assert!(value["error"].as_str().unwrap().contains(&format!(
            "failed to connect to daemon at {}",
            socket_path.display()
        )));
    }

    async fn spawn_mcp_harness() -> McpHarness {
        let socket_dir = tempfile::tempdir().unwrap();
        let socket_path = socket_dir.path().join("tcfsd.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let incoming = UnixListenerStream::new(listener);
        let daemon = FakeDaemon::new();
        let calls = daemon.calls.clone();
        let shutdown = Arc::new(Notify::new());
        let shutdown_for_server = shutdown.clone();

        let handle = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(TcfsDaemonServer::new(daemon))
                .serve_with_incoming_shutdown(incoming, shutdown_for_server.notified())
                .await
        });

        McpHarness {
            mcp: TcfsMcp::new(socket_path, None),
            calls,
            _socket_dir: socket_dir,
            shutdown,
            handle,
        }
    }

    #[tonic::async_trait]
    impl TcfsDaemon for FakeDaemon {
        type PushStream =
            Pin<Box<dyn Stream<Item = Result<PushProgress, Status>> + Send + 'static>>;
        type PullStream =
            Pin<Box<dyn Stream<Item = Result<PullProgress, Status>> + Send + 'static>>;
        type HydrateStream =
            Pin<Box<dyn Stream<Item = Result<HydrateProgress, Status>> + Send + 'static>>;
        type WatchStream = Pin<Box<dyn Stream<Item = Result<WatchEvent, Status>> + Send + 'static>>;

        async fn status(
            &self,
            _request: Request<StatusRequest>,
        ) -> Result<Response<StatusResponse>, Status> {
            self.calls.lock().await.status_calls += 1;
            Ok(Response::new(StatusResponse {
                version: "0.12.0-test".into(),
                storage_endpoint: "http://seaweedfs:8333".into(),
                storage_ok: true,
                nats_ok: false,
                active_mounts: 2,
                uptime_secs: 42,
                device_id: "device-123".into(),
                device_name: "test-device".into(),
                conflict_mode: "manual".into(),
            }))
        }

        async fn credential_status(
            &self,
            _request: Request<Empty>,
        ) -> Result<Response<CredentialStatusResponse>, Status> {
            self.calls.lock().await.credential_status_calls += 1;
            Ok(Response::new(CredentialStatusResponse {
                loaded: true,
                source: "env".into(),
                loaded_at: 123,
                needs_reload: false,
            }))
        }

        async fn push(
            &self,
            request: Request<tonic::Streaming<PushChunk>>,
        ) -> Result<Response<Self::PushStream>, Status> {
            let mut stream = request.into_inner();
            let mut chunks = Vec::new();
            while let Some(chunk) = stream.next().await {
                chunks.push(chunk?);
            }
            let total_bytes: u64 = chunks.iter().map(|chunk| chunk.data.len() as u64).sum();
            self.calls.lock().await.push_chunks = chunks;

            Ok(Response::new(Box::pin(tokio_stream::iter(vec![Ok(
                PushProgress {
                    bytes_sent: total_bytes,
                    total_bytes,
                    chunk_hash: "hash-123".into(),
                    done: true,
                    error: String::new(),
                },
            )]))))
        }

        async fn pull(
            &self,
            request: Request<PullRequest>,
        ) -> Result<Response<Self::PullStream>, Status> {
            let req = request.into_inner();
            self.calls.lock().await.pull_requests.push(req);
            Ok(Response::new(Box::pin(tokio_stream::iter(vec![
                Ok(PullProgress {
                    bytes_received: 4,
                    total_bytes: 8,
                    done: false,
                    error: String::new(),
                }),
                Ok(PullProgress {
                    bytes_received: 8,
                    total_bytes: 8,
                    done: true,
                    error: String::new(),
                }),
            ]))))
        }

        async fn sync_status(
            &self,
            request: Request<SyncStatusRequest>,
        ) -> Result<Response<SyncStatusResponse>, Status> {
            let req = request.into_inner();
            let path = req.path;
            self.calls.lock().await.sync_status_paths.push(path.clone());
            Ok(Response::new(SyncStatusResponse {
                path,
                state: "synced".into(),
                blake3: "abc123".into(),
                size: 99,
                last_synced: 1_717_171_717,
            }))
        }

        async fn resolve_conflict(
            &self,
            request: Request<ResolveConflictRequest>,
        ) -> Result<Response<ResolveConflictResponse>, Status> {
            let req = request.into_inner();
            self.calls.lock().await.resolve_requests.push(req.clone());
            Ok(Response::new(ResolveConflictResponse {
                success: true,
                resolved_path: req.path,
                error: String::new(),
            }))
        }

        async fn mount(
            &self,
            _request: Request<MountRequest>,
        ) -> Result<Response<MountResponse>, Status> {
            Err(Status::unimplemented("mount"))
        }

        async fn unmount(
            &self,
            _request: Request<UnmountRequest>,
        ) -> Result<Response<UnmountResponse>, Status> {
            Err(Status::unimplemented("unmount"))
        }

        async fn unsync(
            &self,
            _request: Request<UnsyncRequest>,
        ) -> Result<Response<UnsyncResponse>, Status> {
            Err(Status::unimplemented("unsync"))
        }

        async fn list_files(
            &self,
            _request: Request<ListFilesRequest>,
        ) -> Result<Response<ListFilesResponse>, Status> {
            Err(Status::unimplemented("list_files"))
        }

        async fn auth_unlock(
            &self,
            _request: Request<AuthUnlockRequest>,
        ) -> Result<Response<AuthUnlockResponse>, Status> {
            Err(Status::unimplemented("auth_unlock"))
        }

        async fn auth_lock(
            &self,
            _request: Request<Empty>,
        ) -> Result<Response<AuthLockResponse>, Status> {
            Err(Status::unimplemented("auth_lock"))
        }

        async fn auth_status(
            &self,
            _request: Request<Empty>,
        ) -> Result<Response<AuthStatusResponse>, Status> {
            Err(Status::unimplemented("auth_status"))
        }

        async fn auth_enroll(
            &self,
            _request: Request<AuthEnrollRequest>,
        ) -> Result<Response<AuthEnrollResponse>, Status> {
            Err(Status::unimplemented("auth_enroll"))
        }

        async fn auth_complete_enroll(
            &self,
            _request: Request<AuthCompleteEnrollRequest>,
        ) -> Result<Response<AuthCompleteEnrollResponse>, Status> {
            Err(Status::unimplemented("auth_complete_enroll"))
        }

        async fn auth_challenge(
            &self,
            _request: Request<AuthChallengeRequest>,
        ) -> Result<Response<AuthChallengeResponse>, Status> {
            Err(Status::unimplemented("auth_challenge"))
        }

        async fn auth_verify(
            &self,
            _request: Request<AuthVerifyRequest>,
        ) -> Result<Response<AuthVerifyResponse>, Status> {
            Err(Status::unimplemented("auth_verify"))
        }

        async fn auth_revoke(
            &self,
            _request: Request<AuthRevokeRequest>,
        ) -> Result<Response<AuthRevokeResponse>, Status> {
            Err(Status::unimplemented("auth_revoke"))
        }

        async fn device_enroll(
            &self,
            _request: Request<DeviceEnrollRequest>,
        ) -> Result<Response<DeviceEnrollResponse>, Status> {
            Err(Status::unimplemented("device_enroll"))
        }

        async fn diagnostics(
            &self,
            _request: Request<DiagnosticsRequest>,
        ) -> Result<Response<DiagnosticsResponse>, Status> {
            Err(Status::unimplemented("diagnostics"))
        }

        async fn hydrate(
            &self,
            _request: Request<HydrateRequest>,
        ) -> Result<Response<Self::HydrateStream>, Status> {
            Err(Status::unimplemented("hydrate"))
        }

        async fn watch(
            &self,
            _request: Request<WatchRequest>,
        ) -> Result<Response<Self::WatchStream>, Status> {
            Err(Status::unimplemented("watch"))
        }
    }

    #[tokio::test]
    async fn daemon_status_maps_rpc_fields_to_json() {
        let harness = spawn_mcp_harness().await;

        let value = parse_json(harness.mcp.daemon_status().await);

        assert_object_keys(
            &value,
            &[
                "active_mounts",
                "conflict_mode",
                "device_id",
                "device_name",
                "nats_ok",
                "storage_endpoint",
                "storage_ok",
                "uptime_secs",
                "version",
            ],
        );
        assert_eq!(value["version"], "0.12.0-test");
        assert_eq!(value["device_id"], "device-123");
        assert_eq!(value["active_mounts"], 2);
        assert_eq!(harness.calls.lock().await.status_calls, 1);

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn credential_status_maps_rpc_fields_to_json() {
        let harness = spawn_mcp_harness().await;

        let value = parse_json(harness.mcp.credential_status().await);

        assert_object_keys(&value, &["loaded", "loaded_at", "needs_reload", "source"]);
        assert_eq!(value["loaded"], true);
        assert_eq!(value["source"], "env");
        assert_eq!(harness.calls.lock().await.credential_status_calls, 1);

        harness.shutdown().await;
    }

    #[test]
    fn config_show_serializes_config_json() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut config = tcfs_core::config::TcfsConfig::default();
        config.storage.bucket = "bucket-a".into();
        config.sync.conflict_mode = "keep_local".into();
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

        let mcp = TcfsMcp::new(PathBuf::from("/tmp/unused.sock"), Some(config_path));
        let value = parse_json(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(mcp.config_show()),
        );

        assert_eq!(value["storage"]["bucket"], "bucket-a");
        assert_eq!(value["sync"]["conflict_mode"], "keep_local");
        assert!(value.get("daemon").is_some());
        assert!(value.get("fuse").is_some());
        assert!(value.get("crypto").is_some());
    }

    #[tokio::test]
    async fn sync_status_maps_request_and_response() {
        let harness = spawn_mcp_harness().await;

        let value = parse_json(
            harness
                .mcp
                .sync_status(Parameters(SyncStatusInput {
                    path: "/tmp/example.txt".into(),
                }))
                .await,
        );

        assert_object_keys(&value, &["blake3", "last_synced", "path", "size", "state"]);
        assert_eq!(value["path"], "/tmp/example.txt");
        assert_eq!(value["state"], "synced");
        assert_eq!(
            harness.calls.lock().await.sync_status_paths,
            vec!["/tmp/example.txt".to_string()]
        );

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn pull_maps_request_and_last_stream_progress() {
        let harness = spawn_mcp_harness().await;

        let value = parse_json(
            harness
                .mcp
                .pull(Parameters(PullInput {
                    remote_path: "remote/file.txt".into(),
                    local_path: "/tmp/local.txt".into(),
                }))
                .await,
        );

        assert_object_keys(&value, &["bytes_received", "done", "error", "total_bytes"]);
        assert_eq!(value["bytes_received"], 8);
        assert_eq!(value["total_bytes"], 8);
        assert_eq!(value["done"], true);
        assert!(value["error"].is_null());
        assert_eq!(
            harness.calls.lock().await.pull_requests,
            vec![PullRequest {
                remote_path: "remote/file.txt".into(),
                local_path: "/tmp/local.txt".into(),
            }]
        );

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn resolve_conflict_maps_request_and_response() {
        let harness = spawn_mcp_harness().await;

        let value = parse_json(
            harness
                .mcp
                .resolve_conflict(Parameters(ResolveConflictInput {
                    rel_path: "docs/report.md".into(),
                    resolution: "keep_both".into(),
                }))
                .await,
        );

        assert_object_keys(&value, &["error", "resolved_path", "success"]);
        assert_eq!(value["success"], true);
        assert_eq!(value["resolved_path"], "docs/report.md");
        assert!(value["error"].is_null());
        assert_eq!(
            harness.calls.lock().await.resolve_requests,
            vec![ResolveConflictRequest {
                path: "docs/report.md".into(),
                resolution: "keep_both".into(),
            }]
        );

        harness.shutdown().await;
    }

    #[test]
    fn device_status_reads_registry_and_counts_active_devices() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config_root = dir.path().join("xdg");
        let registry_path = config_root.join("tcfs").join("devices.json");
        let registry = tcfs_secrets::device::DeviceRegistry {
            devices: vec![
                tcfs_secrets::device::DeviceIdentity {
                    name: "laptop".into(),
                    device_id: "device-a".into(),
                    public_key: "age1laptop".into(),
                    signing_key_hash: "abc".into(),
                    description: Some("primary".into()),
                    enrolled_at: 1,
                    revoked: false,
                    last_nats_seq: 7,
                },
                tcfs_secrets::device::DeviceIdentity {
                    name: "phone".into(),
                    device_id: "device-b".into(),
                    public_key: "age1phone".into(),
                    signing_key_hash: "def".into(),
                    description: None,
                    enrolled_at: 2,
                    revoked: true,
                    last_nats_seq: 3,
                },
            ],
        };
        registry.save(&registry_path).unwrap();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &config_root);
        }
        let mcp = TcfsMcp::new(PathBuf::from("/tmp/unused.sock"), None);
        let value = parse_json(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(mcp.device_status()),
        );
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        assert_object_keys(&value, &["active", "devices", "total"]);
        assert_eq!(value["total"], 2);
        assert_eq!(value["active"], 1);
        assert_eq!(value["devices"].as_array().unwrap().len(), 2);
        assert_eq!(value["devices"][0]["name"], "laptop");
    }

    #[tokio::test]
    async fn push_reads_local_file_and_maps_stream_progress() {
        let harness = spawn_mcp_harness().await;
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("upload.txt");
        std::fs::write(&local_path, b"hello mcp").unwrap();

        let value = parse_json(
            harness
                .mcp
                .push(Parameters(PushInput {
                    local_path: local_path.display().to_string(),
                }))
                .await,
        );

        assert_object_keys(
            &value,
            &["bytes_sent", "chunk_hash", "done", "error", "total_bytes"],
        );
        assert_eq!(value["bytes_sent"], 9);
        assert_eq!(value["total_bytes"], 9);
        assert_eq!(value["chunk_hash"], "hash-123");
        assert_eq!(value["done"], true);
        assert!(value["error"].is_null());

        let chunks = {
            let calls = harness.calls.lock().await;
            calls.push_chunks.clone()
        };
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].path, local_path.display().to_string());
        assert_eq!(chunks[0].data, b"hello mcp");
        assert!(chunks[0].last);

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn daemon_backed_tools_report_connect_errors_when_daemon_is_unavailable() {
        let socket_path = std::env::temp_dir().join(format!(
            "tcfs-mcp-missing-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mcp = TcfsMcp::new(socket_path.clone(), None);
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("upload.txt");
        std::fs::write(&local_path, b"hello mcp").unwrap();

        assert_daemon_error(mcp.daemon_status().await, &socket_path);
        assert_daemon_error(mcp.credential_status().await, &socket_path);
        assert_daemon_error(
            mcp.sync_status(Parameters(SyncStatusInput {
                path: "/tmp/example.txt".into(),
            }))
            .await,
            &socket_path,
        );
        assert_daemon_error(
            mcp.pull(Parameters(PullInput {
                remote_path: "remote/file.txt".into(),
                local_path: "/tmp/local.txt".into(),
            }))
            .await,
            &socket_path,
        );
        assert_daemon_error(
            mcp.resolve_conflict(Parameters(ResolveConflictInput {
                rel_path: "docs/report.md".into(),
                resolution: "keep_local".into(),
            }))
            .await,
            &socket_path,
        );
        assert_daemon_error(
            mcp.push(Parameters(PushInput {
                local_path: local_path.display().to_string(),
            }))
            .await,
            &socket_path,
        );
    }
}
