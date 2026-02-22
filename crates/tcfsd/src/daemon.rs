//! Daemon lifecycle: startup, health checks, systemd notify, gRPC server

use anyhow::Result;
use secrecy::ExposeSecret;
use std::sync::Arc;
use tcfs_core::config::TcfsConfig;
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

    // Start Prometheus metrics endpoint
    let metrics_addr = config.daemon.metrics_addr.clone();
    if let Some(addr) = metrics_addr {
        let registry = Arc::new(crate::metrics::Registry::default());
        let registry_clone = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::metrics::serve(addr, registry_clone).await {
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
        operator,
    );

    info!(socket = %socket_path.display(), "gRPC: listening");

    crate::grpc::serve(&socket_path, impl_).await?;

    Ok(())
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
