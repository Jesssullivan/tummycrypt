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
    device_id: String,
    device_name: String,
    nats_ok: std::sync::atomic::AtomicBool,
    nats: Arc<TokioMutex<Option<tcfs_sync::NatsClient>>>,
    active_mounts: Arc<TokioMutex<std::collections::HashMap<String, tokio::process::Child>>>,
}

impl TcfsDaemonImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cred_store: SharedCredStore,
        config: Arc<TcfsConfig>,
        storage_ok: bool,
        storage_endpoint: String,
        state_cache: tcfs_sync::state::StateCache,
        operator: Arc<TokioMutex<Option<opendal::Operator>>>,
        device_id: String,
        device_name: String,
    ) -> Self {
        Self {
            cred_store,
            config,
            storage_ok,
            storage_endpoint,
            start_time: std::time::Instant::now(),
            state_cache: Arc::new(TokioMutex::new(state_cache)),
            operator,
            device_id,
            device_name,
            nats_ok: std::sync::atomic::AtomicBool::new(false),
            nats: Arc::new(TokioMutex::new(None)),
            active_mounts: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
        }
    }

    /// Get a handle to the state cache for shutdown flushing.
    pub fn state_cache_handle(&self) -> Arc<TokioMutex<tcfs_sync::state::StateCache>> {
        self.state_cache.clone()
    }

    /// Get a handle to the NATS client for shutdown notification.
    pub fn nats_handle(&self) -> Arc<TokioMutex<Option<tcfs_sync::NatsClient>>> {
        self.nats.clone()
    }

    /// Publish a ConflictResolved event via NATS (best-effort).
    async fn publish_conflict_resolved(&self, rel_path: &str, resolution: &str) {
        if let Some(nats) = self.nats.lock().await.as_ref() {
            // Build merged vclock from state cache
            let merged_vclock = {
                let cache = self.state_cache.lock().await;
                let path = std::path::PathBuf::from(rel_path);
                cache
                    .get(&path)
                    .map(|e| e.vclock.clone())
                    .unwrap_or_default()
            };

            let event = tcfs_sync::StateEvent::ConflictResolved {
                device_id: self.device_id.clone(),
                rel_path: rel_path.to_string(),
                resolution: resolution.to_string(),
                merged_vclock,
                timestamp: tcfs_sync::StateEvent::now(),
            };
            if let Err(e) = nats.publish_state_event(&event).await {
                tracing::warn!("failed to publish ConflictResolved: {e}");
            }
        }
    }

    /// Set the NATS client (called from daemon after connecting).
    pub fn set_nats(&self, client: tcfs_sync::NatsClient) {
        // set_nats_ok is implicitly true if we have a client
        self.nats_ok
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // We need a runtime handle since this might be called from sync context
        // but the Mutex is tokio::sync::Mutex, so just use block_in_place
        let nats = self.nats.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async {
                *nats.lock().await = Some(client);
            });
        });
    }
}

#[tonic::async_trait]
impl TcfsDaemon for TcfsDaemonImpl {
    async fn status(
        &self,
        _request: tonic::Request<StatusRequest>,
    ) -> Result<tonic::Response<StatusResponse>, tonic::Status> {
        let uptime = self.start_time.elapsed().as_secs() as i64;
        let mount_count = self.active_mounts.lock().await.len() as i32;
        Ok(tonic::Response::new(StatusResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            storage_endpoint: self.storage_endpoint.clone(),
            storage_ok: self.storage_ok,
            nats_ok: self.nats_ok.load(std::sync::atomic::Ordering::Relaxed),
            active_mounts: mount_count,
            uptime_secs: uptime,
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            conflict_mode: self.config.sync.conflict_mode.clone(),
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
        request: tonic::Request<MountRequest>,
    ) -> Result<tonic::Response<MountResponse>, tonic::Status> {
        let req = request.into_inner();

        if req.mountpoint.is_empty() || req.remote.is_empty() {
            return Ok(tonic::Response::new(MountResponse {
                success: false,
                error: "mountpoint and remote are required".into(),
            }));
        }

        let mountpoint = std::path::PathBuf::from(&req.mountpoint);

        // Check not already mounted
        {
            let mounts = self.active_mounts.lock().await;
            if mounts.contains_key(&req.mountpoint) {
                return Ok(tonic::Response::new(MountResponse {
                    success: false,
                    error: format!("already mounted at: {}", req.mountpoint),
                }));
            }
        }

        // Ensure mountpoint directory exists
        if !mountpoint.exists() {
            std::fs::create_dir_all(&mountpoint).map_err(|e| {
                tonic::Status::internal(format!("create mountpoint {}: {e}", req.mountpoint))
            })?;
        }

        info!(
            mountpoint = %req.mountpoint,
            remote = %req.remote,
            "spawning FUSE mount"
        );

        // Spawn tcfs mount as subprocess
        let child = tokio::process::Command::new("tcfs")
            .args(["mount", &req.remote, &req.mountpoint])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| tonic::Status::internal(format!("spawn tcfs mount: {e}")))?;

        // Give the mount a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        {
            let mut mounts = self.active_mounts.lock().await;
            mounts.insert(req.mountpoint.clone(), child);
        }

        Ok(tonic::Response::new(MountResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn unmount(
        &self,
        request: tonic::Request<UnmountRequest>,
    ) -> Result<tonic::Response<UnmountResponse>, tonic::Status> {
        let req = request.into_inner();

        if req.mountpoint.is_empty() {
            return Ok(tonic::Response::new(UnmountResponse {
                success: false,
                error: "mountpoint is required".into(),
            }));
        }

        info!(mountpoint = %req.mountpoint, "unmount requested");

        // Try fusermount3 first, fallback to fusermount
        let result = tokio::process::Command::new("fusermount3")
            .args(["-u", &req.mountpoint])
            .output()
            .await;

        let ok = match result {
            Ok(output) if output.status.success() => true,
            _ => {
                // Fallback to fusermount
                match tokio::process::Command::new("fusermount")
                    .args(["-u", &req.mountpoint])
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => true,
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        return Ok(tonic::Response::new(UnmountResponse {
                            success: false,
                            error: format!("fusermount failed: {stderr}"),
                        }));
                    }
                    Err(e) => {
                        return Ok(tonic::Response::new(UnmountResponse {
                            success: false,
                            error: format!("neither fusermount3 nor fusermount available: {e}"),
                        }));
                    }
                }
            }
        };

        if ok {
            // Remove from active mounts and kill child if still running
            let mut mounts = self.active_mounts.lock().await;
            if let Some(mut child) = mounts.remove(&req.mountpoint) {
                let _ = child.kill().await;
            }

            info!(mountpoint = %req.mountpoint, "unmounted");
        }

        Ok(tonic::Response::new(UnmountResponse {
            success: ok,
            error: String::new(),
        }))
    }

    // ── Push: client-streaming upload ─────────────────────────────────────

    type PushStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<PushProgress, tonic::Status>> + Send>,
    >;

    async fn push(
        &self,
        request: tonic::Request<tonic::Streaming<PushChunk>>,
    ) -> Result<tonic::Response<Self::PushStream>, tonic::Status> {
        use tokio_stream::StreamExt;

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator — check credentials"))?;
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
            return Err(tonic::Status::invalid_argument(
                "no path provided in push stream",
            ));
        }

        // Write to a temp file and upload via sync engine
        let tmp_dir =
            tempfile::tempdir().map_err(|e| tonic::Status::internal(format!("tempdir: {e}")))?;
        let local_path = tmp_dir.path().join(&path);
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| tonic::Status::internal(format!("mkdir: {e}")))?;
        }
        std::fs::write(&local_path, &data)
            .map_err(|e| tonic::Status::internal(format!("write temp: {e}")))?;

        let total_bytes = data.len() as u64;
        let device_id = self.device_id.clone();

        let result = {
            let mut cache = state_cache.lock().await;
            tcfs_sync::engine::upload_file_with_device(
                &op,
                &local_path,
                &prefix,
                &mut cache,
                None,
                &device_id,
                Some(&path),
            )
            .await
        };

        match result {
            Ok(upload) => {
                // Publish state event if NATS is connected and file was actually uploaded
                if !upload.skipped {
                    let nats = self.nats.clone();
                    let device_id = self.device_id.clone();
                    let rel_path = path.clone();
                    let blake3 = upload.hash.clone();
                    let size = total_bytes;
                    let remote_path = upload.remote_path.clone();
                    tokio::spawn(async move {
                        if let Some(nats) = nats.lock().await.as_ref() {
                            let event = tcfs_sync::StateEvent::FileSynced {
                                device_id,
                                rel_path,
                                blake3,
                                size,
                                vclock: tcfs_sync::conflict::VectorClock::default(),
                                manifest_path: remote_path,
                                timestamp: tcfs_sync::StateEvent::now(),
                            };
                            if let Err(e) = nats.publish_state_event(&event).await {
                                tracing::warn!("failed to publish state event: {e}");
                            }
                        }
                    });
                }

                let progress = PushProgress {
                    bytes_sent: total_bytes,
                    total_bytes,
                    chunk_hash: upload.hash,
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
            Err(e) => {
                let progress = PushProgress {
                    bytes_sent: 0,
                    total_bytes,
                    chunk_hash: String::new(),
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
        }
    }

    // ── Pull: server-streaming download ───────────────────────────────────

    type PullStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<PullProgress, tonic::Status>> + Send>,
    >;

    async fn pull(
        &self,
        request: tonic::Request<PullRequest>,
    ) -> Result<tonic::Response<Self::PullStream>, tonic::Status> {
        let req = request.into_inner();

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator — check credentials"))?;
        let op = op.clone();

        let prefix = self.config.storage.bucket.clone();
        let local_path = std::path::PathBuf::from(&req.local_path);
        let device_id = self.device_id.clone();
        let state_cache = self.state_cache.clone();

        let result = {
            let mut cache = state_cache.lock().await;
            tcfs_sync::engine::download_file_with_device(
                &op,
                &req.remote_path,
                &local_path,
                &prefix,
                None,
                &device_id,
                Some(&mut cache),
            )
            .await
        };

        match result {
            Ok(dl) => {
                let progress = PullProgress {
                    bytes_received: dl.bytes,
                    total_bytes: dl.bytes,
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
            Err(e) => {
                let progress = PullProgress {
                    bytes_received: 0,
                    total_bytes: 0,
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
        }
    }

    // ── Hydrate ───────────────────────────────────────────────────────────

    type HydrateStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<HydrateProgress, tonic::Status>> + Send>,
    >;

    async fn hydrate(
        &self,
        request: tonic::Request<HydrateRequest>,
    ) -> Result<tonic::Response<Self::HydrateStream>, tonic::Status> {
        let req = request.into_inner();
        let stub_path = std::path::PathBuf::from(&req.stub_path);

        info!(stub = %req.stub_path, "hydrate requested");

        // Read and parse stub file
        let stub_content = std::fs::read_to_string(&stub_path)
            .map_err(|e| tonic::Status::not_found(format!("read stub: {e}")))?;
        let meta = tcfs_fuse::stub::StubMeta::parse(&stub_content)
            .map_err(|e| tonic::Status::invalid_argument(format!("parse stub: {e}")))?;

        // Derive real file path from stub path
        let real_path =
            tcfs_fuse::stub::stub_to_real_name(stub_path.as_os_str()).ok_or_else(|| {
                tonic::Status::invalid_argument(format!(
                    "cannot derive real name from stub: {}",
                    req.stub_path
                ))
            })?;

        // Extract manifest hash from oid
        let blake3_hex = meta
            .blake3_hex()
            .ok_or_else(|| tonic::Status::invalid_argument("stub oid missing blake3: prefix"))?;
        let prefix = self.config.storage.bucket.clone();
        let manifest_path = format!("{prefix}/manifests/{blake3_hex}");

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?;
        let op = op.clone();
        drop(self.operator.lock().await);

        let total_bytes = meta.size;

        let result = {
            let mut cache = self.state_cache.lock().await;
            tcfs_sync::engine::download_file_with_device(
                &op,
                &manifest_path,
                &real_path,
                &prefix,
                None,
                &self.device_id,
                Some(&mut cache),
            )
            .await
        };

        match result {
            Ok(dl) => {
                // Remove stub file after successful hydration
                let _ = std::fs::remove_file(&stub_path);

                info!(
                    real_path = %real_path.display(),
                    bytes = dl.bytes,
                    "hydration complete"
                );

                let progress = HydrateProgress {
                    bytes_received: dl.bytes,
                    total_bytes,
                    local_path: real_path.to_string_lossy().to_string(),
                    done: true,
                    error: String::new(),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
            Err(e) => {
                let progress = HydrateProgress {
                    bytes_received: 0,
                    total_bytes,
                    local_path: String::new(),
                    done: true,
                    error: format!("{e}"),
                };
                Ok(tonic::Response::new(Box::pin(tokio_stream::once(Ok(
                    progress,
                )))))
            }
        }
    }

    // ── Unsync ────────────────────────────────────────────────────────────

    async fn unsync(
        &self,
        request: tonic::Request<UnsyncRequest>,
    ) -> Result<tonic::Response<UnsyncResponse>, tonic::Status> {
        let req = request.into_inner();
        let path = std::path::PathBuf::from(&req.path);

        info!(path = %req.path, force = req.force, "unsync requested");

        let mut cache = self.state_cache.lock().await;
        if cache.get(&path).is_none() {
            return Ok(tonic::Response::new(UnsyncResponse {
                success: false,
                stub_path: String::new(),
                error: format!("path not in sync state: {}", req.path),
            }));
        }

        cache.remove(&path);
        if let Err(e) = cache.flush() {
            return Ok(tonic::Response::new(UnsyncResponse {
                success: false,
                stub_path: String::new(),
                error: format!("state cache flush failed: {e}"),
            }));
        }

        info!(path = %req.path, "unsynced successfully");

        Ok(tonic::Response::new(UnsyncResponse {
            success: true,
            stub_path: String::new(),
            error: String::new(),
        }))
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

    // ── Resolve Conflict ──────────────────────────────────────────────────

    async fn resolve_conflict(
        &self,
        request: tonic::Request<ResolveConflictRequest>,
    ) -> Result<tonic::Response<ResolveConflictResponse>, tonic::Status> {
        let req = request.into_inner();

        let resolution = match req.resolution.as_str() {
            "keep_local" | "keep_remote" | "keep_both" | "defer" => req.resolution.clone(),
            other => {
                return Ok(tonic::Response::new(ResolveConflictResponse {
                    success: false,
                    resolved_path: String::new(),
                    error: format!(
                        "invalid resolution '{}': use keep_local, keep_remote, keep_both, or defer",
                        other
                    ),
                }));
            }
        };

        info!(
            path = %req.path,
            resolution = %resolution,
            device = %self.device_id,
            "conflict resolution requested"
        );

        let path = std::path::PathBuf::from(&req.path);

        match resolution.as_str() {
            "defer" => {
                info!(path = %req.path, "conflict deferred");
                Ok(tonic::Response::new(ResolveConflictResponse {
                    success: true,
                    resolved_path: req.path,
                    error: String::new(),
                }))
            }
            "keep_local" => {
                // Read local state, tick vclock, build new manifest, upload
                let local_state = {
                    let cache = self.state_cache.lock().await;
                    cache.get(&path).cloned()
                };

                let local_state = match local_state {
                    Some(s) => s,
                    None => {
                        return Ok(tonic::Response::new(ResolveConflictResponse {
                            success: false,
                            resolved_path: String::new(),
                            error: format!("no local state for path: {}", req.path),
                        }));
                    }
                };

                // Tick our vclock and build updated manifest
                let mut vclock = local_state.vclock.clone();
                vclock.tick(&self.device_id);

                let manifest = tcfs_sync::manifest::SyncManifest {
                    version: 2,
                    file_hash: local_state.blake3.clone(),
                    file_size: local_state.size,
                    chunks: vec![],
                    vclock: vclock.clone(),
                    written_by: self.device_id.clone(),
                    written_at: tcfs_sync::StateEvent::now(),
                    rel_path: Some(req.path.clone()),
                };

                // Upload updated manifest
                let op = self.operator.lock().await;
                if let Some(op) = op.as_ref() {
                    let manifest_key = local_state.remote_path.clone();
                    let manifest_bytes = manifest
                        .to_bytes()
                        .map_err(|e| tonic::Status::internal(format!("manifest serialize: {e}")))?;
                    op.write(&manifest_key, manifest_bytes)
                        .await
                        .map_err(|e| tonic::Status::internal(format!("manifest upload: {e}")))?;
                }
                drop(op);

                // Update state cache
                {
                    let mut cache = self.state_cache.lock().await;
                    if let Some(entry) = cache.get(&path).cloned() {
                        let updated = tcfs_sync::state::SyncState {
                            vclock,
                            last_synced: tcfs_sync::StateEvent::now(),
                            ..entry
                        };
                        cache.set(&path, updated);
                        let _ = cache.flush();
                    }
                }

                // Publish ConflictResolved via NATS
                self.publish_conflict_resolved(&req.path, "keep_local")
                    .await;

                Ok(tonic::Response::new(ResolveConflictResponse {
                    success: true,
                    resolved_path: req.path,
                    error: String::new(),
                }))
            }
            "keep_remote" => {
                // Download remote version to local path
                let (remote_path, prefix) = {
                    let cache = self.state_cache.lock().await;
                    let entry = cache.get(&path);
                    let remote = entry.map(|e| e.remote_path.clone()).unwrap_or_default();
                    let prefix = self.config.storage.bucket.clone();
                    (remote, prefix)
                };

                if remote_path.is_empty() {
                    return Ok(tonic::Response::new(ResolveConflictResponse {
                        success: false,
                        resolved_path: String::new(),
                        error: format!("no remote path for: {}", req.path),
                    }));
                }

                let op = self.operator.lock().await;
                let op = op
                    .as_ref()
                    .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?;
                let op = op.clone();
                drop(self.operator.lock().await);

                let result = {
                    let mut cache = self.state_cache.lock().await;
                    tcfs_sync::engine::download_file_with_device(
                        &op,
                        &remote_path,
                        &path,
                        &prefix,
                        None,
                        &self.device_id,
                        Some(&mut cache),
                    )
                    .await
                };

                match result {
                    Ok(_dl) => {
                        self.publish_conflict_resolved(&req.path, "keep_remote")
                            .await;
                        Ok(tonic::Response::new(ResolveConflictResponse {
                            success: true,
                            resolved_path: req.path,
                            error: String::new(),
                        }))
                    }
                    Err(e) => Ok(tonic::Response::new(ResolveConflictResponse {
                        success: false,
                        resolved_path: String::new(),
                        error: format!("download failed: {e}"),
                    })),
                }
            }
            "keep_both" => {
                // Rename local file to {stem}.conflict-{device_id}{ext}, then download remote
                let (remote_path, prefix) = {
                    let cache = self.state_cache.lock().await;
                    let entry = cache.get(&path);
                    let remote = entry.map(|e| e.remote_path.clone()).unwrap_or_default();
                    let prefix = self.config.storage.bucket.clone();
                    (remote, prefix)
                };

                if remote_path.is_empty() {
                    return Ok(tonic::Response::new(ResolveConflictResponse {
                        success: false,
                        resolved_path: String::new(),
                        error: format!("no remote path for: {}", req.path),
                    }));
                }

                // Rename local file
                let conflict_path = {
                    let p = std::path::Path::new(&req.path);
                    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                    let ext = p
                        .extension()
                        .map(|e| format!(".{}", e.to_string_lossy()))
                        .unwrap_or_default();
                    let parent = p.parent().unwrap_or(std::path::Path::new(""));
                    parent
                        .join(format!("{}.conflict-{}{}", stem, self.device_id, ext))
                        .to_string_lossy()
                        .to_string()
                };

                if path.exists() {
                    if let Err(e) = std::fs::rename(&path, &conflict_path) {
                        return Ok(tonic::Response::new(ResolveConflictResponse {
                            success: false,
                            resolved_path: String::new(),
                            error: format!("rename failed: {e}"),
                        }));
                    }
                }

                // Download remote to original path
                let op = self.operator.lock().await;
                let op = op
                    .as_ref()
                    .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?;
                let op = op.clone();
                drop(self.operator.lock().await);

                let result = {
                    let mut cache = self.state_cache.lock().await;
                    tcfs_sync::engine::download_file_with_device(
                        &op,
                        &remote_path,
                        &path,
                        &prefix,
                        None,
                        &self.device_id,
                        Some(&mut cache),
                    )
                    .await
                };

                match result {
                    Ok(_dl) => {
                        self.publish_conflict_resolved(&req.path, "keep_both").await;
                        Ok(tonic::Response::new(ResolveConflictResponse {
                            success: true,
                            resolved_path: conflict_path,
                            error: String::new(),
                        }))
                    }
                    Err(e) => {
                        // Try to rename back on failure
                        let _ = std::fs::rename(&conflict_path, &path);
                        Ok(tonic::Response::new(ResolveConflictResponse {
                            success: false,
                            resolved_path: String::new(),
                            error: format!("download after rename failed: {e}"),
                        }))
                    }
                }
            }
            _ => unreachable!("already validated"),
        }
    }

    // ── Watch ─────────────────────────────────────────────────────────────

    type WatchStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<WatchEvent, tonic::Status>> + Send>,
    >;

    async fn watch(
        &self,
        request: tonic::Request<WatchRequest>,
    ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status> {
        use notify::{RecursiveMode, Watcher};

        let req = request.into_inner();
        if req.paths.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "at least one path is required",
            ));
        }

        info!(paths = ?req.paths, "watch requested");

        let (sync_tx, sync_rx) = std::sync::mpsc::channel();

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let _ = sync_tx.send(res);
        })
        .map_err(|e| tonic::Status::internal(format!("create watcher: {e}")))?;

        for path_str in &req.paths {
            let path = std::path::Path::new(path_str);
            if !path.exists() {
                return Err(tonic::Status::not_found(format!(
                    "watch path does not exist: {path_str}"
                )));
            }
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|e| tonic::Status::internal(format!("watch {path_str}: {e}")))?;
        }

        let (async_tx, async_rx) = tokio::sync::mpsc::channel(256);

        // Bridge sync watcher events to async channel
        tokio::task::spawn_blocking(move || {
            // Keep watcher alive while client is connected
            let _watcher = watcher;
            while let Ok(result) = sync_rx.recv() {
                let event = match result {
                    Ok(event) => {
                        let event_type = match event.kind {
                            notify::EventKind::Create(_) => "created",
                            notify::EventKind::Modify(_) => "modified",
                            notify::EventKind::Remove(_) => "deleted",
                            notify::EventKind::Access(_) => continue,
                            notify::EventKind::Other => continue,
                            notify::EventKind::Any => continue,
                        };
                        let path = event
                            .paths
                            .first()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        WatchEvent {
                            path,
                            event_type: event_type.to_string(),
                            timestamp,
                        }
                    }
                    Err(e) => WatchEvent {
                        path: String::new(),
                        event_type: format!("error: {e}"),
                        timestamp: 0,
                    },
                };
                if async_tx.blocking_send(Ok(event)).is_err() {
                    break; // Client disconnected
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(async_rx);
        Ok(tonic::Response::new(Box::pin(stream)))
    }
}

/// Start the gRPC server on a Unix domain socket with graceful shutdown support.
pub async fn serve(
    socket_path: &Path,
    impl_: TcfsDaemonImpl,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
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
        .serve_with_incoming_shutdown(stream, shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))
}
