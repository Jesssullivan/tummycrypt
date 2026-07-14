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
    /// Permit credentials to be sent over plaintext HTTP.
    ///
    /// This is intentionally false by default. Callers may enable it only for
    /// isolated development or test endpoints.
    pub allow_insecure_http: bool,
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
            allow_insecure_http: false,
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
    validate_endpoint_transport(cfg)?;

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

fn validate_endpoint_transport(cfg: &StorageConfig) -> Result<()> {
    let endpoint = reqwest::Url::parse(&cfg.endpoint)
        .with_context(|| format!("parsing S3 endpoint URL {}", cfg.endpoint))?;

    if let Some(warning) = insecure_transport_warning(endpoint.scheme(), cfg.allow_insecure_http) {
        tracing::warn!(endpoint = %cfg.endpoint, "{warning}");
    }

    match endpoint.scheme() {
        "https" => Ok(()),
        "http" if cfg.allow_insecure_http => Ok(()),
        "http" => anyhow::bail!(
            "S3 endpoint uses plaintext HTTP ({}). Use an HTTPS endpoint. For isolated development or tests only, explicitly set storage.enforce_tls = false (tcfs config) or allow_insecure_http = true (low-level client).",
            cfg.endpoint
        ),
        scheme => anyhow::bail!(
            "unsupported S3 endpoint scheme {scheme:?} in {}; HTTPS is required",
            cfg.endpoint
        ),
    }
}

fn insecure_transport_warning(scheme: &str, allow_insecure_http: bool) -> Option<&'static str> {
    if !allow_insecure_http {
        return None;
    }
    match scheme {
        "http" => Some(
            "S3 endpoint uses explicitly allowed plaintext HTTP; credentials are transmitted \
             unencrypted. This mode is for isolated development and tests only.",
        ),
        "https" => Some(
            "S3 insecure-HTTP compatibility is enabled for an HTTPS endpoint; the first hop uses \
             TLS, but redirects may downgrade to plaintext HTTP. Disable this development/test \
             opt-in to enforce HTTPS for the complete request chain.",
        ),
        _ => None,
    }
}

fn build_s3_http_client(cfg: &StorageConfig) -> Result<Option<HttpClient>> {
    // OpenDAL requires redirect support, while reqwest's default policy also
    // permits an HTTPS endpoint to redirect to plaintext HTTP. Install a
    // bounded policy for every operator so `allow_insecure_http = false`
    // covers the complete request chain, not only the configured first hop.
    let allow_insecure_http = cfg.allow_insecure_http;
    let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many S3 redirects");
        }
        if redirect_scheme_allowed(attempt.url().scheme(), allow_insecure_http) {
            attempt.follow()
        } else {
            attempt.error("S3 redirect to insecure or unsupported transport rejected")
        }
    });
    let mut builder = reqwest::Client::builder().redirect(redirect_policy);
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
        allow_insecure_http = cfg.allow_insecure_http,
        "S3 HTTP client security and transport controls enabled"
    );
    Ok(Some(HttpClient::with(client)))
}

fn redirect_scheme_allowed(scheme: &str, allow_insecure_http: bool) -> bool {
    scheme == "https" || (allow_insecure_http && scheme == "http")
}

/// Build an operator from tcfs-core config + loaded credentials.
///
/// HTTPS is required by default. A plaintext HTTP endpoint is accepted only
/// when the core config explicitly sets `enforce_tls = false` for isolated
/// development or tests.
pub fn build_from_core_config(
    storage: &tcfs_core::config::StorageConfig,
    access_key_id: &str,
    secret_access_key: &str,
) -> Result<Operator> {
    build_operator_with_limits(
        &StorageConfig {
            endpoint: storage.endpoint.clone(),
            region: storage.region.clone(),
            bucket: storage.bucket.clone(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
            allow_insecure_http: !storage.enforce_tls,
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
    fn test_build_operator_https_valid() {
        let cfg = StorageConfig {
            endpoint: "https://localhost:8333".to_string(),
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
    fn test_build_operator_rejects_http_by_default() {
        let cfg = StorageConfig {
            endpoint: "http://localhost:8333".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            ..Default::default()
        };

        let err = build_operator(&cfg).unwrap_err();
        assert!(err.to_string().contains("plaintext HTTP"), "{err:#}");
    }

    #[test]
    fn test_build_operator_allows_explicit_insecure_http() {
        let cfg = StorageConfig {
            endpoint: "http://localhost:8333".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            allow_insecure_http: true,
            ..Default::default()
        };

        assert!(build_operator(&cfg).is_ok());
    }

    #[test]
    fn test_build_operator_with_s3_http_controls() {
        let cfg = StorageConfig {
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            allow_insecure_http: false,
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
    fn strict_redirect_policy_rejects_tls_downgrade() {
        assert!(redirect_scheme_allowed("https", false));
        assert!(!redirect_scheme_allowed("http", false));
        assert!(!redirect_scheme_allowed("file", false));
    }

    #[test]
    fn explicit_dev_opt_in_allows_http_redirects_only() {
        assert!(redirect_scheme_allowed("https", true));
        assert!(redirect_scheme_allowed("http", true));
        assert!(!redirect_scheme_allowed("file", true));
    }

    #[test]
    fn insecure_opt_in_warns_for_plaintext_and_https_downgrade_risk() {
        let plaintext = insecure_transport_warning("http", true).unwrap();
        let downgrade = insecure_transport_warning("https", true).unwrap();

        assert!(plaintext.contains("credentials are transmitted"));
        assert!(downgrade.contains("redirects may downgrade"));
        assert_ne!(plaintext, downgrade);
        assert!(insecure_transport_warning("https", false).is_none());
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
    fn test_build_from_core_config_http_explicit_dev_opt_in() {
        // HTTP endpoint with explicit enforce_tls=false should succeed (and warn).
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
            "error message should mention the plaintext transport"
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
