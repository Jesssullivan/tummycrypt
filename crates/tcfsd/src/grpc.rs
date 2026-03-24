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
use tcfs_sync::state::StateCacheBackend;

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
    master_key: Arc<TokioMutex<Option<tcfs_crypto::MasterKey>>>,
    nats_ok: std::sync::atomic::AtomicBool,
    nats: Arc<TokioMutex<Option<tcfs_sync::NatsClient>>>,
    active_mounts: Arc<TokioMutex<std::collections::HashMap<String, tokio::process::Child>>>,
    // Auth infrastructure
    session_store: tcfs_auth::SessionStore,
    totp_provider: Arc<tcfs_auth::totp::TotpProvider>,
    webauthn_provider: Arc<tcfs_auth::webauthn::WebAuthnProvider>,
    rate_limiter: tcfs_auth::RateLimiter,
}

impl TcfsDaemonImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cred_store: SharedCredStore,
        config: Arc<TcfsConfig>,
        storage_ok: bool,
        storage_endpoint: String,
        state_cache: Arc<TokioMutex<tcfs_sync::state::StateCache>>,
        operator: Arc<TokioMutex<Option<opendal::Operator>>>,
        device_id: String,
        device_name: String,
        master_key: Option<tcfs_crypto::MasterKey>,
    ) -> Self {
        let totp_config = tcfs_auth::totp::TotpConfig {
            issuer: config.auth.totp.issuer.clone(),
            digits: config.auth.totp.digits as usize,
            ..tcfs_auth::totp::TotpConfig::default()
        };
        let totp_provider = Arc::new(tcfs_auth::totp::TotpProvider::new(totp_config));

        let webauthn_config = tcfs_auth::webauthn::WebAuthnConfig {
            rp_name: config.auth.webauthn.relying_party_name.clone(),
            rp_id: config.auth.webauthn.relying_party_id.clone(),
            rp_origin: format!("https://{}", config.auth.webauthn.relying_party_id),
        };
        let webauthn_provider = Arc::new(
            tcfs_auth::webauthn::WebAuthnProvider::new(webauthn_config).unwrap_or_else(|e| {
                tracing::warn!("WebAuthn provider init failed: {e}, using defaults");
                tcfs_auth::webauthn::WebAuthnProvider::new(
                    tcfs_auth::webauthn::WebAuthnConfig::default(),
                )
                .expect("default WebAuthn config should always work")
            }),
        );

        let rate_limiter = tcfs_auth::RateLimiter::new(tcfs_auth::RateLimitConfig {
            max_attempts: config.auth.rate_limit.max_attempts,
            lockout_duration: chrono::Duration::seconds(config.auth.rate_limit.lockout_secs as i64),
            backoff_multiplier: config.auth.rate_limit.backoff_multiplier,
        });

        Self {
            cred_store,
            config,
            storage_ok,
            storage_endpoint,
            start_time: std::time::Instant::now(),
            state_cache,
            operator,
            device_id,
            device_name,
            master_key: Arc::new(TokioMutex::new(master_key)),
            nats_ok: std::sync::atomic::AtomicBool::new(false),
            nats: Arc::new(TokioMutex::new(None)),
            active_mounts: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            session_store: tcfs_auth::SessionStore::new(),
            totp_provider,
            webauthn_provider,
            rate_limiter,
        }
    }

    /// Get a clone of the session store (for background tasks).
    pub fn session_store(&self) -> tcfs_auth::SessionStore {
        self.session_store.clone()
    }

    /// Load persisted TOTP credentials from disk.
    pub async fn load_totp_credentials(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.totp_provider.load_from_file(path).await
    }

    /// Load persisted sessions from disk.
    pub async fn load_sessions(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.session_store.load_from_file(path).await
    }

    /// Save sessions to disk (called after session changes).
    async fn persist_sessions(&self) {
        let path = dirs::data_dir()
            .unwrap_or_default()
            .join("tcfsd/sessions.json");
        if let Err(e) = self.session_store.save_to_file(&path).await {
            tracing::warn!("failed to persist sessions: {e}");
        }
    }

    /// Validate a session token from gRPC request metadata.
    ///
    /// Returns Ok(Session) if the session is valid, or a gRPC UNAUTHENTICATED
    /// error if auth is required and the token is missing/invalid/expired.
    ///
    /// When `config.auth.require_session` is false (default for alpha), this
    /// returns a synthetic session with full permissions (bypass mode).
    async fn require_session<T>(
        &self,
        request: &tonic::Request<T>,
    ) -> Result<tcfs_auth::Session, tonic::Status> {
        // Alpha bypass: if auth is not required, allow all requests with full permissions
        if !self.config.auth.require_session {
            return Ok(tcfs_auth::Session::new(&self.device_id, "local", "bypass"));
        }

        // Extract token from "authorization" metadata
        let token = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_string());

        match token {
            Some(t) => match self.session_store.validate(&t).await {
                Some(session) => Ok(session),
                None => Err(tonic::Status::unauthenticated(
                    "invalid or expired session token",
                )),
            },
            None => Err(tonic::Status::unauthenticated(
                "session token required — run 'tcfs auth verify' first",
            )),
        }
    }

    /// Check that the session has the required permission, returning
    /// PERMISSION_DENIED if not.
    fn check_permission(
        session: &tcfs_auth::Session,
        permission: &str,
    ) -> Result<(), tonic::Status> {
        let allowed = match permission {
            "mount" => session.permissions.can_mount,
            "push" => session.permissions.can_push,
            "pull" => session.permissions.can_pull,
            "admin" => session.permissions.can_admin,
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(tonic::Status::permission_denied(format!(
                "device {} lacks '{}' permission",
                session.device_id, permission
            )))
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "mount")?;
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

        let use_nfs = req.options.iter().any(|o| o == "nfs");
        let backend = if use_nfs { "NFS loopback" } else { "FUSE" };

        info!(
            mountpoint = %req.mountpoint,
            remote = %req.remote,
            backend = %backend,
            "spawning mount"
        );

        // Get the storage operator from daemon state
        let op = {
            let guard = self.operator.lock().await;
            guard
                .clone()
                .ok_or_else(|| tonic::Status::unavailable("storage operator not initialized"))?
        };

        // Parse prefix from remote spec
        let (_endpoint, _bucket, prefix) = tcfs_storage::parse_remote_spec(&req.remote)
            .map_err(|e| tonic::Status::invalid_argument(format!("bad remote spec: {e}")))?;

        let mp = mountpoint.clone();
        let cache_dir = self.config.fuse.cache_dir.clone();
        let cache_max = self.config.fuse.cache_max_mb as u64 * 1024 * 1024;
        let neg_ttl = self.config.fuse.negative_cache_ttl_secs;
        let mountpoint_key = req.mountpoint.clone();
        let active_mounts = self.active_mounts.clone();

        // Start NFS server in-process (tokio task) instead of spawning a
        // subprocess.  This avoids the recursive gRPC mount call, credential
        // loss, and the process dying before the wrapper can sudo-retry the
        // mount command.
        let nfs_handle = tokio::spawn(async move {
            tracing::info!("NFS mount task starting (prefix={prefix})");
            match tcfs_nfs::serve_and_mount(tcfs_nfs::NfsMountConfig {
                op,
                prefix,
                mountpoint: mp,
                port: 0, // auto-assign
                cache_dir: std::path::PathBuf::from(&cache_dir),
                cache_max_bytes: cache_max,
                negative_ttl_secs: neg_ttl,
            })
            .await
            {
                Ok(()) => {
                    tracing::warn!("NFS serve_and_mount returned Ok (server stopped)");
                }
                Err(e) => {
                    tracing::error!(error = %e, error_debug = ?e, "in-process NFS mount failed");
                }
            }
        });

        // Monitor the NFS task in a separate watcher so we detect panics
        let active_mounts_watcher = self.active_mounts.clone();
        let mountpoint_key_watcher = req.mountpoint.clone();
        tokio::spawn(async move {
            match nfs_handle.await {
                Ok(()) => {
                    tracing::warn!("NFS task exited normally");
                }
                Err(e) => {
                    tracing::error!(error = %e, "NFS task PANICKED: {e}");
                }
            }
            let mut mounts = active_mounts_watcher.lock().await;
            mounts.remove(&mountpoint_key_watcher);
        });

        // Give the NFS server a moment to bind + mount
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Record as active (no Child to track — it's an in-process task)
        {
            // Use a dummy Child (PID 0) since we're in-process now.
            // The active_mounts map is only used for "already mounted" checks.
            let mut mounts = self.active_mounts.lock().await;
            // Only insert if not already there (the spawned task may have failed fast)
            mounts.entry(req.mountpoint.clone()).or_insert_with(|| {
                // Dummy process — `cat` exits immediately, we just need a Child value
                tokio::process::Command::new("sleep")
                    .arg("infinity")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn sleep sentinel")
            });
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "mount")?;
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "push")?;
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
                None,
            )
            .await
        };

        match result {
            Ok(upload) => {
                // Record conflict in state cache if detected
                if let Some(tcfs_sync::conflict::SyncOutcome::Conflict(ref info)) = upload.outcome {
                    tracing::warn!(
                        path = %path,
                        local_device = %info.local_device,
                        remote_device = %info.remote_device,
                        "push: conflict detected"
                    );
                    let mut cache = state_cache.lock().await;
                    if let Some(entry) = cache.get(&local_path).cloned() {
                        let updated = tcfs_sync::state::SyncState {
                            conflict: Some(info.clone()),
                            ..entry
                        };
                        cache.set(&local_path, updated);
                        let _ = cache.flush();
                    }
                }

                // Publish state event if NATS is connected and file was actually uploaded
                if !upload.skipped {
                    // Read the actual vclock from state cache (keyed by temp local_path)
                    let vclock = {
                        let cache = state_cache.lock().await;
                        cache
                            .get(&local_path)
                            .map(|e| e.vclock.clone())
                            .unwrap_or_default()
                    };

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
                                vclock,
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "pull")?;
        let req = request.into_inner();

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator — check credentials"))?;
        let op = op.clone();

        let prefix = self.config.storage.bucket.clone();
        let local_path = std::path::PathBuf::from(&req.local_path);
        let state_cache = self.state_cache.clone();

        let result = {
            let mut cache = state_cache.lock().await;
            tcfs_sync::engine::download_file_with_device(
                &op,
                &req.remote_path,
                &local_path,
                &prefix,
                None,
                &self.device_id,
                Some(&mut cache),
                None,
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "pull")?;
        let req = request.into_inner();
        let stub_path = std::path::PathBuf::from(&req.stub_path);

        info!(stub = %req.stub_path, "hydrate requested");

        // Read and parse stub file
        let stub_content = std::fs::read_to_string(&stub_path)
            .map_err(|e| tonic::Status::not_found(format!("read stub: {e}")))?;
        let meta = tcfs_vfs::StubMeta::parse(&stub_content)
            .map_err(|e| tonic::Status::invalid_argument(format!("parse stub: {e}")))?;

        // Derive real file path from stub path
        let real_path = tcfs_vfs::stub_to_real_name(stub_path.as_os_str()).ok_or_else(|| {
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
                None,
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "mount")?;
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

    // ── List Files ────────────────────────────────────────────────────────

    async fn list_files(
        &self,
        request: tonic::Request<ListFilesRequest>,
    ) -> Result<tonic::Response<ListFilesResponse>, tonic::Status> {
        let req = request.into_inner();
        let prefix = req.prefix;

        let cache = self.state_cache.lock().await;
        let all = cache.all_entries();

        let files: Vec<FileEntry> = all
            .into_iter()
            .filter(|(_, state)| {
                if prefix.is_empty() {
                    return true;
                }
                // Match entries whose remote_path contains the prefix
                state.remote_path.contains(&prefix)
            })
            .map(|(key, state): (String, &tcfs_sync::state::SyncState)| {
                // Extract filename from the key (last path component)
                let filename = key.rsplit('/').next().unwrap_or(&key).to_string();
                let is_directory = state.remote_path.ends_with('/');

                FileEntry {
                    path: state.remote_path.clone(),
                    filename,
                    size: state.size,
                    last_synced: state.last_synced as i64,
                    is_directory,
                    blake3: state.blake3.clone(),
                }
            })
            .collect();

        Ok(tonic::Response::new(ListFilesResponse { files }))
    }

    // ── Resolve Conflict ──────────────────────────────────────────────────

    async fn resolve_conflict(
        &self,
        request: tonic::Request<ResolveConflictRequest>,
    ) -> Result<tonic::Response<ResolveConflictResponse>, tonic::Status> {
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "push")?;
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

        // Reload state from disk in case the CLI wrote new entries
        {
            let mut cache = self.state_cache.lock().await;
            if let Err(e) = cache.reload_from_disk() {
                tracing::warn!("failed to reload state cache: {e}");
            }
        }

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
                    encrypted_file_key: None,
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

                // Update state cache (clear conflict)
                {
                    let mut cache = self.state_cache.lock().await;
                    if let Some(entry) = cache.get(&path).cloned() {
                        let updated = tcfs_sync::state::SyncState {
                            vclock,
                            last_synced: tcfs_sync::StateEvent::now(),
                            conflict: None,
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
                        None,
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
                        None,
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
        use tracing::{debug, warn};

        let req = request.into_inner();
        if req.paths.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "at least one path is required",
            ));
        }

        let since = req.since_timestamp;
        info!(paths = ?req.paths, since, "watch requested");

        let (async_tx, async_rx) = tokio::sync::mpsc::channel(256);

        // ── Emit initial deltas from state cache (catch-up since anchor) ────
        if since > 0 {
            let cache = self.state_cache.lock().await;
            let all = cache.all_entries();
            for (path, state) in &all {
                let last = state.last_synced as i64;
                if last > since {
                    let filename = std::path::Path::new(path.as_str())
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let event = WatchEvent {
                        path: path.clone(),
                        event_type: "modified".into(),
                        timestamp: last,
                        filename,
                        size: state.size,
                        blake3: state.blake3.clone(),
                        is_directory: false,
                        device_id: state.device_id.clone(),
                    };
                    if async_tx.send(Ok(event)).await.is_err() {
                        // Client already disconnected
                        let stream = tokio_stream::wrappers::ReceiverStream::new(async_rx);
                        return Ok(tonic::Response::new(Box::pin(stream)));
                    }
                }
            }
        }

        // ── Live local filesystem events via notify ─────────────────────────
        let (sync_tx, sync_rx) = std::sync::mpsc::channel();
        let state_cache_for_notify = self.state_cache.clone();

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

        let notify_tx = async_tx.clone();
        tokio::task::spawn_blocking(move || {
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
                        let filename = event
                            .paths
                            .first()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;

                        // Enrich with state cache metadata (best-effort)
                        let (size, blake3) = {
                            let path_buf = std::path::PathBuf::from(&path);
                            let cache = state_cache_for_notify.blocking_lock();
                            cache
                                .get(&path_buf)
                                .map(|s| (s.size, s.blake3.clone()))
                                .unwrap_or((0, String::new()))
                        };

                        WatchEvent {
                            path,
                            event_type: event_type.to_string(),
                            timestamp,
                            filename,
                            size,
                            blake3,
                            is_directory: false,
                            device_id: String::new(), // local event
                        }
                    }
                    Err(e) => WatchEvent {
                        path: String::new(),
                        event_type: format!("error: {e}"),
                        timestamp: 0,
                        ..Default::default()
                    },
                };
                if notify_tx.blocking_send(Ok(event)).is_err() {
                    break; // Client disconnected
                }
            }
        });

        // ── Live remote events via NATS STATE_UPDATES ───────────────────────
        let nats_tx = async_tx;
        let nats_client = self.nats.clone();
        let device_id = self.device_id.clone();
        tokio::spawn(async move {
            let client = nats_client.lock().await;
            let Some(nats) = client.as_ref() else {
                debug!("watch: NATS not connected, skipping remote events");
                return;
            };
            match nats.state_consumer(&device_id).await {
                Ok(mut consumer) => {
                    use futures::StreamExt;
                    while let Some(msg_result) = consumer.next().await {
                        match msg_result {
                            Ok(state_msg) => {
                                let event = state_msg.event.clone();
                                // Ack before processing (at-most-once for watch events is fine)
                                let _ = state_msg.ack().await;

                                let watch_event = match event {
                                    tcfs_sync::StateEvent::FileSynced {
                                        device_id: dev,
                                        rel_path,
                                        blake3,
                                        size,
                                        timestamp,
                                        ..
                                    } => {
                                        let filename = std::path::Path::new(&rel_path)
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_default();
                                        WatchEvent {
                                            path: rel_path,
                                            event_type: "modified".into(),
                                            timestamp: timestamp as i64,
                                            filename,
                                            size,
                                            blake3,
                                            is_directory: false,
                                            device_id: dev,
                                        }
                                    }
                                    tcfs_sync::StateEvent::FileDeleted {
                                        device_id: dev,
                                        rel_path,
                                        timestamp,
                                        ..
                                    } => {
                                        let filename = std::path::Path::new(&rel_path)
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_default();
                                        WatchEvent {
                                            path: rel_path,
                                            event_type: "deleted".into(),
                                            timestamp: timestamp as i64,
                                            filename,
                                            device_id: dev,
                                            ..Default::default()
                                        }
                                    }
                                    tcfs_sync::StateEvent::FileRenamed {
                                        device_id: dev,
                                        new_path,
                                        timestamp,
                                        ..
                                    } => {
                                        let filename = std::path::Path::new(&new_path)
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_default();
                                        WatchEvent {
                                            path: new_path,
                                            event_type: "renamed".into(),
                                            timestamp: timestamp as i64,
                                            filename,
                                            device_id: dev,
                                            ..Default::default()
                                        }
                                    }
                                    _ => continue, // Skip DeviceOnline/Offline etc
                                };
                                if nats_tx.send(Ok(watch_event)).await.is_err() {
                                    break; // Client disconnected
                                }
                            }
                            Err(e) => {
                                warn!("watch: NATS state consumer error: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("watch: failed to create NATS state consumer: {e}");
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(async_rx);
        Ok(tonic::Response::new(Box::pin(stream)))
    }

    // ── Auth (encryption key management) ────────────────────────────────

    async fn auth_unlock(
        &self,
        request: tonic::Request<AuthUnlockRequest>,
    ) -> Result<tonic::Response<AuthUnlockResponse>, tonic::Status> {
        let req = request.into_inner();

        if req.master_key.len() != tcfs_crypto::KEY_SIZE {
            return Ok(tonic::Response::new(AuthUnlockResponse {
                success: false,
                error: format!(
                    "master key must be {} bytes, got {}",
                    tcfs_crypto::KEY_SIZE,
                    req.master_key.len()
                ),
            }));
        }

        let mut key_bytes = [0u8; tcfs_crypto::KEY_SIZE];
        key_bytes.copy_from_slice(&req.master_key);
        let master_key = tcfs_crypto::MasterKey::from_bytes(key_bytes);

        let mut guard = self.master_key.lock().await;
        *guard = Some(master_key);

        info!("encryption unlocked via gRPC");
        Ok(tonic::Response::new(AuthUnlockResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn auth_lock(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<AuthLockResponse>, tonic::Status> {
        let mut guard = self.master_key.lock().await;
        let was_unlocked = guard.is_some();
        *guard = None;

        if was_unlocked {
            info!("encryption locked via gRPC");
        }

        Ok(tonic::Response::new(AuthLockResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn auth_status(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<AuthStatusResponse>, tonic::Status> {
        use tcfs_auth::AuthProvider;
        let guard = self.master_key.lock().await;

        // Build available methods dynamically
        let mut methods = vec!["master_key".into()];
        if self.totp_provider.is_available() {
            methods.push("totp".into());
        }
        if self.webauthn_provider.is_available() {
            methods.push("webauthn".into());
        }

        // Check active session count
        self.session_store.cleanup_expired().await;
        let active_sessions = self.session_store.active_count().await;

        Ok(tonic::Response::new(AuthStatusResponse {
            unlocked: guard.is_some(),
            crypto_enabled: self.config.crypto.enabled,
            session_device_id: self.device_id.clone(),
            auth_method: if active_sessions > 0 {
                "session".into()
            } else if guard.is_some() {
                "master_key".into()
            } else {
                String::new()
            },
            available_methods: methods,
        }))
    }

    // ── MFA Enrollment ───────────────────────────────────────────────────

    async fn auth_enroll(
        &self,
        request: tonic::Request<AuthEnrollRequest>,
    ) -> Result<tonic::Response<AuthEnrollResponse>, tonic::Status> {
        use tcfs_auth::AuthProvider;

        let req = request.into_inner();
        info!(device_id = %req.device_id, method = %req.method, "auth enroll requested");

        match req.method.as_str() {
            "totp" => match self.totp_provider.register(&req.device_id).await {
                Ok(reg) => {
                    // Persist TOTP credentials to data dir
                    let cred_path = std::path::PathBuf::from(&format!(
                        "{}/tcfsd/totp-credentials.json",
                        dirs::data_dir().unwrap_or_default().display()
                    ));
                    if let Err(e) = self.totp_provider.save_to_file(&cred_path).await {
                        tracing::warn!("failed to persist TOTP credentials: {e}");
                    }
                    Ok(tonic::Response::new(AuthEnrollResponse {
                        success: true,
                        registration_data: reg.data,
                        instructions: reg.instructions,
                        error: String::new(),
                    }))
                }
                Err(e) => Ok(tonic::Response::new(AuthEnrollResponse {
                    success: false,
                    registration_data: Vec::new(),
                    instructions: String::new(),
                    error: format!("TOTP enrollment failed: {e}"),
                })),
            },
            "webauthn" => match self.webauthn_provider.register(&req.device_id).await {
                Ok(reg) => {
                    // Persist WebAuthn credentials
                    let cred_path = std::path::PathBuf::from(&format!(
                        "{}/tcfsd/webauthn-credentials.json",
                        dirs::data_dir().unwrap_or_default().display()
                    ));
                    if let Err(e) = self.webauthn_provider.save_to_file(&cred_path).await {
                        tracing::warn!("failed to persist WebAuthn credentials: {e}");
                    }
                    Ok(tonic::Response::new(AuthEnrollResponse {
                        success: true,
                        registration_data: reg.data,
                        instructions: reg.instructions,
                        error: String::new(),
                    }))
                }
                Err(e) => Ok(tonic::Response::new(AuthEnrollResponse {
                    success: false,
                    registration_data: Vec::new(),
                    instructions: String::new(),
                    error: format!("WebAuthn enrollment failed: {e}"),
                })),
            },
            other => Ok(tonic::Response::new(AuthEnrollResponse {
                success: false,
                registration_data: Vec::new(),
                instructions: String::new(),
                error: format!("unsupported auth method: {other}"),
            })),
        }
    }

    async fn auth_complete_enroll(
        &self,
        request: tonic::Request<AuthCompleteEnrollRequest>,
    ) -> Result<tonic::Response<AuthCompleteEnrollResponse>, tonic::Status> {
        let req = request.into_inner();
        info!(device_id = %req.device_id, method = %req.method, "auth complete enroll requested");

        match req.method.as_str() {
            "webauthn" => {
                match self
                    .webauthn_provider
                    .complete_registration_from_bytes(&req.device_id, &req.attestation_data)
                    .await
                {
                    Ok(()) => {
                        // Persist updated credentials
                        let cred_path = std::path::PathBuf::from(&format!(
                            "{}/tcfsd/webauthn-credentials.json",
                            dirs::data_dir().unwrap_or_default().display()
                        ));
                        if let Err(e) = self.webauthn_provider.save_to_file(&cred_path).await {
                            tracing::warn!("failed to persist WebAuthn credentials: {e}");
                        }
                        Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                            success: true,
                            error: String::new(),
                        }))
                    }
                    Err(e) => Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                        success: false,
                        error: format!("registration completion failed: {e}"),
                    })),
                }
            }
            "totp" => {
                // TOTP doesn't have a second step — enroll + first verify completes it
                Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                    success: true,
                    error: String::new(),
                }))
            }
            other => Ok(tonic::Response::new(AuthCompleteEnrollResponse {
                success: false,
                error: format!("unsupported method for complete_enroll: {other}"),
            })),
        }
    }

    async fn auth_challenge(
        &self,
        request: tonic::Request<AuthChallengeRequest>,
    ) -> Result<tonic::Response<AuthChallengeResponse>, tonic::Status> {
        let req = request.into_inner();
        info!(device_id = %req.device_id, method = %req.method, "auth challenge requested");

        let provider: &dyn tcfs_auth::AuthProvider = match req.method.as_str() {
            "totp" => self.totp_provider.as_ref(),
            "webauthn" => self.webauthn_provider.as_ref(),
            other => {
                return Err(tonic::Status::invalid_argument(format!(
                    "unsupported auth method: {other}"
                )));
            }
        };

        match provider.challenge(&req.device_id).await {
            Ok(challenge) => Ok(tonic::Response::new(AuthChallengeResponse {
                challenge_id: challenge.challenge_id,
                data: challenge.data,
                prompt: challenge.prompt,
                expires_at: challenge.expires_at,
            })),
            Err(e) => Err(tonic::Status::failed_precondition(format!(
                "{} challenge failed: {e}",
                req.method
            ))),
        }
    }

    async fn auth_verify(
        &self,
        request: tonic::Request<AuthVerifyRequest>,
    ) -> Result<tonic::Response<AuthVerifyResponse>, tonic::Status> {
        use tcfs_auth::AuthProvider;

        let req = request.into_inner();
        info!(device_id = %req.device_id, "auth verify requested");

        // Rate limit check — reject early if device is locked out
        if let Some(remaining_secs) = self.rate_limiter.check(&req.device_id).await {
            tracing::warn!(device_id = %req.device_id, remaining_secs, "auth attempt rejected: rate limited");
            return Ok(tonic::Response::new(AuthVerifyResponse {
                success: false,
                session_token: String::new(),
                error: format!("too many failed attempts, try again in {remaining_secs}s"),
            }));
        }

        let response = tcfs_auth::AuthResponse {
            challenge_id: req.challenge_id,
            data: req.data,
            device_id: req.device_id.clone(),
        };

        // Try TOTP first, then WebAuthn (method is implicit in the response data)
        let verify_result = match self.totp_provider.verify(&response).await {
            Ok(r @ tcfs_auth::VerifyResult::Success { .. }) => Ok(r),
            _ => self.webauthn_provider.verify(&response).await,
        };

        match verify_result {
            Ok(tcfs_auth::VerifyResult::Success {
                session_token: _,
                device_id,
            }) => {
                // Create and store session
                let auth_method = if self
                    .totp_provider
                    .verify(&tcfs_auth::AuthResponse {
                        challenge_id: String::new(),
                        data: response.data.clone(),
                        device_id: device_id.clone(),
                    })
                    .await
                    .is_ok_and(|r| matches!(r, tcfs_auth::VerifyResult::Success { .. }))
                {
                    "totp"
                } else {
                    "webauthn"
                };
                let session = tcfs_auth::Session::new(&device_id, &device_id, auth_method)
                    .with_expiry(self.config.auth.session_expiry_hours);
                let token = session.token.clone();
                self.session_store.insert(session).await;
                self.persist_sessions().await;
                self.rate_limiter.clear(&device_id).await;

                info!(device_id = %device_id, method = auth_method, "auth succeeded, session created");
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: true,
                    session_token: token,
                    error: String::new(),
                }))
            }
            Ok(tcfs_auth::VerifyResult::Failure { reason }) => {
                self.rate_limiter.record_failure(&req.device_id).await;
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: false,
                    session_token: String::new(),
                    error: reason,
                }))
            }
            Ok(tcfs_auth::VerifyResult::Expired) => {
                self.rate_limiter.record_failure(&req.device_id).await;
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: false,
                    session_token: String::new(),
                    error: "challenge expired".into(),
                }))
            }
            Err(e) => {
                self.rate_limiter.record_failure(&req.device_id).await;
                Ok(tonic::Response::new(AuthVerifyResponse {
                    success: false,
                    session_token: String::new(),
                    error: format!("verification error: {e}"),
                }))
            }
        }
    }

    // ── Session Revocation ────────────────────────────────────────────────

    async fn auth_revoke(
        &self,
        request: tonic::Request<AuthRevokeRequest>,
    ) -> Result<tonic::Response<AuthRevokeResponse>, tonic::Status> {
        let req = request.into_inner();

        if !req.session_token.is_empty() {
            // Revoke specific session by token
            info!(
                token_prefix = &req.session_token[..8.min(req.session_token.len())],
                "revoking session by token"
            );
            self.session_store.revoke(&req.session_token).await;
        } else if !req.device_id.is_empty() {
            // Revoke all sessions for device
            info!(device_id = %req.device_id, "revoking all sessions for device");
            self.session_store.revoke_device(&req.device_id).await;
        } else {
            return Ok(tonic::Response::new(AuthRevokeResponse {
                success: false,
                error: "must specify session_token or device_id".into(),
            }));
        }

        self.persist_sessions().await;
        Ok(tonic::Response::new(AuthRevokeResponse {
            success: true,
            error: String::new(),
        }))
    }

    // ── Device Enrollment ────────────────────────────────────────────────

    async fn device_enroll(
        &self,
        request: tonic::Request<DeviceEnrollRequest>,
    ) -> Result<tonic::Response<DeviceEnrollResponse>, tonic::Status> {
        let req = request.into_inner();
        info!(device_name = %req.device_name, platform = %req.platform, "device enroll requested");

        // Decode and validate the enrollment invite
        let invite = tcfs_auth::EnrollmentInvite::decode(&req.invite_data)
            .map_err(|e| tonic::Status::invalid_argument(format!("invalid invite: {e}")))?;

        if invite.is_expired() {
            return Ok(tonic::Response::new(DeviceEnrollResponse {
                success: false,
                error: "invite has expired".into(),
                ..Default::default()
            }));
        }

        // Validate invite signature against master key
        if let Some(mk) = self.master_key.lock().await.as_ref() {
            let signing_key: [u8; 32] = *blake3::hash(mk.as_bytes()).as_bytes();
            if !invite.verify_signature(&signing_key) {
                return Ok(tonic::Response::new(DeviceEnrollResponse {
                    success: false,
                    error: "invalid invite signature".into(),
                    ..Default::default()
                }));
            }
        } else {
            // No master key loaded — cannot verify invite
            return Err(tonic::Status::failed_precondition(
                "daemon master key not loaded — cannot verify invite signature",
            ));
        }

        // Enroll device in the local registry
        let device_id = uuid::Uuid::new_v4().to_string();
        info!(
            device_id = %device_id,
            device_name = %req.device_name,
            platform = %req.platform,
            invited_by = %invite.created_by,
            "device enrolled via invite"
        );

        Ok(tonic::Response::new(DeviceEnrollResponse {
            success: true,
            device_id,
            nats_url: invite.nats_url.unwrap_or_default(),
            storage_endpoint: invite.storage_endpoint.unwrap_or_default(),
            available_auth_methods: vec!["totp".into()],
            error: String::new(),
            storage_bucket: invite.storage_bucket.unwrap_or_default(),
            storage_access_key: invite.storage_access_key.unwrap_or_default(),
            storage_secret: invite.storage_secret_key.unwrap_or_default(),
            remote_prefix: invite.remote_prefix.unwrap_or_default(),
            encryption_passphrase: invite.encryption_passphrase.unwrap_or_default(),
            encryption_salt: invite.encryption_salt.unwrap_or_default(),
        }))
    }
}

/// Bind a Unix domain socket, removing any stale socket and creating parent dirs.
async fn bind_uds(socket_path: &Path) -> Result<UnixListenerStream> {
    if socket_path.exists() {
        tokio::fs::remove_file(socket_path).await?;
    }
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let listener = UnixListener::bind(socket_path)?;
    Ok(UnixListenerStream::new(listener))
}

/// Start the gRPC server on a Unix domain socket with graceful shutdown support.
///
/// If `fileprovider_socket` is provided, a second server is spawned on that socket
/// for sandboxed macOS FileProvider access (App Group container).
pub async fn serve(
    socket_path: &Path,
    fileprovider_socket: Option<&Path>,
    impl_: TcfsDaemonImpl,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let primary = bind_uds(socket_path).await?;
    info!(socket = %socket_path.display(), "gRPC server ready");

    let service = TcfsDaemonServer::new(impl_);

    // Spawn a second gRPC server on the FileProvider socket if configured.
    // Uses a separate tokio task with a shared shutdown notify.
    let fp_handle = if let Some(fp_path) = fileprovider_socket {
        let secondary = bind_uds(fp_path).await?;
        info!(socket = %fp_path.display(), "gRPC FileProvider socket ready");

        let fp_service = service.clone();
        let fp_shutdown = Arc::new(tokio::sync::Notify::new());
        let fp_shutdown_clone = fp_shutdown.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = Server::builder()
                .add_service(fp_service)
                .serve_with_incoming_shutdown(secondary, fp_shutdown_clone.notified())
                .await
            {
                tracing::warn!("FileProvider gRPC server error: {e}");
            }
        });

        Some((handle, fp_shutdown))
    } else {
        None
    };

    let result = Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(primary, shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"));

    // Stop the FileProvider server when the primary shuts down
    if let Some((handle, notify)) = fp_handle {
        notify.notify_one();
        let _ = handle.await;
    }

    result
}
