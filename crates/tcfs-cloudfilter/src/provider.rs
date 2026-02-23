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
    /// # Implementation
    ///
    /// On Windows, calls:
    /// - `CfRegisterSyncRoot()` with provider name and hydration/population policies
    /// - `CfConnectSyncRoot()` with callback table for FETCH_DATA, CANCEL_FETCH_DATA, FETCH_PLACEHOLDERS
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

        // TODO: Full CfRegisterSyncRoot implementation
        //
        // use windows::Win32::Storage::CloudFilters::*;
        // use windows::core::HSTRING;
        //
        // let display_name = HSTRING::from(&config.provider_name);
        // let root_path = HSTRING::from(config.root_path.to_string_lossy().as_ref());
        //
        // let policies = CF_SYNC_POLICIES {
        //     Hydration: CF_HYDRATION_POLICY_FULL,
        //     Population: CF_POPULATION_POLICY_FULL,
        //     ..Default::default()
        // };
        //
        // let registration = CF_SYNC_REGISTRATION {
        //     ProviderName: display_name.as_ptr(),
        //     ProviderVersion: HSTRING::from("0.6.0").as_ptr(),
        //     SyncRootIdentity: provider_id.as_ptr() as _,
        //     SyncRootIdentityLength: provider_id.len() as u32,
        //     Policies: policies,
        //     ..Default::default()
        // };
        //
        // unsafe { CfRegisterSyncRoot(&root_path, &registration, &policies, CF_REGISTER_FLAG_NONE)? };

        warn!("Cloud Files sync root registration not yet fully implemented");

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
///
/// Calls `CfUnregisterSyncRoot()` on Windows.
pub async fn unregister(root_path: &Path) -> Result<()> {
    info!(root = %root_path.display(), "unregistering Cloud Files sync root");
    // TODO: CfUnregisterSyncRoot(root_path)
    //
    // use windows::Win32::Storage::CloudFilters::CfUnregisterSyncRoot;
    // use windows::core::HSTRING;
    // let path = HSTRING::from(root_path.to_string_lossy().as_ref());
    // unsafe { CfUnregisterSyncRoot(&path)? };
    Ok(())
}

/// Check if a path is registered as a Cloud Files sync root.
pub fn is_sync_root(root_path: &Path) -> bool {
    // TODO: CfGetSyncRootInfoByPath()
    let _ = root_path;
    false
}
