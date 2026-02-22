use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level daemon configuration (loaded from tcfs.toml)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TcfsConfig {
    pub daemon: DaemonConfig,
    pub storage: StorageConfig,
    pub secrets: SecretsConfig,
    pub sync: SyncConfig,
    pub fuse: FuseConfig,
    pub crypto: CryptoConfig,
    pub sops: SopsConfig,
    /// Warn if the config file is world-readable (default: true)
    #[serde(default = "default_true")]
    pub config_file_mode_check: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Unix socket path for gRPC (default: /run/tcfsd/tcfsd.sock)
    pub socket: PathBuf,
    /// TCP listen address for remote gRPC (optional)
    pub listen: Option<String>,
    /// Prometheus metrics endpoint (default: 127.0.0.1:9100)
    pub metrics_addr: Option<String>,
    /// Log level (default: info)
    pub log_level: String,
    /// Log format: "json" or "text"
    pub log_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// SeaweedFS S3 endpoint
    pub endpoint: String,
    /// S3 region (default: us-east-1)
    pub region: String,
    /// Default bucket name
    pub bucket: String,
    /// SOPS credential file path
    pub credentials_file: Option<PathBuf>,
    /// Enforce HTTPS for S3 connections (warn/error on HTTP endpoints)
    pub enforce_tls: bool,
    /// Path to a custom CA certificate for S3 TLS verification
    pub ca_cert_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretsConfig {
    /// Age identity file (default: ~/.config/sops/age/keys.txt)
    pub age_identity: Option<PathBuf>,
    /// KDBX database file path
    pub kdbx_path: Option<PathBuf>,
    /// SOPS credentials directory
    pub sops_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// NATS JetStream endpoint
    pub nats_url: String,
    /// Enforce TLS for NATS connections
    pub nats_tls: bool,
    /// Path to a custom CA certificate for NATS TLS verification
    pub nats_ca_cert: Option<PathBuf>,
    /// RocksDB state cache path
    pub state_db: PathBuf,
    /// Worker thread count (0 = cpu_count)
    pub workers: usize,
    /// Retry limit for failed tasks
    pub max_retries: u32,
    /// Path to device identity JSON file
    pub device_identity: Option<PathBuf>,
    /// Device name (defaults to hostname)
    pub device_name: Option<String>,
    /// Conflict resolution mode: "auto", "interactive", or "defer"
    pub conflict_mode: String,
    /// Whether to sync .git directories
    pub sync_git_dirs: bool,
    /// Git sync mode: "bundle" or "raw"
    pub git_sync_mode: String,
    /// Whether to sync hidden directories (dotfiles/dotdirs)
    pub sync_hidden_dirs: bool,
    /// Glob patterns to exclude from sync
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FuseConfig {
    /// Negative dentry cache TTL in seconds (default: 30)
    pub negative_cache_ttl_secs: u64,
    /// Disk cache directory for partial downloads
    pub cache_dir: PathBuf,
    /// Maximum disk cache size in MB
    pub cache_max_mb: u64,
}

/// E2E encryption configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CryptoConfig {
    /// Enable client-side encryption (default: false until key is set up)
    pub enabled: bool,
    /// Argon2id memory cost in KiB (default: 65536 = 64 MiB)
    pub argon2_mem_cost_kib: u32,
    /// Argon2id time cost (iterations, default: 3)
    pub argon2_time_cost: u32,
    /// Argon2id parallelism (default: 4)
    pub argon2_parallelism: u32,
    /// Path to the encrypted master key file
    pub master_key_file: Option<PathBuf>,
    /// Path to the device identity file
    pub device_identity: Option<PathBuf>,
}

impl Default for CryptoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            argon2_mem_cost_kib: 65536,
            argon2_time_cost: 3,
            argon2_parallelism: 4,
            master_key_file: None,
            device_identity: None,
        }
    }
}

/// SOPS secret propagation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SopsConfig {
    /// Enable SOPS secret propagation
    pub enabled: bool,
    /// Local SOPS-managed directory to watch/sync
    pub sops_dir: PathBuf,
    /// S3 prefix for SOPS sync data
    pub sync_prefix: String,
    /// Machine identifier (defaults to hostname)
    pub machine_id: Option<String>,
    /// Local backup directory for pre-mutation snapshots
    pub backup_dir: PathBuf,
    /// Auto-watch for filesystem changes and push
    pub watch: bool,
}

impl Default for SopsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sops_dir: PathBuf::from("~/.config/sops/age"),
            sync_prefix: "sops-sync".into(),
            machine_id: None,
            backup_dir: PathBuf::from("~/.local/share/tcfs/sops-backups"),
            watch: false,
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket: PathBuf::from("/run/tcfsd/tcfsd.sock"),
            listen: None,
            metrics_addr: Some("127.0.0.1:9100".into()),
            log_level: "info".into(),
            log_format: "json".into(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8333".into(),
            region: "us-east-1".into(),
            bucket: "tcfs".into(),
            credentials_file: None,
            enforce_tls: false,
            ca_cert_path: None,
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            nats_url: "nats://localhost:4222".into(),
            nats_tls: false,
            nats_ca_cert: None,
            state_db: PathBuf::from("~/.local/share/tcfsd/state.db"),
            workers: 0,
            max_retries: 3,
            device_identity: None,
            device_name: None,
            conflict_mode: "auto".into(),
            sync_git_dirs: false,
            git_sync_mode: "bundle".into(),
            sync_hidden_dirs: false,
            exclude_patterns: Vec::new(),
        }
    }
}

impl Default for FuseConfig {
    fn default() -> Self {
        Self {
            negative_cache_ttl_secs: 30,
            cache_dir: PathBuf::from("~/.cache/tcfs"),
            cache_max_mb: 10240,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
config_file_mode_check = true

[daemon]
socket = "/tmp/tcfsd.sock"
log_level = "debug"
log_format = "text"

[storage]
endpoint = "https://s3.example.com:8333"
region = "us-west-2"
bucket = "my-bucket"
enforce_tls = true

[secrets]
age_identity = "/home/user/.age/key.txt"

[sync]
nats_url = "tls://nats.example.com:4222"
nats_tls = true
workers = 4
max_retries = 5

[fuse]
negative_cache_ttl_secs = 60
cache_dir = "/var/cache/tcfs"
cache_max_mb = 20480

[crypto]
enabled = true
argon2_mem_cost_kib = 131072
argon2_time_cost = 4
argon2_parallelism = 8
"#;
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();

        assert_eq!(config.daemon.socket, PathBuf::from("/tmp/tcfsd.sock"));
        assert_eq!(config.daemon.log_level, "debug");
        assert_eq!(config.storage.endpoint, "https://s3.example.com:8333");
        assert!(config.storage.enforce_tls);
        assert_eq!(config.storage.bucket, "my-bucket");
        assert!(config.sync.nats_tls);
        assert_eq!(config.sync.workers, 4);
        assert_eq!(config.fuse.cache_max_mb, 20480);
        assert!(config.crypto.enabled);
        assert_eq!(config.crypto.argon2_mem_cost_kib, 131072);
        assert!(config.config_file_mode_check);
    }

    #[test]
    fn test_parse_defaults() {
        let config: TcfsConfig = toml::from_str("").unwrap();

        assert_eq!(config.daemon.socket, PathBuf::from("/run/tcfsd/tcfsd.sock"));
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.storage.endpoint, "http://localhost:8333");
        assert!(!config.storage.enforce_tls);
        assert_eq!(config.storage.bucket, "tcfs");
        assert_eq!(config.sync.nats_url, "nats://localhost:4222");
        assert!(!config.sync.nats_tls);
        assert!(!config.crypto.enabled);
        assert_eq!(config.crypto.argon2_mem_cost_kib, 65536);
        assert!(config.config_file_mode_check);
    }

    #[test]
    fn test_parse_partial_config() {
        let toml_str = r#"
[storage]
endpoint = "http://192.168.1.100:8333"
"#;
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();

        // Overridden
        assert_eq!(config.storage.endpoint, "http://192.168.1.100:8333");
        // Defaults
        assert_eq!(config.storage.region, "us-east-1");
        assert_eq!(config.storage.bucket, "tcfs");
        assert_eq!(config.daemon.log_level, "info");
    }

    #[test]
    fn test_serialize_roundtrip() {
        let config = TcfsConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: TcfsConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.daemon.socket, parsed.daemon.socket);
        assert_eq!(config.storage.endpoint, parsed.storage.endpoint);
        assert_eq!(config.sync.nats_url, parsed.sync.nats_url);
    }
}
