//! OpenDAL Operator factory for tcfs storage backends

use anyhow::{Context, Result};
use opendal::Operator;

/// Minimal config needed to build an operator
/// (full config lives in tcfs-core's StorageConfig)
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// Build an OpenDAL Operator for SeaweedFS S3 (or any S3-compatible endpoint)
///
/// Uses path-style addressing (default in opendal 0.55), which is required by
/// SeaweedFS and MinIO. Do NOT call enable_virtual_host_style() for these.
pub fn build_operator(cfg: &StorageConfig) -> Result<Operator> {
    // opendal 0.55: S3 builder uses consuming pattern (methods take `self`, return `Self`)
    let builder = opendal::services::S3::default()
        .endpoint(&cfg.endpoint)
        .region(&cfg.region)
        .bucket(&cfg.bucket)
        .access_key_id(&cfg.access_key_id)
        .secret_access_key(&cfg.secret_access_key);
    // Note: path-style addressing is the default — no enable_virtual_host_style() needed

    let op = Operator::new(builder)
        .context("creating OpenDAL S3 operator")?
        .layer(opendal::layers::LoggingLayer::default())
        .layer(
            opendal::layers::RetryLayer::new()
                .with_max_times(5)
                .with_jitter(),
        )
        .finish();

    Ok(op)
}

/// Build an operator from tcfs-core config + loaded credentials.
///
/// If `enforce_tls` is true and the endpoint uses HTTP, this returns an error.
/// Otherwise, a warning is logged for non-HTTPS endpoints.
pub fn build_from_core_config(
    storage: &tcfs_core::config::StorageConfig,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<Operator> {
    if storage.endpoint.starts_with("http://") {
        if storage.enforce_tls {
            anyhow::bail!(
                "S3 endpoint uses plaintext HTTP ({}), but enforce_tls is enabled. \
                 Use an HTTPS endpoint or set storage.enforce_tls = false for local development.",
                storage.endpoint
            );
        }
        tracing::warn!(
            endpoint = %storage.endpoint,
            "S3 endpoint uses plaintext HTTP — credentials are transmitted unencrypted. \
             Set storage.enforce_tls = true and use HTTPS in production."
        );
    }

    build_operator(&StorageConfig {
        endpoint: storage.endpoint.clone(),
        region: storage.region.clone(),
        bucket: storage.bucket.clone(),
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_operator_valid() {
        let cfg = StorageConfig {
            endpoint: "http://localhost:8333".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
        };
        let op = build_operator(&cfg);
        assert!(op.is_ok(), "operator construction should succeed");
    }

    #[test]
    fn test_build_from_core_config_http_warning() {
        // HTTP endpoint with enforce_tls=false should succeed (but log warning)
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "http://localhost:8333".into(),
            enforce_tls: false,
            ..Default::default()
        };
        let result = build_from_core_config(&storage, "key", "secret");
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_from_core_config_http_enforce_tls() {
        // HTTP endpoint with enforce_tls=true should fail
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "http://insecure:8333".into(),
            enforce_tls: true,
            ..Default::default()
        };
        let result = build_from_core_config(&storage, "key", "secret");
        assert!(result.is_err(), "HTTP + enforce_tls must fail");
        assert!(
            result.unwrap_err().to_string().contains("enforce_tls"),
            "error message should mention enforce_tls"
        );
    }

    #[test]
    fn test_build_from_core_config_https() {
        // HTTPS endpoint with enforce_tls=true should succeed
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "https://s3.example.com:8333".into(),
            enforce_tls: true,
            ..Default::default()
        };
        let result = build_from_core_config(&storage, "key", "secret");
        assert!(result.is_ok());
    }
}
