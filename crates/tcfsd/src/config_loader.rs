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
    toml::from_str(&content).map_err(|_| {
        anyhow::anyhow!(
            "parsing config {} failed; check TOML syntax and field types",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parse_error_never_echoes_offending_source_line() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let malformed = r#"
[sync]
nats_token = ["TIN2860-malformed-left", "malformed-middle", "malformed-right"]
"#;
        std::fs::write(&config_path, malformed).unwrap();

        let error = load_config(&config_path)
            .await
            .expect_err("malformed token type must fail daemon config loading");
        let rendered = format!("{error:#}");

        for forbidden in [
            "TIN2860-malformed-left",
            "malformed-middle",
            "malformed-right",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "daemon parse error leaked config source material: {rendered}"
            );
        }
        assert!(rendered.contains(&config_path.display().to_string()));
        assert!(rendered.contains("check TOML syntax and field types"));
    }
}
