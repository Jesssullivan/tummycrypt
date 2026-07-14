use tcfs_storage::operator::StorageConfig;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StorageTransportBounds {
    pub allow_insecure_http: Option<bool>,
    pub max_concurrent_ops: Option<usize>,
    pub s3_connect_timeout_secs: Option<u64>,
    pub s3_pool_idle_timeout_secs: Option<u64>,
    pub s3_pool_max_idle_per_host: Option<usize>,
    pub s3_http1_only: Option<bool>,
}

impl StorageTransportBounds {
    fn merge_missing(self, fallback: Self) -> Self {
        Self {
            allow_insecure_http: self.allow_insecure_http.or(fallback.allow_insecure_http),
            max_concurrent_ops: self.max_concurrent_ops.or(fallback.max_concurrent_ops),
            s3_connect_timeout_secs: self
                .s3_connect_timeout_secs
                .or(fallback.s3_connect_timeout_secs),
            s3_pool_idle_timeout_secs: self
                .s3_pool_idle_timeout_secs
                .or(fallback.s3_pool_idle_timeout_secs),
            s3_pool_max_idle_per_host: self
                .s3_pool_max_idle_per_host
                .or(fallback.s3_pool_max_idle_per_host),
            s3_http1_only: self.s3_http1_only.or(fallback.s3_http1_only),
        }
    }

    fn apply_to_storage_config(self, config: &mut StorageConfig) {
        if let Some(value) = self.allow_insecure_http {
            config.allow_insecure_http = value;
        }
        if let Some(value) = self.s3_connect_timeout_secs {
            config.s3_connect_timeout_secs = value;
        }
        if let Some(value) = self.s3_pool_idle_timeout_secs {
            config.s3_pool_idle_timeout_secs = value;
        }
        if let Some(value) = self.s3_pool_max_idle_per_host {
            config.s3_pool_max_idle_per_host = value;
        }
        if let Some(value) = self.s3_http1_only {
            config.s3_http1_only = value;
        }
    }

    pub(crate) fn max_concurrent_ops(self) -> usize {
        self.max_concurrent_ops.unwrap_or(0)
    }
}

pub(crate) fn build_operator_from_json(config: &serde_json::Value) -> Option<opendal::Operator> {
    let endpoint = config["s3_endpoint"].as_str().unwrap_or_default();
    let bucket = config["s3_bucket"].as_str().unwrap_or("tcfs");
    let access = config["s3_access"].as_str().unwrap_or_default();
    let secret = config["s3_secret"].as_str().unwrap_or_default();

    if endpoint.is_empty() || access.is_empty() || secret.is_empty() {
        return None;
    }

    match build_operator_from_parts(
        endpoint.to_string(),
        bucket.to_string(),
        access.to_string(),
        secret.to_string(),
        bounds_from_json_and_env(config),
    ) {
        Ok(operator) => Some(operator),
        Err(error) => {
            tracing::error!(%error, "FileProvider storage operator rejected");
            None
        }
    }
}

#[cfg_attr(not(feature = "uniffi"), allow(dead_code))]
pub(crate) fn build_operator_from_parts_with_env(
    endpoint: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
) -> anyhow::Result<opendal::Operator> {
    build_operator_from_parts(
        endpoint,
        bucket,
        access_key_id,
        secret_access_key,
        bounds_from_env(),
    )
}

fn build_operator_from_parts(
    endpoint: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    bounds: StorageTransportBounds,
) -> anyhow::Result<opendal::Operator> {
    let mut config = StorageConfig {
        endpoint,
        region: "us-east-1".to_string(),
        bucket,
        access_key_id,
        secret_access_key,
        ..Default::default()
    };
    bounds.apply_to_storage_config(&mut config);

    tcfs_storage::operator::build_operator_with_limits(&config, bounds.max_concurrent_ops())
}

fn bounds_from_json_and_env(config: &serde_json::Value) -> StorageTransportBounds {
    bounds_from_json(config).merge_missing(bounds_from_env())
}

fn bounds_from_json(config: &serde_json::Value) -> StorageTransportBounds {
    StorageTransportBounds {
        allow_insecure_http: json_bool(
            config,
            &["allow_insecure_http", "storage_allow_insecure_http"],
        ),
        max_concurrent_ops: json_usize(
            config,
            &["max_concurrent_ops", "storage_max_concurrent_ops"],
        ),
        s3_connect_timeout_secs: json_u64(
            config,
            &["s3_connect_timeout_secs", "storage_s3_connect_timeout_secs"],
        ),
        s3_pool_idle_timeout_secs: json_u64(
            config,
            &[
                "s3_pool_idle_timeout_secs",
                "storage_s3_pool_idle_timeout_secs",
            ],
        ),
        s3_pool_max_idle_per_host: json_usize(
            config,
            &[
                "s3_pool_max_idle_per_host",
                "storage_s3_pool_max_idle_per_host",
            ],
        ),
        s3_http1_only: json_bool(config, &["s3_http1_only", "storage_s3_http1_only"]),
    }
}

fn bounds_from_env() -> StorageTransportBounds {
    StorageTransportBounds {
        allow_insecure_http: env_bool("TCFS_STORAGE_ALLOW_INSECURE_HTTP"),
        max_concurrent_ops: env_usize("TCFS_STORAGE_MAX_CONCURRENT_OPS"),
        s3_connect_timeout_secs: env_u64("TCFS_STORAGE_S3_CONNECT_TIMEOUT_SECS"),
        s3_pool_idle_timeout_secs: env_u64("TCFS_STORAGE_S3_POOL_IDLE_TIMEOUT_SECS"),
        s3_pool_max_idle_per_host: env_usize("TCFS_STORAGE_S3_POOL_MAX_IDLE_PER_HOST"),
        s3_http1_only: env_bool("TCFS_STORAGE_S3_HTTP1_ONLY"),
    }
}

fn json_u64(config: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        config.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| parse_u64_allow_zero(value.as_str()?))
        })
    })
}

fn json_usize(config: &serde_json::Value, keys: &[&str]) -> Option<usize> {
    json_u64(config, keys).and_then(|value| usize::try_from(value).ok())
}

fn json_bool(config: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        let value = config.get(*key)?;
        value
            .as_bool()
            .or_else(|| value.as_u64().map(|raw| raw != 0))
            .or_else(|| parse_bool(value.as_str()?))
    })
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().as_deref().and_then(parse_u64)
}

fn env_usize(name: &str) -> Option<usize> {
    env_u64(name).and_then(|value| usize::try_from(value).ok())
}

fn env_bool(name: &str) -> Option<bool> {
    std::env::var(name).ok().as_deref().and_then(parse_bool)
}

fn parse_u64(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

fn parse_u64_allow_zero(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok()
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_storage_bounds_from_json() {
        let config = serde_json::json!({
            "allow_insecure_http": false,
            "max_concurrent_ops": "7",
            "s3_connect_timeout_secs": 5,
            "s3_pool_idle_timeout_secs": "13",
            "s3_pool_max_idle_per_host": 3,
            "s3_http1_only": "true"
        });

        assert_eq!(
            bounds_from_json(&config),
            StorageTransportBounds {
                allow_insecure_http: Some(false),
                max_concurrent_ops: Some(7),
                s3_connect_timeout_secs: Some(5),
                s3_pool_idle_timeout_secs: Some(13),
                s3_pool_max_idle_per_host: Some(3),
                s3_http1_only: Some(true),
            }
        );
    }

    #[test]
    fn json_bounds_accept_storage_prefixed_keys_and_explicit_false() {
        let config = serde_json::json!({
            "storage_allow_insecure_http": true,
            "storage_max_concurrent_ops": 4,
            "storage_s3_connect_timeout_secs": "6",
            "storage_s3_pool_idle_timeout_secs": "0",
            "storage_s3_pool_max_idle_per_host": "9",
            "storage_s3_http1_only": false
        });

        assert_eq!(
            bounds_from_json(&config),
            StorageTransportBounds {
                allow_insecure_http: Some(true),
                max_concurrent_ops: Some(4),
                s3_connect_timeout_secs: Some(6),
                s3_pool_idle_timeout_secs: Some(0),
                s3_pool_max_idle_per_host: Some(9),
                s3_http1_only: Some(false),
            }
        );
    }

    #[test]
    fn direct_operator_rejects_plaintext_without_explicit_opt_in() {
        let config = serde_json::json!({
            "s3_endpoint": "http://localhost:8333",
            "s3_bucket": "tcfs",
            "s3_access": "test-access",
            "s3_secret": "test-secret"
        });

        assert!(build_operator_from_json(&config).is_none());
    }

    #[test]
    fn direct_operator_allows_explicit_plaintext_dev_opt_in() {
        let config = serde_json::json!({
            "s3_endpoint": "http://localhost:8333",
            "s3_bucket": "tcfs",
            "s3_access": "test-access",
            "s3_secret": "test-secret",
            "allow_insecure_http": true
        });

        assert!(build_operator_from_json(&config).is_some());
    }
}
