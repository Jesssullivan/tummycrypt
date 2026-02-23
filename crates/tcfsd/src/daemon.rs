//! Daemon lifecycle: startup, health checks, systemd notify, gRPC server

use anyhow::Result;
use secrecy::ExposeSecret;
use std::sync::Arc;
use tcfs_core::config::TcfsConfig;
use tcfs_sync::conflict::ConflictResolver;
use tracing::{error, info, warn};

use crate::cred_store::{new_shared as new_cred_store, SharedCredStore};
use crate::grpc::TcfsDaemonImpl;

pub async fn run(config: TcfsConfig) -> Result<()> {
    info!("daemon starting");

    // ── Device identity ──────────────────────────────────────────────────
    let device_name = config
        .sync
        .device_name
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_device_name);

    let registry_path = config
        .sync
        .device_identity
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);

    let mut registry =
        tcfs_secrets::device::DeviceRegistry::load(&registry_path).unwrap_or_else(|e| {
            warn!("device registry load failed: {e} (starting empty)");
            tcfs_secrets::device::DeviceRegistry::default()
        });

    // Auto-enroll this device on first run
    let device_id = if let Some(dev) = registry.find(&device_name) {
        info!(device = %device_name, id = %dev.device_id, "device identity loaded");
        dev.device_id.clone()
    } else {
        let public_key = format!(
            "age1-device-{}",
            &blake3::hash(device_name.as_bytes()).to_hex().as_str()[..8]
        );
        let id = registry.enroll(&device_name, &public_key, None);
        if let Err(e) = registry.save(&registry_path) {
            warn!("failed to save device registry: {e}");
        }
        info!(device = %device_name, id = %id, "device auto-enrolled");
        id
    };

    // Load credentials
    let cred_store: SharedCredStore = new_cred_store();
    match tcfs_secrets::CredStore::load(&config.secrets, &config.storage).await {
        Ok(cs) => {
            info!(source = %cs.source, "credentials loaded");
            cred_store.write().await.replace(cs);
        }
        Err(e) => {
            warn!("credential load failed: {e}  (daemon will start without creds)");
        }
    }

    // Build storage operator and verify connectivity
    let mut operator: Option<opendal::Operator> = None;
    let storage_ok = if let Some(s3) = cred_store.read().await.as_ref().and_then(|c| c.s3.as_ref())
    {
        let op = tcfs_storage::operator::build_from_core_config(
            &config.storage,
            &s3.access_key_id,
            s3.secret_access_key.expose_secret(),
        )?;
        match tcfs_storage::check_health(&op).await {
            Ok(()) => {
                info!(endpoint = %config.storage.endpoint, "SeaweedFS: connected");
                operator = Some(op);
                true
            }
            Err(e) => {
                warn!(endpoint = %config.storage.endpoint, "SeaweedFS: {e}");
                // Still keep the operator for retry
                operator = Some(op);
                false
            }
        }
    } else {
        warn!("no S3 credentials — storage connectivity not verified");
        false
    };

    // Open state cache
    let state_cache =
        tcfs_sync::state::StateCache::open(&config.sync.state_db).unwrap_or_else(|e| {
            warn!("state cache open failed: {e}  (starting fresh)");
            tcfs_sync::state::StateCache::open(&std::path::PathBuf::from(
                "/tmp/tcfsd-state.db.json",
            ))
            .expect("fallback state cache")
        });

    // Wrap operator in Arc<Mutex> for shared access
    let operator = Arc::new(tokio::sync::Mutex::new(operator));

    // Start Prometheus metrics + health check endpoint
    let metrics_addr = config.daemon.metrics_addr.clone();
    if let Some(addr) = metrics_addr {
        let health_state = crate::metrics::HealthState {
            registry: Arc::new(crate::metrics::Registry::default()),
            operator: operator.clone(),
        };
        tokio::spawn(async move {
            if let Err(e) = crate::metrics::serve(addr, health_state).await {
                error!("metrics server failed: {e}");
            }
        });
    }

    // Start credential file watcher (if a credentials_file is configured)
    let _cred_watcher = if let Some(ref cred_file) = config.storage.credentials_file {
        if cred_file.exists() {
            match crate::cred_store::watch_credentials(
                cred_file.clone(),
                config.secrets.clone(),
                config.storage.clone(),
                cred_store.clone(),
            ) {
                Ok(watcher) => {
                    info!(path = %watcher.path().display(), "credential file watcher started");
                    Some(watcher)
                }
                Err(e) => {
                    warn!("credential file watcher failed to start: {e}");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Log device identity for troubleshooting
    info!(
        device_name = %device_name,
        device_id = %device_id,
        conflict_mode = %config.sync.conflict_mode,
        "fleet identity ready"
    );

    // Send systemd ready notification
    notify_ready();

    // Start gRPC server
    let socket_path = config.daemon.socket.clone();
    let config = Arc::new(config);
    let impl_ = TcfsDaemonImpl::new(
        cred_store,
        config.clone(),
        storage_ok,
        config.storage.endpoint.clone(),
        state_cache,
        operator.clone(),
        device_id.clone(),
        device_name.clone(),
    );

    // Connect to NATS for fleet state sync (non-blocking, best-effort)
    let nats_url = &config.sync.nats_url;
    if nats_url != "nats://localhost:4222" || std::env::var("TCFS_NATS_URL").is_ok() {
        let url = std::env::var("TCFS_NATS_URL").unwrap_or_else(|_| nats_url.clone());
        match tcfs_sync::NatsClient::connect(&url).await {
            Ok(nats) => {
                if let Err(e) = nats.ensure_streams().await {
                    warn!("NATS stream setup failed: {e}");
                } else {
                    // Publish DeviceOnline event
                    let online_event = tcfs_sync::StateEvent::DeviceOnline {
                        device_id: device_id.clone(),
                        last_seq: 0,
                        timestamp: tcfs_sync::StateEvent::now(),
                    };
                    if let Err(e) = nats.publish_state_event(&online_event).await {
                        warn!("failed to publish DeviceOnline: {e}");
                    } else {
                        info!("NATS: published DeviceOnline");
                    }

                    // Spawn state sync loop with auto-pull support
                    let sync_device_id = device_id.clone();
                    let sync_conflict_mode = config.sync.conflict_mode.clone();
                    let sync_root = config.sync.sync_root.clone();
                    let storage_prefix = config.storage.bucket.clone();
                    spawn_state_sync_loop(
                        &nats,
                        &sync_device_id,
                        &sync_conflict_mode,
                        operator.clone(),
                        impl_.state_cache_handle(),
                        sync_root,
                        storage_prefix,
                    )
                    .await;

                    impl_.set_nats(nats);
                }
            }
            Err(e) => {
                warn!("NATS connection failed: {e} (fleet sync disabled)");
            }
        }
    }

    // Prepare shutdown handles
    let state_cache_for_shutdown = impl_.state_cache_handle();
    let nats_for_shutdown = impl_.nats_handle();
    let device_id_for_shutdown = device_id.clone();

    // Set up graceful shutdown on SIGTERM/SIGINT
    let shutdown_signal = async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT, initiating graceful shutdown");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM, initiating graceful shutdown");
            }
        }

        // Notify systemd we're stopping
        notify_stopping();

        // Flush state cache before exit
        let mut cache = state_cache_for_shutdown.lock().await;
        if let Err(e) = cache.flush() {
            error!("failed to flush state cache on shutdown: {e}");
        } else {
            info!("state cache flushed");
        }

        // Publish DeviceOffline event if NATS connected
        if let Some(nats) = nats_for_shutdown.lock().await.as_ref() {
            let offline_event = tcfs_sync::StateEvent::DeviceOffline {
                device_id: device_id_for_shutdown.clone(),
                last_seq: 0,
                timestamp: tcfs_sync::StateEvent::now(),
            };
            if let Err(e) = nats.publish_state_event(&offline_event).await {
                warn!("failed to publish DeviceOffline: {e}");
            } else {
                info!("NATS: published DeviceOffline");
            }
        }

        info!("shutdown complete");
    };

    info!(socket = %socket_path.display(), "gRPC: listening");

    crate::grpc::serve(&socket_path, impl_, shutdown_signal).await?;

    // Clean up socket file
    let _ = tokio::fs::remove_file(&socket_path).await;

    Ok(())
}

/// Spawn a background task that consumes state events from NATS.
#[allow(clippy::too_many_arguments)]
async fn spawn_state_sync_loop(
    nats: &tcfs_sync::NatsClient,
    device_id: &str,
    conflict_mode: &str,
    operator: Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    sync_root: Option<std::path::PathBuf>,
    storage_prefix: String,
) {
    use futures::StreamExt;

    match nats.state_consumer(device_id).await {
        Ok(stream) => {
            let device_id = device_id.to_string();
            let conflict_mode = conflict_mode.to_string();
            tokio::spawn(async move {
                let mut stream = std::pin::pin!(stream);
                info!(device = %device_id, "state sync loop started");
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(msg) => {
                            let event_type = msg.event.event_type();
                            let event_device = msg.event.device_id().to_string();

                            // Skip events from our own device
                            if event_device == device_id {
                                if let Err(e) = msg.ack().await {
                                    warn!("ack own event failed: {e}");
                                }
                                continue;
                            }

                            match &msg.event {
                                tcfs_sync::StateEvent::FileSynced {
                                    rel_path,
                                    blake3,
                                    size,
                                    vclock: remote_vclock,
                                    manifest_path,
                                    ..
                                } => {
                                    info!(
                                        from_device = %event_device,
                                        path = %rel_path,
                                        hash = &blake3[..8.min(blake3.len())],
                                        size,
                                        mode = %conflict_mode,
                                        "remote file synced"
                                    );

                                    match conflict_mode.as_str() {
                                        "auto" => {
                                            handle_auto_pull(
                                                &device_id,
                                                &event_device,
                                                rel_path,
                                                blake3,
                                                remote_vclock,
                                                manifest_path,
                                                &operator,
                                                &state_cache,
                                                sync_root.as_deref(),
                                                &storage_prefix,
                                            )
                                            .await;
                                        }
                                        "interactive" => {
                                            info!(
                                                path = %rel_path,
                                                from = %event_device,
                                                "conflict queued for review"
                                            );
                                        }
                                        _ => {
                                            // defer or unknown — log and skip
                                        }
                                    }
                                }
                                tcfs_sync::StateEvent::ConflictResolved {
                                    rel_path,
                                    merged_vclock,
                                    ..
                                } => {
                                    info!(
                                        from_device = %event_device,
                                        path = %rel_path,
                                        "remote conflict resolved, merging vclock"
                                    );
                                    // Merge the resolved vclock into our local state
                                    let mut cache = state_cache.lock().await;
                                    let local_path = sync_root
                                        .as_ref()
                                        .map(|r| r.join(rel_path))
                                        .unwrap_or_else(|| std::path::PathBuf::from(rel_path));
                                    if let Some(entry) = cache.get(&local_path).cloned() {
                                        let mut updated_vclock = entry.vclock.clone();
                                        updated_vclock.merge(merged_vclock);
                                        let updated = tcfs_sync::state::SyncState {
                                            vclock: updated_vclock,
                                            ..entry
                                        };
                                        cache.set(&local_path, updated);
                                        let _ = cache.flush();
                                    }
                                }
                                tcfs_sync::StateEvent::DeviceOnline { device_id: did, .. } => {
                                    info!(device = %did, "remote device online");
                                }
                                tcfs_sync::StateEvent::DeviceOffline { device_id: did, .. } => {
                                    info!(device = %did, "remote device offline");
                                }
                                _ => {
                                    info!(
                                        event = %event_type,
                                        device = %event_device,
                                        "state event received"
                                    );
                                }
                            }

                            if let Err(e) = msg.ack().await {
                                warn!("ack state event failed: {e}");
                            }
                        }
                        Err(e) => {
                            warn!("state sync stream error: {e}");
                        }
                    }
                }
                info!("state sync loop ended");
            });
        }
        Err(e) => {
            warn!("failed to create state consumer: {e}");
        }
    }
}

/// Handle auto-pull logic for a remote FileSynced event.
#[allow(clippy::too_many_arguments)]
async fn handle_auto_pull(
    device_id: &str,
    remote_device: &str,
    rel_path: &str,
    remote_blake3: &str,
    remote_vclock: &tcfs_sync::conflict::VectorClock,
    manifest_path: &str,
    operator: &Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    sync_root: Option<&std::path::Path>,
    storage_prefix: &str,
) {
    // Determine local path for this rel_path
    let local_path = match sync_root {
        Some(root) => root.join(rel_path),
        None => {
            // Try to find in state cache by rel_path
            let cache = state_cache.lock().await;
            match cache.get_by_rel_path(rel_path) {
                Some((key, _)) => std::path::PathBuf::from(key),
                None => {
                    info!(
                        path = %rel_path,
                        "no sync_root configured and file not in state cache, skipping auto-pull"
                    );
                    return;
                }
            }
        }
    };

    // Compare vector clocks
    let (local_blake3, local_vclock) = {
        let cache = state_cache.lock().await;
        match cache.get(&local_path) {
            Some(entry) => (entry.blake3.clone(), entry.vclock.clone()),
            None => {
                // New file from remote — download it
                info!(path = %rel_path, from = %remote_device, "new file from remote, pulling");
                drop(cache);
                do_auto_download(
                    device_id,
                    manifest_path,
                    &local_path,
                    operator,
                    state_cache,
                    storage_prefix,
                )
                .await;
                return;
            }
        }
    };

    let outcome = tcfs_sync::conflict::compare_clocks(
        &local_vclock,
        remote_vclock,
        &local_blake3,
        remote_blake3,
        rel_path,
        device_id,
        remote_device,
    );

    match outcome {
        tcfs_sync::conflict::SyncOutcome::UpToDate => {
            info!(path = %rel_path, "already up to date");
        }
        tcfs_sync::conflict::SyncOutcome::LocalNewer => {
            info!(path = %rel_path, "local is newer, skipping pull");
        }
        tcfs_sync::conflict::SyncOutcome::RemoteNewer => {
            info!(path = %rel_path, from = %remote_device, "remote is newer, auto-pulling");
            do_auto_download(
                device_id,
                manifest_path,
                &local_path,
                operator,
                state_cache,
                storage_prefix,
            )
            .await;
        }
        tcfs_sync::conflict::SyncOutcome::Conflict(ref conflict_info) => {
            info!(
                path = %rel_path,
                local_device = %conflict_info.local_device,
                remote_device = %conflict_info.remote_device,
                "conflict detected, applying AutoResolver"
            );
            let resolver = tcfs_sync::conflict::AutoResolver;
            match resolver.resolve(conflict_info) {
                Some(tcfs_sync::conflict::Resolution::KeepLocal) => {
                    info!(path = %rel_path, "AutoResolver: keeping local");
                }
                Some(tcfs_sync::conflict::Resolution::KeepRemote) => {
                    info!(path = %rel_path, "AutoResolver: keeping remote");
                    do_auto_download(
                        device_id,
                        manifest_path,
                        &local_path,
                        operator,
                        state_cache,
                        storage_prefix,
                    )
                    .await;
                }
                _ => {
                    info!(path = %rel_path, "AutoResolver: deferred");
                }
            }
        }
    }
}

/// Download a file from remote and update state cache.
async fn do_auto_download(
    device_id: &str,
    manifest_path: &str,
    local_path: &std::path::Path,
    operator: &Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    storage_prefix: &str,
) {
    // Ensure parent directory exists
    if let Some(parent) = local_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %local_path.display(), "mkdir for auto-pull failed: {e}");
            return;
        }
    }

    let op = operator.lock().await;
    let op = match op.as_ref() {
        Some(op) => op.clone(),
        None => {
            warn!("no storage operator for auto-pull");
            return;
        }
    };
    drop(operator.lock().await);

    let result = {
        let mut cache = state_cache.lock().await;
        tcfs_sync::engine::download_file_with_device(
            &op,
            manifest_path,
            local_path,
            storage_prefix,
            None,
            device_id,
            Some(&mut cache),
        )
        .await
    };

    match result {
        Ok(dl) => {
            info!(
                path = %local_path.display(),
                bytes = dl.bytes,
                "auto-pull complete"
            );
            // Flush state cache
            let mut cache = state_cache.lock().await;
            let _ = cache.flush();
        }
        Err(e) => {
            warn!(
                path = %local_path.display(),
                "auto-pull failed: {e}"
            );
        }
    }
}

fn notify_ready() {
    // Send sd_notify(READY=1) to systemd if running as a service
    // Uses $NOTIFY_SOCKET env var; no-op if not set
    if let Ok(socket) = std::env::var("NOTIFY_SOCKET") {
        use std::os::unix::net::UnixDatagram;
        if let Ok(sock) = UnixDatagram::unbound() {
            let _ = sock.send_to(b"READY=1\n", &socket);
            tracing::debug!(notify_socket = %socket, "sent systemd READY=1");
        }
    }
}

fn notify_stopping() {
    if let Ok(socket) = std::env::var("NOTIFY_SOCKET") {
        use std::os::unix::net::UnixDatagram;
        if let Ok(sock) = UnixDatagram::unbound() {
            let _ = sock.send_to(b"STOPPING=1\n", &socket);
            tracing::debug!(notify_socket = %socket, "sent systemd STOPPING=1");
        }
    }
}
