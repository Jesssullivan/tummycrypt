//! OpenDAL Operator factory for tcfs storage backends

use anyhow::{Context, Result};
use opendal::raw::HttpClient;
use opendal::Operator;
use std::path::PathBuf;
use std::time::Duration;

/// Minimal config needed to build an operator
/// (full config lives in tcfs-core's StorageConfig)
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub s3_connect_timeout_secs: u64,
    pub s3_pool_idle_timeout_secs: u64,
    pub s3_pool_max_idle_per_host: usize,
    pub s3_http1_only: bool,
    pub ca_cert_path: Option<PathBuf>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8333".to_string(),
            region: "us-east-1".to_string(),
            bucket: "tcfs".to_string(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            s3_connect_timeout_secs: 0,
            s3_pool_idle_timeout_secs: 0,
            s3_pool_max_idle_per_host: 0,
            s3_http1_only: false,
            ca_cert_path: None,
        }
    }
}

/// Build an OpenDAL Operator for SeaweedFS S3 (or any S3-compatible endpoint)
///
/// Uses path-style addressing (default in opendal 0.55), which is required by
/// SeaweedFS and MinIO. Do NOT call enable_virtual_host_style() for these.
pub fn build_operator(cfg: &StorageConfig) -> Result<Operator> {
    build_operator_with_limits(cfg, 0)
}

/// Build an operator with optional concurrent-operation limiting.
///
/// If `max_concurrent > 0`, applies `ConcurrentLimitLayer` to cap inflight S3 ops.
pub fn build_operator_with_limits(cfg: &StorageConfig, max_concurrent: usize) -> Result<Operator> {
    // opendal 0.55: S3 builder uses consuming pattern (methods take `self`, return `Self`).
    let mut builder = opendal::services::S3::default()
        .endpoint(&cfg.endpoint)
        .region(&cfg.region)
        .bucket(&cfg.bucket)
        .access_key_id(&cfg.access_key_id)
        .secret_access_key(&cfg.secret_access_key);
    // Note: path-style addressing is the default — no enable_virtual_host_style() needed.
    if let Some(http_client) = build_s3_http_client(cfg)? {
        #[allow(deprecated)]
        {
            builder = builder.http_client(http_client);
        }
    }

    let operator_builder = Operator::new(builder)
        .context("creating OpenDAL S3 operator")?
        .layer(opendal::layers::LoggingLayer::default())
        .layer(
            opendal::layers::RetryLayer::new()
                .with_max_times(5)
                .with_factor(2.0)
                .with_jitter(),
        );

    let op = if max_concurrent > 0 {
        tracing::info!(
            max_concurrent,
            "S3 concurrent operation and HTTP request limits enabled"
        );
        operator_builder
            .layer(
                opendal::layers::ConcurrentLimitLayer::new(max_concurrent)
                    .with_http_concurrent_limit(max_concurrent),
            )
            .finish()
    } else {
        operator_builder.finish()
    };

    Ok(op)
}

fn build_s3_http_client(cfg: &StorageConfig) -> Result<Option<HttpClient>> {
    let custom_client_requested = cfg.s3_connect_timeout_secs > 0
        || cfg.s3_pool_idle_timeout_secs > 0
        || cfg.s3_pool_max_idle_per_host > 0
        || cfg.s3_http1_only
        || cfg.ca_cert_path.is_some();

    if !custom_client_requested {
        return Ok(None);
    }

    let mut builder = reqwest::Client::builder();
    if let Some(path) = &cfg.ca_cert_path {
        let pem = std::fs::read(path)
            .with_context(|| format!("reading S3 CA certificate {}", path.display()))?;
        let cert = reqwest::Certificate::from_pem(&pem)
            .with_context(|| format!("parsing S3 CA certificate {}", path.display()))?;
        builder = builder.add_root_certificate(cert);
    }
    if cfg.s3_connect_timeout_secs > 0 {
        builder = builder.connect_timeout(Duration::from_secs(cfg.s3_connect_timeout_secs));
    }
    if cfg.s3_pool_idle_timeout_secs > 0 {
        builder = builder.pool_idle_timeout(Duration::from_secs(cfg.s3_pool_idle_timeout_secs));
    }
    if cfg.s3_pool_max_idle_per_host > 0 {
        builder = builder.pool_max_idle_per_host(cfg.s3_pool_max_idle_per_host);
    }
    if cfg.s3_http1_only {
        builder = builder.http1_only();
    }

    let client = builder.build().context("building bounded S3 HTTP client")?;
    tracing::info!(
        s3_connect_timeout_secs = cfg.s3_connect_timeout_secs,
        s3_pool_idle_timeout_secs = cfg.s3_pool_idle_timeout_secs,
        s3_pool_max_idle_per_host = cfg.s3_pool_max_idle_per_host,
        s3_http1_only = cfg.s3_http1_only,
        ca_cert_path = cfg
            .ca_cert_path
            .as_ref()
            .map(|path| path.display().to_string()),
        "S3 HTTP client controls enabled"
    );
    Ok(Some(HttpClient::with(client)))
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

    build_operator_with_limits(
        &StorageConfig {
            endpoint: storage.endpoint.clone(),
            region: storage.region.clone(),
            bucket: storage.bucket.clone(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
            s3_connect_timeout_secs: storage.s3_connect_timeout_secs,
            s3_pool_idle_timeout_secs: storage.s3_pool_idle_timeout_secs,
            s3_pool_max_idle_per_host: storage.s3_pool_max_idle_per_host,
            s3_http1_only: storage.s3_http1_only,
            ca_cert_path: storage.ca_cert_path.clone(),
        },
        storage.max_concurrent_ops,
    )
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
            ..Default::default()
        };
        let op = build_operator(&cfg);
        assert!(op.is_ok(), "operator construction should succeed");
    }

    #[test]
    fn test_build_operator_with_s3_http_controls() {
        let cfg = StorageConfig {
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            s3_connect_timeout_secs: 5,
            s3_pool_idle_timeout_secs: 15,
            s3_pool_max_idle_per_host: 4,
            s3_http1_only: true,
            ca_cert_path: None,
        };

        assert!(
            build_s3_http_client(&cfg).unwrap().is_some(),
            "nonzero S3 HTTP controls should build a custom client"
        );
        assert!(
            build_operator_with_limits(&cfg, 4).is_ok(),
            "operator construction should succeed with S3 HTTP controls and concurrency limits"
        );
    }

    #[test]
    fn test_build_s3_http_client_reads_configured_ca_cert_path() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("missing-ca.pem");

        let cfg = StorageConfig {
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            ca_cert_path: Some(ca_path.clone()),
            ..Default::default()
        };

        let err = build_s3_http_client(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("reading S3 CA certificate"),
            "missing CA error should name the CA read failure: {err}"
        );
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

    #[test]
    fn test_build_from_core_config_uses_ca_cert_path() {
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("missing-ca.pem");
        let storage = tcfs_core::config::StorageConfig {
            endpoint: "https://s3.example.com:8333".into(),
            enforce_tls: true,
            ca_cert_path: Some(ca_path),
            ..Default::default()
        };

        let err = build_from_core_config(&storage, "key", "secret").unwrap_err();
        assert!(
            err.to_string().contains("reading S3 CA certificate"),
            "core config CA path should be passed to the operator: {err}"
        );
    }
}
