//! Configuration loading for the tcfs daemon binary.

use anyhow::Result;
use std::path::Path;
use tcfs_core::config::TcfsConfig;

/// Load the daemon configuration from disk.
///
/// `tcfsd` intentionally refuses to start from implicit defaults when the
/// requested config file is absent. Packaged installs should guide users through
/// `tcfs init` and then start the daemon against the generated config.
pub async fn load_config(path: &Path) -> Result<TcfsConfig> {
    if !path.exists() {
        anyhow::bail!(
            "tcfsd config not found: {}. Run 'tcfs init --config-out {}' or pass --config <path>.",
            path.display(),
            path.display()
        );
    }

    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
    toml::from_str(&content).map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))
}
