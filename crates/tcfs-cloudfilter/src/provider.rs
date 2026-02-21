//! Sync root provider: registers/unregisters the tcfs Cloud Files sync root.
//!
//! Uses the Windows Cloud Filter API:
//! - CfRegisterSyncRoot() — register the provider with a local directory
//! - CfConnectSyncRoot() — connect and start receiving callbacks
//! - CfDisconnectSyncRoot() — stop callbacks
//! - CfUnregisterSyncRoot() — fully remove the sync root
//!
//! The sync root appears in Explorer's navigation pane alongside OneDrive.

#![cfg(target_os = "windows")]

use anyhow::{Context, Result};
use std::path::Path;
use tracing::{info, warn};

use crate::{HydrationPolicy, PopulationPolicy, SyncRootConfig};

/// Active connection to a registered sync root.
///
/// Dropping this struct disconnects from the sync root but does not
/// unregister it (files remain as placeholders).
pub struct SyncRootConnection {
    _config: SyncRootConfig,
    // Windows handles would go here:
    // connection_key: CF_CONNECTION_KEY,
}

impl SyncRootConnection {
    /// Register and connect a new sync root.
    ///
    /// This:
    /// 1. Creates the local root directory if it doesn't exist
    /// 2. Registers the sync root with CFAPI (appears in Explorer)
    /// 3. Connects and starts handling hydration callbacks
    ///
    /// # Errors
    /// Returns error if:
    /// - Running on Windows < 10 1809
    /// - The directory is already registered by another provider
    /// - Insufficient permissions
    pub async fn connect(config: SyncRootConfig) -> Result<Self> {
        info!(
            root = %config.root_path.display(),
            provider = %config.provider_name,
            "registering Cloud Files sync root"
        );

        // Ensure root directory exists
        tokio::fs::create_dir_all(&config.root_path)
            .await
            .with_context(|| format!("creating sync root: {}", config.root_path.display()))?;

        // TODO: Phase 6c implementation
        // 1. Build CF_SYNC_REGISTRATION struct
        // 2. Call CfRegisterSyncRoot()
        // 3. Set up callback table (CF_CALLBACK_REGISTRATION array)
        //    - FETCH_DATA → hydration::handle_fetch_data
        //    - CANCEL_FETCH_DATA → hydration::handle_cancel_fetch
        //    - FETCH_PLACEHOLDERS → placeholder::handle_fetch_placeholders
        // 4. Call CfConnectSyncRoot()
        // 5. Populate initial placeholders via placeholder::populate_root()

        warn!("Cloud Files sync root registration not yet implemented");

        Ok(SyncRootConnection { _config: config })
    }

    /// Disconnect from the sync root (stop handling callbacks).
    ///
    /// Files remain as placeholders on disk. Call `unregister()` to
    /// fully remove the sync root.
    pub async fn disconnect(self) -> Result<()> {
        info!("disconnecting Cloud Files sync root");
        // TODO: CfDisconnectSyncRoot(self.connection_key)
        Ok(())
    }
}

/// Unregister a sync root completely.
///
/// Removes the provider registration. Existing placeholder files become
/// regular empty files (their cloud status is lost).
pub async fn unregister(root_path: &Path) -> Result<()> {
    info!(root = %root_path.display(), "unregistering Cloud Files sync root");
    // TODO: CfUnregisterSyncRoot(root_path)
    Ok(())
}

/// Check if a path is registered as a Cloud Files sync root.
pub fn is_sync_root(root_path: &Path) -> bool {
    // TODO: CfGetSyncRootInfoByPath()
    let _ = root_path;
    false
}
