//! Daemon lifecycle: startup, health checks, systemd notify, gRPC server

use anyhow::Result;
use secrecy::ExposeSecret;
use std::sync::Arc;
use tcfs_core::config::TcfsConfig;
use tcfs_sync::conflict::ConflictResolver;
use tracing::{error, info, warn};

use tcfs_crypto::MasterKey;

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

    // Auto-enroll this device on first run (with real age X25519 keypair)
    let device_id = if let Some(dev) = registry.find(&device_name) {
        info!(device = %device_name, id = %dev.device_id, "device identity loaded");
        dev.device_id.clone()
    } else {
        let identity = age::x25519::Identity::generate();
        let public_key = identity.to_public().to_string();
        let secret_key = identity.to_string();

        let id = registry.enroll(&device_name, &public_key, None);

        // Persist the device secret key alongside the registry
        let secret_key_path = registry_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(format!("device-{id}.age"));
        if let Err(e) = std::fs::write(&secret_key_path, secret_key.expose_secret().as_bytes()) {
            warn!("failed to write device secret key: {e}");
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &secret_key_path,
                    std::fs::Permissions::from_mode(0o600),
                );
            }
            info!(path = %secret_key_path.display(), "device secret key saved");
        }

        if let Err(e) = registry.save(&registry_path) {
            warn!("failed to save device registry: {e}");
        }
        info!(device = %device_name, id = %id, "device auto-enrolled with age keypair");
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

    // ── Load master key for E2E encryption ─────────────────────────────
    let master_key: Option<MasterKey> = if config.crypto.enabled {
        if let Some(ref key_path) = config.crypto.master_key_file {
            match std::fs::read(key_path) {
                Ok(bytes) if bytes.len() == tcfs_crypto::KEY_SIZE => {
                    let mut key_bytes = [0u8; tcfs_crypto::KEY_SIZE];
                    key_bytes.copy_from_slice(&bytes);
                    info!(path = %key_path.display(), "master key loaded");
                    Some(MasterKey::from_bytes(key_bytes))
                }
                Ok(bytes) => {
                    warn!(
                        path = %key_path.display(),
                        size = bytes.len(),
                        expected = tcfs_crypto::KEY_SIZE,
                        "master key file has wrong size, encryption disabled"
                    );
                    None
                }
                Err(e) => {
                    warn!(
                        path = %key_path.display(),
                        "failed to read master key file: {e} (encryption disabled)"
                    );
                    None
                }
            }
        } else {
            warn!("crypto.enabled = true but no master_key_file configured");
            None
        }
    } else {
        None
    };

    if config.crypto.enabled && master_key.is_some() {
        info!("E2E encryption: active");
    } else if config.crypto.enabled {
        warn!("E2E encryption: configured but master key unavailable");
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

    // Open state cache (wrapped in Arc<Mutex> for shared access)
    let state_cache = Arc::new(tokio::sync::Mutex::new(
        tcfs_sync::state::StateCache::open(&config.sync.state_db).unwrap_or_else(|e| {
            warn!("state cache open failed: {e}  (starting fresh)");
            tcfs_sync::state::StateCache::open(&std::path::PathBuf::from(
                "/tmp/tcfsd-state.db.json",
            ))
            .expect("fallback state cache")
        }),
    ));

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

    // Channel for status change events (consumed by D-Bus signal emitter on Linux)
    #[allow(unused_variables, unused_mut)]
    let (status_tx, mut status_rx) = tokio::sync::mpsc::channel::<(String, String)>(64);

    // ── File Watcher + Scheduler ─────────────────────────────────────
    // If sync_root is configured, start watching for local file changes
    // and feed them through the priority scheduler for automatic sync.
    let _watcher_handle = if let Some(ref sync_root) = config.sync.sync_root {
        if sync_root.exists() {
            let (watch_tx, mut watch_rx) = tokio::sync::mpsc::channel(256);
            let watcher_config = tcfs_sync::watcher::WatcherConfig::default();

            match tcfs_sync::watcher::FileWatcher::start(sync_root, watcher_config, watch_tx) {
                Ok(watcher) => {
                    info!(dir = %sync_root.display(), "file watcher active");

                    let scheduler = std::sync::Arc::new(tcfs_sync::scheduler::SyncScheduler::new(
                        tcfs_sync::scheduler::SchedulerConfig::default(),
                    ));
                    let scheduler_tx = scheduler.sender();

                    // Watcher → Scheduler bridge: convert watch events to sync tasks
                    tokio::spawn(async move {
                        while let Some(event) = watch_rx.recv().await {
                            let op = match event.kind {
                                tcfs_sync::watcher::WatchEventKind::Created
                                | tcfs_sync::watcher::WatchEventKind::Modified => {
                                    tcfs_sync::scheduler::SyncOp::Push
                                }
                                tcfs_sync::watcher::WatchEventKind::Deleted => {
                                    tcfs_sync::scheduler::SyncOp::Delete
                                }
                            };
                            let task = tcfs_sync::scheduler::SyncTask::new(
                                event.path,
                                op,
                                tcfs_sync::scheduler::Priority::Normal,
                            );
                            if scheduler_tx.send(task).await.is_err() {
                                break;
                            }
                        }
                    });

                    // Scheduler run loop: dispatch tasks to sync engine
                    let sched_operator = operator.clone();
                    let sched_state = state_cache.clone();
                    let sched_prefix = config.storage.bucket.clone();
                    let sched_device = device_id.clone();
                    let sched_sync_root = sync_root.clone();
                    let sched_status_tx = status_tx.clone();

                    tokio::spawn({
                        let scheduler = scheduler.clone();
                        async move {
                            scheduler
                                .run(move |task| {
                                    let op = sched_operator.clone();
                                    let state = sched_state.clone();
                                    let prefix = sched_prefix.clone();
                                    let device = sched_device.clone();
                                    let root = sched_sync_root.clone();
                                    let status_tx = sched_status_tx.clone();

                                    Box::pin(async move {
                                        match task.op {
                                            tcfs_sync::scheduler::SyncOp::Push => {
                                                let op_guard = op.lock().await;
                                                let op_ref =
                                                    op_guard.as_ref().ok_or_else(|| {
                                                        anyhow::anyhow!("no storage operator")
                                                    })?;
                                                let rel_path = task
                                                    .path
                                                    .strip_prefix(&root)
                                                    .unwrap_or(&task.path)
                                                    .to_string_lossy()
                                                    .to_string();
                                                let mut cache = state.lock().await;
                                                let upload_result =
                                                    tcfs_sync::engine::upload_file_with_device(
                                                        op_ref,
                                                        &task.path,
                                                        &prefix,
                                                        &mut cache,
                                                        None,
                                                        &device,
                                                        Some(&rel_path),
                                                        None,
                                                    )
                                                    .await?;

                                                // Record conflict in state cache if detected
                                                if let Some(
                                                    tcfs_sync::conflict::SyncOutcome::Conflict(
                                                        ref info,
                                                    ),
                                                ) = upload_result.outcome
                                                {
                                                    warn!(
                                                        path = %task.path.display(),
                                                        local_device = %info.local_device,
                                                        remote_device = %info.remote_device,
                                                        "watcher: conflict detected"
                                                    );
                                                    if let Some(entry) =
                                                        cache.get(&task.path).cloned()
                                                    {
                                                        let updated =
                                                            tcfs_sync::state::SyncState {
                                                                conflict: Some(info.clone()),
                                                                ..entry
                                                            };
                                                        cache.set(&task.path, updated);
                                                    }
                                                    // Emit status change for D-Bus listeners
                                                    let _ = status_tx.try_send((
                                                        task.path.to_string_lossy().to_string(),
                                                        "conflict".to_string(),
                                                    ));
                                                }

                                                let _ = cache.flush();
                                                info!(
                                                    path = %task.path.display(),
                                                    "watcher: auto-pushed"
                                                );
                                                Ok(())
                                            }
                                            tcfs_sync::scheduler::SyncOp::Delete => {
                                                // Remove from state cache on delete
                                                let mut cache = state.lock().await;
                                                cache.remove(&task.path);
                                                let _ = cache.flush();
                                                info!(
                                                    path = %task.path.display(),
                                                    "watcher: removed from state"
                                                );
                                                Ok(())
                                            }
                                            tcfs_sync::scheduler::SyncOp::Pull => {
                                                // Pull is handled by NATS events, not watcher
                                                Ok(())
                                            }
                                        }
                                    })
                                })
                                .await;
                        }
                    });

                    Some(watcher)
                }
                Err(e) => {
                    warn!(dir = %sync_root.display(), "file watcher failed to start: {e}");
                    None
                }
            }
        } else {
            info!(dir = %sync_root.display(), "sync_root does not exist, watcher disabled");
            None
        }
    } else {
        None
    };

    // Send systemd ready notification
    notify_ready();

    // ── D-Bus status interface (Linux only) ──────────────────────────────
    #[cfg(all(target_os = "linux", feature = "dbus"))]
    let _dbus_conn = {
        use tcfs_dbus::{StatusBackend, SyncStatus};

        /// Real D-Bus backend backed by daemon state.
        struct DaemonStatusBackend {
            state_cache: Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
            operator: Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
            device_id: String,
        }

        impl StatusBackend for DaemonStatusBackend {
            async fn get_status(&self, path: &str) -> SyncStatus {
                let cache = self.state_cache.lock().await;
                let p = std::path::Path::new(path);
                match cache.get(p) {
                    Some(entry) => {
                        if entry.conflict.is_some() {
                            SyncStatus::Conflict
                        } else {
                            SyncStatus::Synced
                        }
                    }
                    None => {
                        let op = self.operator.lock().await;
                        if op.is_none() {
                            SyncStatus::Unknown
                        } else {
                            SyncStatus::Placeholder
                        }
                    }
                }
            }

            async fn sync(&self, path: &str) -> anyhow::Result<()> {
                let op_guard = self.operator.lock().await;
                let op = op_guard
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no storage operator available"))?
                    .clone();
                drop(op_guard);

                info!(path, device = %self.device_id, "D-Bus sync requested");

                let p = std::path::Path::new(path);
                let mut cache = self.state_cache.lock().await;
                tcfs_sync::engine::upload_file_with_device(
                    &op,
                    p,
                    &self.device_id,
                    &mut cache,
                    None,
                )
                .await
                .map_err(|e| anyhow::anyhow!("sync failed: {e}"))?;
                let _ = cache.flush();
                Ok(())
            }

            async fn unsync(&self, path: &str) -> anyhow::Result<()> {
                info!(path, device = %self.device_id, "D-Bus unsync requested");
                let p = std::path::Path::new(path);
                let mut cache = self.state_cache.lock().await;
                cache.remove(p);
                let _ = cache.flush();

                // Remove local cached file (dehydrate)
                if p.exists() {
                    std::fs::remove_file(p).ok();
                }
                Ok(())
            }
        }

        let backend = DaemonStatusBackend {
            state_cache: state_cache.clone(),
            operator: operator.clone(),
            device_id: device_id.clone(),
        };

        match tcfs_dbus::serve(backend).await {
            Ok(conn) => {
                info!("D-Bus service started on session bus");

                // Spawn D-Bus signal emitter: reads status change events and
                // emits StatusChanged signals so Nautilus can update overlays.
                let dbus_conn = conn.clone();
                let mut status_rx = status_rx;
                tokio::spawn(async move {
                    while let Some((path, status)) = status_rx.recv().await {
                        tcfs_dbus::emit_status_changed(&dbus_conn, &path, &status).await;
                    }
                });

                Some(conn)
            }
            Err(e) => {
                warn!("D-Bus service failed to start: {e}");
                None
            }
        }
    };

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
        master_key,
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
            None,
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
