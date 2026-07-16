//! Daemon lifecycle: startup, health checks, systemd notify, gRPC server

use anyhow::{Context, Result};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tcfs_core::config::TcfsConfig;
use tcfs_sync::conflict::ConflictResolver;
use tracing::{debug, error, info, warn};

use tcfs_crypto::MasterKey;

use crate::cred_store::{new_shared as new_cred_store, SharedCredStore};
use crate::grpc::TcfsDaemonImpl;

const REMOTE_INDEX_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Record a watcher-detected conflict in the state cache, inserting a synthetic
/// entry when the cache has no prior record for `path`.
///
/// `StateCache::mark_conflict` returns `false` when the entry is missing. The
/// watcher path sees files that were never registered (e.g. created and
/// conflict-detected in a single tick), so we must compensate by inserting a
/// minimal `SyncState` carrying the conflict payload. Without this, the
/// conflict metadata is silently lost — the metric increments but the file
/// never surfaces in conflict UIs or gRPC status queries.
///
/// Mirrors the compensating logic used on the gRPC push path.
pub fn watcher_record_conflict(
    cache: &mut tcfs_sync::state::StateCache,
    path: &std::path::Path,
    info: tcfs_sync::conflict::ConflictInfo,
) {
    if cache.mark_conflict(path, info.clone()) {
        return;
    }

    warn!(
        path = %path.display(),
        "watcher: mark_conflict found no cache entry; inserting synthetic Conflict record"
    );

    let remote_path = info.rel_path.clone();
    let local_blake3 = info.local_blake3.clone();
    let local_vclock = info.local_vclock.clone();
    let local_device = info.local_device.clone();
    let detected_at = info.detected_at;

    let synthetic = tcfs_sync::state::SyncState {
        blake3: local_blake3,
        size: 0,
        mtime: 0,
        chunk_count: 0,
        remote_path,
        last_synced: detected_at,
        vclock: local_vclock,
        device_id: local_device,
        conflict: Some(info),
        status: tcfs_sync::state::FileSyncStatus::Conflict,
    };
    cache.set(path, synthetic);
}

/// Re-export of watcher helpers intended for tests.
///
/// Integration tests under `crates/tcfsd/tests/` compile as external crates
/// and need a stable, public path to the helpers they exercise. Keep this
/// namespace intentionally narrow — add new items only when a test demands
/// them.
pub mod test_support {
    pub use super::watcher_record_conflict;
}

async fn load_invite_redemptions_for_startup(
    daemon: &TcfsDaemonImpl,
    path: &std::path::Path,
) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting invite redemption store: {}", path.display()))
        }
    }

    daemon
        .load_invite_redemptions(path)
        .await
        .with_context(|| format!("loading invite redemption store: {}", path.display()))
}

fn open_automatic_policy_store(
    path: &std::path::Path,
    surface: &str,
) -> Result<tcfs_sync::policy::PolicyStore> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "folder policy for {surface} must be a regular, non-symlink file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspecting folder policy for {surface}: {}", path.display())
            })
        }
    }
    tcfs_sync::policy::PolicyStore::open(path).with_context(|| {
        format!(
            "loading folder policy for {surface} automatic mutation: {}",
            path.display()
        )
    })
}

fn reconcile_action_rel_path(action: &tcfs_sync::reconcile::ReconcileAction) -> &str {
    match action {
        tcfs_sync::reconcile::ReconcileAction::Push { rel_path, .. }
        | tcfs_sync::reconcile::ReconcileAction::Pull { rel_path, .. }
        | tcfs_sync::reconcile::ReconcileAction::DeleteLocal { rel_path, .. }
        | tcfs_sync::reconcile::ReconcileAction::DeleteRemote { rel_path }
        | tcfs_sync::reconcile::ReconcileAction::Conflict { rel_path, .. }
        | tcfs_sync::reconcile::ReconcileAction::CreateDirectory { rel_path }
        | tcfs_sync::reconcile::ReconcileAction::UpToDate { rel_path } => rel_path,
    }
}

fn reconcile_action_mutates(action: &tcfs_sync::reconcile::ReconcileAction) -> bool {
    !matches!(
        action,
        tcfs_sync::reconcile::ReconcileAction::UpToDate { .. }
    )
}

fn reconcile_plan_has_mutations(plan: &tcfs_sync::reconcile::ReconcilePlan) -> bool {
    plan.actions.iter().any(reconcile_action_mutates)
}

/// Mirror `tcfs_sync::state::PathLocks` identity normalization so one
/// reconcile batch cannot deadlock by acquiring two lexical aliases for the
/// same lock key. The lock manager canonicalizes the parent while preserving
/// the final component; sorting and deduplicating must use that same spelling.
fn reconcile_path_lock_identity(path: &std::path::Path) -> std::path::PathBuf {
    path.parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map(|parent| parent.join(path.file_name().unwrap_or_default()))
        .or_else(|| std::fs::canonicalize(path).ok())
        .unwrap_or_else(|| path.to_path_buf())
}

fn reconcile_lock_paths(
    plan: &tcfs_sync::reconcile::ReconcilePlan,
    local_root: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for action in plan
        .actions
        .iter()
        .filter(|action| reconcile_action_mutates(action))
    {
        let rel_path = reconcile_action_rel_path(action);
        tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
            .with_context(|| format!("invalid periodic reconcile path: {rel_path:?}"))?;
        paths.push(reconcile_path_lock_identity(&local_root.join(rel_path)));
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn first_never_reconcile_path(
    plan: &tcfs_sync::reconcile::ReconcilePlan,
    local_root: &std::path::Path,
    policy_store: &tcfs_sync::policy::PolicyStore,
) -> Result<Option<String>> {
    for action in plan
        .actions
        .iter()
        .filter(|action| reconcile_action_mutates(action))
    {
        let rel_path = reconcile_action_rel_path(action);
        tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
            .with_context(|| format!("invalid periodic reconcile policy path: {rel_path:?}"))?;
        if policy_store.effective_mode(&local_root.join(rel_path))
            == tcfs_sync::policy::SyncMode::Never
        {
            return Ok(Some(rel_path.to_string()));
        }
    }
    Ok(None)
}

fn validate_daemon_data_dir_creation_parent(path: &std::path::Path) -> Result<()> {
    let mut ancestor = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("tcfsd data directory has no parent")?;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().with_context(|| {
                    format!(
                        "tcfsd data directory has no existing trusted ancestor: {}",
                        path.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspecting tcfsd data-directory ancestor: {}",
                        ancestor.display()
                    )
                })
            }
        }
    }
    tcfs_sync::conflict_git::validate_trusted_configured_path(ancestor).with_context(|| {
        format!(
            "validating tcfsd data-directory creation ancestor: {}",
            ancestor.display()
        )
    })
}

fn prepare_private_daemon_data_dir(path: &std::path::Path) -> Result<std::path::PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "tcfsd data path must be a real directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_daemon_data_dir_creation_parent(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(path).with_context(|| {
                    format!("creating private tcfsd data directory: {}", path.display())
                })?;
                // `DirBuilderExt::mode` is filtered through the process umask.
                // Set the final directory explicitly so the daemon trust root
                // is always usable and exactly owner-only.
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .with_context(|| {
                        format!("securing private tcfsd data directory: {}", path.display())
                    })?;
            }
            #[cfg(not(unix))]
            anyhow::bail!(
                "private tcfsd data-directory creation is unsupported on this platform: {}",
                path.display()
            );
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting tcfsd data directory: {}", path.display()))
        }
    }

    let metadata = std::fs::symlink_metadata(path).with_context(|| {
        format!(
            "inspecting private tcfsd data directory: {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "tcfsd data path must be a real directory: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        // SAFETY: `geteuid` has no preconditions and only reads process identity.
        let effective_uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            metadata.uid() == effective_uid,
            "tcfsd data directory must be owned by effective uid {effective_uid}, got uid {}: {}",
            metadata.uid(),
            path.display()
        );
        anyhow::ensure!(
            metadata.mode() & 0o7777 == 0o700,
            "tcfsd data directory must be mode 0700: {}",
            path.display()
        );
    }
    #[cfg(not(unix))]
    anyhow::bail!(
        "private tcfsd data-directory validation is unsupported on this platform: {}",
        path.display()
    );

    tcfs_sync::conflict_git::validate_trusted_configured_path(path)
        .with_context(|| format!("validating tcfsd data-directory trust: {}", path.display()))?;
    Ok(path.to_path_buf())
}

fn resolve_private_daemon_data_dir() -> Result<std::path::PathBuf> {
    let base = dirs::data_dir().context(
        "resolving persistent per-user data directory; refusing insecure /tmp tcfsd state",
    )?;
    prepare_private_daemon_data_dir(&base.join("tcfsd"))
}

fn acquire_daemon_instance_lock(
    data_dir: &std::path::Path,
) -> Result<tcfs_sync::state::StateFileLock> {
    tcfs_sync::state::StateFileLock::acquire(&data_dir.join("daemon-instance")).with_context(|| {
        format!(
            "acquiring exclusive tcfsd lifetime lock in {}",
            data_dir.display()
        )
    })
}

fn open_daemon_state_cache(path: &std::path::Path) -> Result<tcfs_sync::state::StateCache> {
    tcfs_sync::state::StateCache::open(path).with_context(|| {
        format!(
            "opening authoritative daemon state cache: {}",
            path.display()
        )
    })
}

pub async fn run(config: TcfsConfig) -> Result<()> {
    info!("daemon starting");

    // Root identities and sensitive-key isolation are daemon trust boundaries.
    // Reject malformed mappings or master-key overlap before opening storage,
    // sockets, or caches; runtime selection adds symlink-aware existence and
    // permission checks.
    crate::grpc::validate_registered_roots_config(&config)?;

    // Persistent daemon authority must never fall back to a shared temporary
    // directory. Establish and lock its private trust root before device/state,
    // storage, watcher, NATS, or socket side effects can begin.
    let data_dir = resolve_private_daemon_data_dir()?;
    let _daemon_instance_lock = acquire_daemon_instance_lock(&data_dir)?;
    let policy_store_path = data_dir.join("folder-policies.json");
    let pending_delete_ledger_path = data_dir.join("pending-remote-deletes.json");
    replay_pending_remote_deletes(
        &pending_delete_ledger_path,
        config.sync.sync_root.as_deref(),
    )
    .context("replaying pending remote-delete staging before daemon side effects")?;

    // Prepare the remaining runtime directories only after durable recovery.
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
        let (id, device_key) = registry.enroll_local(&device_name, None);
        let secret_key_path = tcfs_secrets::device::device_secret_key_path(&registry_path, &id);
        if let Err(e) = tcfs_secrets::device::save_device_secret_key(
            &secret_key_path,
            &device_key.secret_key,
            false,
        ) {
            warn!("failed to write device secret key: {e}");
        } else {
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
        Ok(cs) if cs.s3.is_some() => {
            info!(source = %cs.source, "credentials loaded");
            cred_store.write().await.replace(cs);
        }
        Ok(cs) => {
            warn!(
                source = %cs.source,
                "no S3 credentials found (daemon will start without creds)"
            );
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

    // TIN-1417 B4: ensure the on-disk device registry is signed with the
    // master-derived key. The auto-enroll above runs before the master key is
    // available, so a first-run registry is written unsigned; re-sign it here so
    // per-device wrapping can trust it. Best-effort: a load/verify failure here
    // (e.g. a registry signed by a *different* master) is logged, not fatal.
    if let Some(ref mk) = master_key {
        match tcfs_secrets::device::DeviceRegistry::load_verified(&registry_path, mk.as_bytes()) {
            Ok((_, tcfs_secrets::device::RegistryTrust::Signed)) => {}
            Ok((mut reg, tcfs_secrets::device::RegistryTrust::UnsignedLegacy)) => {
                if let Err(e) = reg.save_signed(&registry_path, mk.as_bytes()) {
                    warn!("TIN-1417 B4: failed to sign device registry: {e}");
                } else {
                    info!("TIN-1417 B4: signed previously-unsigned device registry");
                }
            }
            Err(e) => {
                warn!(
                    "TIN-1417 B4: device registry failed signature verification ({e}); leaving \
                     as-is. Per-device wrapping will refuse to trust it until re-signed with the \
                     correct master key."
                );
            }
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
        let storage_prefix = config.storage.resolved_prefix();
        match tcfs_storage::check_health_for_prefix_detailed(&op, storage_prefix).await {
            Ok(report) => {
                info!(
                    endpoint = %config.storage.endpoint,
                    prefix = %storage_prefix,
                    health_path = %report.path,
                    elapsed_ms = report.elapsed_ms,
                    entry_count = report.entry_count,
                    "SeaweedFS: connected"
                );
                operator = Some(op);
                true
            }
            Err(e) => {
                warn!(
                    endpoint = %config.storage.endpoint,
                    prefix = %storage_prefix,
                    health_kind = %e.kind(),
                    health_path = %e.path(),
                    elapsed_ms = e.elapsed_ms(),
                    backend_kind = e.backend_kind().unwrap_or("none"),
                    "SeaweedFS health check failed: {e}"
                );
                // Still keep the operator for retry
                operator = Some(op);
                false
            }
        }
    } else {
        warn!("no S3 credentials — storage connectivity not verified");
        false
    };

    // Open state cache, purge stale entries, then wrap in Arc<Mutex>. The
    // canonical file is the `.json` sibling of the configured `state_db`; a
    // legacy `state.db`-only host is absorbed into it exactly once.
    let state_json_path = absorb_legacy_state_db(&config.sync.state_db)?;
    let mut state_cache_inner = open_daemon_state_cache(&state_json_path)?;

    // Purge entries with wrong remote prefix or stale tmp paths
    let resolved_prefix = config.storage.resolved_prefix();
    let purged = state_cache_inner.purge_stale(resolved_prefix);
    if purged > 0 {
        info!(
            purged,
            prefix = resolved_prefix,
            "purged stale state cache entries"
        );
        state_cache_inner
            .flush()
            .context("persisting startup state-cache purge before automatic sync")?;
    }

    let state_cache = Arc::new(tokio::sync::Mutex::new(state_cache_inner));

    // Wrap operator in Arc<Mutex> for shared access
    let operator = Arc::new(tokio::sync::Mutex::new(operator));

    // Populate remote-only entries without blocking daemon readiness. The gRPC
    // socket must bind even when object storage is slow or unreachable.
    spawn_remote_index_discovery(
        operator.clone(),
        state_cache.clone(),
        config.sync.sync_root.clone(),
        resolved_prefix.to_string(),
    );

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
            storage_prefix: config.storage.resolved_prefix().to_string(),
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
    // The durable remote-delete journal has one slot. Serialize every handler
    // across path locks so concurrent deletes cannot replace each other's
    // recovery intent.
    let pending_delete_lock = Arc::new(tokio::sync::Mutex::new(()));

    // ── File Watcher + Scheduler ─────────────────────────────────────
    // If sync_root is configured, start watching for local file changes
    // and feed them through the priority scheduler for automatic sync.
    //
    // On macOS with FileProvider active, the watcher is skipped because
    // ~/Library/CloudStorage/TCFSProvider-TCFS/ is the primary interface.
    // The FileProvider extension handles uploads/downloads via gRPC RPCs.
    let fileprovider_active = cfg!(target_os = "macos")
        && (config.daemon.fileprovider_socket.is_some()
            || config.daemon.fileprovider_endpoint.is_some());

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
                    let bridge_policy_path = policy_store_path.clone();
                    tokio::spawn(async move {
                        while let Some(event) = watch_rx.recv().await {
                            // Check blacklist before scheduling. Full component
                            // checks are required so events inside an existing
                            // denied subtree (for example `.ssh/id_ed25519`)
                            // cannot bypass final-name filtering.
                            if let Some(reason) =
                                bridge_blacklist.check_path_components(&event.path)
                            {
                                debug!(
                                    path = %event.path.display(),
                                    reason = %reason,
                                    "watcher: skipped (blacklisted)"
                                );
                                continue;
                            }

                            let bridge_policy_store = match open_automatic_policy_store(
                                &bridge_policy_path,
                                "watcher",
                            ) {
                                Ok(store) => store,
                                Err(error) => {
                                    warn!(
                                        path = %event.path.display(),
                                        error = %error,
                                        "watcher: policy unavailable; skipping event fail-closed"
                                    );
                                    continue;
                                }
                            };

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
                    let sched_config = config.clone();
                    let sched_policy_path = policy_store_path.clone();

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
                                    let cfg = sched_config.clone();
                                    let policy_path = sched_policy_path.clone();

                                    Box::pin(async move {
                                        // Acquire per-path lock to prevent concurrent operations
                                        let _lock_guard = locks.lock(&task.path).await;

                                        let policy_store = match open_automatic_policy_store(
                                            &policy_path,
                                            "watcher scheduler",
                                        ) {
                                            Ok(store) => store,
                                            Err(error) => {
                                                warn!(
                                                    path = %task.path.display(),
                                                    error = %error,
                                                    "watcher scheduler: policy unavailable; refusing mutation"
                                                );
                                                return Ok(());
                                            }
                                        };
                                        if policy_store.effective_mode(&task.path)
                                            == tcfs_sync::policy::SyncMode::Never
                                        {
                                            debug!(
                                                path = %task.path.display(),
                                                "watcher scheduler: skipped (Never policy)"
                                            );
                                            return Ok(());
                                        }

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

                                                let enc_ctx = mk.as_ref().map(|k| crate::grpc::build_encryption_context(&cfg, &device, k));
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
                                                    watcher_record_conflict(
                                                        &mut cache,
                                                        &task.path,
                                                        info.clone(),
                                                    );
                                                    // Emit status change for D-Bus listeners
                                                    let _ = status_tx.try_send((
                                                        task.path.to_string_lossy().to_string(),
                                                        "conflict".to_string(),
                                                    ));
                                                }

                                                if !upload_result.skipped {
                                                    // Set status = Synced after a committed upload
                                                    if let Some(entry) = cache.get(&task.path).cloned() {
                                                        let synced = tcfs_sync::state::SyncState {
                                                            status: tcfs_sync::state::FileSyncStatus::Synced,
                                                            ..entry
                                                        };
                                                        cache.set(&task.path, synced);
                                                    }
                                                }

                                                if let Err(e) = cache.flush() {
                                                    warn!(error = %e, "state cache flush failed");
                                                }

                                                if !upload_result.skipped {
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
                                                    let nats_vclock = upload_result.vclock.clone();
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
                                                                vclock: nats_vclock,
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
                                                }

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
    let listen_addr = config.daemon.listen.clone();
    let config = Arc::new(config);
    let impl_ = TcfsDaemonImpl::new(
        cred_store,
        config.clone(),
        storage_ok,
        config.storage.endpoint.clone(),
        state_cache,
        operator.clone(),
        path_locks.clone(),
        device_id.clone(),
        device_name.clone(),
        master_key,
    );

    // Load persisted auth credentials. Sessions and MFA state remain
    // best-effort, but an existing single-use invite ledger is replay authority
    // and must load successfully before the daemon accepts enrollment traffic.
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
    let invite_redemption_path = data_dir.join("invite-redemptions.json");
    load_invite_redemptions_for_startup(&impl_, &invite_redemption_path).await?;
    let device_authorization_path = data_dir.join("device-authorizations.json");
    if device_authorization_path.exists() {
        if let Err(e) = impl_
            .load_device_authorizations(&device_authorization_path)
            .await
        {
            warn!("failed to load device authorizations: {e}");
        }
    }
    impl_
        .ensure_local_device_authorization()
        .await
        .context("persisting local device authorization")?;

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
                    let policy_path = policy_store_path.clone();
                    let download_threshold = config.sync.auto_download_threshold;
                    spawn_state_sync_loop(
                        &nats,
                        &sync_device_id,
                        &sync_conflict_mode,
                        operator.clone(),
                        impl_.state_cache_handle(),
                        sync_root,
                        storage_prefix,
                        config.clone(),
                        impl_.master_key_handle(),
                        impl_.vfs_handle.clone(),
                        path_locks.clone(),
                        policy_path.clone(),
                        pending_delete_ledger_path.clone(),
                        pending_delete_lock.clone(),
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
            let recon_tcfs_config = config.clone();
            let recon_master_key = impl_.master_key_handle();
            let orphan_chunk_cleanup_grace_secs = config.sync.orphan_chunk_cleanup_grace_secs;
            let orphan_chunk_cleanup_sweep_interval_secs = if orphan_chunk_cleanup_grace_secs > 0 {
                orphan_chunk_cleanup_grace_secs
                    .min(3600)
                    .max(recon_interval)
            } else {
                0
            };
            let recon_blacklist = blacklist.clone();
            let recon_policy_path = policy_store_path.clone();
            let recon_path_locks = path_locks.clone();
            let recon_pending_delete_lock = pending_delete_lock.clone();
            let recon_pending_delete_ledger_path = pending_delete_ledger_path.clone();

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

                    // A missing policy file is the legitimate empty first-run
                    // store. Any existing unreadable/corrupt store is authority
                    // failure, so do no planning that could later be executed.
                    if let Err(error) =
                        open_automatic_policy_store(&recon_policy_path, "periodic reconcile")
                    {
                        warn!(
                            error = %error,
                            "periodic reconcile policy unavailable; skipping cycle fail-closed"
                        );
                        continue;
                    }

                    // Enable `.git`-aware fast-forward conflict resolution when
                    // syncing raw `.git` dirs. Other knobs keep their defaults.
                    let recon_config = tcfs_sync::reconcile::ReconcileConfig {
                        git_sync_mode: recon_blacklist.git_sync_mode().to_string(),
                        git_ff_resolution: recon_blacklist.allows_git_dirs()
                            && recon_blacklist.git_sync_mode() == "raw",
                        ..Default::default()
                    };

                    // Build the encryption context (if a master key is loaded)
                    // up-front: the reconcile pass needs it to read remote `.git`
                    // ref blobs for the fast-forward ancestry check.
                    let recon_enc = {
                        let mk_guard = recon_master_key.lock().await;
                        mk_guard.as_ref().map(|k| {
                            crate::grpc::build_encryption_context(
                                &recon_tcfs_config,
                                &recon_device,
                                k,
                            )
                        })
                    };

                    let cache = recon_state.lock().await;
                    let plan = match tcfs_sync::reconcile::reconcile(
                        &op,
                        &recon_root,
                        &recon_prefix,
                        &cache,
                        &recon_device,
                        &recon_blacklist,
                        &recon_config,
                        recon_enc.as_ref(),
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
                    if !reconcile_plan_has_mutations(&plan) {
                        debug!(up_to_date = s.up_to_date, "reconcile: nothing to do");
                    } else {
                        info!(
                            pushes = s.pushes,
                            pulls = s.pulls,
                            local_deletes = s.local_deletes,
                            remote_deletes = s.remote_deletes,
                            directories = s.directories,
                            conflicts = s.conflicts,
                            up_to_date = s.up_to_date,
                            "reconcile: executing plan"
                        );

                        let lock_paths = match reconcile_lock_paths(&plan, &recon_root) {
                            Ok(paths) => paths,
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    "periodic reconcile plan contains an unsafe lock path; skipping cycle"
                                );
                                continue;
                            }
                        };

                        // Match NATS remote-delete ordering exactly: the single
                        // durable ledger first, then canonical sorted path locks,
                        // then policy/state. This preserves raw-Git plan groups
                        // while preventing a batch executor from crossing a
                        // staged local delete.
                        let _pending_delete_guard = recon_pending_delete_lock.lock().await;
                        if let Err(error) =
                            ensure_pending_delete_ledger_clear(&recon_pending_delete_ledger_path)
                        {
                            warn!(
                                error = %error,
                                "periodic reconcile withheld while remote-delete recovery remains unresolved"
                            );
                            continue;
                        }
                        let mut _path_guards = Vec::with_capacity(lock_paths.len());
                        for path in &lock_paths {
                            _path_guards.push(recon_path_locks.lock(path).await);
                        }

                        let policy_store = match open_automatic_policy_store(
                            &recon_policy_path,
                            "periodic reconcile execution",
                        ) {
                            Ok(store) => store,
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    "periodic reconcile policy changed or became unavailable; skipping execution"
                                );
                                continue;
                            }
                        };
                        match first_never_reconcile_path(&plan, &recon_root, &policy_store) {
                            Ok(Some(path)) => {
                                warn!(
                                    path = %path,
                                    "periodic reconcile plan intersects Never policy; skipping whole cycle to preserve grouped actions"
                                );
                                continue;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    "periodic reconcile policy validation failed; skipping execution"
                                );
                                continue;
                            }
                        }

                        let mut cache = recon_state.lock().await;
                        match tcfs_sync::reconcile::execute_plan(
                            &plan,
                            &op,
                            &recon_root,
                            &recon_prefix,
                            &mut cache,
                            &recon_device,
                            recon_enc.as_ref(),
                            None,
                        )
                        .await
                        {
                            Ok(result) => {
                                info!(
                                    pushed = result.pushed,
                                    pulled = result.pulled,
                                    errors = result.errors.len(),
                                    deferred_git_refs = result.deferred_git_refs.len(),
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
                                        without_atomic_delete =
                                            report.skipped_without_atomic_delete.len(),
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
        let unsync_policy_path = policy_store_path.clone();
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
                    match open_automatic_policy_store(&unsync_policy_path, "auto-unsync") {
                        Ok(store) => store,
                        Err(error) => {
                            warn!(
                                error = %error,
                                "auto-unsync policy unavailable; skipping sweep fail-closed"
                            );
                            continue;
                        }
                    };
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
    if let Some(ref addr) = listen_addr {
        anyhow::bail!(
            "refusing plaintext gRPC TCP listener at {addr}; use the owner-only Unix socket through SSH forwarding until TLS/mTLS transport is configured"
        );
    }

    crate::grpc::serve(
        &socket_path,
        fp_socket_path.as_deref(),
        listen_addr.as_deref(),
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

fn spawn_remote_index_discovery(
    operator: Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    sync_root: Option<std::path::PathBuf>,
    storage_prefix: String,
) {
    tokio::spawn(async move {
        let operator = {
            let guard = operator.lock().await;
            guard.clone()
        };
        let Some(operator) = operator else {
            debug!("remote index discovery skipped: storage operator unavailable");
            return;
        };

        let Some(sync_root) = sync_root else {
            debug!(
                "remote index discovery skipped: no configured sync root for authoritative local keys"
            );
            return;
        };
        let discovery = tokio::time::timeout(
            REMOTE_INDEX_DISCOVERY_TIMEOUT,
            tcfs_sync::reconcile::list_remote_index(&operator, &storage_prefix),
        )
        .await;

        match discovery {
            Ok(Ok(remote_index)) => {
                let mut cache = state_cache.lock().await;
                let mut discovered = 0usize;
                for (rel_path, entry) in &remote_index {
                    let local_key = sync_root.join(rel_path);
                    if cache.get(&local_key).is_none() {
                        cache.set(
                            &local_key,
                            tcfs_sync::state::SyncState {
                                blake3: String::new(),
                                size: entry.size,
                                mtime: 0,
                                chunk_count: entry.chunks,
                                remote_path: format!(
                                    "{}/manifests/{}",
                                    storage_prefix, entry.manifest_hash
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
                    if let Err(e) = cache.flush() {
                        warn!(error = %e, "state cache flush failed");
                    }
                } else {
                    info!(
                        total = remote_index.len(),
                        "remote index discovery: cache already up to date"
                    );
                }
            }
            Ok(Err(e)) => {
                warn!("remote index discovery failed (non-fatal): {e}");
            }
            Err(_elapsed) => {
                warn!(
                    seconds = REMOTE_INDEX_DISCOVERY_TIMEOUT.as_secs(),
                    "remote index discovery timed out (non-fatal); periodic reconcile will retry"
                );
            }
        }
    });
}

/// Spawn a background task that consumes state events from NATS.
///
/// NATS paths are notifications from another machine, not local filesystem
/// authority. Validate them before they participate in policy, state-cache,
/// lock, or filesystem lookups, and keep every configured-root target beneath
/// the canonical sync root (including through existing parent symlinks).
fn nats_automatic_mutation_rel_path(event: &tcfs_sync::StateEvent) -> Option<&str> {
    match event {
        tcfs_sync::StateEvent::FileSynced { rel_path, .. }
        | tcfs_sync::StateEvent::FileDeleted { rel_path, .. }
        | tcfs_sync::StateEvent::ConflictResolved { rel_path, .. } => Some(rel_path),
        _ => None,
    }
}

fn nats_fixed_ingress_deny<'a>(
    blacklist: &tcfs_sync::blacklist::Blacklist,
    event: &'a tcfs_sync::StateEvent,
) -> Option<(&'a str, tcfs_sync::blacklist::BlacklistReason)> {
    let rel_path = nats_automatic_mutation_rel_path(event)?;
    blacklist
        .check_fixed_ingress_path_components(std::path::Path::new(rel_path))
        .map(|reason| (rel_path, reason))
}

fn contained_nats_target(
    sync_root: &std::path::Path,
    rel_path: &str,
) -> Result<std::path::PathBuf> {
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .context("invalid NATS event relative path")?;

    let canonical_root = std::fs::canonicalize(sync_root)
        .with_context(|| format!("canonicalizing sync root: {}", sync_root.display()))?;
    let target = canonical_root.join(rel_path);
    anyhow::ensure!(
        target.starts_with(&canonical_root),
        "NATS event target escaped sync root: {}",
        target.display()
    );

    // Do not follow the final component: replacing or unlinking a symlink at
    // the target itself is contained. Existing parent components, however,
    // must resolve beneath the root or a write/remove could escape through a
    // directory symlink. For not-yet-created parents, inspect the nearest
    // existing ancestor.
    let mut ancestor = target
        .parent()
        .context("NATS event target has no parent beneath sync root")?;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let resolved = std::fs::canonicalize(ancestor).with_context(|| {
                    format!(
                        "canonicalizing existing NATS target ancestor: {}",
                        ancestor.display()
                    )
                })?;
                anyhow::ensure!(
                    resolved == canonical_root || resolved.starts_with(&canonical_root),
                    "NATS event target ancestor escaped sync root: {} -> {}",
                    ancestor.display(),
                    resolved.display()
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .context("NATS event target has no existing ancestor beneath sync root")?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspecting NATS target ancestor: {}", ancestor.display())
                });
            }
        }
    }

    Ok(target)
}

fn validate_nats_resolved_mutation_target(
    config: &TcfsConfig,
    fixed_ingress_blacklist: &tcfs_sync::blacklist::Blacklist,
    rel_path: &str,
    local_path: &std::path::Path,
) -> Result<()> {
    if let Some(reason) = fixed_ingress_blacklist.check_fixed_ingress_path_components(local_path) {
        anyhow::bail!(
            "resolved NATS target for {rel_path:?} crosses a fixed security fence ({reason}): {}",
            local_path.display()
        );
    }
    tcfs_core::config::validate_sync_selection_excludes_master_key(config, local_path)
        .map_err(anyhow::Error::msg)
        .with_context(|| {
            format!(
                "resolved NATS target for {rel_path:?} selects configured master-key material: {}",
                local_path.display()
            )
        })
}

/// Resolve a validated remote relative path to the daemon's local key. A
/// configured sync root is the preferred authority. Legacy rootless operation
/// may select only an already-enrolled state entry, and validation still occurs
/// before opening that state cache.
async fn nats_local_path(
    sync_root: Option<&std::path::Path>,
    rel_path: &str,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    config: &TcfsConfig,
    fixed_ingress_blacklist: &tcfs_sync::blacklist::Blacklist,
) -> Result<Option<std::path::PathBuf>> {
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .context("invalid NATS event relative path")?;

    let local_path = if let Some(root) = sync_root {
        Some(contained_nats_target(root, rel_path)?)
    } else {
        let cache = state_cache.lock().await;
        cache
            .get_by_rel_path(rel_path)
            .map(|(key, _)| std::path::PathBuf::from(key))
    };
    if let Some(local_path) = local_path.as_deref() {
        validate_nats_resolved_mutation_target(
            config,
            fixed_ingress_blacklist,
            rel_path,
            local_path,
        )?;
    }
    Ok(local_path)
}

#[allow(clippy::too_many_arguments)]
async fn spawn_state_sync_loop(
    nats: &tcfs_sync::NatsClient,
    device_id: &str,
    conflict_mode: &str,
    operator: Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    sync_root: Option<std::path::PathBuf>,
    storage_prefix: String,
    tcfs_config: Arc<TcfsConfig>,
    master_key: Arc<tokio::sync::Mutex<Option<MasterKey>>>,
    vfs_handle: tokio::sync::watch::Receiver<Option<std::sync::Arc<tcfs_vfs::TcfsVfs>>>,
    path_locks: tcfs_sync::state::PathLocks,
    policy_path: std::path::PathBuf,
    pending_delete_ledger_path: std::path::PathBuf,
    pending_delete_lock: Arc<tokio::sync::Mutex<()>>,
    auto_download_threshold: u64,
) {
    use futures::StreamExt;

    match nats.state_consumer(device_id).await {
        Ok(stream) => {
            let device_id = device_id.to_string();
            let conflict_mode = conflict_mode.to_string();
            let fixed_ingress_blacklist =
                tcfs_sync::blacklist::Blacklist::from_sync_config(&tcfs_config.sync);
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

                            if let Some((rel_path, reason)) =
                                nats_fixed_ingress_deny(&fixed_ingress_blacklist, &msg.event)
                            {
                                let rel_path = rel_path.to_owned();
                                warn!(
                                    path = %rel_path,
                                    reason = %reason,
                                    event = %event_type,
                                    "ACK-dropping fixed-deny NATS mutation event"
                                );
                                if let Err(error) = msg.ack().await {
                                    warn!(
                                        path = %rel_path,
                                        error = %error,
                                        "ack fixed-deny NATS event failed"
                                    );
                                }
                                continue;
                            }

                            match &msg.event {
                                tcfs_sync::StateEvent::FileSynced {
                                    rel_path,
                                    blake3,
                                    size,
                                    vclock: remote_vclock,
                                    ..
                                } => {
                                    let file_path = match nats_local_path(
                                        sync_root.as_deref(),
                                        rel_path,
                                        &state_cache,
                                        &tcfs_config,
                                        &fixed_ingress_blacklist,
                                    )
                                    .await
                                    {
                                        Ok(Some(path)) => path,
                                        Ok(None) => {
                                            warn!(
                                                path = %rel_path,
                                                "dropping NATS file event: no configured sync root or enrolled state entry"
                                            );
                                            if let Err(e) = msg.ack().await {
                                                warn!("ack invalid state event failed: {e}");
                                            }
                                            continue;
                                        }
                                        Err(error) => {
                                            warn!(
                                                path = %rel_path,
                                                error = %error,
                                                "dropping invalid or uncontained NATS file event"
                                            );
                                            if let Err(e) = msg.ack().await {
                                                warn!("ack invalid state event failed: {e}");
                                            }
                                            continue;
                                        }
                                    };

                                    info!(
                                        from_device = %event_device,
                                        path = %rel_path,
                                        hash = &blake3[..8.min(blake3.len())],
                                        size,
                                        mode = %conflict_mode,
                                        "remote file synced"
                                    );

                                    // Check folder policy before auto-pulling
                                    let policy_store = match open_automatic_policy_store(
                                        &policy_path,
                                        "NATS FileSynced",
                                    ) {
                                        Ok(store) => store,
                                        Err(error) => {
                                            warn!(
                                                path = %rel_path,
                                                error = %error,
                                                "withholding NATS FileSynced ack: policy unavailable"
                                            );
                                            continue;
                                        }
                                    };
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

                                    // The NATS size is only diagnostic: a delayed or forged
                                    // event must not decide OnDemand policy for whichever
                                    // object the exact current path index now selects.
                                    let auto_download_limit = (effective_mode
                                        == tcfs_sync::policy::SyncMode::OnDemand)
                                        .then(|| {
                                            policy_store
                                                .effective_download_threshold(&file_path)
                                                .unwrap_or(auto_download_threshold)
                                        });

                                    // Always mode: unconditional auto-pull
                                    // OnDemand mode (under threshold): conditional auto-pull
                                    match conflict_mode.as_str() {
                                        "auto" => {
                                            let should_ack = handle_auto_pull(
                                                &device_id,
                                                &event_device,
                                                rel_path,
                                                blake3,
                                                *size,
                                                remote_vclock,
                                                &file_path,
                                                &operator,
                                                &state_cache,
                                                &path_locks,
                                                &storage_prefix,
                                                auto_download_limit,
                                                &tcfs_config,
                                                &master_key,
                                            )
                                            .await;
                                            if !should_ack {
                                                continue;
                                            }

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
                                tcfs_sync::StateEvent::ConflictResolved { rel_path, .. } => {
                                    let local_path = match nats_local_path(
                                        sync_root.as_deref(),
                                        rel_path,
                                        &state_cache,
                                        &tcfs_config,
                                        &fixed_ingress_blacklist,
                                    )
                                    .await
                                    {
                                        Ok(Some(path)) => path,
                                        Ok(None) => {
                                            warn!(
                                                path = %rel_path,
                                                "dropping NATS conflict event: no configured sync root or enrolled state entry"
                                            );
                                            if let Err(e) = msg.ack().await {
                                                warn!("ack invalid state event failed: {e}");
                                            }
                                            continue;
                                        }
                                        Err(error) => {
                                            warn!(
                                                path = %rel_path,
                                                error = %error,
                                                "dropping invalid or uncontained NATS conflict event"
                                            );
                                            if let Err(e) = msg.ack().await {
                                                warn!("ack invalid state event failed: {e}");
                                            }
                                            continue;
                                        }
                                    };

                                    info!(
                                        from_device = %event_device,
                                        path = %rel_path,
                                        "remote conflict resolved; reconciling exact current index"
                                    );

                                    let policy_store = match open_automatic_policy_store(
                                        &policy_path,
                                        "NATS ConflictResolved",
                                    ) {
                                        Ok(store) => store,
                                        Err(error) => {
                                            warn!(
                                                path = %rel_path,
                                                error = %error,
                                                "withholding NATS ConflictResolved ack: policy unavailable"
                                            );
                                            continue;
                                        }
                                    };
                                    let effective_mode = policy_store.effective_mode(&local_path);

                                    // ConflictResolved payload clocks are notifications, not
                                    // storage authority. In auto mode, route the wake-up through
                                    // the same exact-index reconciliation as FileSynced. A failed
                                    // durable state update must withhold the JetStream ack.
                                    if conflict_mode == "auto"
                                        && effective_mode != tcfs_sync::policy::SyncMode::Never
                                    {
                                        let auto_download_limit = (effective_mode
                                            == tcfs_sync::policy::SyncMode::OnDemand)
                                            .then(|| {
                                                policy_store
                                                    .effective_download_threshold(&local_path)
                                                    .unwrap_or(auto_download_threshold)
                                            });
                                        let should_ack = handle_conflict_resolved(
                                            &device_id,
                                            &event_device,
                                            rel_path,
                                            &local_path,
                                            &operator,
                                            &state_cache,
                                            &path_locks,
                                            &storage_prefix,
                                            auto_download_limit,
                                            &tcfs_config,
                                            &master_key,
                                        )
                                        .await;
                                        if !should_ack {
                                            continue;
                                        }
                                    }
                                }
                                tcfs_sync::StateEvent::FileDeleted { rel_path, .. } => {
                                    let local_path = match nats_local_path(
                                        sync_root.as_deref(),
                                        rel_path,
                                        &state_cache,
                                        &tcfs_config,
                                        &fixed_ingress_blacklist,
                                    )
                                    .await
                                    {
                                        Ok(Some(path)) => path,
                                        Ok(None) => {
                                            warn!(
                                                path = %rel_path,
                                                "dropping NATS delete event: no configured sync root or enrolled state entry"
                                            );
                                            if let Err(e) = msg.ack().await {
                                                warn!("ack invalid state event failed: {e}");
                                            }
                                            continue;
                                        }
                                        Err(error) => {
                                            warn!(
                                                path = %rel_path,
                                                error = %error,
                                                "dropping invalid or uncontained NATS delete event"
                                            );
                                            if let Err(e) = msg.ack().await {
                                                warn!("ack invalid state event failed: {e}");
                                            }
                                            continue;
                                        }
                                    };

                                    info!(
                                        from_device = %event_device,
                                        path = %rel_path,
                                        "remote file deleted"
                                    );

                                    let policy_store = match open_automatic_policy_store(
                                        &policy_path,
                                        "NATS FileDeleted",
                                    ) {
                                        Ok(store) => store,
                                        Err(error) => {
                                            warn!(
                                                path = %rel_path,
                                                error = %error,
                                                "withholding NATS FileDeleted ack: policy unavailable"
                                            );
                                            continue;
                                        }
                                    };
                                    let effective_mode = policy_store.effective_mode(&local_path);
                                    let outcome = handle_remote_delete(
                                        &device_id,
                                        &event_device,
                                        rel_path,
                                        &local_path,
                                        &operator,
                                        &state_cache,
                                        &path_locks,
                                        &storage_prefix,
                                        effective_mode,
                                        &pending_delete_ledger_path,
                                        &pending_delete_lock,
                                    )
                                    .await;
                                    if outcome == RemoteDeleteOutcome::Withhold {
                                        continue;
                                    }

                                    if outcome == RemoteDeleteOutcome::AckDeleted {
                                        // Invalidate FUSE cache only after both the guarded
                                        // local delete and state-cache flush have committed.
                                        if let Some(ref vfs) = *vfs_handle.borrow() {
                                            let vpath = format!("/{}", rel_path);
                                            vfs.invalidate_path(&vpath);
                                            debug!(path = %vpath, "FUSE cache invalidated for deleted file");
                                        }
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

/// keep-both PR-1 (S2): true when an auto-resolvable conflict path is
/// `.git`-internal and must be deferred rather than resolved per file.
///
/// Automatic per-file resolution over `.git` internals is the G5-git-5
/// corruption vector; the daemon's NATS auto path defers these so the operator
/// resolves the repo group deliberately.
fn auto_conflict_must_defer(rel_path: &str) -> bool {
    tcfs_sync::git_safety::repo_root_for_git_path(std::path::Path::new(""), rel_path).is_some()
}

async fn nats_snapshot_still_current(
    op: &opendal::Operator,
    snapshot: &tcfs_sync::engine::IndexedManifestSnapshot,
) -> bool {
    match tcfs_sync::engine::indexed_manifest_snapshot_is_current(op, snapshot).await {
        Ok(true) => true,
        Ok(false) => {
            warn!(
                path = %snapshot.rel_path(),
                manifest = %snapshot.manifest_path(),
                "NATS indexed snapshot changed before acknowledgement; withholding ack"
            );
            false
        }
        Err(error) => {
            warn!(
                path = %snapshot.rel_path(),
                manifest = %snapshot.manifest_path(),
                error = %error,
                "failed to recheck NATS indexed snapshot; withholding ack"
            );
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteDeleteOutcome {
    /// The event is safe to acknowledge, but no local state was mutated.
    AckIgnored,
    /// The authoritative remote absence was committed locally and is safe to acknowledge.
    AckDeleted,
    /// A retry or repo-group reconcile is required; the caller must not acknowledge.
    Withhold,
}

enum CurrentRemoteDeleteState {
    Missing,
    Deleted,
    Live(Box<tcfs_sync::engine::IndexedManifestSnapshot>),
}

/// Resolve the exact path without collapsing a durable v4 tombstone into a
/// missing object. Only `Deleted` may authorize local removal.
async fn current_remote_delete_state(
    op: &opendal::Operator,
    rel_path: &str,
    storage_prefix: &str,
) -> Result<CurrentRemoteDeleteState> {
    match tcfs_sync::index_entry::read_exact_index_path_state(op, storage_prefix, rel_path).await? {
        tcfs_sync::index_entry::ExactIndexPathState::Missing => {
            Ok(CurrentRemoteDeleteState::Missing)
        }
        tcfs_sync::index_entry::ExactIndexPathState::Deleted => {
            Ok(CurrentRemoteDeleteState::Deleted)
        }
        tcfs_sync::index_entry::ExactIndexPathState::Live => {
            let snapshot = tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(
                op,
                rel_path,
                storage_prefix,
            )
            .await?
            .context("exact index changed or is preparing while binding remote-delete authority")?;
            Ok(CurrentRemoteDeleteState::Live(Box::new(snapshot)))
        }
    }
}

const PENDING_REMOTE_DELETE_LEDGER_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRemoteDelete {
    rel_path: String,
    staged_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRemoteDeleteLedger {
    version: u32,
    pending: Option<PendingRemoteDelete>,
}

impl Default for PendingRemoteDeleteLedger {
    fn default() -> Self {
        Self {
            version: PENDING_REMOTE_DELETE_LEDGER_VERSION,
            pending: None,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe extern "C" {
    #[link_name = "renamex_np"]
    fn tcfs_renamex_np(
        from: *const libc::c_char,
        to: *const libc::c_char,
        flags: libc::c_uint,
    ) -> libc::c_int;
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
fn path_to_c_string(path: &std::path::Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path contains a NUL byte: {}", path.display()),
        )
    })
}

/// Atomically rename `source` to `destination` only while the destination is
/// absent. An ordinary POSIX rename silently overwrites a path recreated after
/// our last check, which is never safe for delete staging or rollback.
#[cfg(target_os = "linux")]
fn rename_noreplace(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    let source = path_to_c_string(source)?;
    let destination = path_to_c_string(destination)?;
    // SAFETY: both C strings are live for the call, use valid AT_FDCWD
    // descriptors, and `renameat2` does not retain their pointers.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn rename_noreplace(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    // Darwin's public RENAME_EXCL flag for renamex_np(2).
    const RENAME_EXCL: libc::c_uint = 0x0000_0004;

    let source = path_to_c_string(source)?;
    let destination = path_to_c_string(destination)?;
    // SAFETY: both C strings are live for the call and renamex_np does not
    // retain their pointers.
    let result = unsafe { tcfs_renamex_np(source.as_ptr(), destination.as_ptr(), RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
fn rename_noreplace(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "atomic rename-without-replacement is unsupported: {} -> {}",
            source.display(),
            destination.display()
        ),
    ))
}

fn sync_remote_delete_directory(path: &std::path::Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::File::open(parent)
            .with_context(|| format!("opening remote-delete directory: {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("syncing remote-delete directory: {}", parent.display()))?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = path;
    Ok(())
}

fn validate_pending_delete_ledger_file(file: &std::fs::File, path: &std::path::Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("inspecting pending-delete ledger: {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "pending-delete ledger must be a regular file: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        // SAFETY: `geteuid` has no preconditions and only reads identity.
        let effective_uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            metadata.uid() == effective_uid,
            "pending-delete ledger must be owned by effective uid {effective_uid}: {}",
            path.display()
        );
        anyhow::ensure!(
            metadata.nlink() == 1,
            "pending-delete ledger must have exactly one hard link: {}",
            path.display()
        );
        anyhow::ensure!(
            metadata.mode() & 0o077 == 0,
            "pending-delete ledger must be mode 0600 or stricter: {}",
            path.display()
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    tcfs_sync::path_acl::reject_write_grant_acl_fd(file, path).with_context(|| {
        format!(
            "validating pending-delete ledger descriptor ACL: {}",
            path.display()
        )
    })?;
    Ok(())
}

fn read_pending_delete_ledger(path: &std::path::Path) -> Result<PendingRemoteDeleteLedger> {
    use std::io::Read;

    let link_metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingRemoteDeleteLedger::default())
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspecting pending-delete ledger path: {}", path.display())
            })
        }
    };
    anyhow::ensure!(
        !link_metadata.file_type().is_symlink(),
        "pending-delete ledger must not be a symlink: {}",
        path.display()
    );

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening pending-delete ledger: {}", path.display()))?;
    validate_pending_delete_ledger_file(&file, path)?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("reading pending-delete ledger: {}", path.display()))?;
    validate_pending_delete_ledger_file(&file, path)?;
    let ledger: PendingRemoteDeleteLedger = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing pending-delete ledger: {}", path.display()))?;
    anyhow::ensure!(
        ledger.version == PENDING_REMOTE_DELETE_LEDGER_VERSION,
        "unsupported pending-delete ledger version {}: {}",
        ledger.version,
        path.display()
    );
    Ok(ledger)
}

fn write_pending_delete_ledger(
    path: &std::path::Path,
    ledger: &PendingRemoteDeleteLedger,
) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "creating pending-delete ledger directory: {}",
            parent.display()
        )
    })?;
    let bytes = serde_json::to_vec_pretty(ledger).context("serializing pending-delete ledger")?;
    let tmp_path = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("pending-remote-deletes"),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&tmp_path).with_context(|| {
            format!(
                "creating pending-delete ledger temp: {}",
                tmp_path.display()
            )
        })?;
        file.write_all(&bytes).with_context(|| {
            format!("writing pending-delete ledger temp: {}", tmp_path.display())
        })?;
        file.sync_all().with_context(|| {
            format!("syncing pending-delete ledger temp: {}", tmp_path.display())
        })?;
        drop(file);
        rename_noreplace(&tmp_path, path)
            .with_context(|| format!("installing pending-delete ledger: {}", path.display()))?;
        sync_remote_delete_directory(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn clear_pending_remote_delete(
    ledger_path: &std::path::Path,
    expected: &PendingRemoteDelete,
) -> Result<()> {
    let ledger = read_pending_delete_ledger(ledger_path)?;
    anyhow::ensure!(
        ledger.pending.as_ref() == Some(expected),
        "pending-delete ledger changed before clear: {}",
        ledger_path.display()
    );
    std::fs::remove_file(ledger_path).with_context(|| {
        format!(
            "removing committed pending-delete ledger: {}",
            ledger_path.display()
        )
    })?;
    sync_remote_delete_directory(ledger_path)
}

fn validate_remote_delete_staging_name(
    local_path: &std::path::Path,
    staged_name: &str,
) -> Result<()> {
    let local_name = local_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("remote-delete local filename is not valid UTF-8")?;
    let prefix = format!(".{local_name}.tcfs-delete-");
    let nonce = staged_name
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(".tc"))
        .context("pending remote-delete staging name has an invalid shape")?;
    let parsed = uuid::Uuid::parse_str(nonce).context("invalid remote-delete staging UUID")?;
    anyhow::ensure!(
        parsed.hyphenated().to_string() == nonce,
        "pending remote-delete staging UUID is not canonical"
    );
    anyhow::ensure!(
        std::path::Path::new(staged_name).components().count() == 1,
        "pending remote-delete staging name must be one path component"
    );
    Ok(())
}

fn pending_remote_delete_entry(
    rel_path: &str,
    local_path: &std::path::Path,
    staged_path: &std::path::Path,
) -> Result<PendingRemoteDelete> {
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .context("invalid pending remote-delete relative path")?;
    anyhow::ensure!(
        local_path.parent() == staged_path.parent(),
        "pending remote-delete stage must be a same-directory sibling"
    );
    let staged_name = staged_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("pending remote-delete staging filename is not valid UTF-8")?;
    validate_remote_delete_staging_name(local_path, staged_name)?;
    Ok(PendingRemoteDelete {
        rel_path: rel_path.to_string(),
        staged_name: staged_name.to_string(),
    })
}

fn pending_remote_delete_paths(
    sync_root: &std::path::Path,
    pending: &PendingRemoteDelete,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let local_path = contained_nats_target(sync_root, &pending.rel_path)
        .context("resolving pending remote-delete local path")?;
    validate_remote_delete_staging_name(&local_path, &pending.staged_name)?;
    let staged_path = local_path.with_file_name(&pending.staged_name);
    anyhow::ensure!(
        staged_path.parent() == local_path.parent(),
        "pending remote-delete stage escaped its local parent"
    );
    Ok((local_path, staged_path))
}

fn ensure_pending_delete_ledger_clear(ledger_path: &std::path::Path) -> Result<()> {
    let ledger = read_pending_delete_ledger(ledger_path)?;
    anyhow::ensure!(
        ledger.pending.is_none(),
        "another remote delete remains pending: {}",
        ledger_path.display()
    );
    Ok(())
}

fn stage_remote_delete_with_hook(
    ledger_path: &std::path::Path,
    rel_path: &str,
    local_path: &std::path::Path,
    staged_path: &std::path::Path,
    after_rename: impl FnOnce() -> Result<()>,
) -> Result<PendingRemoteDelete> {
    let pending = pending_remote_delete_entry(rel_path, local_path, staged_path)?;
    ensure_pending_delete_ledger_clear(ledger_path)?;
    write_pending_delete_ledger(
        ledger_path,
        &PendingRemoteDeleteLedger {
            version: PENDING_REMOTE_DELETE_LEDGER_VERSION,
            pending: Some(pending.clone()),
        },
    )?;

    if let Err(rename_error) = rename_noreplace(local_path, staged_path) {
        if let Err(clear_error) = clear_pending_remote_delete(ledger_path, &pending) {
            anyhow::bail!(
                "staging remote delete failed: {rename_error}; clearing its durable intent also failed: {clear_error:#}"
            );
        }
        return Err(rename_error).with_context(|| {
            format!(
                "renaming remote-delete target into staging: {} -> {}",
                local_path.display(),
                staged_path.display()
            )
        });
    }
    sync_remote_delete_directory(local_path)?;
    after_rename()?;
    Ok(pending)
}

fn stage_remote_delete(
    ledger_path: &std::path::Path,
    rel_path: &str,
    local_path: &std::path::Path,
    staged_path: &std::path::Path,
) -> Result<PendingRemoteDelete> {
    stage_remote_delete_with_hook(ledger_path, rel_path, local_path, staged_path, || Ok(()))
}

fn replay_pending_remote_deletes(
    ledger_path: &std::path::Path,
    sync_root: Option<&std::path::Path>,
) -> Result<()> {
    let ledger = read_pending_delete_ledger(ledger_path)?;
    let Some(pending) = ledger.pending else {
        return Ok(());
    };
    let sync_root = sync_root.context(
        "pending remote-delete recovery requires the configured sync root that owns its bytes",
    )?;
    let (local_path, staged_path) = pending_remote_delete_paths(sync_root, &pending)?;

    let staged_metadata = match std::fs::symlink_metadata(&staged_path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspecting pending remote-delete stage: {}",
                    staged_path.display()
                )
            })
        }
    };
    if let Some(metadata) = staged_metadata.as_ref() {
        anyhow::ensure!(
            metadata.is_file() || metadata.file_type().is_symlink(),
            "pending remote-delete stage is neither a file nor symlink: {}",
            staged_path.display()
        );
    }

    match (
        staged_metadata.is_some(),
        std::fs::symlink_metadata(&local_path),
    ) {
        (true, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            match rename_noreplace(&staged_path, &local_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    warn!(
                        path = %pending.rel_path,
                        staged = %staged_path.display(),
                        "pending remote-delete replay preserved staged bytes because the original path was recreated during recovery"
                    );
                    return Ok(());
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "restoring pending remote-delete bytes: {} -> {}",
                            staged_path.display(),
                            local_path.display()
                        )
                    })
                }
            }
            sync_remote_delete_directory(&local_path)?;
            clear_pending_remote_delete(ledger_path, &pending)?;
            info!(
                path = %pending.rel_path,
                "restored pending remote-delete bytes during startup replay"
            );
        }
        (true, Ok(_)) => {
            warn!(
                path = %pending.rel_path,
                staged = %staged_path.display(),
                "pending remote-delete replay preserved staged bytes because the original path was recreated"
            );
        }
        (false, Ok(_)) => {
            clear_pending_remote_delete(ledger_path, &pending)?;
            info!(
                path = %pending.rel_path,
                "cleared completed or rolled-back pending remote-delete intent"
            );
        }
        (false, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            clear_pending_remote_delete(ledger_path, &pending)?;
            info!(
                path = %pending.rel_path,
                "cleared completed pending remote-delete intent"
            );
        }
        (false, Err(error)) => {
            return Err(error).with_context(|| {
                format!(
                    "inspecting pending remote-delete original: {}",
                    local_path.display()
                )
            })
        }
        (true, Err(error)) => {
            return Err(error).with_context(|| {
                format!(
                    "inspecting pending remote-delete original: {}",
                    local_path.display()
                )
            })
        }
    }
    Ok(())
}

fn remote_delete_staging_path(local_path: &std::path::Path) -> std::path::PathBuf {
    let mut staged_name = std::ffi::OsString::from(".");
    staged_name.push(
        local_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("tcfs-entry")),
    );
    staged_name.push(format!(
        ".tcfs-delete-{}.tc",
        uuid::Uuid::new_v4().hyphenated()
    ));
    local_path.with_file_name(staged_name)
}

/// Restore a same-directory delete staging rename without overwriting a path
/// that reappeared independently. A failed restore leaves the uniquely named
/// staging file in place so the original bytes are never discarded.
async fn restore_staged_remote_delete(
    rel_path: &str,
    staged_path: &std::path::Path,
    local_path: &std::path::Path,
    ledger_path: &std::path::Path,
    pending: &PendingRemoteDelete,
) -> bool {
    match rename_noreplace(staged_path, local_path) {
        Ok(()) => {
            if let Err(error) = sync_remote_delete_directory(local_path) {
                warn!(
                    path = %rel_path,
                    error = %error,
                    "restored staged remote delete but failed its directory fsync; durable intent retained"
                );
                return false;
            }
            if let Err(error) = clear_pending_remote_delete(ledger_path, pending) {
                warn!(
                    path = %rel_path,
                    error = %error,
                    "restored staged remote delete but failed to clear durable intent"
                );
                return false;
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            warn!(
                path = %rel_path,
                staged = %staged_path.display(),
                "cannot restore staged remote delete without overwriting a newly created local path"
            );
            false
        }
        Err(error) => {
            warn!(
                path = %rel_path,
                staged = %staged_path.display(),
                error = %error,
                "failed to restore staged remote delete; original bytes remain staged"
            );
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_remote_delete_conflict(
    device_id: &str,
    remote_device: &str,
    rel_path: &str,
    local_path: &std::path::Path,
    actual_local_hash: Option<&str>,
    cached: Option<&tcfs_sync::state::SyncState>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
) -> bool {
    let mut local_vclock = cached.map(|entry| entry.vclock.clone()).unwrap_or_default();
    local_vclock.tick(device_id);
    let conflict = tcfs_sync::conflict::ConflictInfo {
        rel_path: rel_path.to_string(),
        local_vclock,
        // Remote index absence proves deletion, but it carries no
        // authoritative clock. The NATS payload is only a wake-up signal and
        // must not inject an unbound clock into durable conflict state.
        remote_vclock: tcfs_sync::conflict::VectorClock::new(),
        local_blake3: actual_local_hash.unwrap_or("<absent>").to_string(),
        remote_blake3: "<deleted>".to_string(),
        local_device: device_id.to_string(),
        remote_device: remote_device.to_string(),
        detected_at: tcfs_sync::StateEvent::now(),
        times_recorded: 0,
        remote_manifest_key: None,
    };

    let mut cache = state_cache.lock().await;
    let previous = cache.get(local_path).cloned();
    watcher_record_conflict(&mut cache, local_path, conflict);
    if let Err(error) = cache.flush() {
        match previous {
            Some(previous) => cache.set(local_path, previous),
            None => cache.remove(local_path),
        }
        warn!(
            path = %rel_path,
            error = %error,
            "failed to persist remote-delete conflict; withholding ack"
        );
        return false;
    }
    true
}

/// Treat a NATS FileDeleted event as a wake-up signal and apply it only while
/// the exact current path index contains a durable v4 deletion tombstone and
/// the local object still matches its cached identity. Physical absence is not
/// deletion authority. The file is first parked by a same-directory rename so
/// a state-cache flush failure can restore both halves of the local state.
#[allow(clippy::too_many_arguments)]
async fn handle_remote_delete(
    device_id: &str,
    remote_device: &str,
    rel_path: &str,
    local_path: &std::path::Path,
    operator: &Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    path_locks: &tcfs_sync::state::PathLocks,
    storage_prefix: &str,
    effective_mode: tcfs_sync::policy::SyncMode,
    pending_delete_ledger_path: &std::path::Path,
    pending_delete_lock: &Arc<tokio::sync::Mutex<()>>,
) -> RemoteDeleteOutcome {
    if let Err(error) = tcfs_sync::index_entry::validate_canonical_rel_path(rel_path) {
        warn!(
            path = %rel_path,
            error = %error,
            "dropping invalid NATS remote-delete path"
        );
        return RemoteDeleteOutcome::AckIgnored;
    }

    // Never-mode is an explicit local policy decision. It is safe to consume
    // the notification without consulting or mutating storage/state.
    if effective_mode == tcfs_sync::policy::SyncMode::Never {
        debug!(
            path = %rel_path,
            "ignoring remote delete: folder policy is Never"
        );
        return RemoteDeleteOutcome::AckIgnored;
    }

    let _pending_delete_guard = pending_delete_lock.lock().await;
    if let Err(error) = ensure_pending_delete_ledger_clear(pending_delete_ledger_path) {
        warn!(
            path = %rel_path,
            error = %error,
            "withholding remote delete while durable recovery intent remains unresolved"
        );
        return RemoteDeleteOutcome::Withhold;
    }

    let git_internal = auto_conflict_must_defer(rel_path);

    let _lock_guard = path_locks.lock(local_path).await;
    let op = {
        let guard = operator.lock().await;
        match guard.as_ref() {
            Some(op) => op.clone(),
            None => {
                warn!(path = %rel_path, "no storage operator for NATS remote delete");
                return RemoteDeleteOutcome::Withhold;
            }
        }
    };

    // A live exact snapshot makes this an obsolete delete notification. A
    // physically missing index is not deletion authority: only a durable v4
    // tombstone permits the local-delete state machine to continue.
    match current_remote_delete_state(&op, rel_path, storage_prefix).await {
        Ok(CurrentRemoteDeleteState::Live(snapshot)) => {
            info!(
                path = %rel_path,
                current_hash = %snapshot.content_hash(),
                "remote delete event is obsolete; exact path index is populated"
            );
            return if nats_snapshot_still_current(&op, &snapshot).await {
                RemoteDeleteOutcome::AckIgnored
            } else {
                RemoteDeleteOutcome::Withhold
            };
        }
        Ok(CurrentRemoteDeleteState::Deleted) => {}
        Ok(CurrentRemoteDeleteState::Missing) => {
            warn!(
                path = %rel_path,
                "remote delete notification has no durable v4 tombstone; withholding ack"
            );
            return RemoteDeleteOutcome::Withhold;
        }
        Err(error) => {
            warn!(
                path = %rel_path,
                error = %error,
                "failed to resolve exact delete authority for NATS remote delete; withholding ack"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    }

    let cached = {
        let cache = state_cache.lock().await;
        cache.get(local_path).cloned()
    };
    if cached.as_ref().is_some_and(|entry| {
        matches!(
            entry.status,
            tcfs_sync::state::FileSyncStatus::Active | tcfs_sync::state::FileSyncStatus::Locked
        )
    }) {
        info!(
            path = %rel_path,
            "deferring remote delete: local state is active or locked"
        );
        return RemoteDeleteOutcome::Withhold;
    }

    let expected_local = match tcfs_sync::engine::capture_local_fingerprint(local_path) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            warn!(
                path = %rel_path,
                error = %error,
                "failed to capture local identity before NATS remote delete; withholding ack"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    };
    let actual_local_hash = match &expected_local {
        tcfs_sync::engine::ExpectedLocalFingerprint::Absent => None,
        tcfs_sync::engine::ExpectedLocalFingerprint::Tracked { blake3, .. } => {
            Some(blake3.as_str())
        }
    };

    // `.git` internals may only be changed by repo-group reconciliation, which
    // owns Git locking, ref CAS, and loser parking. Still consume a no-op event
    // after that reconcile has removed both local and cached state; otherwise a
    // successfully completed grouped delete would redeliver forever.
    if git_internal {
        if cached.is_some() || actual_local_hash.is_some() {
            warn!(
                path = %rel_path,
                "deferring .git-internal remote delete to repo-group reconciliation"
            );
            return RemoteDeleteOutcome::Withhold;
        }
        match current_remote_delete_state(&op, rel_path, storage_prefix).await {
            Ok(CurrentRemoteDeleteState::Live(snapshot)) => {
                return if nats_snapshot_still_current(&op, &snapshot).await {
                    RemoteDeleteOutcome::AckIgnored
                } else {
                    RemoteDeleteOutcome::Withhold
                };
            }
            Ok(CurrentRemoteDeleteState::Deleted) => {}
            Ok(CurrentRemoteDeleteState::Missing) => {
                warn!(
                    path = %rel_path,
                    "grouped Git delete convergence has no durable remote tombstone"
                );
                return RemoteDeleteOutcome::Withhold;
            }
            Err(error) => {
                warn!(
                    path = %rel_path,
                    error = %error,
                    "failed to recheck converged .git-internal remote delete"
                );
                return RemoteDeleteOutcome::Withhold;
            }
        }
        let mut cache = state_cache.lock().await;
        if let Err(error) = cache.flush() {
            warn!(
                path = %rel_path,
                error = %error,
                "failed to flush converged .git-internal delete state"
            );
            return RemoteDeleteOutcome::Withhold;
        }
        return RemoteDeleteOutcome::AckIgnored;
    }

    let local_diverged = match (&cached, actual_local_hash) {
        (None, None) => false,
        (Some(entry), Some(actual)) => entry.blake3 != actual,
        // Untracked bytes and a missing tracked object are both local changes.
        _ => true,
    };

    if local_diverged {
        // Hashing may take time. Do not record a delete conflict if the exact
        // path was republished while its local identity was being captured.
        match current_remote_delete_state(&op, rel_path, storage_prefix).await {
            Ok(CurrentRemoteDeleteState::Live(snapshot)) => {
                return if nats_snapshot_still_current(&op, &snapshot).await {
                    RemoteDeleteOutcome::AckIgnored
                } else {
                    RemoteDeleteOutcome::Withhold
                };
            }
            Ok(CurrentRemoteDeleteState::Deleted) => {}
            Ok(CurrentRemoteDeleteState::Missing) => {
                warn!(
                    path = %rel_path,
                    "remote tombstone disappeared before delete-conflict persistence"
                );
                return RemoteDeleteOutcome::Withhold;
            }
            Err(error) => {
                warn!(
                    path = %rel_path,
                    error = %error,
                    "failed to recheck remote absence before recording delete conflict"
                );
                return RemoteDeleteOutcome::Withhold;
            }
        }

        let persisted = persist_remote_delete_conflict(
            device_id,
            remote_device,
            rel_path,
            local_path,
            actual_local_hash,
            cached.as_ref(),
            state_cache,
        )
        .await;
        if persisted {
            warn!(
                path = %rel_path,
                "local identity diverged from cached state; preserving it and withholding delete ack"
            );
        }
        return RemoteDeleteOutcome::Withhold;
    }

    if cached
        .as_ref()
        .is_some_and(|entry| entry.status != tcfs_sync::state::FileSyncStatus::Synced)
    {
        info!(
            path = %rel_path,
            "deferring remote delete: tracked local state is not safely synchronized"
        );
        return RemoteDeleteOutcome::Withhold;
    }

    let tcfs_sync::engine::ExpectedLocalFingerprint::Tracked { .. } = &expected_local else {
        // Both the local path and its cache entry are absent. Recheck remote
        // authority, then require any pending state-cache changes to flush
        // before acknowledging the completed deletion.
        match current_remote_delete_state(&op, rel_path, storage_prefix).await {
            Ok(CurrentRemoteDeleteState::Live(snapshot)) => {
                return if nats_snapshot_still_current(&op, &snapshot).await {
                    RemoteDeleteOutcome::AckIgnored
                } else {
                    RemoteDeleteOutcome::Withhold
                };
            }
            Ok(CurrentRemoteDeleteState::Deleted) => {}
            Ok(CurrentRemoteDeleteState::Missing) => {
                warn!(
                    path = %rel_path,
                    "already-missing local path has no durable remote tombstone"
                );
                return RemoteDeleteOutcome::Withhold;
            }
            Err(error) => {
                warn!(
                    path = %rel_path,
                    error = %error,
                    "failed to recheck remote absence for already-missing local path"
                );
                return RemoteDeleteOutcome::Withhold;
            }
        }
        let mut cache = state_cache.lock().await;
        if let Err(error) = cache.flush() {
            warn!(
                path = %rel_path,
                error = %error,
                "state cache flush failed for already-completed remote delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
        return RemoteDeleteOutcome::AckDeleted;
    };

    let cached = cached.expect("matching tracked local identity requires cached state");
    let staged_path = remote_delete_staging_path(local_path);
    let expected_pending = match pending_remote_delete_entry(rel_path, local_path, &staged_path) {
        Ok(pending) => pending,
        Err(error) => {
            warn!(
                path = %rel_path,
                error = %error,
                "failed to validate guarded remote-delete staging path"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    };
    let pending = match stage_remote_delete(
        pending_delete_ledger_path,
        rel_path,
        local_path,
        &staged_path,
    ) {
        Ok(pending) => pending,
        Err(error) => {
            if std::fs::symlink_metadata(&staged_path).is_ok() {
                let _ = restore_staged_remote_delete(
                    rel_path,
                    &staged_path,
                    local_path,
                    pending_delete_ledger_path,
                    &expected_pending,
                )
                .await;
            }
            warn!(
                path = %rel_path,
                error = %error,
                "failed to durably stage local file for guarded remote delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    };

    let staged_fingerprint = match tcfs_sync::engine::capture_local_fingerprint(&staged_path) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            let _ = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            warn!(
                path = %rel_path,
                error = %error,
                "failed to verify staged local identity; withholding remote delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    };
    if staged_fingerprint != expected_local {
        let _ = restore_staged_remote_delete(
            rel_path,
            &staged_path,
            local_path,
            pending_delete_ledger_path,
            &pending,
        )
        .await;
        warn!(
            path = %rel_path,
            "local identity changed while staging remote delete; withholding ack"
        );
        return RemoteDeleteOutcome::Withhold;
    }

    // The remote path may have been republished after the first absence read.
    // Restore the local file before acknowledging such an obsolete event.
    match current_remote_delete_state(&op, rel_path, storage_prefix).await {
        Ok(CurrentRemoteDeleteState::Live(snapshot)) => {
            if !restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await
            {
                return RemoteDeleteOutcome::Withhold;
            }
            return if nats_snapshot_still_current(&op, &snapshot).await {
                RemoteDeleteOutcome::AckIgnored
            } else {
                RemoteDeleteOutcome::Withhold
            };
        }
        Ok(CurrentRemoteDeleteState::Deleted) => {}
        Ok(CurrentRemoteDeleteState::Missing) => {
            let _ = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            warn!(
                path = %rel_path,
                "remote tombstone disappeared after staging local delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
        Err(error) => {
            let _ = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            warn!(
                path = %rel_path,
                error = %error,
                "failed to recheck remote absence after staging local delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    }

    {
        let mut cache = state_cache.lock().await;
        cache.remove(local_path);
        if let Err(error) = cache.flush() {
            cache.set(local_path, cached.clone());
            let _ = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            warn!(
                path = %rel_path,
                error = %error,
                "state cache flush failed; rolled back staged remote delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    }

    // Close the remote publication window that included the state-cache
    // commit. If the path was republished, roll the cache and local file back
    // before consuming the obsolete delete notification.
    match current_remote_delete_state(&op, rel_path, storage_prefix).await {
        Ok(CurrentRemoteDeleteState::Live(snapshot)) => {
            let restored = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            let cache_restored = {
                let mut cache = state_cache.lock().await;
                cache.set(local_path, cached.clone());
                match cache.flush() {
                    Ok(()) => true,
                    Err(error) => {
                        warn!(
                            path = %rel_path,
                            error = %error,
                            "failed to persist state rollback after remote republish"
                        );
                        false
                    }
                }
            };
            return if restored
                && cache_restored
                && nats_snapshot_still_current(&op, &snapshot).await
            {
                RemoteDeleteOutcome::AckIgnored
            } else {
                RemoteDeleteOutcome::Withhold
            };
        }
        Ok(CurrentRemoteDeleteState::Deleted) => {}
        Ok(CurrentRemoteDeleteState::Missing) => {
            let restored = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            let mut cache = state_cache.lock().await;
            cache.set(local_path, cached.clone());
            let cache_restored = cache.flush().is_ok();
            warn!(
                path = %rel_path,
                restored,
                cache_restored,
                "remote tombstone disappeared before local delete commit; rolled back"
            );
            return RemoteDeleteOutcome::Withhold;
        }
        Err(error) => {
            let restored = restore_staged_remote_delete(
                rel_path,
                &staged_path,
                local_path,
                pending_delete_ledger_path,
                &pending,
            )
            .await;
            let mut cache = state_cache.lock().await;
            cache.set(local_path, cached.clone());
            let cache_restored = cache.flush().is_ok();
            warn!(
                path = %rel_path,
                error = %error,
                restored,
                cache_restored,
                "failed final remote-absence check; rolled back guarded delete"
            );
            return RemoteDeleteOutcome::Withhold;
        }
    }

    if let Err(error) = tokio::fs::remove_file(&staged_path).await {
        let restored = restore_staged_remote_delete(
            rel_path,
            &staged_path,
            local_path,
            pending_delete_ledger_path,
            &pending,
        )
        .await;
        let mut cache = state_cache.lock().await;
        cache.set(local_path, cached);
        let cache_restored = cache.flush().is_ok();
        warn!(
            path = %rel_path,
            error = %error,
            restored,
            cache_restored,
            "failed to commit staged remote delete; rolled back and withheld ack"
        );
        return RemoteDeleteOutcome::Withhold;
    }
    if let Err(error) = sync_remote_delete_directory(local_path) {
        warn!(
            path = %rel_path,
            error = %error,
            "remote delete removed staged bytes but directory fsync failed; retaining durable intent"
        );
        return RemoteDeleteOutcome::Withhold;
    }
    if let Err(error) = clear_pending_remote_delete(pending_delete_ledger_path, &pending) {
        warn!(
            path = %rel_path,
            error = %error,
            "remote delete committed but durable intent clear failed; withholding ack"
        );
        return RemoteDeleteOutcome::Withhold;
    }

    info!(
        path = %rel_path,
        from_device = %remote_device,
        "committed guarded local file removal for authoritative remote delete"
    );
    RemoteDeleteOutcome::AckDeleted
}

/// Reconcile a ConflictResolved notification against the exact current path
/// index. The event-supplied merged clock is deliberately absent from this
/// interface: only the indexed manifest may advance local state.
#[allow(clippy::too_many_arguments)]
async fn handle_conflict_resolved(
    device_id: &str,
    event_device: &str,
    rel_path: &str,
    local_path: &std::path::Path,
    operator: &Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    path_locks: &tcfs_sync::state::PathLocks,
    storage_prefix: &str,
    auto_download_limit: Option<u64>,
    tcfs_config: &TcfsConfig,
    master_key: &Arc<tokio::sync::Mutex<Option<MasterKey>>>,
) -> bool {
    // These placeholder values are diagnostics only. `handle_auto_pull`
    // resolves and retains a typed snapshot from the live exact index before
    // it classifies or mutates anything.
    let notification_clock = tcfs_sync::conflict::VectorClock::new();
    handle_auto_pull(
        device_id,
        event_device,
        rel_path,
        "<conflict-resolved-notification>",
        0,
        &notification_clock,
        local_path,
        operator,
        state_cache,
        path_locks,
        storage_prefix,
        auto_download_limit,
        tcfs_config,
        master_key,
    )
    .await
}

/// Persist knowledge from an exact indexed snapshot when local and remote
/// content are already identical. A changed snapshot or any flush failure
/// rolls the in-memory update back and withholds acknowledgement.
async fn persist_current_snapshot_vclock(
    rel_path: &str,
    local_path: &std::path::Path,
    op: &opendal::Operator,
    snapshot: &tcfs_sync::engine::IndexedManifestSnapshot,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
) -> bool {
    if !nats_snapshot_still_current(op, snapshot).await {
        return false;
    }

    let previous = {
        let cache = state_cache.lock().await;
        match cache.get(local_path).cloned() {
            Some(entry) if entry.blake3 == snapshot.content_hash() => entry,
            Some(_) => {
                warn!(
                    path = %rel_path,
                    "local cache identity changed before authoritative vclock update"
                );
                return false;
            }
            None => {
                warn!(
                    path = %rel_path,
                    "local cache entry vanished before authoritative vclock update"
                );
                return false;
            }
        }
    };

    let mut merged_vclock = previous.vclock.clone();
    merged_vclock.merge(snapshot.vclock());
    if merged_vclock == previous.vclock {
        return nats_snapshot_still_current(op, snapshot).await;
    }

    {
        let mut cache = state_cache.lock().await;
        cache.set(
            local_path,
            tcfs_sync::state::SyncState {
                vclock: merged_vclock,
                ..previous.clone()
            },
        );
        if let Err(error) = cache.flush() {
            cache.set(local_path, previous.clone());
            warn!(
                path = %rel_path,
                error = %error,
                "failed to persist authoritative conflict-resolution vclock; withholding ack"
            );
            return false;
        }
    }

    if nats_snapshot_still_current(op, snapshot).await {
        return true;
    }

    let mut cache = state_cache.lock().await;
    cache.set(local_path, previous);
    if let Err(error) = cache.flush() {
        warn!(
            path = %rel_path,
            error = %error,
            "failed to persist vclock rollback after indexed snapshot changed"
        );
    }
    false
}

/// Handle auto-pull logic for a remote FileSynced event.
#[allow(clippy::too_many_arguments)]
async fn handle_auto_pull(
    device_id: &str,
    remote_device: &str,
    rel_path: &str,
    remote_blake3: &str,
    remote_size: u64,
    remote_vclock: &tcfs_sync::conflict::VectorClock,
    local_path: &std::path::Path,
    operator: &Arc<tokio::sync::Mutex<Option<opendal::Operator>>>,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    path_locks: &tcfs_sync::state::PathLocks,
    storage_prefix: &str,
    auto_download_limit: Option<u64>,
    tcfs_config: &TcfsConfig,
    master_key: &Arc<tokio::sync::Mutex<Option<MasterKey>>>,
) -> bool {
    // Keep this private helper fail-closed even if a future caller bypasses
    // spawn_state_sync_loop's containment resolver. Invalid poison events are
    // safe to acknowledge because no retry can make their path valid.
    if let Err(error) = tcfs_sync::index_entry::validate_canonical_rel_path(rel_path) {
        warn!(
            path = %rel_path,
            error = %error,
            "dropping invalid NATS auto-pull path"
        );
        return true;
    }
    let git_internal = auto_conflict_must_defer(rel_path);

    let _lock_guard = path_locks.lock(local_path).await;

    // The NATS manifest_path is deliberately ignored. It is only a historical
    // notification field and can lag or be forged. Resolve the exact current
    // path index and retain that typed authority through the checked download.
    let op = {
        let guard = operator.lock().await;
        match guard.as_ref() {
            Some(op) => op.clone(),
            None => {
                warn!(path = %rel_path, "no storage operator for NATS auto-pull");
                return false;
            }
        }
    };
    let snapshot = match tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(
        &op,
        rel_path,
        storage_prefix,
    )
    .await
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            warn!(
                path = %rel_path,
                "NATS auto-pull has no exact current index entry; withholding ack"
            );
            return false;
        }
        Err(error) => {
            warn!(
                path = %rel_path,
                error = %error,
                "failed to snapshot current NATS index entry; withholding ack"
            );
            return false;
        }
    };

    if remote_blake3 != snapshot.content_hash()
        || remote_size != snapshot.size()
        || remote_vclock != snapshot.vclock()
        || remote_device != snapshot.written_by()
    {
        info!(
            path = %rel_path,
            event_hash = %remote_blake3,
            current_hash = %snapshot.content_hash(),
            event_size = remote_size,
            current_size = snapshot.size(),
            event_device = %remote_device,
            current_device = %snapshot.written_by(),
            "NATS event metadata lags the exact current indexed snapshot"
        );
    }

    if !git_internal && auto_download_limit.is_some_and(|limit| snapshot.size() > limit) {
        debug!(
            path = %rel_path,
            event_size = remote_size,
            current_size = snapshot.size(),
            threshold = auto_download_limit.unwrap_or_default(),
            "skipping auto-pull: authoritative OnDemand object exceeds download threshold"
        );
        return nats_snapshot_still_current(&op, &snapshot).await;
    }

    let cached = {
        let cache = state_cache.lock().await;
        cache.get(local_path).cloned()
    };
    if cached
        .as_ref()
        .is_some_and(|entry| entry.status == tcfs_sync::state::FileSyncStatus::Active)
    {
        info!(path = %rel_path, "deferring auto-pull: file is actively being modified");
        return false;
    }

    let expected_local = match tcfs_sync::engine::capture_local_fingerprint(local_path) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            warn!(
                path = %rel_path,
                error = %error,
                "failed to capture local identity before NATS auto-pull; withholding ack"
            );
            return false;
        }
    };

    let actual_local_hash = match &expected_local {
        tcfs_sync::engine::ExpectedLocalFingerprint::Absent => None,
        tcfs_sync::engine::ExpectedLocalFingerprint::Tracked { blake3, .. } => {
            Some(blake3.as_str())
        }
    };
    let local_diverged = match (&cached, actual_local_hash) {
        (None, None) => false,
        (Some(entry), Some(actual)) => entry.blake3 != actual,
        // An existing untracked entry and a missing tracked entry are both
        // local changes. Neither may be silently replaced by a remote wake-up.
        _ => true,
    };

    if local_diverged {
        let mut local_vclock = cached
            .as_ref()
            .map(|entry| entry.vclock.clone())
            .unwrap_or_default();
        local_vclock.tick(device_id);
        let local_blake3 = actual_local_hash.unwrap_or("<absent>").to_string();
        let detected_at = tcfs_sync::StateEvent::now();
        let conflict = tcfs_sync::conflict::ConflictInfo {
            rel_path: rel_path.to_string(),
            local_vclock,
            remote_vclock: snapshot.vclock().clone(),
            local_blake3,
            remote_blake3: snapshot.content_hash().to_string(),
            local_device: device_id.to_string(),
            remote_device: snapshot.written_by().to_string(),
            detected_at,
            times_recorded: 0,
            remote_manifest_key: Some(snapshot.manifest_path().to_string()),
        };
        let mut cache = state_cache.lock().await;
        let previous = cache.get(local_path).cloned();
        watcher_record_conflict(&mut cache, local_path, conflict);
        if let Err(error) = cache.flush() {
            match previous {
                Some(previous) => cache.set(local_path, previous),
                None => cache.remove(local_path),
            }
            warn!(
                path = %rel_path,
                error = %error,
                "failed to persist NATS local-divergence conflict; withholding ack"
            );
            return false;
        }
        warn!(
            path = %rel_path,
            "local bytes differ from cached identity; preserving them and withholding NATS ack"
        );
        return false;
    }

    let Some(cached) = cached else {
        if git_internal {
            info!(
                path = %rel_path,
                "deferring new .git-internal remote path to repo-group reconciliation"
            );
            return false;
        }
        info!(path = %rel_path, from = %snapshot.written_by(), "new file from current remote snapshot, pulling");
        return do_auto_download(
            device_id,
            remote_blake3,
            remote_size,
            rel_path,
            local_path,
            &op,
            &snapshot,
            &expected_local,
            state_cache,
            tcfs_config,
            master_key,
        )
        .await;
    };

    let local_blake3 = cached.blake3;
    let local_vclock = cached.vclock;

    let outcome = tcfs_sync::conflict::compare_clocks(
        &local_vclock,
        snapshot.vclock(),
        &local_blake3,
        snapshot.content_hash(),
        rel_path,
        device_id,
        snapshot.written_by(),
    );

    match outcome {
        tcfs_sync::conflict::SyncOutcome::UpToDate => {
            info!(path = %rel_path, "already up to date");
            persist_current_snapshot_vclock(rel_path, local_path, &op, &snapshot, state_cache).await
        }
        tcfs_sync::conflict::SyncOutcome::LocalNewer => {
            info!(path = %rel_path, "local is newer, skipping pull");
            nats_snapshot_still_current(&op, &snapshot).await
        }
        tcfs_sync::conflict::SyncOutcome::RemoteNewer => {
            if git_internal {
                info!(
                    path = %rel_path,
                    "deferring newer .git-internal remote path to repo-group reconciliation"
                );
                return false;
            }
            info!(path = %rel_path, from = %snapshot.written_by(), "current remote snapshot is newer, auto-pulling");
            do_auto_download(
                device_id,
                remote_blake3,
                remote_size,
                rel_path,
                local_path,
                &op,
                &snapshot,
                &expected_local,
                state_cache,
                tcfs_config,
                master_key,
            )
            .await
        }
        tcfs_sync::conflict::SyncOutcome::Conflict(conflict_info) => {
            let mut conflict_info = conflict_info;
            conflict_info.remote_manifest_key = Some(snapshot.manifest_path().to_string());
            // keep-both PR-1 (safety invariant S2): automatic per-file
            // resolution must NEVER touch `.git` internals. AutoResolver's
            // lexicographic KeepRemote would auto-download the remote ref/index
            // over this device's object store with zero `.git` awareness — the
            // G5-git-5 interleave vector. Defer `.git`-internal conflicts
            // unconditionally; the operator resolves the repo group deliberately
            // (`tcfs resolve <repo> --strategy keep-both --execute`; inspect
            // with `tcfs conflicts`). The conflict stays recorded by the
            // reconcile engine's Conflict arm, so it remains visible and
            // re-tried.
            if git_internal {
                {
                    let mut cache = state_cache.lock().await;
                    let previous = cache.get(local_path).cloned();
                    watcher_record_conflict(&mut cache, local_path, conflict_info.clone());
                    if let Err(e) = cache.flush() {
                        match previous {
                            Some(previous) => cache.set(local_path, previous),
                            None => cache.remove(local_path),
                        }
                        warn!(path = %rel_path, "failed to persist deferred git conflict: {e}");
                        return false;
                    }
                }
                info!(
                    path = %rel_path,
                    "AutoResolver: deferring .git-internal conflict (repo-group resolution required)"
                );
                return false;
            }
            info!(
                path = %rel_path,
                local_device = %conflict_info.local_device,
                remote_device = %conflict_info.remote_device,
                "conflict detected, applying AutoResolver"
            );
            let resolver = tcfs_sync::conflict::AutoResolver;
            match resolver.resolve(&conflict_info) {
                Some(tcfs_sync::conflict::Resolution::KeepLocal) => {
                    info!(path = %rel_path, "AutoResolver: keeping local");
                }
                Some(tcfs_sync::conflict::Resolution::KeepRemote) => {
                    info!(path = %rel_path, "AutoResolver: keeping remote");
                    return do_auto_download(
                        device_id,
                        remote_blake3,
                        remote_size,
                        rel_path,
                        local_path,
                        &op,
                        &snapshot,
                        &expected_local,
                        state_cache,
                        tcfs_config,
                        master_key,
                    )
                    .await;
                }
                _ => {
                    info!(path = %rel_path, "AutoResolver: deferred");
                }
            }
            nats_snapshot_still_current(&op, &snapshot).await
        }
    }
}

/// Hydrate the exact object selected by the current path index.
///
/// A NATS event is a wake-up signal, not manifest authority. The caller has
/// already captured and validated the live index plus manifest bytes. This
/// function preserves that single snapshot through a guarded commit: the exact
/// index authority and local bytes must both still match immediately before the
/// atomic replacement.
#[allow(clippy::too_many_arguments)]
async fn do_auto_download(
    device_id: &str,
    event_blake3: &str,
    event_size: u64,
    rel_path: &str,
    local_path: &std::path::Path,
    op: &opendal::Operator,
    snapshot: &tcfs_sync::engine::IndexedManifestSnapshot,
    expected_local: &tcfs_sync::engine::ExpectedLocalFingerprint,
    state_cache: &Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
    tcfs_config: &TcfsConfig,
    master_key: &Arc<tokio::sync::Mutex<Option<MasterKey>>>,
) -> bool {
    let encryption = {
        let guard = master_key.lock().await;
        guard
            .as_ref()
            .map(|key| crate::grpc::build_encryption_context(tcfs_config, device_id, key))
    };

    let mut cache = state_cache.lock().await;
    let previous = cache.get(local_path).cloned();
    let result = tcfs_sync::engine::hydrate_indexed_snapshot_with_device(
        op,
        snapshot,
        local_path,
        None,
        device_id,
        Some(&mut cache),
        encryption.as_ref(),
        expected_local,
    )
    .await;

    match result {
        Ok(download) => {
            if let Some(current) = download.sync_state.as_ref() {
                if current.blake3 != event_blake3 || current.size != event_size {
                    info!(
                        path = %rel_path,
                        event_hash = %event_blake3,
                        current_hash = %current.blake3,
                        event_size,
                        current_size = current.size,
                        manifest = %snapshot.manifest_path(),
                        "NATS event lagged current path index; hydrated current indexed object"
                    );
                }
            }

            if let Err(error) = cache.flush() {
                warn!(
                    path = %local_path.display(),
                    error = %error,
                    "auto-pull state cache flush failed; withholding ack"
                );
                match previous {
                    Some(previous) => cache.set(local_path, previous),
                    None => cache.remove(local_path),
                }
                return false;
            }

            info!(
                path = %local_path.display(),
                manifest = %snapshot.manifest_path(),
                bytes = download.bytes,
                "auto-pull hydrated current indexed object"
            );
            drop(cache);
            nats_snapshot_still_current(op, snapshot).await
        }
        Err(error) => {
            match previous {
                Some(previous) => cache.set(local_path, previous),
                None => cache.remove(local_path),
            }
            warn!(
                path = %local_path.display(),
                manifest = %snapshot.manifest_path(),
                error = %error,
                "auto-pull failed to hydrate current indexed object; withholding ack"
            );
            false
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

/// Resolve the canonical `state.json` path from the configured `state_db` and,
/// exactly once, absorb a legacy sibling `state.db` into it.
///
/// `state_db` is `~`-expanded first (config defaults carry a literal `~` and
/// the loader does no normalization; without expansion the absorb would write
/// a CWD-relative `./~/…`). The one-time absorb fires **only** when the
/// canonical `.json` is absent and a *distinct* sibling `.db` exists — so an
/// existing `.json` always wins untouched, with no size heuristic or merge
/// (the live reality on hosts where both files exist).
///
/// The legacy file is first validated through the real secure `StateCache`
/// reader, then atomically renamed without replacement and its parent is
/// fsynced. Any validation/rename/fsync failure aborts daemon startup; an
/// existing legacy authority must never silently degrade into fresh state.
fn absorb_legacy_state_db(state_db: &std::path::Path) -> Result<std::path::PathBuf> {
    let state_db = tcfs_core::config::expand_tilde(state_db);
    let state_json_path = state_db.with_extension("json");
    let entry_exists = |path: &std::path::Path| -> Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("inspecting daemon state path: {}", path.display())),
        }
    };

    // A dangling symlink or non-regular canonical entry counts as present and
    // is rejected by the subsequent secure open instead of being overwritten.
    if !entry_exists(&state_json_path)? && state_db != state_json_path && entry_exists(&state_db)? {
        let mut legacy_state = open_daemon_state_cache(&state_db)
            .context("validating legacy daemon state before absorb")?;
        // `StateCache::open` can recover content corruption from the secure
        // `.json.bak` generation. Make that repair explicit and fallible here;
        // relying on Drop would only warn on a failed flush and could rename a
        // still-corrupt primary into canonical authority.
        legacy_state
            .flush()
            .context("durably repairing recovered legacy daemon state before absorb")?;
        drop(legacy_state);
        rename_noreplace(&state_db, &state_json_path).with_context(|| {
            format!(
                "atomically absorbing legacy daemon state without replacement: {} -> {}",
                state_db.display(),
                state_json_path.display()
            )
        })?;
        if let Err(sync_error) = sync_remote_delete_directory(&state_json_path) {
            let rollback = rename_noreplace(&state_json_path, &state_db)
                .context("rolling back legacy daemon state absorb after directory sync failure")
                .and_then(|()| {
                    sync_remote_delete_directory(&state_db)
                        .context("syncing rolled-back legacy daemon state authority")
                });
            match rollback {
                Ok(()) => anyhow::bail!(
                    "syncing legacy daemon state absorb failed and was rolled back: {sync_error:#}"
                ),
                Err(rollback_error) => anyhow::bail!(
                    "syncing legacy daemon state absorb failed: {sync_error:#}; rollback also failed: {rollback_error:#}"
                ),
            }
        }
        info!("migrated legacy state.db → state.json");
    }
    Ok(state_json_path)
}

#[cfg(test)]
mod invite_redemption_startup_tests {
    use super::{load_invite_redemptions_for_startup, TcfsDaemonImpl};
    use std::sync::Arc;

    fn test_daemon(temp: &tempfile::TempDir) -> TcfsDaemonImpl {
        TcfsDaemonImpl::new(
            crate::cred_store::new_shared(),
            Arc::new(tcfs_core::config::TcfsConfig::default()),
            false,
            "memory://".into(),
            Arc::new(tokio::sync::Mutex::new(
                tcfs_sync::state::StateCache::open(&temp.path().join("state.json")).unwrap(),
            )),
            Arc::new(tokio::sync::Mutex::new(None)),
            tcfs_sync::state::PathLocks::new(),
            "startup-test-device".into(),
            "startup-test-device".into(),
            None,
        )
    }

    async fn seed_redeemed_invite(path: &std::path::Path) {
        let store = tcfs_auth::InviteRedemptionStore::new();
        store
            .claim("invite-a", "nonce-a", "laptop", "age1test", "linux-x86_64")
            .await
            .unwrap();
        store.save_to_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn first_start_allows_missing_invite_redemption_store() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon(&temp);
        let path = temp.path().join("invite-redemptions.json");

        load_invite_redemptions_for_startup(&daemon, &path)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_fails_closed_on_corrupt_invite_redemption_store() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon(&temp);
        let path = temp.path().join("invite-redemptions.json");
        seed_redeemed_invite(&path).await;

        std::fs::write(&path, b"{\"invite-a:nonce-a\":").unwrap();

        let error = load_invite_redemptions_for_startup(&daemon, &path)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("loading invite redemption store"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restart_fails_closed_on_unsafe_invite_redemption_store() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let daemon = test_daemon(&temp);
        let path = temp.path().join("invite-redemptions.json");
        seed_redeemed_invite(&path).await;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = load_invite_redemptions_for_startup(&daemon, &path)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("loading invite redemption store"));
    }
}

#[cfg(test)]
mod keep_both_pr1_tests {
    use super::{
        acquire_daemon_instance_lock, auto_conflict_must_defer, contained_nats_target,
        first_never_reconcile_path, handle_auto_pull, handle_conflict_resolved,
        handle_remote_delete, nats_fixed_ingress_deny, open_automatic_policy_store,
        open_daemon_state_cache, pending_remote_delete_paths, prepare_private_daemon_data_dir,
        reconcile_lock_paths, reconcile_plan_has_mutations, remote_delete_staging_path,
        rename_noreplace, replay_pending_remote_deletes, stage_remote_delete_with_hook,
        validate_nats_resolved_mutation_target, write_pending_delete_ledger, PendingRemoteDelete,
        PendingRemoteDeleteLedger, RemoteDeleteOutcome, PENDING_REMOTE_DELETE_LEDGER_VERSION,
    };
    use opendal::services::Memory;
    use opendal::Operator;

    fn test_config() -> tcfs_core::config::TcfsConfig {
        tcfs_core::config::TcfsConfig::default()
    }

    fn no_master_key() -> std::sync::Arc<tokio::sync::Mutex<Option<tcfs_crypto::MasterKey>>> {
        std::sync::Arc::new(tokio::sync::Mutex::new(None))
    }

    fn memory_operator() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    async fn memory_operator_with_tombstone(rel_path: &str) -> Operator {
        let op = memory_operator();
        op.write(
            &format!("data/index/{rel_path}"),
            tcfs_sync::index_entry::VersionedIndexEntry::deleted()
                .to_json_bytes()
                .unwrap(),
        )
        .await
        .unwrap();
        op
    }

    fn test_manifest(
        rel_path: &str,
        file_hash: &str,
        file_size: u64,
        vclock: tcfs_sync::conflict::VectorClock,
    ) -> tcfs_sync::manifest::SyncManifest {
        tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: file_hash.into(),
            file_size,
            chunks: Vec::new(),
            vclock,
            written_by: "honey".into(),
            written_at: 1_700_000_000,
            rel_path: Some(rel_path.into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        }
    }

    async fn seed_current_manifest(
        op: &Operator,
        rel_path: &str,
        object_id: &str,
        manifest: &tcfs_sync::manifest::SyncManifest,
    ) {
        op.write(
            &format!("data/manifests/{object_id}"),
            manifest.to_bytes().unwrap(),
        )
        .await
        .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            op,
            "data",
            &format!("data/index/{rel_path}"),
            &tcfs_sync::index_entry::RemoteIndexEntry::new(
                object_id,
                manifest.file_size,
                manifest.chunks.len(),
            ),
        )
        .await
        .unwrap();
    }

    fn bytes_hash(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    async fn seed_current_bytes(
        op: &Operator,
        rel_path: &str,
        bytes: &[u8],
        vclock: tcfs_sync::conflict::VectorClock,
        written_by: &str,
    ) -> String {
        let file_hash = bytes_hash(bytes);
        let chunks = if bytes.is_empty() {
            Vec::new()
        } else {
            op.write(&format!("data/chunks/{file_hash}"), bytes.to_vec())
                .await
                .unwrap();
            vec![file_hash.clone()]
        };
        let manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: file_hash.clone(),
            file_size: bytes.len() as u64,
            chunks,
            vclock,
            written_by: written_by.into(),
            written_at: 1_700_000_000,
            rel_path: Some(rel_path.into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        seed_current_manifest(op, rel_path, &file_hash, &manifest).await;
        file_hash
    }

    fn synced_state(
        rel_path: &str,
        blake3: String,
        size: u64,
        vclock: tcfs_sync::conflict::VectorClock,
        device_id: &str,
    ) -> tcfs_sync::state::SyncState {
        tcfs_sync::state::SyncState {
            blake3,
            size,
            mtime: 0,
            chunk_count: usize::from(size > 0),
            remote_path: rel_path.into(),
            last_synced: 0,
            vclock,
            device_id: device_id.into(),
            conflict: None,
            status: tcfs_sync::state::FileSyncStatus::Synced,
        }
    }

    fn state_cache_with_blocked_flush(dir: &std::path::Path) -> tcfs_sync::state::StateCache {
        // StateCache::open now rejects an already-invalid parent topology.
        // Open against a valid directory, then replace that directory with a
        // file so the operation under test encounters the intended flush
        // failure rather than bypassing startup validation.
        let blocked_parent = dir.join("state-parent-is-file");
        std::fs::create_dir(&blocked_parent).unwrap();
        let cache = tcfs_sync::state::StateCache::open(&blocked_parent.join("state.json")).unwrap();
        std::fs::remove_dir(&blocked_parent).unwrap();
        std::fs::write(&blocked_parent, b"not a directory").unwrap();
        cache
    }

    async fn remote_delete_outcome(
        rel_path: &str,
        local_path: &std::path::Path,
        operator: Option<Operator>,
        state_cache: &std::sync::Arc<tokio::sync::Mutex<tcfs_sync::state::StateCache>>,
        effective_mode: tcfs_sync::policy::SyncMode,
    ) -> RemoteDeleteOutcome {
        let ledger_path = local_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("pending-remote-deletes.json");
        let pending_delete_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        handle_remote_delete(
            "neo",
            "honey",
            rel_path,
            local_path,
            &std::sync::Arc::new(tokio::sync::Mutex::new(operator)),
            state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            effective_mode,
            &ledger_path,
            &pending_delete_lock,
        )
        .await
    }

    #[test]
    fn auto_defers_git_internal_conflicts() {
        // keep-both PR-1 (S2): the NATS auto path defers `.git`-internal
        // conflicts instead of auto-downloading them per file.
        assert!(auto_conflict_must_defer("myrepo/.git/refs/heads/main"));
        assert!(auto_conflict_must_defer("myrepo/.git/index"));
        assert!(auto_conflict_must_defer("nested/dir/proj/.git/HEAD"));
    }

    #[test]
    fn auto_resolves_normal_files() {
        // Ordinary (non-`.git`) file conflicts keep today's auto behavior.
        assert!(!auto_conflict_must_defer("notes/todo.txt"));
        assert!(!auto_conflict_must_defer("src/main.rs"));
        // A file merely named like git but not under a `.git` dir is not fenced.
        assert!(!auto_conflict_must_defer("docs/gitignore-notes.md"));
    }

    #[test]
    fn remote_delete_staging_is_watcher_fenced() {
        let staged = remote_delete_staging_path(std::path::Path::new("sync/notes/todo.txt"));
        assert_eq!(
            staged.extension().and_then(std::ffi::OsStr::to_str),
            Some("tc")
        );
        assert!(
            staged
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".tcfs-delete-"),
            "staging path remains recognizable for recovery diagnostics"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn private_daemon_data_dir_is_owner_only_and_rejects_unsafe_existing_mode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private/tcfsd");
        prepare_private_daemon_data_dir(&private).unwrap();
        let metadata = std::fs::symlink_metadata(&private).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o700);

        let unsafe_dir = dir.path().join("unsafe-tcfsd");
        std::fs::create_dir(&unsafe_dir).unwrap();
        std::fs::set_permissions(&unsafe_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        let error = prepare_private_daemon_data_dir(&unsafe_dir).unwrap_err();
        assert!(error.to_string().contains("mode 0700"), "{error:#}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn private_daemon_data_dir_validates_existing_ancestor_before_creation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let unsafe_parent = dir.path().join("unsafe-parent");
        std::fs::create_dir(&unsafe_parent).unwrap();
        std::fs::set_permissions(&unsafe_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let candidate = unsafe_parent.join("missing/tcfsd");

        let error = prepare_private_daemon_data_dir(&candidate).unwrap_err();
        assert!(
            format!("{error:#}").contains("creation ancestor"),
            "{error:#}"
        );
        assert!(
            !unsafe_parent.join("missing").exists(),
            "untrusted ancestors must be rejected before directory creation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_daemon_data_dir_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let alias = dir.path().join("tcfsd");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let error = prepare_private_daemon_data_dir(&alias).unwrap_err();
        assert!(error.to_string().contains("real directory"), "{error:#}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn daemon_lifetime_lock_rejects_second_instance_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = prepare_private_daemon_data_dir(&dir.path().join("tcfsd")).unwrap();
        let first = acquire_daemon_instance_lock(&data_dir).unwrap();
        let error = acquire_daemon_instance_lock(&data_dir).unwrap_err();
        assert!(format!("{error:#}").contains("locked by another process"));
        drop(first);
        acquire_daemon_instance_lock(&data_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn daemon_state_cache_corruption_fails_closed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::write(&state_path, b"{corrupt daemon state").unwrap();
        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = open_daemon_state_cache(&state_path)
            .err()
            .expect("corrupt daemon state must fail closed");
        assert!(
            format!("{error:#}").contains("opening authoritative daemon state cache"),
            "{error:#}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
    #[test]
    fn atomic_no_replace_preserves_both_source_and_recreated_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("staged.tc");
        let destination = dir.path().join("original.txt");
        std::fs::write(&source, b"parked bytes").unwrap();
        std::fs::write(&destination, b"recreated bytes").unwrap();

        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&source).unwrap(), b"parked bytes");
        assert_eq!(std::fs::read(&destination).unwrap(), b"recreated bytes");
    }

    #[test]
    fn fixed_ingress_ack_drops_every_automatic_nats_mutation_variant() {
        let blacklist = tcfs_sync::blacklist::Blacklist::from_sync_config(&test_config().sync);
        let events = [
            tcfs_sync::StateEvent::FileSynced {
                device_id: "honey".into(),
                rel_path: ".ssh/id_ed25519".into(),
                blake3: "forged".into(),
                size: 1,
                vclock: Default::default(),
                manifest_path: "data/manifests/forged".into(),
                timestamp: 1,
            },
            tcfs_sync::StateEvent::FileDeleted {
                device_id: "honey".into(),
                rel_path: "secrets/AUTH.JSON".into(),
                vclock: Default::default(),
                timestamp: 1,
            },
            tcfs_sync::StateEvent::ConflictResolved {
                device_id: "honey".into(),
                rel_path: "repo/.git/index.lock".into(),
                resolution: "remote".into(),
                merged_vclock: Default::default(),
                timestamp: 1,
            },
        ];

        for event in &events {
            assert!(
                nats_fixed_ingress_deny(&blacklist, event).is_some(),
                "fixed ingress must ACK-drop {event:?}"
            );
        }
        let ordinary = tcfs_sync::StateEvent::FileDeleted {
            device_id: "honey".into(),
            rel_path: "notes/todo.txt".into(),
            vclock: Default::default(),
            timestamp: 1,
        };
        assert!(nats_fixed_ingress_deny(&blacklist, &ordinary).is_none());
    }

    #[test]
    fn rootless_nats_target_cannot_alias_configured_master_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("custom-sensitive-material.bin");
        std::fs::write(&key_path, [7_u8; tcfs_crypto::KEY_SIZE]).unwrap();
        let mut config = test_config();
        config.crypto.master_key_file = Some(key_path.clone());
        let blacklist = tcfs_sync::blacklist::Blacklist::from_sync_config(&config.sync);

        let error = validate_nats_resolved_mutation_target(
            &config,
            &blacklist,
            "ordinary-looking-name.bin",
            &key_path,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("master-key material"));
    }

    #[test]
    fn corrupt_policy_is_unavailable_to_automatic_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("folder-policies.json");
        std::fs::write(&policy_path, b"{not valid policy JSON").unwrap();

        for surface in [
            "watcher",
            "watcher scheduler",
            "NATS FileSynced",
            "NATS FileDeleted",
            "NATS ConflictResolved",
            "auto-unsync",
            "periodic reconcile",
            "periodic reconcile execution",
        ] {
            let error = open_automatic_policy_store(&policy_path, surface)
                .err()
                .expect("corrupt policy must fail closed");
            assert!(
                format!("{error:#}").contains("parsing policy store"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn periodic_reconcile_never_policy_suppresses_whole_group_and_locks_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        let never = root.join("private");
        std::fs::create_dir_all(&never).unwrap();
        let policy_path = dir.path().join("folder-policies.json");
        let mut policy = tcfs_sync::policy::PolicyStore::open(&policy_path).unwrap();
        policy.set(
            &never,
            tcfs_sync::policy::FolderPolicy {
                sync_mode: tcfs_sync::policy::SyncMode::Never,
                ..Default::default()
            },
        );
        let plan = tcfs_sync::reconcile::ReconcilePlan {
            actions: vec![
                tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
                    rel_path: "z-last.txt".into(),
                },
                tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
                    rel_path: "private/secret.txt".into(),
                },
                tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
                    rel_path: "a-first.txt".into(),
                },
                tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
                    rel_path: "z-last.txt".into(),
                },
            ],
            summary: Default::default(),
            device_id: "neo".into(),
            generated_at: 1,
        };

        assert!(reconcile_plan_has_mutations(&plan));
        assert_eq!(
            first_never_reconcile_path(&plan, &root, &policy).unwrap(),
            Some("private/secret.txt".into())
        );
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(
            reconcile_lock_paths(&plan, &root).unwrap(),
            vec![
                canonical_root.join("a-first.txt"),
                canonical_root.join("private/secret.txt"),
                canonical_root.join("z-last.txt")
            ]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn periodic_reconcile_lock_paths_deduplicate_symlinked_parent_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        let real_parent = root.join("real");
        let alias_parent = root.join("alias");
        std::fs::create_dir_all(&real_parent).unwrap();
        std::os::unix::fs::symlink(&real_parent, &alias_parent).unwrap();
        let plan = tcfs_sync::reconcile::ReconcilePlan {
            actions: vec![
                tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
                    rel_path: "real/file.txt".into(),
                },
                tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
                    rel_path: "alias/file.txt".into(),
                },
            ],
            summary: Default::default(),
            device_id: "neo".into(),
            generated_at: 1,
        };

        assert_eq!(
            reconcile_lock_paths(&plan, &root).unwrap(),
            vec![std::fs::canonicalize(real_parent).unwrap().join("file.txt")]
        );
    }

    #[test]
    fn crash_after_staging_rename_replays_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        let local_path = root.join("notes/todo.txt");
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, b"original bytes").unwrap();
        let staged_path = remote_delete_staging_path(&local_path);
        let ledger_path = dir.path().join("state/pending-remote-deletes.json");

        let error = stage_remote_delete_with_hook(
            &ledger_path,
            "notes/todo.txt",
            &local_path,
            &staged_path,
            || anyhow::bail!("injected crash after staging rename"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected crash"), "{error:#}");
        assert!(!local_path.exists());
        assert_eq!(std::fs::read(&staged_path).unwrap(), b"original bytes");
        assert!(ledger_path.exists());

        replay_pending_remote_deletes(&ledger_path, Some(&root)).unwrap();

        assert_eq!(std::fs::read(&local_path).unwrap(), b"original bytes");
        assert!(!staged_path.exists());
        assert!(!ledger_path.exists());
    }

    #[test]
    fn pending_delete_replay_never_overwrites_recreated_original() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        let local_path = root.join("notes/todo.txt");
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, b"park me").unwrap();
        let staged_path = remote_delete_staging_path(&local_path);
        let ledger_path = dir.path().join("state/pending-remote-deletes.json");
        stage_remote_delete_with_hook(
            &ledger_path,
            "notes/todo.txt",
            &local_path,
            &staged_path,
            || anyhow::bail!("injected crash"),
        )
        .unwrap_err();
        std::fs::write(&local_path, b"recreated bytes").unwrap();

        replay_pending_remote_deletes(&ledger_path, Some(&root)).unwrap();

        assert_eq!(std::fs::read(&local_path).unwrap(), b"recreated bytes");
        assert_eq!(std::fs::read(&staged_path).unwrap(), b"park me");
        assert!(
            ledger_path.exists(),
            "unresolved recovery intent is retained"
        );
    }

    #[test]
    fn pending_delete_ledger_validates_containment_and_stage_shape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        std::fs::create_dir_all(root.join("notes")).unwrap();

        let traversal = PendingRemoteDelete {
            rel_path: "../outside.txt".into(),
            staged_name: ".outside.txt.tcfs-delete-00000000-0000-0000-0000-000000000000.tc".into(),
        };
        assert!(pending_remote_delete_paths(&root, &traversal).is_err());

        let malformed_stage = PendingRemoteDelete {
            rel_path: "notes/todo.txt".into(),
            staged_name: "../../outside.tc".into(),
        };
        assert!(pending_remote_delete_paths(&root, &malformed_stage).is_err());

        let ledger_path = dir.path().join("pending.json");
        write_pending_delete_ledger(
            &ledger_path,
            &PendingRemoteDeleteLedger {
                version: PENDING_REMOTE_DELETE_LEDGER_VERSION,
                pending: Some(traversal),
            },
        )
        .unwrap();
        assert!(
            replay_pending_remote_deletes(&ledger_path, Some(&root)).is_err(),
            "startup replay must fail closed on an escaping ledger entry"
        );
    }

    #[test]
    fn nats_targets_require_canonical_contained_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        std::fs::create_dir_all(&root).unwrap();

        let safe = contained_nats_target(&root, "notes/todo.txt").unwrap();
        assert_eq!(
            safe,
            std::fs::canonicalize(&root).unwrap().join("notes/todo.txt")
        );

        for invalid in [
            "",
            "/absolute",
            "../outside",
            "notes/../outside",
            "notes/./todo.txt",
            "notes//todo.txt",
            "notes\\todo.txt",
            "notes/line\nfeed",
            "C:/windows/path",
        ] {
            assert!(
                contained_nats_target(&root, invalid).is_err(),
                "NATS path must be rejected: {invalid:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn nats_target_rejects_parent_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sync");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

        let error = contained_nats_target(&root, "escape/victim.txt")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("escaped sync root"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn git_conflict_flush_failure_withholds_ack_signal() {
        // If a deferred `.git` conflict cannot be persisted, the auto-pull
        // handler must return false so the NATS caller skips ack and lets
        // JetStream redeliver. This is the event-loss guard for PR-1.
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = sync_root.join(rel_path);
        let local_bytes = b"local-ref";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();
        let local_hash = bytes_hash(local_bytes);

        let mut state_cache = state_cache_with_blocked_flush(dir.path());
        state_cache.set(
            &local_path,
            tcfs_sync::state::SyncState {
                blake3: local_hash,
                size: local_bytes.len() as u64,
                mtime: 0,
                chunk_count: 0,
                remote_path: rel_path.into(),
                last_synced: 0,
                vclock: tcfs_sync::conflict::VectorClock::new(),
                device_id: "neo".into(),
                conflict: None,
                status: tcfs_sync::state::FileSyncStatus::Synced,
            },
        );

        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(state_cache));
        let remote_vclock = tcfs_sync::conflict::VectorClock::new();
        let op = memory_operator();
        let remote_bytes = b"remote-ref";
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));
        let config = test_config();
        let master_key = no_master_key();

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &config,
            &master_key,
        )
        .await;

        assert!(
            !should_ack,
            "failed deferred-conflict persistence must withhold ack"
        );
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("original in-memory state");
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(
            state.conflict.is_none(),
            "failed flush must roll back memory"
        );
    }

    #[tokio::test]
    async fn auto_pull_defers_new_git_internal_path_without_hydration_or_cache_advance() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap(),
        ));
        let mut remote_vclock = tcfs_sync::conflict::VectorClock::new();
        remote_vclock.tick("honey");
        let remote_bytes = b"0123456789abcdef\n";
        let op = memory_operator();
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            // A policy threshold must not turn repo-group deferral into ack.
            Some(0),
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(!should_ack, "repo-group reconcile must consume the event");
        assert!(!local_path.exists(), "NATS must not hydrate a Git ref");
        assert!(
            state_cache.lock().await.get(&local_path).is_none(),
            "NATS must not advance cache state for a new Git ref"
        );
    }

    #[tokio::test]
    async fn auto_pull_defers_remote_newer_git_internal_path_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"aaaaaaaaaaaaaaaa\n";
        let remote_bytes = b"bbbbbbbbbbbbbbbb\n";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();

        let mut local_vclock = tcfs_sync::conflict::VectorClock::new();
        local_vclock.tick("neo");
        let mut remote_vclock = local_vclock.clone();
        remote_vclock.tick("honey");
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                local_vclock.clone(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));
        let op = memory_operator();
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(!should_ack, "newer Git state remains repo-group work");
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("original Git cache state");
        assert_eq!(state.blake3, bytes_hash(local_bytes));
        assert_eq!(state.vclock, local_vclock);
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(state.conflict.is_none());
    }

    #[tokio::test]
    async fn auto_pull_acknowledges_read_only_up_to_date_git_internal_path() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = dir.path().join("sync").join(rel_path);
        let bytes = b"aaaaaaaaaaaaaaaa\n";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, bytes).unwrap();

        let mut vclock = tcfs_sync::conflict::VectorClock::new();
        vclock.tick("honey");
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(bytes),
                bytes.len() as u64,
                vclock.clone(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));
        let op = memory_operator();
        let remote_hash = seed_current_bytes(&op, rel_path, bytes, vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            bytes.len() as u64,
            &vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(should_ack, "UpToDate is safe read-only acknowledgement");
        assert_eq!(std::fs::read(&local_path).unwrap(), bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("unchanged Git cache state");
        assert_eq!(state.blake3, bytes_hash(bytes));
        assert_eq!(state.vclock, vclock);
    }

    #[tokio::test]
    async fn conflict_resolved_uses_indexed_clock_not_notification_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/resolved.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let state_path = dir.path().join("state.json");
        let bytes = b"same resolved content";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, bytes).unwrap();

        let mut local_vclock = tcfs_sync::conflict::VectorClock::new();
        local_vclock.tick("neo");
        let mut cache = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(bytes),
                bytes.len() as u64,
                local_vclock,
                "neo",
            ),
        );
        cache.flush().unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let mut indexed_vclock = tcfs_sync::conflict::VectorClock::new();
        indexed_vclock.tick("honey");
        indexed_vclock.tick("honey");
        let op = memory_operator();
        seed_current_bytes(&op, rel_path, bytes, indexed_vclock, "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        // The event sender can disagree with the indexed manifest writer, and
        // ConflictResolved's payload clock is not accepted by this interface.
        let should_ack = handle_conflict_resolved(
            "neo",
            "mallory",
            rel_path,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(should_ack, "durable authoritative merge is ack-safe");
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("resolved cache state");
        assert_eq!(state.vclock.get("neo"), 1);
        assert_eq!(state.vclock.get("honey"), 2);
        assert_eq!(state.vclock.get("mallory"), 0);
        drop(cache);
        let persisted = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert_eq!(persisted.get(&local_path).unwrap().vclock.get("honey"), 2);
    }

    #[tokio::test]
    async fn conflict_resolved_flush_failure_rolls_back_and_withholds_ack() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/resolved.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let bytes = b"same resolved content";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, bytes).unwrap();

        let mut cache = state_cache_with_blocked_flush(dir.path());
        let mut local_vclock = tcfs_sync::conflict::VectorClock::new();
        local_vclock.tick("neo");
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(bytes),
                bytes.len() as u64,
                local_vclock.clone(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let mut indexed_vclock = local_vclock.clone();
        indexed_vclock.tick("honey");
        let op = memory_operator();
        seed_current_bytes(&op, rel_path, bytes, indexed_vclock, "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_conflict_resolved(
            "neo",
            "honey",
            rel_path,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(!should_ack, "failed durable merge must be redelivered");
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("rolled-back cache state");
        assert_eq!(state.vclock, local_vclock);
        assert_eq!(state.vclock.get("honey"), 0);
    }

    #[tokio::test]
    async fn auto_pull_persists_git_conflict_and_withholds_ack_for_grouped_reconcile() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"aaaaaaaaaaaaaaaa\n";
        let remote_bytes = b"bbbbbbbbbbbbbbbb\n";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();

        let mut local_vclock = tcfs_sync::conflict::VectorClock::new();
        local_vclock.tick("neo");
        let mut remote_vclock = tcfs_sync::conflict::VectorClock::new();
        remote_vclock.tick("honey");
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                local_vclock,
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));
        let op = memory_operator();
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(!should_ack, "grouped reconcile must consume the conflict");
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("persisted Git conflict");
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Conflict);
        let conflict = state.conflict.as_ref().expect("Git conflict payload");
        assert_eq!(conflict.local_blake3, bytes_hash(local_bytes));
        assert_eq!(conflict.remote_blake3, remote_hash);
        assert!(conflict.remote_manifest_key.is_some());
    }

    #[tokio::test]
    async fn auto_download_failure_withholds_ack_signal() {
        // Once handle_auto_pull owns the ack/no-ack decision, ordinary
        // auto-download verification failures must also return false. Otherwise
        // a missing operator/bad manifest/missing manifest would ack and drop
        // the event before the remote file is represented in state.
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        let state_path = dir.path().join("state.json");
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&state_path).unwrap(),
        ));
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let remote_vclock = tcfs_sync::conflict::VectorClock::new();
        let local_path = sync_root.join("notes/todo.txt");
        let config = test_config();
        let master_key = no_master_key();

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            "notes/todo.txt",
            "remote",
            0,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &config,
            &master_key,
        )
        .await;

        assert!(
            !should_ack,
            "failed auto-download verification must withhold ack"
        );
    }

    #[tokio::test]
    async fn auto_download_flush_failure_rolls_back_in_memory_state() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        let rel_path = "notes/todo.txt";
        let local_path = sync_root.join(rel_path);
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            state_cache_with_blocked_flush(dir.path()),
        ));

        let mut remote_vclock = tcfs_sync::conflict::VectorClock::new();
        remote_vclock.tick("honey");
        let remote_hash = bytes_hash(b"");
        let manifest = test_manifest(rel_path, &remote_hash, 0, remote_vclock.clone());
        let op = memory_operator();
        seed_current_manifest(&op, rel_path, &remote_hash, &manifest).await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));
        let config = test_config();
        let master_key = no_master_key();

        for attempt in 0..2 {
            let should_ack = handle_auto_pull(
                "neo",
                "honey",
                rel_path,
                &remote_hash,
                0,
                &remote_vclock,
                &local_path,
                &operator,
                &state_cache,
                &tcfs_sync::state::PathLocks::new(),
                "data",
                None,
                &config,
                &master_key,
            )
            .await;
            assert!(
                !should_ack,
                "attempt {attempt}: failed flush must keep withholding ack"
            );
            let cache = state_cache.lock().await;
            assert!(
                cache.get(&local_path).is_none(),
                "attempt {attempt}: failed flush must not leave remote state in memory"
            );
        }
    }

    #[tokio::test]
    async fn auto_pull_classifies_the_current_snapshot_not_a_delayed_event() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let local_bytes = b"version-a";
        let current_bytes = b"version-b";
        std::fs::write(&local_path, local_bytes).unwrap();

        let mut local_vclock = tcfs_sync::conflict::VectorClock::new();
        local_vclock.tick("neo");
        let local_hash = bytes_hash(local_bytes);
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                local_hash.clone(),
                local_bytes.len() as u64,
                local_vclock.clone(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let mut current_vclock = local_vclock.clone();
        current_vclock.tick("honey");
        let op = memory_operator();
        let current_hash =
            seed_current_bytes(&op, rel_path, current_bytes, current_vclock, "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "old-publisher",
            rel_path,
            &local_hash,
            local_bytes.len() as u64,
            &local_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(
            should_ack,
            "the authoritative newer snapshot should hydrate"
        );
        assert_eq!(std::fs::read(&local_path).unwrap(), current_bytes);
        let cache = state_cache.lock().await;
        assert_eq!(cache.get(&local_path).unwrap().blake3, current_hash);
    }

    #[tokio::test]
    async fn auto_pull_tie_break_uses_the_current_manifest_writer() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let local_bytes = b"local-wins";
        let remote_bytes = b"remote-loses";
        std::fs::write(&local_path, local_bytes).unwrap();

        let mut local_vclock = tcfs_sync::conflict::VectorClock::new();
        local_vclock.tick("neo");
        let mut remote_vclock = tcfs_sync::conflict::VectorClock::new();
        remote_vclock.tick("zulu");
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                local_vclock,
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));
        let op = memory_operator();
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "zulu").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            // If this event publisher incorrectly controlled the tie-break,
            // `neo > aardvark` would choose KeepRemote and destroy local bytes.
            "aardvark",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(should_ack);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
    }

    #[tokio::test]
    async fn auto_pull_preserves_unsynced_local_bytes_and_records_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let cached_bytes = b"last-synced";
        let edited_bytes = b"unsynced-local-edit";
        let remote_bytes = b"remote-newer";
        std::fs::write(&local_path, edited_bytes).unwrap();

        let mut cached_vclock = tcfs_sync::conflict::VectorClock::new();
        cached_vclock.tick("neo");
        let mut remote_vclock = cached_vclock.clone();
        remote_vclock.tick("honey");
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(cached_bytes),
                cached_bytes.len() as u64,
                cached_vclock,
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));
        let op = memory_operator();
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(!should_ack, "local divergence must remain retryable");
        assert_eq!(std::fs::read(&local_path).unwrap(), edited_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).unwrap();
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Conflict);
        let conflict = state.conflict.as_ref().unwrap();
        assert_eq!(conflict.local_blake3, bytes_hash(edited_bytes));
        assert_eq!(conflict.remote_blake3, remote_hash);
    }

    #[tokio::test]
    async fn auto_pull_preserves_an_existing_untracked_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let local_bytes = b"untracked-local";
        let remote_bytes = b"new-remote";
        std::fs::write(&local_path, local_bytes).unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap(),
        ));
        let mut remote_vclock = tcfs_sync::conflict::VectorClock::new();
        remote_vclock.tick("honey");
        let op = memory_operator();
        let remote_hash =
            seed_current_bytes(&op, rel_path, remote_bytes, remote_vclock.clone(), "honey").await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            &remote_hash,
            remote_bytes.len() as u64,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            None,
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(!should_ack);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        assert_eq!(
            state_cache.lock().await.get(&local_path).unwrap().status,
            tcfs_sync::state::FileSyncStatus::Conflict
        );
    }

    #[tokio::test]
    async fn watcher_only_file_deleted_cannot_erase_live_remote_index_or_local_peer() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"last-synced-local";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();

        let local_vclock = tcfs_sync::conflict::VectorClock::new();
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                local_vclock,
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let mut current_vclock = tcfs_sync::conflict::VectorClock::new();
        current_vclock.tick("honey");
        let op = memory_operator();
        seed_current_bytes(
            &op,
            rel_path,
            b"republished-current",
            current_vclock,
            "honey",
        )
        .await;

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(op.clone()),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::AckIgnored);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let current =
            tcfs_sync::engine::resolve_exact_indexed_manifest_snapshot(&op, rel_path, "data")
                .await
                .unwrap()
                .expect("live remote index remains authoritative");
        assert_eq!(current.content_hash(), bytes_hash(b"republished-current"));
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("cached state is preserved");
        assert_eq!(state.blake3, bytes_hash(local_bytes));
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(state.conflict.is_none());
    }

    #[tokio::test]
    async fn remote_delete_without_tombstone_never_removes_matching_tracked_file() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/list-lag.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let state_path = dir.path().join("state.json");
        let local_bytes = b"last-synced";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();

        let mut cache = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        cache.flush().unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator()),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::Withhold);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("cached state is preserved");
        assert_eq!(state.blake3, bytes_hash(local_bytes));
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(state.conflict.is_none());
        drop(cache);

        let persisted = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert_eq!(
            persisted.get(&local_path).unwrap().blake3,
            bytes_hash(local_bytes)
        );
    }

    #[tokio::test]
    async fn remote_delete_preserves_edit_without_persisting_forged_event_clock() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let state_path = dir.path().join("state.json");
        let cached_bytes = b"last-synced";
        let edited_bytes = b"unsynced-local-edit";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, edited_bytes).unwrap();

        let mut cache = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(cached_bytes),
                cached_bytes.len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let mut forged_clock = tcfs_sync::conflict::VectorClock::new();
        for _ in 0..100 {
            forged_clock.tick("mallory");
        }
        let notification = tcfs_sync::StateEvent::FileDeleted {
            device_id: "mallory".into(),
            rel_path: rel_path.into(),
            vclock: forged_clock,
            timestamp: tcfs_sync::StateEvent::now(),
        };
        let notification_path = match &notification {
            tcfs_sync::StateEvent::FileDeleted { rel_path, .. } => rel_path.as_str(),
            _ => unreachable!(),
        };

        let outcome = remote_delete_outcome(
            notification_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::Withhold);
        assert_eq!(std::fs::read(&local_path).unwrap(), edited_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("conflict state");
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Conflict);
        let conflict = state.conflict.as_ref().expect("delete conflict payload");
        assert_eq!(conflict.local_blake3, bytes_hash(edited_bytes));
        assert_eq!(conflict.remote_blake3, "<deleted>");
        assert!(
            conflict.remote_vclock.clocks.is_empty(),
            "an unbound FileDeleted payload clock must never become durable authority"
        );
        drop(cache);

        let persisted = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert_eq!(
            persisted.get(&local_path).unwrap().status,
            tcfs_sync::state::FileSyncStatus::Conflict
        );
    }

    #[tokio::test]
    async fn remote_delete_preserves_untracked_local_file_as_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/untracked.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"local-only";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap(),
        ));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::Withhold);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("synthetic conflict state");
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Conflict);
        assert_eq!(
            state.conflict.as_ref().unwrap().local_blake3,
            bytes_hash(local_bytes)
        );
    }

    #[tokio::test]
    async fn remote_delete_treats_missing_tracked_file_as_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/missing.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(b"last-synced"),
                b"last-synced".len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::Withhold);
        assert!(!local_path.exists());
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("missing-file conflict state");
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Conflict);
        assert_eq!(state.conflict.as_ref().unwrap().local_blake3, "<absent>");
    }

    #[tokio::test]
    async fn remote_delete_commits_matching_tracked_file_and_cache_together() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let state_path = dir.path().join("state.json");
        let local_bytes = b"last-synced";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();
        let mut cache = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::AckDeleted);
        assert!(!local_path.exists());
        assert!(state_cache.lock().await.get(&local_path).is_none());
        let persisted = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert!(persisted.get(&local_path).is_none());
        assert!(
            std::fs::read_dir(local_path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tcfs-delete-")),
            "successful delete must not leak a staging file"
        );
    }

    #[tokio::test]
    async fn remote_delete_acks_when_local_and_cache_are_already_absent() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/already-gone.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap(),
        ));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::AckDeleted);
        assert!(!local_path.exists());
        assert!(state_cache.lock().await.get(&local_path).is_none());
    }

    #[tokio::test]
    async fn remote_delete_flush_failure_restores_file_and_in_memory_cache() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"last-synced";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();

        let mut cache = state_cache_with_blocked_flush(dir.path());
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::Withhold);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("rolled-back cache state");
        assert_eq!(state.blake3, bytes_hash(local_bytes));
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(state.conflict.is_none());
        assert!(
            std::fs::read_dir(local_path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tcfs-delete-")),
            "rollback must restore rather than leak the staging file"
        );
    }

    #[tokio::test]
    async fn remote_delete_never_policy_acknowledges_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "private/todo.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"policy-protected";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            None,
            &state_cache,
            tcfs_sync::policy::SyncMode::Never,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::AckIgnored);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("policy-protected cache");
        assert_eq!(state.blake3, bytes_hash(local_bytes));
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(state.conflict.is_none());
    }

    #[tokio::test]
    async fn remote_delete_defers_git_internal_path_without_per_file_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = dir.path().join("sync").join(rel_path);
        let local_bytes = b"0123456789abcdef\n";
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        std::fs::write(&local_path, local_bytes).unwrap();
        let mut cache = tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap();
        cache.set(
            &local_path,
            synced_state(
                rel_path,
                bytes_hash(local_bytes),
                local_bytes.len() as u64,
                tcfs_sync::conflict::VectorClock::new(),
                "neo",
            ),
        );
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(cache));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::Withhold);
        assert_eq!(std::fs::read(&local_path).unwrap(), local_bytes);
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("git cache is preserved");
        assert_eq!(state.blake3, bytes_hash(local_bytes));
        assert_eq!(state.status, tcfs_sync::state::FileSyncStatus::Synced);
        assert!(state.conflict.is_none());
    }

    #[tokio::test]
    async fn remote_delete_acks_git_internal_path_after_grouped_reconcile_converges() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "repo/.git/refs/heads/main";
        let local_path = dir.path().join("sync").join(rel_path);
        std::fs::create_dir_all(local_path.parent().unwrap()).unwrap();
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap(),
        ));

        let outcome = remote_delete_outcome(
            rel_path,
            &local_path,
            Some(memory_operator_with_tombstone(rel_path).await),
            &state_cache,
            tcfs_sync::policy::SyncMode::Always,
        )
        .await;

        assert_eq!(outcome, RemoteDeleteOutcome::AckIgnored);
        assert!(!local_path.exists());
        assert!(state_cache.lock().await.get(&local_path).is_none());
    }

    #[tokio::test]
    async fn on_demand_threshold_uses_the_current_manifest_size() {
        let dir = tempfile::tempdir().unwrap();
        let rel_path = "notes/large.txt";
        let local_path = dir.path().join("sync").join(rel_path);
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&dir.path().join("state.json")).unwrap(),
        ));
        let mut current_vclock = tcfs_sync::conflict::VectorClock::new();
        current_vclock.tick("honey");
        let op = memory_operator();
        let manifest = test_manifest(rel_path, "large-current", 64, current_vclock.clone());
        seed_current_manifest(&op, rel_path, "large-current", &manifest).await;
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            "forged-small-event",
            0,
            &current_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            Some(1),
            &test_config(),
            &no_master_key(),
        )
        .await;

        assert!(should_ack, "policy skips are safe to acknowledge");
        assert!(
            !local_path.exists(),
            "the authoritative large object must not hydrate"
        );
        assert!(state_cache.lock().await.get(&local_path).is_none());
    }

    #[tokio::test]
    async fn auto_download_uses_current_index_when_event_metadata_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        let rel_path = "notes/todo.txt";
        let local_path = sync_root.join(rel_path);
        let state_path = dir.path().join("state.json");
        let state_cache = std::sync::Arc::new(tokio::sync::Mutex::new(
            tcfs_sync::state::StateCache::open(&state_path).unwrap(),
        ));

        let mut remote_vclock = tcfs_sync::conflict::VectorClock::new();
        remote_vclock.tick("honey");
        let current_hash = bytes_hash(b"");
        let manifest = test_manifest(rel_path, &current_hash, 0, remote_vclock.clone());
        let op = memory_operator();
        seed_current_manifest(&op, rel_path, &current_hash, &manifest).await;
        let stale_manifest = test_manifest(rel_path, "stale-event", 0, remote_vclock.clone());
        op.write(
            "data/manifests/stale-event",
            stale_manifest.to_bytes().unwrap(),
        )
        .await
        .unwrap();
        let operator = std::sync::Arc::new(tokio::sync::Mutex::new(Some(op)));
        let config = test_config();
        let master_key = no_master_key();

        let should_ack = handle_auto_pull(
            "neo",
            "honey",
            rel_path,
            "stale-event",
            999,
            &remote_vclock,
            &local_path,
            &operator,
            &state_cache,
            &tcfs_sync::state::PathLocks::new(),
            "data",
            Some(0),
            &config,
            &master_key,
        )
        .await;

        assert!(
            should_ack,
            "current indexed object should hydrate successfully"
        );
        assert_eq!(std::fs::read(&local_path).unwrap(), b"");
        let cache = state_cache.lock().await;
        let state = cache.get(&local_path).expect("hydrated state");
        assert_eq!(state.blake3, current_hash);
        assert_eq!(state.remote_path, format!("data/manifests/{current_hash}"));
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", target_os = "ios")))]
mod state_migration_tests {
    use super::{absorb_legacy_state_db, open_daemon_state_cache};

    /// Seed `path` (opened as a JSON cache regardless of extension) with the
    /// given `(cache-key, remote_path)` entries and flush.
    fn seed_state(path: &std::path::Path, entries: &[(&str, &str)]) {
        let mut cache = tcfs_sync::state::StateCache::open(path).unwrap();
        for (key, remote_path) in entries {
            cache.set(
                std::path::Path::new(key),
                tcfs_sync::state::SyncState {
                    blake3: "hash".into(),
                    size: 0,
                    mtime: 0,
                    chunk_count: 0,
                    remote_path: (*remote_path).into(),
                    last_synced: 0,
                    vclock: tcfs_sync::conflict::VectorClock::new(),
                    device_id: "neo".into(),
                    conflict: None,
                    status: tcfs_sync::state::FileSyncStatus::Synced,
                },
            );
        }
        cache.flush().unwrap();
    }

    #[test]
    fn absorb_seeds_json_from_db_only_host() {
        // `.db`-only, no `.json`: the one-time absorb seeds `state.json` from the
        // legacy `.db` and the daemon serves the migrated entry.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");
        seed_state(&db, &[("/sync/a.txt", "data/index/a.txt")]);
        assert!(!json.exists(), "precondition: no .json yet");

        let resolved = absorb_legacy_state_db(&db).unwrap();
        assert_eq!(resolved, json, "resolves to the .json sibling");
        assert!(json.exists(), ".json must be seeded from .db");
        assert!(!db.exists(), "validated legacy authority is renamed once");

        let cache = tcfs_sync::state::StateCache::open(&resolved).unwrap();
        assert!(
            cache.get(std::path::Path::new("/sync/a.txt")).is_some(),
            "migrated entry must be visible after absorb"
        );
    }

    #[test]
    fn absorb_leaves_existing_json_untouched_when_both_exist() {
        // Both exist: `.json` (2 entries) wins untouched; the *different* `.db`
        // (1 entry) is never copied in — no size heuristic, no merge.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");
        seed_state(&db, &[("/sync/from-db.txt", "data/index/from-db.txt")]);
        seed_state(
            &json,
            &[
                ("/sync/keep-1.txt", "data/index/keep-1.txt"),
                ("/sync/keep-2.txt", "data/index/keep-2.txt"),
            ],
        );

        let resolved = absorb_legacy_state_db(&db).unwrap();
        assert_eq!(resolved, json);

        let cache = tcfs_sync::state::StateCache::open(&resolved).unwrap();
        assert!(
            cache
                .get(std::path::Path::new("/sync/keep-1.txt"))
                .is_some(),
            "existing .json entry 1 must survive"
        );
        assert!(
            cache
                .get(std::path::Path::new("/sync/keep-2.txt"))
                .is_some(),
            "existing .json entry 2 must survive"
        );
        assert!(
            cache
                .get(std::path::Path::new("/sync/from-db.txt"))
                .is_none(),
            ".json must win untouched; .db content must not leak in"
        );
    }

    #[test]
    fn absorb_neither_present_starts_fresh() {
        // Neither file exists: no crash, returns the `.json` path, nothing seeded.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");

        let resolved = absorb_legacy_state_db(&db).unwrap();
        assert_eq!(resolved, json);
        assert!(!json.exists(), "no source → nothing seeded");

        // Opening the still-absent `.json` starts fresh (empty cache, no crash).
        let cache = tcfs_sync::state::StateCache::open(&resolved).unwrap();
        assert!(cache.get(std::path::Path::new("/sync/nope.txt")).is_none());
    }

    #[test]
    fn absorb_ignores_unrelated_stale_legacy_temp() {
        // The migration no longer uses a predictable copy temp. A stale file
        // from an older daemon cannot poison or become authority for the direct,
        // validated no-replace rename.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");
        let tmp = dir.path().join("state.json.tmp");
        seed_state(&db, &[("/sync/a.txt", "data/index/a.txt")]);
        std::fs::write(&tmp, b"{\"entries\": {\"trunc").unwrap(); // simulated ENOSPC remnant

        let resolved = absorb_legacy_state_db(&db).unwrap();

        assert_eq!(resolved, json);
        assert!(json.exists(), "stale legacy temp must not block migration");
        assert!(
            tmp.exists(),
            "unrelated stale temp is never opened or trusted"
        );
        let cache = tcfs_sync::state::StateCache::open(&resolved).unwrap();
        assert!(
            cache.get(std::path::Path::new("/sync/a.txt")).is_some(),
            "canonical .json carries the validated .db content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn absorb_corrupt_db_source_fails_startup_and_preserves_authority() {
        use std::os::unix::fs::PermissionsExt;

        // A corrupt existing legacy authority aborts startup. It is not copied,
        // renamed, deleted, or silently replaced with an empty canonical cache.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");
        let garbage: &[u8] = b"{\"last_nats_seq\": 7, \"entries\": {\"/sync/a.t"; // truncated JSON
        std::fs::write(&db, garbage).unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = absorb_legacy_state_db(&db).unwrap_err();

        assert!(
            format!("{error:#}").contains("validating legacy daemon state"),
            "{error:#}"
        );
        assert!(
            !json.exists(),
            "corrupt source must not produce a canonical .json"
        );
        assert_eq!(
            std::fs::read(&db).unwrap(),
            garbage,
            "source .db must be left untouched for manual recovery"
        );
    }

    #[test]
    fn absorb_repairs_valid_backup_before_promoting_legacy_authority() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");
        seed_state(&db, &[("/sync/recovered.txt", "data/index/recovered.txt")]);

        // A second durable generation creates the secure `.json.bak` recovery
        // copy used by StateCache even while the legacy primary is named `.db`.
        let mut cache = tcfs_sync::state::StateCache::open(&db).unwrap();
        cache.set_last_nats_seq(9);
        cache.flush().unwrap();
        drop(cache);
        std::fs::write(&db, b"{corrupt legacy primary").unwrap();

        let resolved = absorb_legacy_state_db(&db).unwrap();
        assert_eq!(resolved, json);
        assert!(!db.exists());
        let repaired = open_daemon_state_cache(&json).unwrap();
        assert!(
            repaired
                .get(std::path::Path::new("/sync/recovered.txt"))
                .is_some(),
            "validated backup content must be repaired before canonical promotion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn absorb_treats_dangling_canonical_symlink_as_present_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let json = dir.path().join("state.json");
        seed_state(&db, &[("/sync/a.txt", "data/index/a.txt")]);
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &json).unwrap();

        let resolved = absorb_legacy_state_db(&db).unwrap();
        assert_eq!(resolved, json);
        assert!(db.exists(), "legacy authority must not be consumed");
        assert!(
            open_daemon_state_cache(&resolved).is_err(),
            "dangling canonical entry must reach the secure open and fail"
        );
    }

    #[test]
    fn absorb_expands_tilde_in_configured_state_db() {
        // Adversarial gate Fix B: SyncConfig::default() carries a literal
        // `~/.local/share/tcfsd/state.db` and the config loader does zero
        // normalization; absorb must target the $HOME-expanded path, never a
        // CWD-relative `./~/…`. Guarded with a temp HOME (set + restored).
        let home_dir = tempfile::tempdir().unwrap();
        let saved_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home_dir.path());

        let expanded_db = home_dir.path().join("tcfsd-tin2657/state.db");
        std::fs::create_dir_all(expanded_db.parent().unwrap()).unwrap();
        seed_state(&expanded_db, &[("/sync/a.txt", "data/index/a.txt")]);

        let resolved = absorb_legacy_state_db(std::path::Path::new("~/tcfsd-tin2657/state.db"));

        // Restore HOME before asserting so a panic can't leak the temp value.
        match saved_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let resolved = resolved.unwrap();

        let expected_json = home_dir.path().join("tcfsd-tin2657/state.json");
        assert_eq!(
            resolved, expected_json,
            "absorb must resolve under $HOME, not CWD-relative ./~/…"
        );
        assert!(
            expected_json.exists(),
            "migration must land at the expanded path"
        );
        assert!(
            !std::path::Path::new("~/tcfsd-tin2657").exists(),
            "no literal ./~/tcfsd-tin2657 path may be created"
        );
    }
}
