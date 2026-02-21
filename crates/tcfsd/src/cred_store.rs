//! Credential store: shared state + file-watching reload
//!
//! Watches the SOPS credential file for changes (using the `notify` crate)
//! and automatically reloads credentials when the file is modified.
//! This enables zero-downtime credential rotation: an external process
//! (or `tcfs rotate-credentials`) updates the SOPS file, and tcfsd
//! picks up the new credentials within seconds.

use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared reference to a credential store instance
pub type SharedCredStore = Arc<RwLock<Option<tcfs_secrets::CredStore>>>;

/// Create a new empty shared credential store
pub fn new_shared() -> SharedCredStore {
    Arc::new(RwLock::new(None))
}

/// Start watching a SOPS credential file for changes.
///
/// When the file is modified (or created), re-decrypts it and updates
/// the shared credential store. The watcher runs in a background tokio
/// task and continues until the returned `CredentialWatcher` is dropped.
///
/// # Arguments
/// * `cred_file` — Path to the SOPS-encrypted YAML credential file
/// * `secrets_config` — Secrets configuration (age identity, etc.)
/// * `storage_config` — Storage configuration (for credential parsing)
/// * `store` — Shared credential store to update on reload
pub fn watch_credentials(
    cred_file: PathBuf,
    secrets_config: tcfs_core::config::SecretsConfig,
    storage_config: tcfs_core::config::StorageConfig,
    store: SharedCredStore,
) -> Result<CredentialWatcher> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);

    // Set up the file watcher
    let tx_clone = tx.clone();
    let watch_path = cred_file.clone();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(event) => {
                    match event.kind {
                        EventKind::Modify(_) | EventKind::Create(_) => {
                            tracing::debug!("credential file changed: {:?}", event.kind);
                            // Non-blocking send — if the channel is full, the reload
                            // is already queued and we can skip this notification
                            let _ = tx_clone.try_send(());
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::warn!("credential file watch error: {e}");
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("creating file watcher: {e}"))?;

    // Watch the parent directory (file watchers don't survive renames/recreations)
    let watch_dir = watch_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();

    watcher
        .watch(&watch_dir, RecursiveMode::NonRecursive)
        .map_err(|e| anyhow::anyhow!("watching {}: {e}", watch_dir.display()))?;

    tracing::info!(
        "watching credential file for changes: {}",
        cred_file.display()
    );

    // Spawn the reload task
    let cred_file_clone = cred_file.clone();
    let task = tokio::spawn(async move {
        // Debounce: wait a short time after a change notification before reloading
        // to coalesce rapid successive changes (e.g., atomic_replace tmp + rename)
        while rx.recv().await.is_some() {
            // Drain any queued notifications (debounce)
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            while rx.try_recv().is_ok() {}

            tracing::info!("reloading credentials from {}", cred_file_clone.display());

            match tcfs_secrets::CredStore::load(&secrets_config, &storage_config).await {
                Ok(cs) => {
                    let source = cs.source.clone();
                    store.write().await.replace(cs);
                    tracing::info!(source = %source, "credentials reloaded successfully");
                }
                Err(e) => {
                    tracing::error!("credential reload failed: {e}");
                    tracing::warn!("keeping previous credentials — fix the file and save again");
                }
            }
        }
    });

    Ok(CredentialWatcher {
        _watcher: watcher,
        _task: task,
        path: cred_file,
    })
}

/// Handle to a running credential file watcher.
///
/// The watcher stops when this handle is dropped.
pub struct CredentialWatcher {
    /// Keep the watcher alive — dropped when this struct is dropped
    _watcher: RecommendedWatcher,
    /// Background reload task
    _task: tokio::task::JoinHandle<()>,
    /// Path being watched
    path: PathBuf,
}

impl CredentialWatcher {
    /// Get the path being watched
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}
