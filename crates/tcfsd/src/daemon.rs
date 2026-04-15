//! Daemon lifecycle: startup, health checks, systemd notify, gRPC server

use anyhow::Result;
use secrecy::ExposeSecret;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tcfs_core::config::TcfsConfig;
use tcfs_sync::conflict::ConflictResolver;
use tracing::{debug, error, info, warn};

use tcfs_crypto::MasterKey;

use crate::cred_store::{new_shared as new_cred_store, SharedCredStore};
use crate::grpc::TcfsDaemonImpl;

pub async fn run(config: TcfsConfig) -> Result<()> {
    info!("daemon starting");

    // Ensure all required directories exist before anything else
    ensure_dirs(&config);

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
        if dev.device_id.is_empty() {
            // Backfill device_id for entries created before UUID generation was added
            let new_id = registry
                .backfill_device_id(&device_name)
                .expect("backfill_device_id with valid device name");
            if let Err(e) = registry.save(&registry_path) {
                warn!("failed to save backfilled device registry: {e}");
            }
            info!(device = %device_name, id = %new_id, "backfilled missing device_id");
            new_id
        } else {
            info!(device = %device_name, id = %dev.device_id, "device identity loaded");
            dev.device_id.clone()
        }
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
        // Try master_key_file first
        let from_file = if let Some(ref key_path) = config.crypto.master_key_file {
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
                        "master key file has wrong size"
                    );
                    None
                }
                Err(e) => {
                    warn!(
                        path = %key_path.display(),
                        "failed to read master key file: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };

        // Fallback: derive from passphrase_file (auto-unlock)
        if from_file.is_some() {
            from_file
        } else if let Some(ref pf) = config.crypto.passphrase_file {
            match std::fs::read_to_string(pf) {
                Ok(passphrase) => {
                    let salt = config
                        .crypto
                        .kdf_salt
                        .as_deref()
                        .and_then(|s| (0..s.len())
                            .step_by(2)
                            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
                            .collect::<Result<Vec<u8>, _>>()
                            .ok())
                        .and_then(|b| <[u8; 16]>::try_from(b).ok())
                        .unwrap_or_else(|| {
                            warn!("no kdf_salt configured — generating ephemeral salt (key will differ across restarts!)");
                            tcfs_crypto::recovery::generate_passphrase_salt()
                        });
                    match tcfs_crypto::recovery::derive_from_passphrase(passphrase.trim(), &salt) {
                        Ok(mk) => {
                            info!(
                                path = %pf.display(),
                                "master key derived from passphrase file (auto-unlock)"
                            );
                            Some(mk)
                        }
                        Err(e) => {
                            warn!(
                                path = %pf.display(),
                                "passphrase key derivation failed: {e}"
                            );
                            None
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        path = %pf.display(),
                        "failed to read passphrase file: {e}"
                    );
                    None
                }
            }
        } else {
            if config.crypto.master_key_file.is_none() {
                warn!("crypto.enabled = true but no master_key_file or passphrase_file configured");
            }
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

    // Open state cache, purge stale entries, then wrap in Arc<Mutex>
    let state_json_path = config.sync.state_db.with_extension("json");
    let mut state_cache_inner = tcfs_sync::state::StateCache::open(&state_json_path)
        .unwrap_or_else(|e| {
            warn!("state cache open failed: {e}  (starting fresh)");
            tcfs_sync::state::StateCache::open(&std::path::PathBuf::from(
                "/tmp/tcfsd-state.db.json",
            ))
            .expect("fallback state cache")
        });

    // Purge entries with wrong remote prefix or stale tmp paths
    let resolved_prefix = config.storage.resolved_prefix();
    let purged = state_cache_inner.purge_stale(resolved_prefix);
    if purged > 0 {
        info!(
            purged,
            prefix = resolved_prefix,
            "purged stale state cache entries"
        );
        let _ = state_cache_inner.flush();
    }

    // ── Remote index discovery ────────────────────────────────────────────
    // Populate state cache with remote-only files so FileProvider can enumerate
    // the full tree, not just locally-synced files.
    if let Some(ref op) = operator {
        let sync_root = config
            .sync
            .sync_root
            .as_deref()
            .unwrap_or(std::path::Path::new("/tmp/tcfs"));
        match tcfs_sync::reconcile::list_remote_index(op, resolved_prefix).await {
            Ok(remote_index) => {
                let mut discovered = 0usize;
                for (rel_path, entry) in &remote_index {
                    let local_key = std::path::PathBuf::from(sync_root).join(rel_path);
                    if state_cache_inner.get(&local_key).is_none() {
                        state_cache_inner.set(
                            &local_key,
                            tcfs_sync::state::SyncState {
                                blake3: String::new(),
                                size: entry.size,
                                mtime: 0,
                                chunk_count: entry.chunks,
                                remote_path: format!(
                                    "{}/manifests/{}",
                                    resolved_prefix, entry.manifest_hash
                                ),
                                last_synced: 0,
                                vclock: Default::default(),
                                device_id: String::new(),
                                conflict: None,
                                status: tcfs_sync::state::FileSyncStatus::NotSynced,
                            },
                        );
                        discovered += 1;
                    }
                }
                if discovered > 0 {
                    info!(
                        discovered,
                        total = remote_index.len(),
                        "remote index discovery: added new entries to state cache"
                    );
                    let _ = state_cache_inner.flush();
                } else {
                    info!(
                        total = remote_index.len(),
                        "remote index discovery: cache already up to date"
                    );
                }
            }
            Err(e) => {
                warn!("remote index discovery failed (non-fatal): {e}");
            }
        }
    }

    let state_cache = Arc::new(tokio::sync::Mutex::new(state_cache_inner));

    // Wrap operator in Arc<Mutex> for shared access
    let operator = Arc::new(tokio::sync::Mutex::new(operator));

    // Shared NATS handle — created early so the watcher scheduler can publish events.
    // Starts as None; populated later when NATS connects (see set_nats call below).
    let shared_nats: Arc<tokio::sync::Mutex<Option<tcfs_sync::NatsClient>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Start Prometheus metrics + health check endpoint
    // Prometheus metrics — create registry + counters before starting server
    let mut metrics_registry = crate::metrics::Registry::default();
    let daemon_metrics = crate::metrics::DaemonMetrics::new(&mut metrics_registry);
    let metrics_registry = Arc::new(metrics_registry);

    let metrics_addr = config.daemon.metrics_addr.clone();
    if let Some(addr) = metrics_addr {
        let health_state = crate::metrics::HealthState {
            registry: metrics_registry.clone(),
            operator: operator.clone(),
            sync_root: config.sync.sync_root.clone(),
        };
        tokio::spawn(async move {
            if let Err(e) = crate::metrics::serve(addr, health_state).await {
                error!("metrics server failed: {e}");
            }
        });
    }

    // Set initial storage health
    if storage_ok {
        daemon_metrics.storage_health.set(1);
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

    // ── Blacklist (centralized exclusion filter) ───────────────────────
    let blacklist = tcfs_sync::blacklist::Blacklist::from_sync_config(&config.sync);
    info!(
        patterns = config.sync.exclude_patterns.len(),
        git_dirs = config.sync.sync_git_dirs,
        hidden_dirs = config.sync.sync_hidden_dirs,
        "blacklist configured"
    );

    // Per-path locks: prevent concurrent operations on the same file.
    // Shared across the watcher/scheduler and the state sync loop.
    let path_locks = tcfs_sync::state::PathLocks::new();

    // ── File Watcher + Scheduler ─────────────────────────────────────
    // If sync_root is configured, start watching for local file changes
    // and feed them through the priority scheduler for automatic sync.
    //
    // On macOS with FileProvider active, the watcher is skipped because
    // ~/Library/CloudStorage/TCFSProvider-TCFS/ is the primary interface.
    // The FileProvider extension handles uploads/downloads via gRPC RPCs.
    let fileprovider_active =
        cfg!(target_os = "macos") && config.daemon.fileprovider_socket.is_some();

    let _watcher_handle = if let Some(ref sync_root) = config.sync.sync_root {
        if fileprovider_active {
            info!(
                dir = %sync_root.display(),
                "FileProvider active — sync_root watcher disabled. Use CloudStorage."
            );
            None
        } else if sync_root.exists() {
            let (watch_tx, mut watch_rx) = tokio::sync::mpsc::channel(256);
            let watcher_config = tcfs_sync::watcher::WatcherConfig::default();

            match tcfs_sync::watcher::FileWatcher::start(sync_root, watcher_config, watch_tx) {
                Ok(watcher) => {
                    info!(dir = %sync_root.display(), "file watcher active");

                    let scheduler = std::sync::Arc::new(tcfs_sync::scheduler::SyncScheduler::new(
                        tcfs_sync::scheduler::SchedulerConfig::default(),
                    ));
                    let scheduler_tx = scheduler.sender();

                    // Watcher → Scheduler bridge: convert watch events to sync tasks.
                    // Consults the blacklist to filter excluded paths before scheduling.
                    let bridge_blacklist = blacklist.clone();
                    let bridge_policy_store = {
                        let policy_path = sync_root.join(".tcfs-policy.json");
                        tcfs_sync::policy::PolicyStore::open(&policy_path).unwrap_or_default()
                    };
                    tokio::spawn(async move {
                        while let Some(event) = watch_rx.recv().await {
                            // Check blacklist before scheduling
                            let filename = event
                                .path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("");
                            let is_dir = event.path.is_dir();
                            if let Some(reason) = bridge_blacklist.check_name(filename, is_dir) {
                                debug!(
                                    path = %event.path.display(),
                                    reason = %reason,
                                    "watcher: skipped (blacklisted)"
                                );
                                continue;
                            }

                            // Check per-folder policy (Never = skip)
                            if let Some(policy) = bridge_policy_store.get(&event.path) {
                                if policy.sync_mode == tcfs_sync::policy::SyncMode::Never {
                                    debug!(
                                        path = %event.path.display(),
                                        "watcher: skipped (Never policy)"
                                    );
                                    continue;
                                }
                            }

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
                    let sched_prefix = config.storage.resolved_prefix().to_string();
                    let sched_device = device_id.clone();
                    let sched_sync_root = sync_root.clone();
                    let sched_status_tx = status_tx.clone();
                    let sched_nats = shared_nats.clone();
                    let sched_metrics = daemon_metrics.clone();
                    let sched_master_key = master_key.clone();
                    let sched_path_locks = path_locks.clone();

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
                                    let nats = sched_nats.clone();
                                    let metrics = sched_metrics.clone();
                                    let mk = sched_master_key.clone();
                                    let locks = sched_path_locks.clone();

                                    Box::pin(async move {
                                        // Acquire per-path lock to prevent concurrent operations
                                        let _lock_guard = locks.lock(&task.path).await;

                                        match task.op {
                                            tcfs_sync::scheduler::SyncOp::Push => {
                                                // Skip directories — only push regular files
                                                if task.path.is_dir() {
                                                    return Ok(());
                                                }
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

                                                // Set status = Active while uploading
                                                if let Some(entry) = cache.get(&task.path).cloned() {
                                                    let active = tcfs_sync::state::SyncState {
                                                        status: tcfs_sync::state::FileSyncStatus::Active,
                                                        ..entry
                                                    };
                                                    cache.set(&task.path, active);
                                                }

                                                let enc_ctx = mk.as_ref().map(|k| tcfs_sync::engine::EncryptionContext {
                                                    master_key: k.clone(),
                                                });
                                                let upload_result =
                                                    tcfs_sync::engine::upload_file_with_device(
                                                        op_ref,
                                                        &task.path,
                                                        &prefix,
                                                        &mut cache,
                                                        None,
                                                        &device,
                                                        Some(&rel_path),
                                                        enc_ctx.as_ref(),
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
                                                    metrics.sync_conflicts.inc();
                                                    if let Some(entry) =
                                                        cache.get(&task.path).cloned()
                                                    {
                                                        let updated = tcfs_sync::state::SyncState {
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

                                                // Set status = Synced after successful upload
                                                if let Some(entry) = cache.get(&task.path).cloned() {
                                                    let synced = tcfs_sync::state::SyncState {
                                                        status: tcfs_sync::state::FileSyncStatus::Synced,
                                                        ..entry
                                                    };
                                                    cache.set(&task.path, synced);
                                                }

                                                if let Err(e) = cache.flush() {
                                                    warn!(error = %e, "state cache flush failed");
                                                }
                                                info!(
                                                    path = %task.path.display(),
                                                    "watcher: auto-pushed"
                                                );
                                                metrics.files_pushed.inc();

                                                // Publish NATS event so other hosts learn about the change
                                                let rel_path = task
                                                    .path
                                                    .strip_prefix(&root)
                                                    .unwrap_or(&task.path)
                                                    .to_string_lossy()
                                                    .to_string();
                                                let nats_device = device.clone();
                                                let nats_hash = upload_result.hash.clone();
                                                let nats_size = upload_result.bytes;
                                                let nats_remote = upload_result.remote_path.clone();
                                                let nats_handle = nats.clone();
                                                let pub_metrics = metrics.clone();
                                                tokio::spawn(async move {
                                                    if let Some(client) = nats_handle.lock().await.as_ref() {
                                                        let event = tcfs_sync::StateEvent::FileSynced {
                                                            device_id: nats_device,
                                                            rel_path,
                                                            blake3: nats_hash,
                                                            size: nats_size,
                                                            vclock: Default::default(),
                                                            manifest_path: nats_remote,
                                                            timestamp: tcfs_sync::StateEvent::now(),
                                                        };
                                                        if let Err(e) = client.publish_state_event(&event).await {
                                                            tracing::warn!("watcher: failed to publish NATS event: {e}");
                                                        } else {
                                                            pub_metrics.nats_events_published.inc();
                                                        }
                                                    }
                                                });

                                                Ok(())
                                            }
                                            tcfs_sync::scheduler::SyncOp::Delete => {
                                                // Remove from state cache on delete
                                                let mut cache = state.lock().await;
                                                cache.remove(&task.path);
                                                if let Err(e) = cache.flush() {
                                                    warn!(error = %e, "state cache flush failed");
                                                }
                                                info!(
                                                    path = %task.path.display(),
                                                    "watcher: removed from state"
                                                );

                                                // Publish NATS delete event
                                                let rel_path = task
                                                    .path
                                                    .strip_prefix(&root)
                                                    .unwrap_or(&task.path)
                                                    .to_string_lossy()
                                                    .to_string();
                                                let nats_device = device.clone();
                                                let nats_handle = nats.clone();
                                                let del_metrics = metrics.clone();
                                                tokio::spawn(async move {
                                                    if let Some(client) = nats_handle.lock().await.as_ref() {
                                                        let event = tcfs_sync::StateEvent::FileDeleted {
                                                            device_id: nats_device,
                                                            rel_path,
                                                            vclock: Default::default(),
                                                            timestamp: tcfs_sync::StateEvent::now(),
                                                        };
                                                        if let Err(e) = client.publish_state_event(&event).await {
                                                            tracing::warn!("watcher: failed to publish delete event: {e}");
                                                        } else {
                                                            del_metrics.nats_events_published.inc();
                                                        }
                                                    }
                                                });

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
                if let Err(e) = cache.flush() {
                    warn!(error = %e, "state cache flush failed");
                }
                Ok(())
            }

            async fn unsync(&self, path: &str) -> anyhow::Result<()> {
                info!(path, device = %self.device_id, "D-Bus unsync requested");
                let p = std::path::Path::new(path);
                let mut cache = self.state_cache.lock().await;
                cache.remove(p);
                if let Err(e) = cache.flush() {
                    warn!(error = %e, "state cache flush failed");
                }

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
    let fp_socket_path = config.daemon.fileprovider_socket.clone();
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

    // Load persisted auth credentials (best-effort)
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("tcfsd");
    let totp_cred_path = data_dir.join("totp-credentials.json");
    if totp_cred_path.exists() {
        if let Err(e) = impl_.load_totp_credentials(&totp_cred_path).await {
            warn!("failed to load TOTP credentials: {e}");
        }
    }
    let session_path = data_dir.join("sessions.json");
    if session_path.exists() {
        if let Err(e) = impl_.load_sessions(&session_path).await {
            warn!("failed to load sessions: {e}");
        }
    }

    // Connect to NATS for fleet state sync (non-blocking, best-effort)
    let nats_url = &config.sync.nats_url;
    // Config.toml is authoritative when nats_url is explicitly set.
    // TCFS_NATS_URL env var only overrides the default (localhost:4222).
    let is_default = nats_url == "nats://localhost:4222";
    if !is_default || std::env::var("TCFS_NATS_URL").is_ok() {
        let url = if is_default {
            std::env::var("TCFS_NATS_URL").unwrap_or_else(|_| nats_url.clone())
        } else {
            if let Ok(ref env_url) = std::env::var("TCFS_NATS_URL") {
                if env_url != nats_url {
                    warn!(
                        config_url = %nats_url,
                        env_url = %env_url,
                        "TCFS_NATS_URL env var differs from config — using config value"
                    );
                }
            }
            nats_url.clone()
        };
        match tcfs_sync::NatsClient::connect(
            &url,
            config.sync.nats_tls,
            config.sync.nats_token.as_deref(),
        )
        .await
        {
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
                    let storage_prefix = config.storage.resolved_prefix().to_string();
                    let policy_path = data_dir.join("folder-policies.json");
                    let download_threshold = config.sync.auto_download_threshold;
                    spawn_state_sync_loop(
                        &nats,
                        &sync_device_id,
                        &sync_conflict_mode,
                        operator.clone(),
                        impl_.state_cache_handle(),
                        sync_root,
                        storage_prefix,
                        impl_.vfs_handle.clone(),
                        path_locks.clone(),
                        policy_path.clone(),
                        download_threshold,
                    )
                    .await;

                    // Populate shared NATS handle for watcher scheduler
                    *shared_nats.lock().await = Some(nats.clone());
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

    // Periodic session cleanup (every 5 minutes)
    {
        let store = impl_.session_store();
        let cleanup_session_path = data_dir.join("sessions.json");
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let before = store.active_count().await;
                store.cleanup_expired().await;
                let after = store.active_count().await;
                if before != after {
                    info!(
                        expired = before - after,
                        remaining = after,
                        "cleaned up expired sessions"
                    );
                    if let Err(e) = store.save_to_file(&cleanup_session_path).await {
                        warn!("failed to persist sessions after cleanup: {e}");
                    }
                }
            }
        });
    }

    // ── Periodic reconciliation ────────────────────────────────────────
    // Reconciles local sync_root against remote index on a timer.
    // Respects per-folder policies (Always/OnDemand/Never).
    if config.sync.reconcile_interval_secs > 0 {
        if let Some(ref sync_root) = config.sync.sync_root {
            let recon_interval = config.sync.reconcile_interval_secs;
            let recon_root = sync_root.clone();
            let recon_prefix = config.storage.resolved_prefix().to_string();
            let recon_state = impl_.state_cache_handle();
            let recon_op = operator.clone();
            let recon_device = device_id.clone();
            let recon_master_key = impl_.master_key_handle();
            let orphan_chunk_cleanup_grace_secs = config.sync.orphan_chunk_cleanup_grace_secs;
            let orphan_chunk_cleanup_sweep_interval_secs = if orphan_chunk_cleanup_grace_secs > 0 {
                orphan_chunk_cleanup_grace_secs
                    .min(3600)
                    .max(recon_interval)
            } else {
                0
            };
            let _recon_policy_path = data_dir.join("folder-policies.json");

            info!(
                interval_secs = recon_interval,
                prefix = %recon_prefix,
                root = %recon_root.display(),
                "periodic reconciliation enabled"
            );
            if orphan_chunk_cleanup_sweep_interval_secs > 0 {
                info!(
                    grace_secs = orphan_chunk_cleanup_grace_secs,
                    sweep_interval_secs = orphan_chunk_cleanup_sweep_interval_secs,
                    "periodic orphan chunk cleanup enabled"
                );
            }

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(recon_interval));
                let orphan_chunk_cleanup_grace =
                    Duration::from_secs(orphan_chunk_cleanup_grace_secs);
                let orphan_chunk_cleanup_sweep_interval =
                    Duration::from_secs(orphan_chunk_cleanup_sweep_interval_secs);
                let mut last_orphan_chunk_sweep = None;
                // Skip the first immediate tick — startup index discovery covers it
                interval.tick().await;

                loop {
                    interval.tick().await;

                    let op_guard = recon_op.lock().await;
                    let op = match op_guard.as_ref() {
                        Some(op) => op.clone(),
                        None => {
                            debug!("reconcile: no storage operator, skipping");
                            continue;
                        }
                    };
                    drop(op_guard);

                    let blacklist = tcfs_sync::blacklist::Blacklist::default();
                    let recon_config = tcfs_sync::reconcile::ReconcileConfig::default();

                    let cache = recon_state.lock().await;
                    let plan = match tcfs_sync::reconcile::reconcile(
                        &op,
                        &recon_root,
                        &recon_prefix,
                        &cache,
                        &recon_device,
                        &blacklist,
                        &recon_config,
                    )
                    .await
                    {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("periodic reconcile failed: {e}");
                            continue;
                        }
                    };
                    drop(cache);

                    let s = &plan.summary;
                    if s.pushes == 0 && s.pulls == 0 && s.conflicts == 0 {
                        debug!(up_to_date = s.up_to_date, "reconcile: nothing to do");
                    } else {
                        info!(
                            pushes = s.pushes,
                            pulls = s.pulls,
                            conflicts = s.conflicts,
                            up_to_date = s.up_to_date,
                            "reconcile: executing plan"
                        );

                        // Build encryption context from master key (if loaded)
                        let mk_guard = recon_master_key.lock().await;
                        let enc_ctx =
                            mk_guard
                                .as_ref()
                                .map(|k| tcfs_sync::engine::EncryptionContext {
                                    master_key: k.clone(),
                                });
                        drop(mk_guard);

                        let mut cache = recon_state.lock().await;
                        match tcfs_sync::reconcile::execute_plan(
                            &plan,
                            &op,
                            &recon_root,
                            &recon_prefix,
                            &mut cache,
                            &recon_device,
                            enc_ctx.as_ref(),
                            None,
                        )
                        .await
                        {
                            Ok(result) => {
                                info!(
                                    pushed = result.pushed,
                                    pulled = result.pulled,
                                    errors = result.errors.len(),
                                    bytes_up = result.bytes_uploaded,
                                    bytes_down = result.bytes_downloaded,
                                    "reconcile: plan executed"
                                );
                                if let Err(e) = cache.flush() {
                                    warn!("reconcile: state cache flush failed: {e}");
                                }
                            }
                            Err(e) => {
                                warn!("reconcile: plan execution failed: {e}");
                            }
                        }
                    }

                    let now = SystemTime::now();
                    let should_sweep_orphan_chunks = orphan_chunk_cleanup_sweep_interval_secs > 0
                        && last_orphan_chunk_sweep
                            .and_then(|last| now.duration_since(last).ok())
                            .map(|elapsed| elapsed >= orphan_chunk_cleanup_sweep_interval)
                            .unwrap_or(true);

                    if should_sweep_orphan_chunks {
                        match tcfs_sync::reconcile::cleanup_orphaned_chunks(
                            &op,
                            &recon_prefix,
                            orphan_chunk_cleanup_grace,
                            now,
                        )
                        .await
                        {
                            Ok(report) => {
                                last_orphan_chunk_sweep = Some(now);
                                if report.orphaned_chunks_found == 0
                                    && report.delete_errors.is_empty()
                                {
                                    debug!(
                                        scanned = report.scanned_chunks,
                                        "reconcile: no orphaned chunks found"
                                    );
                                } else {
                                    info!(
                                        orphaned_found = report.orphaned_chunks_found,
                                        deleted = report.deleted_chunks.len(),
                                        within_grace = report.skipped_within_grace.len(),
                                        missing_last_modified =
                                            report.skipped_missing_last_modified.len(),
                                        delete_errors = report.delete_errors.len(),
                                        scanned = report.scanned_chunks,
                                        referenced = report.referenced_chunks,
                                        "reconcile: orphan chunk sweep completed"
                                    );
                                }
                            }
                            Err(e) => {
                                warn!("reconcile: orphan chunk cleanup failed: {e}");
                            }
                        }
                    }
                }
            });
        }
    }

    // Auto-unsync sweep with dehydration (if configured)
    if config.sync.auto_unsync_max_age_secs > 0 {
        let unsync_state = impl_.state_cache_handle();
        let unsync_interval = config.sync.auto_unsync_interval_secs;
        let unsync_max_age = config.sync.auto_unsync_max_age_secs;
        let unsync_dry_run = config.sync.auto_unsync_dry_run;
        let unsync_policy_path = data_dir.join("folder-policies.json");
        let unsync_vfs = impl_.vfs_handle.clone();
        let disk_pressure_pct = config.sync.auto_unsync_disk_pressure_pct;
        let max_per_sweep = config.sync.auto_unsync_max_per_sweep;
        let sync_root = config.sync.sync_root.clone();

        info!(
            max_age_secs = unsync_max_age,
            interval_secs = unsync_interval,
            dry_run = unsync_dry_run,
            disk_pressure_pct,
            max_per_sweep,
            "auto-unsync enabled"
        );

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(unsync_interval));
            loop {
                interval.tick().await;

                // Check disk pressure — if under threshold and not time-based, skip
                let under_pressure = sync_root.as_ref().is_some_and(|root| {
                    tcfs_sync::auto_unsync::disk_pressure_check(root, disk_pressure_pct)
                });

                let policy_store =
                    tcfs_sync::policy::PolicyStore::open(&unsync_policy_path).unwrap_or_default();
                let mut cache = unsync_state.lock().await;

                if unsync_dry_run {
                    // Dry-run: use the original sweep (doesn't call callbacks)
                    let result = tcfs_sync::auto_unsync::sweep(
                        &mut cache,
                        &policy_store,
                        unsync_max_age,
                        true,
                    );
                    if result.unsynced > 0 || result.skipped_dirty > 0 {
                        info!(
                            scanned = result.scanned,
                            unsynced = result.unsynced,
                            skipped_exempt = result.skipped_exempt,
                            skipped_dirty = result.skipped_dirty,
                            bytes_reclaimed = result.bytes_reclaimed,
                            "auto-unsync dry-run sweep complete"
                        );
                    }
                } else {
                    // Real dehydration sweep — use VFS unsync_path if available
                    let vfs_ref = unsync_vfs.borrow().clone();
                    let effective_max_age = if under_pressure {
                        // Under disk pressure: halve the max age for more aggressive eviction
                        unsync_max_age / 2
                    } else {
                        unsync_max_age
                    };

                    let result = tcfs_sync::auto_unsync::sweep_with_dehydration(
                        &mut cache,
                        &policy_store,
                        effective_max_age,
                        max_per_sweep,
                        |path| {
                            let vfs_opt = vfs_ref.clone();
                            async move {
                                if let Some(ref vfs) = vfs_opt {
                                    let ur = vfs.unsync_path(&path).await?;
                                    Ok(ur.bytes_freed)
                                } else {
                                    Ok(0) // No VFS, just update state
                                }
                            }
                        },
                    )
                    .await;

                    if result.dehydrated > 0 || result.failed > 0 {
                        info!(
                            scanned = result.scanned,
                            dehydrated = result.dehydrated,
                            skipped_exempt = result.skipped_exempt,
                            skipped_dirty = result.skipped_dirty,
                            skipped_missing = result.skipped_missing,
                            failed = result.failed,
                            bytes_freed = result.bytes_freed,
                            under_pressure,
                            "auto-unsync dehydration sweep complete"
                        );
                    }
                }
            }
        });
    }

    info!(socket = %socket_path.display(), "gRPC: listening");
    if let Some(ref fp) = fp_socket_path {
        info!(socket = %fp.display(), "gRPC: FileProvider socket");
    }

    crate::grpc::serve(
        &socket_path,
        fp_socket_path.as_deref(),
        impl_,
        shutdown_signal,
    )
    .await?;

    // Clean up socket files
    let _ = tokio::fs::remove_file(&socket_path).await;
    if let Some(ref fp) = fp_socket_path {
        let _ = tokio::fs::remove_file(fp).await;
    }

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
    vfs_handle: tokio::sync::watch::Receiver<Option<std::sync::Arc<tcfs_vfs::TcfsVfs>>>,
    path_locks: tcfs_sync::state::PathLocks,
    policy_path: std::path::PathBuf,
    auto_download_threshold: u64,
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

                                    // Check folder policy before auto-pulling
                                    let policy_store =
                                        tcfs_sync::policy::PolicyStore::open(&policy_path)
                                            .unwrap_or_default();
                                    let file_path = sync_root
                                        .as_ref()
                                        .map(|r| r.join(rel_path))
                                        .unwrap_or_else(|| std::path::PathBuf::from(rel_path));
                                    let effective_mode = policy_store.effective_mode(&file_path);

                                    // Never-mode paths are completely ignored
                                    if effective_mode == tcfs_sync::policy::SyncMode::Never {
                                        debug!(
                                            path = %rel_path,
                                            "skipping auto-pull: folder policy is Never"
                                        );
                                        if let Err(e) = msg.ack().await {
                                            warn!("ack failed: {e}");
                                        }
                                        continue;
                                    }

                                    // OnDemand mode: only auto-pull if size ≤ threshold
                                    if effective_mode == tcfs_sync::policy::SyncMode::OnDemand
                                        && !policy_store.should_auto_download(
                                            &file_path,
                                            *size,
                                            auto_download_threshold,
                                        )
                                    {
                                        debug!(
                                            path = %rel_path,
                                            size,
                                            threshold = auto_download_threshold,
                                            "skipping auto-pull: OnDemand file exceeds download threshold"
                                        );
                                        if let Err(e) = msg.ack().await {
                                            warn!("ack failed: {e}");
                                        }
                                        continue;
                                    }

                                    // Always mode: unconditional auto-pull
                                    // OnDemand mode (under threshold): conditional auto-pull
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

                                            // Invalidate FUSE negative cache so the
                                            // new file appears in readdir immediately
                                            if let Some(ref vfs) = *vfs_handle.borrow() {
                                                let vpath = format!("/{}", rel_path);
                                                vfs.invalidate_path(&vpath);
                                                debug!(path = %vpath, "FUSE negative cache invalidated for remote file");
                                            }
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
                                        if let Err(e) = cache.flush() {
                                            warn!(error = %e, "state cache flush failed");
                                        }
                                    }
                                }
                                tcfs_sync::StateEvent::FileDeleted {
                                    rel_path,
                                    vclock: _remote_vclock,
                                    ..
                                } => {
                                    info!(
                                        from_device = %event_device,
                                        path = %rel_path,
                                        "remote file deleted"
                                    );

                                    // Determine local path
                                    let local_path = sync_root
                                        .as_ref()
                                        .map(|r| r.join(rel_path))
                                        .unwrap_or_else(|| std::path::PathBuf::from(rel_path));

                                    // Guard: defer delete if the file is locked by an active operation
                                    if path_locks.is_locked(&local_path).await {
                                        warn!(path = %local_path.display(), "deferring remote delete: file is locked by active operation");
                                        continue;
                                    }

                                    // Remove local file if it exists
                                    if local_path.exists() {
                                        if let Err(e) = tokio::fs::remove_file(&local_path).await {
                                            warn!(
                                                path = %local_path.display(),
                                                "failed to remove local file for remote delete: {e}"
                                            );
                                        } else {
                                            info!(
                                                path = %local_path.display(),
                                                from_device = %event_device,
                                                "removed local file (remote delete)"
                                            );
                                        }
                                    }

                                    // Remove from state cache and merge vclock
                                    let mut cache = state_cache.lock().await;
                                    cache.remove(&local_path);
                                    // Also try by rel_path (handles path normalization)
                                    if let Some((key, _)) = cache.get_by_rel_path(rel_path) {
                                        let key_owned = key.to_string();
                                        cache.remove(std::path::Path::new(&key_owned));
                                    }
                                    if let Err(e) = cache.flush() {
                                        warn!(error = %e, "state cache flush failed");
                                    }

                                    // Invalidate FUSE cache so the file disappears
                                    if let Some(ref vfs) = *vfs_handle.borrow() {
                                        let vpath = format!("/{}", rel_path);
                                        vfs.invalidate_path(&vpath);
                                        debug!(path = %vpath, "FUSE cache invalidated for deleted file");
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
            // Guard: defer auto-pull if file is actively being modified
            {
                let cache = state_cache.lock().await;
                if let Some(entry) = cache.get(&local_path) {
                    if entry.status == tcfs_sync::state::FileSyncStatus::Active {
                        info!(path = %rel_path, "deferring auto-pull: file is actively being modified");
                        return;
                    }
                }
            }
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

/// Handle auto-pull for a remote file sync event.
///
/// Index-first strategy: does NOT download files to local disk. The push
/// from the remote host already wrote index + manifest + chunks to S3.
/// The FUSE mount discovers new files via readdir (S3 index listing) and
/// hydrates on demand when the user opens them. This avoids writing to
/// the FUSE mount (which may be read-only from the daemon's perspective)
/// and eliminates the EROFS errors that occurred when sync_root was the
/// FUSE mountpoint.
///
/// We only update the state cache so vector clocks stay in sync.
async fn do_auto_download(
    _device_id: &str,
    manifest_path: &str,
    local_path: &std::path::Path,
    operator: &Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    _storage_prefix: &str,
) {
    // Verify the manifest exists in S3 (confirms push completed)
    let op = {
        let guard = operator.lock().await;
        match guard.as_ref() {
            Some(op) => op.clone(),
            None => {
                warn!("no storage operator for auto-pull verification");
                return;
            }
        }
    };

    match op.read(manifest_path).await {
        Ok(manifest_data) => {
            // Parse manifest to extract file hash and vclock for state cache
            let manifest_bytes = manifest_data.to_bytes();
            match tcfs_sync::manifest::SyncManifest::from_bytes(&manifest_bytes) {
                Ok(manifest) => {
                    info!(
                        path = %local_path.display(),
                        manifest = %manifest_path,
                        hash = %manifest.file_hash,
                        written_by = %manifest.written_by,
                        "auto-pull: S3 data verified, updating state cache"
                    );
                    // Update state cache with the remote's metadata so
                    // vector clocks stay in sync for future conflict detection.
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let mut cache = state_cache.lock().await;
                    cache.set(
                        local_path,
                        tcfs_sync::state::SyncState {
                            blake3: manifest.file_hash.clone(),
                            size: manifest.file_size,
                            mtime: manifest.written_at,
                            chunk_count: manifest.chunks.len(),
                            remote_path: manifest_path.to_string(),
                            last_synced: now,
                            vclock: manifest.vclock.clone(),
                            device_id: manifest.written_by.clone(),
                            conflict: None,
                            status: tcfs_sync::state::FileSyncStatus::Synced,
                        },
                    );
                    if let Err(e) = cache.flush() {
                        warn!(error = %e, "state cache flush failed");
                    }
                    debug!(
                        key = %local_path.display(),
                        hash = %manifest.file_hash,
                        "auto-pull: state cache updated with remote metadata"
                    );
                }
                Err(e) => {
                    warn!(
                        manifest = %manifest_path,
                        "auto-pull: failed to parse manifest: {e}"
                    );
                    // Still mark as verified even if parse fails
                    let mut cache = state_cache.lock().await;
                    if let Err(e) = cache.flush() {
                        warn!(error = %e, "state cache flush failed");
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                path = %local_path.display(),
                manifest = %manifest_path,
                "auto-pull: manifest not found in S3: {e}"
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

/// Create directories needed by the daemon at startup.
fn ensure_dirs(config: &TcfsConfig) {
    // Socket parent directory
    if let Some(parent) = config.daemon.socket.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %parent.display(), "failed to create socket dir: {e}");
        }
    }

    // State cache parent directory
    if let Some(parent) = config.sync.state_db.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %parent.display(), "failed to create state dir: {e}");
        }
    }

    // FUSE cache directory
    if let Err(e) = std::fs::create_dir_all(&config.fuse.cache_dir) {
        warn!(path = %config.fuse.cache_dir.display(), "failed to create cache dir: {e}");
    }

    // sync_root (mount target)
    if let Some(ref root) = config.sync.sync_root {
        if let Err(e) = std::fs::create_dir_all(root) {
            warn!(path = %root.display(), "failed to create sync_root dir: {e}");
        }
    }

    // FileProvider App Group Container directory (macOS)
    if let Some(ref fp_socket) = config.daemon.fileprovider_socket {
        if let Some(parent) = fp_socket.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(path = %parent.display(), "failed to create FileProvider socket dir: {e}");
            }
        }
    }
}
