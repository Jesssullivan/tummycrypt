//! tonic gRPC server over Unix domain socket and optional TCP

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::Mutex as TokioMutex;
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::transport::Server;
use tracing::{info, warn};

use crate::cred_store::SharedCredStore;

use base64::Engine;
use secrecy::ExposeSecret;
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
    path_locks: tcfs_sync::state::PathLocks,
    data_dir: std::path::PathBuf,
    /// VFS handle from active FUSE mount — used to invalidate negative cache
    /// on NATS events so remote files appear in readdir immediately.
    pub vfs_handle: tokio::sync::watch::Receiver<Option<std::sync::Arc<tcfs_vfs::TcfsVfs>>>,
    vfs_tx: tokio::sync::watch::Sender<Option<std::sync::Arc<tcfs_vfs::TcfsVfs>>>,
    // Auth infrastructure
    session_store: tcfs_auth::SessionStore,
    invite_redemptions: tcfs_auth::InviteRedemptionStore,
    totp_provider: Arc<tcfs_auth::totp::TotpProvider>,
    webauthn_provider: Arc<tcfs_auth::webauthn::WebAuthnProvider>,
    rate_limiter: tcfs_auth::RateLimiter,
}

/// Validate a client-provided relative path before joining it under a tempdir.
fn sanitize_rel_path(path: &str) -> std::result::Result<String, String> {
    use std::path::Component;

    if path.is_empty() {
        return Err("path must not be empty".to_string());
    }

    let rel_path = Path::new(path);
    if rel_path.is_absolute() {
        return Err(format!("absolute path not allowed: {path}"));
    }

    for component in rel_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(format!("path traversal not allowed: {path}"));
        }
    }

    Ok(path.to_string())
}

fn logical_rel_path_from_state_key(
    key: &str,
    state: &tcfs_sync::state::SyncState,
    sync_root: Option<&Path>,
    storage_prefix: &str,
) -> Option<String> {
    if let Some(root) = sync_root {
        let root = root.to_string_lossy();
        let root = root.trim_end_matches('/');
        if !root.is_empty() {
            let root_prefix = format!("{root}/");
            if let Some(rel) = key.strip_prefix(&root_prefix) {
                let rel = rel.trim_start_matches('/');
                if !rel.is_empty() {
                    return Some(rel.to_string());
                }
            }
        }
    }

    let index_prefix = format!("{}/index/", storage_prefix.trim_end_matches('/'));
    state
        .remote_path
        .strip_prefix(&index_prefix)
        .map(|rel| rel.trim_start_matches('/'))
        .filter(|rel| !rel.is_empty())
        .map(ToOwned::to_owned)
}

fn logical_rel_path_from_fs_path(path: &Path, sync_root: Option<&Path>) -> String {
    if let Some(root) = sync_root {
        if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel.to_string_lossy();
            let rel = rel.trim_start_matches('/');
            return rel.to_string();
        }
    }

    path.to_string_lossy().to_string()
}

fn normalize_watch_root(path: &str) -> String {
    path.trim_matches('/').to_string()
}

fn rel_path_matches_watch_roots(rel_path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        if root.is_empty() {
            true
        } else {
            rel_path == root
                || rel_path
                    .strip_prefix(root)
                    .is_some_and(|r| r.starts_with('/'))
        }
    })
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
        path_locks: tcfs_sync::state::PathLocks,
        device_id: String,
        device_name: String,
        master_key: Option<tcfs_crypto::MasterKey>,
    ) -> Self {
        let (vfs_tx, vfs_rx) = tokio::sync::watch::channel(None);

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

        let data_dir = dirs::data_dir().unwrap_or_default().join("tcfsd");

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
            data_dir,
            master_key: Arc::new(TokioMutex::new(master_key)),
            nats_ok: std::sync::atomic::AtomicBool::new(false),
            nats: Arc::new(TokioMutex::new(None)),
            active_mounts: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            path_locks,
            vfs_handle: vfs_rx,
            vfs_tx,
            session_store: tcfs_auth::SessionStore::new(),
            invite_redemptions: tcfs_auth::InviteRedemptionStore::new(),
            totp_provider,
            webauthn_provider,
            rate_limiter,
        }
    }

    async fn enrollment_bootstrap_for_invite(
        &self,
        invite: &tcfs_auth::EnrollmentInvite,
    ) -> Result<tcfs_auth::EnrollmentBootstrap, tonic::Status> {
        let (storage_access_key, storage_secret_key) =
            self.enrollment_storage_credentials(invite).await;
        if storage_access_key.is_none() || storage_secret_key.is_none() {
            return Err(tonic::Status::failed_precondition(
                "storage credentials unavailable for enrollment bootstrap",
            ));
        }

        let master_key_base64 = {
            let master = self.master_key.lock().await;
            let master = master.as_ref().ok_or_else(|| {
                tonic::Status::failed_precondition(
                    "daemon master key not loaded — cannot wrap enrollment bootstrap",
                )
            })?;
            base64::engine::general_purpose::STANDARD.encode(master.as_bytes())
        };

        Ok(tcfs_auth::EnrollmentBootstrap {
            nats_url: invite
                .nats_url
                .clone()
                .or_else(|| Some(self.config.sync.nats_url.clone()).filter(|url| !url.is_empty())),
            storage_endpoint: invite.storage_endpoint.clone().or_else(|| {
                Some(self.config.storage.endpoint.clone()).filter(|url| !url.is_empty())
            }),
            storage_bucket: invite.storage_bucket.clone().or_else(|| {
                Some(self.config.storage.bucket.clone()).filter(|bucket| !bucket.is_empty())
            }),
            storage_access_key,
            storage_secret_key,
            remote_prefix: invite
                .remote_prefix
                .clone()
                .or_else(|| Some(self.config.storage.resolved_prefix().to_string())),
            master_key_base64: Some(master_key_base64),
            encryption_salt: invite
                .encryption_salt
                .clone()
                .or_else(|| self.config.crypto.kdf_salt.clone()),
        })
    }

    async fn enrollment_storage_credentials(
        &self,
        invite: &tcfs_auth::EnrollmentInvite,
    ) -> (Option<String>, Option<String>) {
        if invite.storage_access_key.is_some() && invite.storage_secret_key.is_some() {
            return (
                invite.storage_access_key.clone(),
                invite.storage_secret_key.clone(),
            );
        }

        let store = self.cred_store.read().await;
        if let Some(s3) = store.as_ref().and_then(|store| store.s3.as_ref()) {
            return (
                Some(s3.access_key_id.clone()),
                Some(s3.secret_access_key.expose_secret().to_string()),
            );
        }

        (
            invite.storage_access_key.clone(),
            invite.storage_secret_key.clone(),
        )
    }

    /// Get a clone of the session store (for background tasks).
    pub fn session_store(&self) -> tcfs_auth::SessionStore {
        self.session_store.clone()
    }

    #[cfg(test)]
    fn with_data_dir(mut self, data_dir: std::path::PathBuf) -> Self {
        self.data_dir = data_dir;
        self
    }

    /// Load persisted TOTP credentials from disk.
    pub async fn load_totp_credentials(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.totp_provider.load_from_file(path).await
    }

    /// Load persisted sessions from disk.
    pub async fn load_sessions(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.session_store.load_from_file(path).await
    }

    /// Load persisted invite redemptions from disk.
    pub async fn load_invite_redemptions(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.invite_redemptions.load_from_file(path).await
    }

    /// Save sessions to disk (called after session changes).
    async fn persist_sessions(&self) {
        let path = self.data_dir.join("sessions.json");
        if let Err(e) = self.session_store.save_to_file(&path).await {
            tracing::warn!("failed to persist sessions: {e}");
        }
    }

    /// Save invite redemptions to disk (called after successful invite use).
    async fn persist_invite_redemptions(&self) -> anyhow::Result<()> {
        let path = self.data_dir.join("invite-redemptions.json");
        self.invite_redemptions.save_to_file(&path).await
    }

    /// Validate a session token from gRPC request metadata.
    ///
    /// Returns Ok(Session) if the session is valid, or a gRPC UNAUTHENTICATED
    /// error if auth is required and the token is missing/invalid/expired.
    ///
    /// When `config.auth.require_session` is false, this returns a synthetic
    /// session with full permissions (bypass mode). A warning is logged on
    /// each bypassed request.
    async fn require_session<T>(
        &self,
        request: &tonic::Request<T>,
    ) -> Result<tcfs_auth::Session, tonic::Status> {
        if !self.config.auth.require_session {
            tracing::warn!(
                "AUTH BYPASS: request granted full permissions — \
                 set auth.require_session=true for production"
            );
            return Ok(tcfs_auth::Session::new(&self.device_id, "local", "bypass")
                .with_permissions(tcfs_auth::DevicePermissions::admin()));
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

    /// Get a handle to the master key for background tasks (e.g., periodic reconciliation).
    pub fn master_key_handle(&self) -> Arc<TokioMutex<Option<tcfs_crypto::MasterKey>>> {
        self.master_key.clone()
    }

    fn lock_path_for_request(&self, path: &Path) -> std::path::PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        if let Some(root) = self.config.sync.sync_root.as_deref() {
            return root.join(path);
        }
        path.to_path_buf()
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
        let storage_prefix = self.config.storage.resolved_prefix().to_string();
        let operator = self.operator.lock().await.as_ref().cloned();
        let storage_ok = match operator {
            Some(op) => {
                match tcfs_storage::check_health_for_prefix_detailed(&op, &storage_prefix).await {
                    Ok(report) => {
                        tracing::debug!(
                            health_path = %report.path,
                            elapsed_ms = report.elapsed_ms,
                            entry_count = report.entry_count,
                            "status storage health probe passed"
                        );
                        true
                    }
                    Err(err) => {
                        warn!(
                            health_kind = %err.kind(),
                            health_path = %err.path(),
                            elapsed_ms = err.elapsed_ms(),
                            backend_kind = err.backend_kind().unwrap_or("none"),
                            "status storage health probe failed: {err}"
                        );
                        false
                    }
                }
            }
            None => false,
        };

        Ok(tonic::Response::new(StatusResponse {
            version: env!("CARGO_PKG_VERSION").into(),
            storage_endpoint: self.storage_endpoint.clone(),
            storage_ok,
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
        let cache_max = self.config.fuse.cache_max_mb * 1024 * 1024;
        let neg_ttl = self.config.fuse.negative_cache_ttl_secs;
        let mountpoint_key = req.mountpoint.clone();
        let active_mounts_watcher = self.active_mounts.clone();

        if use_nfs {
            // NFS loopback (fallback — use --nfs flag or "nfs" option)
            let mount_handle = tokio::spawn(async move {
                tracing::info!("NFS mount task starting (prefix={prefix})");
                match tcfs_nfs::serve_and_mount(tcfs_nfs::NfsMountConfig {
                    op,
                    prefix,
                    mountpoint: mp,
                    port: 0,
                    cache_dir: std::path::PathBuf::from(&cache_dir),
                    cache_max_bytes: cache_max,
                    negative_ttl_secs: neg_ttl,
                })
                .await
                {
                    Ok(()) => tracing::warn!("NFS serve_and_mount returned Ok"),
                    Err(e) => tracing::error!(error = %e, "NFS mount failed"),
                }
            });

            let mk = mountpoint_key.clone();
            tokio::spawn(async move {
                let _ = mount_handle.await;
                active_mounts_watcher.lock().await.remove(&mk);
            });
        } else {
            // FUSE3 (default — unprivileged mount via fusermount3)
            //
            // Wire NATS publish callback: when a file is flushed to S3 via
            // FUSE write, notify other hosts so their FUSE mounts pick it up.
            let nats_handle = self.nats.clone();
            let flush_device_id = self.device_id.clone();
            let mount_device_id = self.device_id.clone();
            let flush_prefix = prefix.clone();
            let mount_master_key = self.master_key.clone();
            let on_flush: Option<tcfs_vfs::OnFlushCallback> = Some(std::sync::Arc::new(
                move |vpath: &str,
                      hash: &str,
                      size: u64,
                      _chunks: usize,
                      vclock: &tcfs_sync::conflict::VectorClock| {
                    let nats = nats_handle.clone();
                    let device = flush_device_id.clone();
                    // Strip leading '/' from FUSE virtual path to produce a
                    // relative path matching the S3 index key format. Without
                    // this, receiving hosts get `/file.txt` instead of `file.txt`
                    // and sync_root.join() produces malformed local paths.
                    let path = vpath.trim_start_matches('/').to_string();
                    let hash = hash.to_string();
                    let vclock = vclock.clone();
                    let pfx = flush_prefix.clone();
                    tokio::spawn(async move {
                        if let Some(ref client) = *nats.lock().await {
                            let event = tcfs_sync::StateEvent::FileSynced {
                                device_id: device,
                                rel_path: path.clone(),
                                blake3: hash.clone(),
                                size,
                                vclock,
                                manifest_path: format!("{}/manifests/{}", pfx, hash),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            };
                            if let Err(e) = client.publish_state_event(&event).await {
                                tracing::warn!(path = %path, "NATS publish on FUSE flush failed: {e}");
                            } else {
                                tracing::debug!(path = %path, "NATS FileSynced published from FUSE write");
                            }
                        }
                    });
                },
            ));

            let vfs_sender = self.vfs_tx.clone();
            let mount_handle = tokio::spawn(async move {
                tracing::info!("FUSE mount task starting (prefix={prefix})");
                match tcfs_fuse::mount(
                    tcfs_fuse::MountConfig {
                        op,
                        prefix,
                        mountpoint: mp,
                        cache_dir: std::path::PathBuf::from(&cache_dir),
                        cache_max_bytes: cache_max,
                        negative_ttl_secs: neg_ttl,
                        read_only: req.read_only,
                        allow_other: false,
                        on_flush,
                        device_id: mount_device_id,
                        master_key: Some(mount_master_key),
                    },
                    Some(&vfs_sender),
                )
                .await
                {
                    Ok(()) => tracing::info!("FUSE mount unmounted cleanly"),
                    Err(e) => tracing::error!(error = %e, "FUSE mount failed"),
                }
                // Clear VFS handle on unmount
                let _ = vfs_sender.send(None);
            });

            let mk = mountpoint_key.clone();
            tokio::spawn(async move {
                let _ = mount_handle.await;
                active_mounts_watcher.lock().await.remove(&mk);
            });
        }

        // Give the mount a moment to establish
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Record as active
        {
            let mut mounts = self.active_mounts.lock().await;
            mounts.entry(req.mountpoint.clone()).or_insert_with(|| {
                tokio::process::Command::new("sleep")
                    .arg("infinity")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn sentinel")
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
        let prefix = self.config.storage.resolved_prefix().to_string();

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

        let path = sanitize_rel_path(&path).map_err(tonic::Status::invalid_argument)?;
        let lock_path = self.lock_path_for_request(Path::new(&path));
        let _lock_guard = self.path_locks.lock(&lock_path).await;

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

        // Normalize the path for consistent S3 index keys (matches pull's resolve_manifest_path)
        let sync_root = self.config.sync.sync_root.as_deref();
        let normalized_rel =
            tcfs_sync::engine::normalize_rel_path(std::path::Path::new(&path), sync_root);

        let result = {
            let mut cache = state_cache.lock().await;
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| tcfs_sync::engine::EncryptionContext::new(mk.clone()));
            tcfs_sync::engine::upload_file_with_device(
                &op,
                &local_path,
                &prefix,
                &mut cache,
                None,
                &device_id,
                Some(&normalized_rel),
                enc_ctx.as_ref(),
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
                    if cache.mark_conflict(&local_path, info.clone()) {
                        let _ = cache.flush();
                    }
                }

                // Rel-path publication is owned by upload_file_with_device so
                // the daemon follows the same crash-aware manifest/index flow
                // as CLI and tree push.

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
        let req_inner = request.get_ref();
        info!(
            remote = %req_inner.remote_path,
            local = %req_inner.local_path,
            "pull requested"
        );
        Self::check_permission(&session, "pull")?;
        let req = request.into_inner();

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator — check credentials"))?;
        let op = op.clone();

        let prefix = self.config.storage.resolved_prefix().to_string();
        let local_path = std::path::PathBuf::from(&req.local_path);
        let state_cache = self.state_cache.clone();

        let sync_root = self.config.sync.sync_root.as_deref();
        let _lock_guard = self.path_locks.lock(&local_path).await;

        let resolved_manifest =
            tcfs_sync::engine::resolve_manifest_path(&op, &req.remote_path, &prefix, sync_root)
                .await
                .map_err(|e| tonic::Status::not_found(format!("resolve manifest: {e}")))?;

        let result = {
            let mut cache = state_cache.lock().await;
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| tcfs_sync::engine::EncryptionContext::new(mk.clone()));
            tcfs_sync::engine::download_file_with_device(
                &op,
                &resolved_manifest,
                &local_path,
                &prefix,
                None,
                &self.device_id,
                Some(&mut cache),
                enc_ctx.as_ref(),
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
                warn!(error = %e, "pull download failed");
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
        let prefix = self.config.storage.resolved_prefix().to_string();
        let manifest_path = format!("{prefix}/manifests/{blake3_hex}");

        let op = self.operator.lock().await;
        let op = op
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("no storage operator"))?;
        let op = op.clone();
        drop(self.operator.lock().await);

        let total_bytes = meta.size;
        let _lock_guard = self.path_locks.lock(&real_path).await;

        let result = {
            let mut cache = self.state_cache.lock().await;
            let mk_guard = self.master_key.lock().await;
            let enc_ctx = mk_guard
                .as_ref()
                .map(|mk| tcfs_sync::engine::EncryptionContext::new(mk.clone()));
            tcfs_sync::engine::download_file_with_device(
                &op,
                &manifest_path,
                &real_path,
                &prefix,
                None,
                &self.device_id,
                Some(&mut cache),
                enc_ctx.as_ref(),
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
        let _lock_guard = self.path_locks.lock(&path).await;

        info!(path = %req.path, force = req.force, "unsync requested");

        let mut cache = self.state_cache.lock().await;
        if cache.get(&path).is_none() {
            return Ok(tonic::Response::new(UnsyncResponse {
                success: false,
                stub_path: String::new(),
                error: format!("path not in sync state: {}", req.path),
            }));
        }

        // Dirty-child safety check: if this is a directory, verify no children
        // have unsynced local modifications before removing from state cache.
        if path.is_dir() && !req.force {
            let children = cache.children_with_prefix(&path);
            let mut dirty_paths = Vec::new();
            for (child_key, _child_state) in &children {
                let child_path = std::path::Path::new(child_key);
                if child_path.exists() {
                    if let Ok(Some(reason)) = cache.needs_sync(child_path) {
                        dirty_paths.push(format!("{}: {reason}", child_key));
                    }
                }
            }
            if !dirty_paths.is_empty() {
                let count = dirty_paths.len();
                let detail = dirty_paths
                    .into_iter()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Ok(tonic::Response::new(UnsyncResponse {
                    success: false,
                    stub_path: String::new(),
                    error: format!(
                        "{count} dirty child(ren) with unsynced changes (use force=true to override): {detail}"
                    ),
                }));
            }

            // Transition children to NotSynced (preserve metadata for re-hydration)
            let child_keys: Vec<String> = children.into_iter().map(|(k, _)| k).collect();
            for child_key in &child_keys {
                let child_path = std::path::PathBuf::from(child_key);
                cache.set_status(&child_path, tcfs_sync::state::FileSyncStatus::NotSynced);
            }
        }

        // Transition to NotSynced instead of removing — preserves metadata for re-hydration
        cache.set_status(&path, tcfs_sync::state::FileSyncStatus::NotSynced);
        if let Err(e) = cache.flush() {
            return Ok(tonic::Response::new(UnsyncResponse {
                success: false,
                stub_path: String::new(),
                error: format!("state cache flush failed: {e}"),
            }));
        }

        // Evict from VFS disk cache if VFS is active
        // Clone the Arc out of the watch::Ref to avoid holding a !Send borrow across .await
        let mut bytes_freed = 0u64;
        let vfs_opt = self.vfs_handle.borrow().clone();
        if let Some(ref vfs) = vfs_opt {
            match vfs.unsync_path(&req.path).await {
                Ok(result) => {
                    info!(
                        path = %req.path,
                        bytes_freed = result.bytes_freed,
                        was_cached = result.was_cached,
                        "unsync: VFS cache evicted"
                    );
                    bytes_freed = result.bytes_freed;
                }
                Err(e) => {
                    warn!(path = %req.path, error = %e, "unsync: VFS cache eviction failed (non-fatal)");
                }
            }
        }

        info!(path = %req.path, bytes_freed, "unsynced successfully");

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
            Some(entry) => {
                let state = if entry.status == tcfs_sync::state::FileSyncStatus::Synced {
                    match cache.needs_sync(&path) {
                        Ok(Some(_)) => "pending".to_string(),
                        Ok(None) => entry.status.to_string(),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %path.display(),
                                "needs_sync failed during sync_status; reporting unknown"
                            );
                            "unknown".to_string()
                        }
                    }
                } else {
                    entry.status.to_string()
                };

                Ok(tonic::Response::new(SyncStatusResponse {
                    path: req.path,
                    state,
                    blake3: entry.blake3.clone(),
                    size: entry.size,
                    last_synced: entry.last_synced as i64,
                }))
            }
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
        let prefix = req.prefix; // logical directory prefix (e.g., "dotfiles/" or "")

        let cache = self.state_cache.lock().await;
        let all = cache.all_entries();

        let mut dirs_seen = std::collections::HashSet::new();
        let mut files: Vec<FileEntry> = Vec::new();

        let storage_prefix = self.config.storage.resolved_prefix();

        for (key, state) in &all {
            let Some(rel_path) = logical_rel_path_from_state_key(
                key,
                state,
                self.config.sync.sync_root.as_deref(),
                &storage_prefix,
            ) else {
                continue;
            };

            if rel_path.is_empty() {
                continue;
            }

            // Filter by prefix: only entries under the requested directory
            let normalized_prefix = if prefix.is_empty() {
                ""
            } else if prefix.ends_with('/') {
                prefix.as_str()
            } else {
                // Caller omitted trailing slash — we'll match with it
                &prefix
            };

            let remainder = if normalized_prefix.is_empty() {
                rel_path.clone()
            } else {
                // Must start with prefix (exact prefix match, not substring)
                let pfx = if normalized_prefix.ends_with('/') {
                    normalized_prefix.to_string()
                } else {
                    format!("{}/", normalized_prefix)
                };
                match rel_path.strip_prefix(&pfx) {
                    Some(r) => r.to_string(),
                    None => continue,
                }
            };

            if remainder.is_empty() {
                continue;
            }

            if remainder.contains('/') {
                // File in a subdirectory — synthesize a directory entry
                let dir_name = remainder.split('/').next().unwrap_or(&remainder);
                if dirs_seen.insert(dir_name.to_string()) {
                    let dir_path = if normalized_prefix.is_empty() {
                        format!("{}/", dir_name)
                    } else {
                        let pfx = normalized_prefix.trim_end_matches('/');
                        format!("{}/{}/", pfx, dir_name)
                    };
                    files.push(FileEntry {
                        path: dir_path,
                        filename: dir_name.to_string(),
                        size: 0,
                        last_synced: 0,
                        is_directory: true,
                        blake3: String::new(),
                        hydration_state: String::new(),
                    });
                }
            } else {
                // Immediate child file
                let hydration = match state.status {
                    tcfs_sync::state::FileSyncStatus::NotSynced => "not_synced",
                    tcfs_sync::state::FileSyncStatus::Synced => "synced",
                    tcfs_sync::state::FileSyncStatus::Active => "active",
                    tcfs_sync::state::FileSyncStatus::Locked => "locked",
                    tcfs_sync::state::FileSyncStatus::Conflict => "conflict",
                };
                files.push(FileEntry {
                    path: rel_path,
                    filename: remainder.clone(),
                    size: state.size,
                    last_synced: state.last_synced as i64,
                    is_directory: false,
                    blake3: state.blake3.clone(),
                    hydration_state: hydration.to_string(),
                });
            }
        }

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
                    mode: None,
                    encrypted_file_key: None,
                    wrapped_file_keys: Vec::new(),
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

                // Update state cache (clear conflict + status)
                {
                    let mut cache = self.state_cache.lock().await;
                    if let Some(entry) = cache.get(&path).cloned() {
                        let updated = tcfs_sync::state::SyncState {
                            vclock,
                            last_synced: tcfs_sync::StateEvent::now(),
                            conflict: None,
                            status: tcfs_sync::state::FileSyncStatus::Synced,
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
                    let prefix = self.config.storage.resolved_prefix().to_string();
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
                        // Ensure conflict is fully cleared (download_file_with_device
                        // creates fresh state, but belt-and-suspenders)
                        {
                            let mut cache = self.state_cache.lock().await;
                            cache.resolve_conflict(&path);
                            let _ = cache.flush();
                        }
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
                    let prefix = self.config.storage.resolved_prefix().to_string();
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
                        // Push the conflict copy to S3 index so FileProvider
                        // enumerates both the original and the conflict version
                        let conflict_file = std::path::Path::new(&conflict_path);
                        if conflict_file.exists() {
                            let mut cache = self.state_cache.lock().await;
                            if let Err(e) = tcfs_sync::engine::upload_file(
                                &op,
                                conflict_file,
                                &prefix,
                                &mut cache,
                                None,
                            )
                            .await
                            {
                                warn!(
                                    path = %conflict_path,
                                    "failed to push conflict copy to S3: {e}"
                                );
                            }
                        }

                        // Ensure conflict is fully cleared on the original path
                        {
                            let mut cache = self.state_cache.lock().await;
                            cache.resolve_conflict(&path);
                            let _ = cache.flush();
                        }

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
            _ => {
                return Err(tonic::Status::invalid_argument(
                    "unsupported resolution strategy",
                ))
            }
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
        let watch_roots: Vec<String> = req
            .paths
            .iter()
            .map(|path| normalize_watch_root(path))
            .collect();

        let (async_tx, async_rx) = tokio::sync::mpsc::channel(256);

        // ── Emit initial deltas from state cache (catch-up since anchor) ────
        if since >= 0 {
            let cache = self.state_cache.lock().await;
            let all = cache.all_entries();
            let storage_prefix = self.config.storage.resolved_prefix();
            let sync_root = self.config.sync.sync_root.as_deref();
            for (key, state) in &all {
                let last = state.last_synced as i64;
                if since == 0 || last > since {
                    let Some(rel_path) =
                        logical_rel_path_from_state_key(key, state, sync_root, &storage_prefix)
                    else {
                        continue;
                    };
                    if rel_path.is_empty() || !rel_path_matches_watch_roots(&rel_path, &watch_roots)
                    {
                        continue;
                    }
                    let filename = std::path::Path::new(rel_path.as_str())
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let is_directory = sync_root
                        .map(|root| root.join(&rel_path).is_dir())
                        .unwrap_or_else(|| std::path::Path::new(key.as_str()).is_dir());
                    let event = WatchEvent {
                        path: rel_path,
                        event_type: "modified".into(),
                        timestamp: last,
                        filename,
                        size: state.size,
                        blake3: state.blake3.clone(),
                        is_directory,
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
        let sync_root = self.config.sync.sync_root.clone();
        let mut watch_targets = Vec::new();

        for root in &watch_roots {
            let Some(path) = sync_root
                .as_deref()
                .map(|sync_root| {
                    if root.is_empty() {
                        sync_root.to_path_buf()
                    } else {
                        sync_root.join(root)
                    }
                })
                .or_else(|| {
                    if root.is_empty() {
                        None
                    } else {
                        Some(std::path::PathBuf::from(root))
                    }
                })
            else {
                debug!("watch: local notify disabled for empty path without sync_root");
                continue;
            };

            if path.exists() {
                watch_targets.push((root.clone(), path));
            } else {
                debug!(
                    root,
                    path = %path.display(),
                    "watch: local notify target missing; using cache/NATS only"
                );
            }
        }

        if !watch_targets.is_empty() {
            let mut watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    let _ = sync_tx.send(res);
                })
                .map_err(|e| tonic::Status::internal(format!("create watcher: {e}")))?;

            for (root, path) in &watch_targets {
                watcher
                    .watch(path, RecursiveMode::Recursive)
                    .map_err(|e| tonic::Status::internal(format!("watch {root}: {e}")))?;
            }

            let notify_tx = async_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _watcher = watcher;
                loop {
                    if notify_tx.is_closed() {
                        break;
                    }
                    let result = match sync_rx.recv_timeout(std::time::Duration::from_secs(1)) {
                        Ok(result) => result,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };
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
                            let path = event.paths.first().cloned().unwrap_or_default();
                            let logical_path =
                                logical_rel_path_from_fs_path(&path, sync_root.as_deref());
                            let filename = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64;

                            // Enrich with state cache metadata (best-effort)
                            let (size, blake3) = {
                                let cache = state_cache_for_notify.blocking_lock();
                                cache
                                    .get(&path)
                                    .map(|s| (s.size, s.blake3.clone()))
                                    .unwrap_or((0, String::new()))
                            };

                            let is_dir = path.is_dir();

                            WatchEvent {
                                path: logical_path,
                                event_type: event_type.to_string(),
                                timestamp,
                                filename,
                                size,
                                blake3,
                                is_directory: is_dir,
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
        } else {
            debug!("watch: no local notify targets; using cache/NATS only");
        }

        // ── Live remote events via NATS STATE_UPDATES ───────────────────────
        // Use an ephemeral consumer so Watch callers don't compete with
        // the daemon's durable state_sync_loop consumer for messages.
        let nats_tx = async_tx;
        let nats_client = self.nats.clone();
        let _device_id = self.device_id.clone();
        tokio::spawn(async move {
            let client = nats_client.lock().await;
            let Some(nats) = client.as_ref() else {
                debug!("watch: NATS not connected, skipping remote events");
                return;
            };
            match nats.state_consumer_ephemeral().await {
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

        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "admin")?;
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "admin")?;
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
        let session = self.require_session(&request).await?;
        Self::check_permission(&session, "admin")?;
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
        let invite = tcfs_auth::EnrollmentInvite::decode_any(&req.invite_data)
            .map_err(|e| tonic::Status::invalid_argument(format!("invalid invite: {e}")))?;

        if invite.is_expired() {
            return Ok(tonic::Response::new(DeviceEnrollResponse {
                success: false,
                error: "invite has expired".into(),
                ..Default::default()
            }));
        }

        if !tcfs_secrets::device::is_real_age_public_key(&req.public_key) {
            return Ok(tonic::Response::new(DeviceEnrollResponse {
                success: false,
                error: "invalid device public key; expected age X25519 recipient".into(),
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

        let bootstrap = self.enrollment_bootstrap_for_invite(&invite).await?;
        let bootstrap_json = serde_json::to_vec(&bootstrap).map_err(|e| {
            tonic::Status::internal(format!("serializing enrollment bootstrap: {e}"))
        })?;
        let wrapped_bootstrap_age =
            tcfs_secrets::age::encrypt_for_recipient(&req.public_key, &bootstrap_json).map_err(
                |e| tonic::Status::invalid_argument(format!("wrapping enrollment bootstrap: {e}")),
            )?;

        match self
            .invite_redemptions
            .claim(
                &invite.invite_id,
                &invite.nonce,
                &req.device_name,
                &req.public_key,
                &req.platform,
            )
            .await
        {
            Ok(_) => {
                if let Err(e) = self.persist_invite_redemptions().await {
                    tracing::warn!(error = %e, invite_id = %invite.invite_id, "failed to persist invite redemption");
                    return Err(tonic::Status::internal(
                        "failed to persist invite redemption state",
                    ));
                }
            }
            Err(tcfs_auth::InviteRedemptionError::AlreadyRedeemed { .. }) => {
                return Ok(tonic::Response::new(DeviceEnrollResponse {
                    success: false,
                    error: "invite has already been redeemed".into(),
                    ..Default::default()
                }));
            }
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
            nats_url: bootstrap.nats_url.unwrap_or_default(),
            storage_endpoint: bootstrap.storage_endpoint.unwrap_or_default(),
            available_auth_methods: vec!["totp".into()],
            error: String::new(),
            storage_bucket: bootstrap.storage_bucket.unwrap_or_default(),
            storage_access_key: String::new(),
            storage_secret: String::new(),
            remote_prefix: bootstrap.remote_prefix.unwrap_or_default(),
            encryption_passphrase: String::new(),
            encryption_salt: bootstrap.encryption_salt.unwrap_or_default(),
            wrapped_bootstrap_age,
        }))
    }

    // ── Diagnostics ──────────────────────────────────────────────────────

    async fn diagnostics(
        &self,
        _request: tonic::Request<DiagnosticsRequest>,
    ) -> Result<tonic::Response<DiagnosticsResponse>, tonic::Status> {
        let cache = self.state_cache.lock().await;

        // Count conflicts
        let mut conflict_count = 0i32;
        for (_key, state) in StateCacheBackend::all_entries(&*cache) {
            if state.conflict.is_some() {
                conflict_count += 1;
            }
        }

        // Count auto-unsync eligible files
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let max_age = self.config.sync.auto_unsync_max_age_secs;
        let eligible = if max_age > 0 {
            StateCacheBackend::all_entries(&*cache)
                .iter()
                .filter(|(_, s)| now.saturating_sub(s.last_synced) > max_age)
                .count() as i32
        } else {
            0
        };

        Ok(tonic::Response::new(DiagnosticsResponse {
            state_cache_entries: StateCacheBackend::len(&*cache) as i32,
            conflict_count,
            last_nats_seq: cache.last_nats_seq() as i64,
            nats_connected: self.nats_ok.load(std::sync::atomic::Ordering::Relaxed),
            auto_unsync_eligible: eligible,
            auto_unsync_max_age_secs: max_age as i64,
            storage_reachable: self.storage_ok,
            uptime_secs: self.start_time.elapsed().as_secs() as i64,
            device_id: self.device_id.clone(),
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

    // Restrict socket to owner-only access (prevents other users from connecting)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow::anyhow!("setting socket permissions: {e}"))?;
    }

    Ok(UnixListenerStream::new(listener))
}

/// Start the gRPC server on a Unix domain socket with graceful shutdown support.
///
/// If `fileprovider_socket` is provided, a second server is spawned on that socket
/// for sandboxed macOS FileProvider access (App Group container).
pub async fn serve(
    socket_path: &Path,
    fileprovider_socket: Option<&Path>,
    listen: Option<&str>,
    impl_: TcfsDaemonImpl,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let primary = bind_uds(socket_path).await?;
    info!(socket = %socket_path.display(), "gRPC server ready");

    let service = TcfsDaemonServer::new(impl_);

    let tcp_handle = if let Some(addr) = listen {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        info!(addr = %local_addr, "gRPC TCP listener ready");

        let tcp_service = service.clone();
        let tcp_shutdown = Arc::new(tokio::sync::Notify::new());
        let tcp_shutdown_clone = tcp_shutdown.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = Server::builder()
                .add_service(tcp_service)
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    tcp_shutdown_clone.notified(),
                )
                .await
            {
                tracing::warn!("TCP gRPC server error: {e}");
            }
        });

        Some((handle, tcp_shutdown))
    } else {
        None
    };

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
    if let Some((handle, notify)) = tcp_handle {
        notify.notify_one();
        let _ = handle.await;
    }

    result
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use opendal::services::Memory;
    use opendal::Operator;
    use secrecy::{ExposeSecret, SecretString};
    use tcfs_core::proto::tcfs_daemon_client::TcfsDaemonClient;
    use tonic::transport::{Channel, Endpoint, Uri};
    use tower::service_fn;

    /// Build a TcfsDaemonImpl with in-memory components for testing.
    fn test_daemon_with_operator_master_and_session_requirement(
        operator_value: Option<Operator>,
        master_key: Option<tcfs_crypto::MasterKey>,
        require_session: bool,
    ) -> TcfsDaemonImpl {
        let mut config = TcfsConfig::default();
        config.auth.require_session = require_session;
        config.storage.bucket = "data".into();
        config.storage.remote_prefix = Some("data".into());
        let config = Arc::new(config);
        let cred_store = crate::cred_store::new_shared();
        let state_dir = tempfile::tempdir().unwrap().keep();
        let state_path = state_dir.join("state.json");
        let state_cache = Arc::new(TokioMutex::new(
            tcfs_sync::state::StateCache::open(&state_path).unwrap(),
        ));
        let operator = Arc::new(TokioMutex::new(operator_value.clone()));

        TcfsDaemonImpl::new(
            cred_store,
            config,
            operator_value.is_some(),
            "http://test:8333".into(),
            state_cache,
            operator,
            tcfs_sync::state::PathLocks::new(),
            "test-device-id".into(),
            "test-device".into(),
            master_key,
        )
    }

    fn test_daemon_with_operator_and_master(
        operator_value: Option<Operator>,
        master_key: Option<tcfs_crypto::MasterKey>,
    ) -> TcfsDaemonImpl {
        test_daemon_with_operator_master_and_session_requirement(operator_value, master_key, false)
    }

    fn test_daemon_with_required_sessions() -> TcfsDaemonImpl {
        test_daemon_with_operator_master_and_session_requirement(None, None, true)
    }

    fn test_daemon_with_operator(operator_value: Option<Operator>) -> TcfsDaemonImpl {
        test_daemon_with_operator_and_master(operator_value, None)
    }

    fn test_daemon() -> TcfsDaemonImpl {
        test_daemon_with_operator(None)
    }

    fn memory_operator() -> Operator {
        Operator::new(Memory::default()).unwrap().finish()
    }

    fn test_device_keypair() -> tcfs_secrets::device::LocalDeviceKey {
        tcfs_secrets::device::generate_local_device_key()
    }

    async fn insert_test_s3_credentials(
        daemon: &TcfsDaemonImpl,
        access_key: &str,
        secret_key: &str,
    ) {
        daemon
            .cred_store
            .write()
            .await
            .replace(tcfs_secrets::CredStore {
                s3: Some(tcfs_secrets::S3Credentials {
                    access_key_id: access_key.into(),
                    secret_access_key: SecretString::from(secret_key.to_string()),
                    endpoint: daemon.config.storage.endpoint.clone(),
                    region: daemon.config.storage.region.clone(),
                }),
                source: "test".into(),
            });
    }

    fn request_with_bearer<T>(message: T, token: &str) -> tonic::Request<T> {
        let mut request = tonic::Request::new(message);
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
    }

    async fn insert_test_session(
        daemon: &TcfsDaemonImpl,
        device_id: &str,
        permissions: tcfs_auth::DevicePermissions,
    ) -> String {
        let session =
            tcfs_auth::Session::new(device_id, device_id, "test").with_permissions(permissions);
        let token = session.token.clone();
        daemon.session_store.insert(session).await;
        token
    }

    fn test_sync_state(remote_path: &str, last_synced: u64) -> tcfs_sync::state::SyncState {
        tcfs_sync::state::SyncState {
            blake3: "test-blake3".into(),
            size: 123,
            mtime: 0,
            chunk_count: 1,
            remote_path: remote_path.into(),
            last_synced,
            vclock: tcfs_sync::conflict::VectorClock::default(),
            device_id: "remote-device".into(),
            conflict: None,
            status: tcfs_sync::state::FileSyncStatus::NotSynced,
        }
    }

    async fn connect_test_client(socket_path: &Path) -> TcfsDaemonClient<Channel> {
        let path = socket_path.to_path_buf();
        let mut last_err = None;

        for _ in 0..50 {
            match Endpoint::from_static("http://[::]:0")
                .connect_with_connector(service_fn({
                    let path = path.clone();
                    move |_: Uri| {
                        let path = path.clone();
                        async move {
                            let stream = tokio::net::UnixStream::connect(&path).await?;
                            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                        }
                    }
                }))
                .await
            {
                Ok(channel) => return TcfsDaemonClient::new(channel),
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        }

        panic!(
            "failed to connect test client to {}: {}",
            socket_path.display(),
            last_err.unwrap()
        );
    }

    async fn spawn_test_server(
        daemon: TcfsDaemonImpl,
    ) -> (
        tempfile::TempDir,
        tokio::task::JoinHandle<Result<()>>,
        Arc<tokio::sync::Notify>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tcfsd.sock");
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let shutdown_for_server = shutdown.clone();

        let handle = tokio::spawn(async move {
            serve(
                &socket_path,
                None,
                None,
                daemon,
                shutdown_for_server.notified(),
            )
            .await
        });

        (dir, handle, shutdown)
    }

    #[tokio::test]
    async fn status_returns_version() {
        let daemon = test_daemon();
        let resp = daemon
            .status(tonic::Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(resp.device_id, "test-device-id");
        assert_eq!(resp.device_name, "test-device");
        assert!(!resp.storage_ok);
        assert!(!resp.nats_ok);
        assert_eq!(resp.active_mounts, 0);
        assert!(resp.uptime_secs >= 0);
    }

    #[tokio::test]
    async fn status_rechecks_live_storage_health() {
        let daemon = test_daemon_with_operator(Some(memory_operator()));
        let resp = daemon
            .status(tonic::Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert!(resp.storage_ok);
    }

    #[tokio::test]
    async fn credential_status_empty() {
        let daemon = test_daemon();
        let resp = daemon
            .credential_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.loaded);
        assert_eq!(resp.source, "none");
        assert!(resp.needs_reload);
    }

    #[tokio::test]
    async fn auth_status_locked_by_default() {
        let daemon = test_daemon();
        let resp = daemon
            .auth_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.unlocked);
        assert!(resp.available_methods.contains(&"master_key".to_string()));
        assert!(resp.auth_method.is_empty());
        assert_eq!(resp.session_device_id, "test-device-id");
    }

    #[tokio::test]
    async fn auth_unlock_then_lock_roundtrip() {
        let daemon = test_daemon();

        // Unlock with a 32-byte key
        let key = vec![0xAA; tcfs_crypto::KEY_SIZE];
        let resp = daemon
            .auth_unlock(tonic::Request::new(AuthUnlockRequest { master_key: key }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.success);

        // Verify unlocked
        let status = daemon
            .auth_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert!(status.unlocked);

        // Lock
        let resp = daemon
            .auth_lock(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.success);

        // Verify locked
        let status = daemon
            .auth_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert!(!status.unlocked);
    }

    #[tokio::test]
    async fn auth_unlock_wrong_key_size_fails() {
        let daemon = test_daemon();

        let resp = daemon
            .auth_unlock(tonic::Request::new(AuthUnlockRequest {
                master_key: vec![0x00; 16], // too short
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("must be"));
    }

    #[tokio::test]
    async fn auth_enroll_requires_admin_session() {
        let daemon = test_daemon_with_required_sessions();
        let request = AuthEnrollRequest {
            device_id: "new-device".into(),
            method: "totp".into(),
        };

        let missing = daemon
            .auth_enroll(tonic::Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let user_token = insert_test_session(
            &daemon,
            "regular-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let non_admin = daemon
            .auth_enroll(request_with_bearer(request.clone(), &user_token))
            .await
            .unwrap_err();
        assert_eq!(non_admin.code(), tonic::Code::PermissionDenied);

        let admin_token = insert_test_session(
            &daemon,
            "admin-device",
            tcfs_auth::DevicePermissions::admin(),
        )
        .await;
        let mut unsupported = request;
        unsupported.method = "unsupported".into();
        let allowed = daemon
            .auth_enroll(request_with_bearer(unsupported, &admin_token))
            .await
            .unwrap()
            .into_inner();
        assert!(!allowed.success);
        assert!(allowed.error.contains("unsupported auth method"));
    }

    #[tokio::test]
    async fn auth_complete_enroll_requires_admin_session() {
        let daemon = test_daemon_with_required_sessions();
        let request = AuthCompleteEnrollRequest {
            device_id: "new-device".into(),
            method: "totp".into(),
            attestation_data: Vec::new(),
        };

        let missing = daemon
            .auth_complete_enroll(tonic::Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let user_token = insert_test_session(
            &daemon,
            "regular-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let non_admin = daemon
            .auth_complete_enroll(request_with_bearer(request.clone(), &user_token))
            .await
            .unwrap_err();
        assert_eq!(non_admin.code(), tonic::Code::PermissionDenied);

        let admin_token = insert_test_session(
            &daemon,
            "admin-device",
            tcfs_auth::DevicePermissions::admin(),
        )
        .await;
        let allowed = daemon
            .auth_complete_enroll(request_with_bearer(request, &admin_token))
            .await
            .unwrap()
            .into_inner();
        assert!(allowed.success, "admin request failed: {}", allowed.error);
    }

    #[tokio::test]
    async fn auth_revoke_requires_admin_session() {
        let daemon = test_daemon_with_required_sessions();
        let target_token = insert_test_session(
            &daemon,
            "target-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let request = AuthRevokeRequest {
            session_token: target_token.clone(),
            device_id: String::new(),
        };

        let missing = daemon
            .auth_revoke(tonic::Request::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(missing.code(), tonic::Code::Unauthenticated);

        let user_token = insert_test_session(
            &daemon,
            "regular-device",
            tcfs_auth::DevicePermissions::default(),
        )
        .await;
        let non_admin = daemon
            .auth_revoke(request_with_bearer(request.clone(), &user_token))
            .await
            .unwrap_err();
        assert_eq!(non_admin.code(), tonic::Code::PermissionDenied);
        assert!(daemon.session_store.validate(&target_token).await.is_some());

        let admin_token = insert_test_session(
            &daemon,
            "admin-device",
            tcfs_auth::DevicePermissions::admin(),
        )
        .await;
        let allowed = daemon
            .auth_revoke(request_with_bearer(request, &admin_token))
            .await
            .unwrap()
            .into_inner();
        assert!(allowed.success, "admin revoke failed: {}", allowed.error);
        assert!(daemon.session_store.validate(&target_token).await.is_none());
    }

    #[tokio::test]
    async fn device_enroll_rejects_reused_invite() {
        let key_bytes = [0xA5; tcfs_crypto::KEY_SIZE];
        let signing_key: [u8; 32] = *blake3::hash(&key_bytes).as_bytes();
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with_operator_and_master(
            None,
            Some(tcfs_crypto::MasterKey::from_bytes(key_bytes)),
        )
        .with_data_dir(temp.path().join("tcfsd"));

        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.storage_access_key = Some("test-access".into());
        invite.storage_secret_key = Some("test-secret".into());
        invite.refresh_signature(&signing_key);
        let invite_data = invite.encode_compact().unwrap();
        let keypair = test_device_keypair();

        let req = DeviceEnrollRequest {
            invite_data: invite_data.clone(),
            device_name: "new-laptop".into(),
            public_key: keypair.public_key.clone(),
            platform: "linux-x86_64".into(),
        };

        let first = daemon
            .device_enroll(tonic::Request::new(req.clone()))
            .await
            .unwrap()
            .into_inner();
        assert!(first.success, "first enrollment failed: {}", first.error);
        assert!(temp.path().join("tcfsd/invite-redemptions.json").exists());

        let second = daemon
            .device_enroll(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert!(!second.success);
        assert!(second.error.contains("already been redeemed"));
    }

    #[tokio::test]
    async fn device_enroll_rejects_invalid_public_key_before_claiming_invite() {
        let key_bytes = [0xC7; tcfs_crypto::KEY_SIZE];
        let signing_key: [u8; 32] = *blake3::hash(&key_bytes).as_bytes();
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with_operator_and_master(
            None,
            Some(tcfs_crypto::MasterKey::from_bytes(key_bytes)),
        )
        .with_data_dir(temp.path().join("tcfsd"));

        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.storage_access_key = Some("test-access".into());
        invite.storage_secret_key = Some("test-secret".into());
        invite.refresh_signature(&signing_key);
        let invite_data = invite.encode_compact().unwrap();

        let rejected = daemon
            .device_enroll(tonic::Request::new(DeviceEnrollRequest {
                invite_data: invite_data.clone(),
                device_name: "new-laptop".into(),
                public_key: "not-an-age-recipient".into(),
                platform: "linux-x86_64".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!rejected.success);
        assert!(rejected.error.contains("invalid device public key"));
        assert!(!temp.path().join("tcfsd/invite-redemptions.json").exists());

        let keypair = test_device_keypair();
        let accepted = daemon
            .device_enroll(tonic::Request::new(DeviceEnrollRequest {
                invite_data,
                device_name: "new-laptop".into(),
                public_key: keypair.public_key.clone(),
                platform: "linux-x86_64".into(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(accepted.success, "retry failed: {}", accepted.error);
    }

    #[tokio::test]
    async fn device_enroll_wraps_bootstrap_to_joining_device_key() {
        let key_bytes = [0xB6; tcfs_crypto::KEY_SIZE];
        let signing_key: [u8; 32] = *blake3::hash(&key_bytes).as_bytes();
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon_with_operator_and_master(
            None,
            Some(tcfs_crypto::MasterKey::from_bytes(key_bytes)),
        )
        .with_data_dir(temp.path().join("tcfsd"));
        insert_test_s3_credentials(&daemon, "daemon-access", "daemon-secret").await;

        let mut invite = tcfs_auth::EnrollmentInvite::new(
            "admin-device",
            &signing_key,
            24,
            tcfs_auth::DevicePermissions::default(),
        );
        invite.storage_endpoint = Some("https://s3.example.invalid".into());
        invite.storage_bucket = Some("tcfs".into());
        invite.remote_prefix = Some("tenant/a".into());
        invite.refresh_signature(&signing_key);
        let invite_data = invite.encode_compact().unwrap();
        let keypair = test_device_keypair();

        let resp = daemon
            .device_enroll(tonic::Request::new(DeviceEnrollRequest {
                invite_data,
                device_name: "new-laptop".into(),
                public_key: keypair.public_key.clone(),
                platform: "linux-x86_64".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(resp.success, "enrollment failed: {}", resp.error);
        assert!(resp.storage_access_key.is_empty());
        assert!(resp.storage_secret.is_empty());
        assert!(resp.encryption_passphrase.is_empty());
        assert!(resp
            .wrapped_bootstrap_age
            .contains("BEGIN AGE ENCRYPTED FILE"));

        let identity = tcfs_secrets::IdentityProvider {
            key_data: keypair.secret_key.expose_secret().to_string(),
            source: "test".into(),
        };
        let plaintext = tcfs_secrets::age::decrypt_with_identity(
            &identity,
            resp.wrapped_bootstrap_age.as_bytes(),
        )
        .unwrap();
        let bootstrap: tcfs_auth::EnrollmentBootstrap = serde_json::from_slice(&plaintext).unwrap();

        assert_eq!(
            bootstrap.storage_endpoint.as_deref(),
            Some("https://s3.example.invalid")
        );
        assert_eq!(bootstrap.storage_bucket.as_deref(), Some("tcfs"));
        assert_eq!(
            bootstrap.storage_access_key.as_deref(),
            Some("daemon-access")
        );
        assert_eq!(
            bootstrap.storage_secret_key.as_deref(),
            Some("daemon-secret")
        );
        assert_eq!(bootstrap.remote_prefix.as_deref(), Some("tenant/a"));
        let expected_master_key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        assert_eq!(
            bootstrap.master_key_base64.as_deref(),
            Some(expected_master_key.as_str())
        );
    }

    #[tokio::test]
    async fn diagnostics_empty_state() {
        let daemon = test_daemon();
        let resp = daemon
            .diagnostics(tonic::Request::new(DiagnosticsRequest {}))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.state_cache_entries, 0);
        assert_eq!(resp.conflict_count, 0);
        assert!(!resp.nats_connected);
        assert!(!resp.storage_reachable);
        assert_eq!(resp.device_id, "test-device-id");
    }

    #[tokio::test]
    async fn sync_status_unknown_path() {
        let daemon = test_daemon();
        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: "/nonexistent/file.txt".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        // Unknown path returns empty/default state
        assert!(resp.state.is_empty() || resp.state == "unknown");
    }

    #[tokio::test]
    async fn sync_status_reports_explicit_conflict_state() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("conflicted.txt");
        std::fs::write(&tracked, b"conflicted").unwrap();

        {
            let mut cache = daemon.state_cache.lock().await;
            let mut entry = tcfs_sync::state::make_sync_state(
                &tracked,
                "abc123".to_string(),
                1,
                "data/manifests/abc123".to_string(),
            )
            .unwrap();
            entry.status = tcfs_sync::state::FileSyncStatus::Conflict;
            cache.set(&tracked, entry);
            cache.flush().unwrap();
        }

        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: tracked.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.state, "conflict");
    }

    #[tokio::test]
    async fn sync_status_reports_pending_for_modified_tracked_file() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("modified.txt");
        std::fs::write(&tracked, b"alpha").unwrap();

        {
            let mut cache = daemon.state_cache.lock().await;
            let entry = tcfs_sync::state::make_sync_state(
                &tracked,
                "tracked-alpha".to_string(),
                1,
                "data/manifests/alpha".to_string(),
            )
            .unwrap();
            cache.set(&tracked, entry);
            cache.flush().unwrap();
        }

        std::fs::write(&tracked, b"alpha updated").unwrap();

        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: tracked.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.state, "pending");
    }

    /// When a Synced entry exists but `needs_sync` errors (e.g. the path can't
    /// be stat'd), we must NOT report the stale "synced" state. We report
    /// "unknown" instead so observers can see the IO failure rather than
    /// trusting a lie.
    #[tokio::test]
    async fn sync_status_surfaces_needs_sync_error_instead_of_synced() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        // Path that will never exist — needs_sync's std::fs::metadata will Err.
        let missing = dir.path().join("vanished.txt");

        {
            let mut cache = daemon.state_cache.lock().await;
            cache.set(
                &missing,
                tcfs_sync::state::SyncState {
                    blake3: "deadbeef".into(),
                    size: 42,
                    mtime: 1_700_000_000,
                    chunk_count: 1,
                    remote_path: "data/manifests/deadbeef".into(),
                    last_synced: 1_700_000_000,
                    vclock: tcfs_sync::conflict::VectorClock::default(),
                    device_id: "test-device-id".into(),
                    conflict: None,
                    status: tcfs_sync::state::FileSyncStatus::Synced,
                },
            );
            cache.flush().unwrap();
        }

        let resp = daemon
            .sync_status(tonic::Request::new(SyncStatusRequest {
                path: missing.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_ne!(
            resp.state, "synced",
            "needs_sync Err was silently collapsed into \"synced\""
        );
        assert_eq!(resp.state, "unknown");
    }

    #[test]
    fn logical_rel_path_prefers_sync_root_key_for_manifest_entries() {
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("ci-smoke/0.12.9/hello.txt");
        let state = test_sync_state("data/manifests/abc123", 1_700_000_000);

        let rel = logical_rel_path_from_state_key(
            &key.to_string_lossy(),
            &state,
            Some(root.path()),
            "data",
        )
        .unwrap();

        assert_eq!(rel, "ci-smoke/0.12.9/hello.txt");
    }

    #[test]
    fn logical_rel_path_falls_back_to_remote_index_key() {
        let root = tempfile::tempdir().unwrap();
        let key = "/tmp/outside-tcfs-state-cache-key";
        let state = test_sync_state("data/index/remote/only.txt", 1_700_000_000);

        let rel = logical_rel_path_from_state_key(key, &state, Some(root.path()), "data").unwrap();

        assert_eq!(rel, "remote/only.txt");
    }

    #[tokio::test]
    async fn watch_empty_root_returns_logical_catch_up_events() {
        use futures::StreamExt;

        let daemon = test_daemon();
        let tracked = std::path::PathBuf::from("/tmp/tcfs-state-key/ci-smoke/0.12.9/hello.txt");

        {
            let mut cache = daemon.state_cache.lock().await;
            cache.set(
                &tracked,
                test_sync_state("data/index/ci-smoke/0.12.9/hello.txt", 1_700_000_000),
            );
            cache.flush().unwrap();
        }

        let mut stream = daemon
            .watch(tonic::Request::new(WatchRequest {
                paths: vec![String::new()],
                since_timestamp: 1,
            }))
            .await
            .unwrap()
            .into_inner();

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .expect("watch should emit catch-up event")
            .expect("watch stream should stay open")
            .expect("catch-up event should not be an error");

        assert_eq!(event.path, "ci-smoke/0.12.9/hello.txt");
        assert_eq!(event.filename, "hello.txt");
        assert_eq!(event.event_type, "modified");
        assert_eq!(event.size, 123);
    }

    #[tokio::test]
    async fn resolve_conflict_invalid_resolution() {
        let daemon = test_daemon();
        let resp = daemon
            .resolve_conflict(tonic::Request::new(ResolveConflictRequest {
                resolution: "invalid_strategy".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("invalid resolution"));
    }

    #[tokio::test]
    async fn mount_missing_required_fields_returns_error_response() {
        let daemon = test_daemon();

        let resp = daemon
            .mount(tonic::Request::new(MountRequest {
                remote: String::new(),
                mountpoint: String::new(),
                read_only: false,
                options: vec![],
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("mountpoint and remote are required"));
    }

    #[tokio::test]
    async fn mount_requires_initialized_operator() {
        let daemon = test_daemon();
        let mountpoint_dir = tempfile::tempdir().unwrap();

        let err = daemon
            .mount(tonic::Request::new(MountRequest {
                remote: "s3://127.0.0.1/test/data".into(),
                mountpoint: mountpoint_dir.path().join("mnt").display().to_string(),
                read_only: false,
                options: vec![],
            }))
            .await
            .unwrap_err();

        assert_eq!(err.code(), tonic::Code::Unavailable);
        assert!(err.message().contains("storage operator not initialized"));
    }

    #[tokio::test]
    async fn mount_rejects_duplicate_active_mountpoint() {
        let daemon = test_daemon();
        let mountpoint = tempfile::tempdir().unwrap();
        let mountpoint_str = mountpoint.path().display().to_string();
        let sentinel = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        daemon
            .active_mounts
            .lock()
            .await
            .insert(mountpoint_str.clone(), sentinel);

        let resp = daemon
            .mount(tonic::Request::new(MountRequest {
                remote: "s3://127.0.0.1/test/data".into(),
                mountpoint: mountpoint_str.clone(),
                read_only: false,
                options: vec![],
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("already mounted"));

        let child = { daemon.active_mounts.lock().await.remove(&mountpoint_str) };
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
    }

    #[tokio::test]
    async fn unmount_requires_mountpoint() {
        let daemon = test_daemon();

        let resp = daemon
            .unmount(tonic::Request::new(UnmountRequest {
                mountpoint: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!resp.success);
        assert!(resp.error.contains("mountpoint is required"));
    }

    #[tokio::test]
    async fn push_then_pull_streams_complete_successfully_over_uds() {
        let daemon = test_daemon_with_operator(Some(memory_operator()));
        let (socket_dir, server_handle, shutdown) = spawn_test_server(daemon).await;
        let socket_path = socket_dir.path().join("tcfsd.sock");
        let mut client = connect_test_client(&socket_path).await;

        let push_chunk = PushChunk {
            path: "docs/hello.txt".into(),
            data: b"hello over grpc".to_vec(),
            offset: 0,
            last: true,
        };

        let mut push_stream = client
            .push(tonic::Request::new(tokio_stream::once(push_chunk)))
            .await
            .unwrap()
            .into_inner();

        let push_progress = push_stream
            .message()
            .await
            .unwrap()
            .expect("push should yield a final progress message");
        assert!(push_progress.done);
        assert!(
            push_progress.error.is_empty(),
            "push error: {}",
            push_progress.error
        );
        assert_eq!(push_progress.bytes_sent, b"hello over grpc".len() as u64);
        assert_eq!(push_progress.total_bytes, b"hello over grpc".len() as u64);
        assert!(!push_progress.chunk_hash.is_empty());
        assert!(push_stream.message().await.unwrap().is_none());

        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("downloaded.txt");

        let mut pull_stream = client
            .pull(tonic::Request::new(PullRequest {
                remote_path: "docs/hello.txt".into(),
                local_path: output_path.display().to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        let pull_progress = pull_stream
            .message()
            .await
            .unwrap()
            .expect("pull should yield a final progress message");
        assert!(pull_progress.done);
        assert!(
            pull_progress.error.is_empty(),
            "pull error: {}",
            pull_progress.error
        );
        assert_eq!(
            pull_progress.bytes_received,
            b"hello over grpc".len() as u64
        );
        assert_eq!(pull_progress.total_bytes, b"hello over grpc".len() as u64);
        assert!(pull_stream.message().await.unwrap().is_none());
        assert_eq!(
            std::fs::read(&output_path).unwrap(),
            b"hello over grpc".to_vec()
        );

        shutdown.notify_one();
        let server_result = server_handle.await.unwrap();
        assert!(
            server_result.is_ok(),
            "server exited with error: {server_result:?}"
        );
    }

    #[tokio::test]
    async fn unsync_waits_for_active_path_lock() {
        let daemon = test_daemon();
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join("tracked.txt");
        std::fs::write(&tracked, b"tracked").unwrap();

        {
            let mut cache = daemon.state_cache.lock().await;
            let entry = tcfs_sync::state::make_sync_state(
                &tracked,
                "abc123".to_string(),
                1,
                "data/manifests/abc123".to_string(),
            )
            .unwrap();
            cache.set(&tracked, entry);
            cache.flush().unwrap();
        }

        let state_cache = daemon.state_cache_handle();
        let guard = daemon.path_locks.lock(&tracked).await;
        let tracked_str = tracked.display().to_string();

        let handle = tokio::spawn(async move {
            daemon
                .unsync(tonic::Request::new(UnsyncRequest {
                    path: tracked_str,
                    force: false,
                }))
                .await
                .unwrap()
                .into_inner()
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "unsync should wait while the path lock is held"
        );

        drop(guard);

        let resp = handle.await.unwrap();
        assert!(resp.success, "unsync error: {}", resp.error);

        let cache = state_cache.lock().await;
        let entry = cache
            .get(&tracked)
            .expect("state should still exist after unsync");
        assert_eq!(entry.status, tcfs_sync::state::FileSyncStatus::NotSynced);
    }

    #[test]
    fn sanitize_rejects_parent_traversal() {
        assert!(sanitize_rel_path("../../etc/passwd").is_err());
        assert!(sanitize_rel_path("foo/../../../bar").is_err());
        assert!(sanitize_rel_path("..").is_err());
    }

    #[test]
    fn sanitize_rejects_absolute_paths() {
        assert!(sanitize_rel_path("/etc/passwd").is_err());
        assert!(sanitize_rel_path("/tmp/file.txt").is_err());
    }

    #[test]
    fn sanitize_rejects_empty_path() {
        assert!(sanitize_rel_path("").is_err());
    }

    #[test]
    fn sanitize_accepts_valid_relative_paths() {
        assert_eq!(sanitize_rel_path("file.txt").unwrap(), "file.txt");
        assert_eq!(sanitize_rel_path("a/b/c.txt").unwrap(), "a/b/c.txt");
        assert_eq!(
            sanitize_rel_path("docs/nested/deep.md").unwrap(),
            "docs/nested/deep.md"
        );
        assert_eq!(sanitize_rel_path("./current.txt").unwrap(), "./current.txt");
    }
}
