use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    pub auth: AuthConfig,
    /// Warn if the config file is world-readable (default: true)
    #[serde(default = "default_true")]
    pub config_file_mode_check: bool,
}

/// Authentication and session configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Enable auth subsystem (default: false)
    pub enabled: bool,
    /// Require a valid session token for protected RPCs (push, pull, mount, unsync).
    /// Default: true. When false, all local requests are trusted (alpha bypass).
    /// WARNING: Setting this to false grants full permissions to any Unix socket client.
    pub require_session: bool,
    /// Session expiry in hours (default: 24)
    pub session_expiry_hours: u64,
    /// Enabled auth methods (default: ["master_key"])
    pub methods: Vec<String>,
    /// TOTP-specific configuration
    pub totp: AuthTotpConfig,
    /// WebAuthn-specific configuration
    pub webauthn: AuthWebAuthnConfig,
    /// Enrollment configuration
    pub enrollment: AuthEnrollmentConfig,
    /// Rate limiting configuration
    pub rate_limit: AuthRateLimitConfig,
}

/// TOTP (RFC 6238) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthTotpConfig {
    /// Issuer name shown in authenticator apps (default: "TummyCrypt")
    pub issuer: String,
    /// Number of digits in TOTP code (default: 6)
    pub digits: u32,
}

impl Default for AuthTotpConfig {
    fn default() -> Self {
        Self {
            issuer: "TummyCrypt".into(),
            digits: 6,
        }
    }
}

/// WebAuthn / FIDO2 configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthWebAuthnConfig {
    /// Relying party ID (domain)
    pub relying_party_id: String,
    /// Relying party display name
    pub relying_party_name: String,
}

impl Default for AuthWebAuthnConfig {
    fn default() -> Self {
        Self {
            relying_party_id: "tcfs.local".into(),
            relying_party_name: "TummyCrypt".into(),
        }
    }
}

/// Device enrollment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthEnrollmentConfig {
    /// Enable QR code generation for enrollment invites
    pub qr_code: bool,
    /// Enable NATS-based device auto-discovery
    pub auto_discovery: bool,
}

impl Default for AuthEnrollmentConfig {
    fn default() -> Self {
        Self {
            qr_code: true,
            auto_discovery: false,
        }
    }
}

/// Rate limiting for auth attempts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthRateLimitConfig {
    /// Maximum failed attempts before lockout (default: 5)
    pub max_attempts: u32,
    /// Base lockout duration in seconds (default: 300 = 5 minutes)
    pub lockout_secs: u64,
    /// Backoff multiplier for consecutive lockouts (default: 2)
    pub backoff_multiplier: u32,
}

impl Default for AuthRateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            lockout_secs: 300,
            backoff_multiplier: 2,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_session: true,
            session_expiry_hours: 24,
            methods: vec!["master_key".into()],
            totp: AuthTotpConfig::default(),
            webauthn: AuthWebAuthnConfig::default(),
            enrollment: AuthEnrollmentConfig::default(),
            rate_limit: AuthRateLimitConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Unix socket path for gRPC (default: /run/tcfsd/tcfsd.sock)
    pub socket: PathBuf,
    /// Additional Unix socket inside macOS App Group container for FileProvider access.
    /// The sandboxed FileProvider .appex cannot reach the primary socket, so the daemon
    /// binds a second listener here (e.g. ~/Library/Group Containers/group.io.tinyland.tcfs/tcfsd.sock).
    pub fileprovider_socket: Option<PathBuf>,
    /// HTTP endpoint handed to the macOS FileProvider extension.
    ///
    /// This is consumed by the provisioning script and used by tcfsd only to
    /// identify FileProvider mode; the actual TCP bind address is `listen`.
    pub fileprovider_endpoint: Option<String>,
    /// Legacy plaintext TCP listen address. tcfsd refuses this; remote
    /// operators must tunnel the owner-only Unix socket over SSH until a
    /// TLS/mTLS transport is configured.
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
    /// Remote prefix within the bucket for index/manifest/chunk objects.
    /// Used by CLI push/pull and FUSE mount. Defaults to bucket name if unset.
    #[serde(default)]
    pub remote_prefix: Option<String>,
    /// SOPS credential file path
    pub credentials_file: Option<PathBuf>,
    /// Enforce HTTPS for S3 connections (default: true).
    /// Set false only for an explicitly isolated development/test endpoint.
    #[serde(default = "default_true")]
    pub enforce_tls: bool,
    /// Path to a custom CA certificate for S3 TLS verification
    pub ca_cert_path: Option<PathBuf>,
    /// Maximum concurrent S3 operations (0 = unlimited). Default: 0.
    #[serde(default)]
    pub max_concurrent_ops: usize,
    /// S3 HTTP connect timeout in seconds (0 = reqwest/OpenDAL default).
    #[serde(default)]
    pub s3_connect_timeout_secs: u64,
    /// S3 HTTP connection-pool idle timeout in seconds (0 = reqwest/OpenDAL default).
    #[serde(default)]
    pub s3_pool_idle_timeout_secs: u64,
    /// Maximum idle S3 HTTP connections per host (0 = reqwest/OpenDAL default).
    #[serde(default)]
    pub s3_pool_max_idle_per_host: usize,
    /// Force S3 HTTP/1 only for transport experiments and S3-compatible servers.
    #[serde(default)]
    pub s3_http1_only: bool,
    /// Maximum upload speed in bytes/sec (0 = unlimited). Default: 0.
    #[serde(default)]
    pub max_upload_bytes_per_sec: u64,
    /// Maximum download speed in bytes/sec (0 = unlimited). Default: 0.
    #[serde(default)]
    pub max_download_bytes_per_sec: u64,
}

impl StorageConfig {
    /// Resolve the effective S3 prefix: explicit `remote_prefix` or fall back to `bucket`.
    ///
    /// ALL code that needs the S3 prefix MUST call this instead of inlining
    /// `remote_prefix.unwrap_or(bucket)` — there were 3 inconsistent copies.
    pub fn resolved_prefix(&self) -> &str {
        self.remote_prefix.as_deref().unwrap_or(&self.bucket)
    }
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
    /// NATS authentication token (optional)
    pub nats_token: Option<String>,
    /// Path to a custom CA certificate for NATS TLS verification
    pub nats_ca_cert: Option<PathBuf>,
    /// State cache path. The key is named `state_db` for the (future) RocksDB
    /// Phase 4 backend, but the live JSON cache is the `.json` sibling of this
    /// path: both the daemon and the CLI derive `state_db.with_extension("json")`,
    /// so a `…/state.db` value resolves to `…/state.json`.
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
    /// Whether to preserve POSIX symbolic links as links during tree sync.
    pub sync_symlinks: bool,
    /// Whether to sync empty directories via `.tcfs_dir` markers.
    /// Default: true.
    pub sync_empty_dirs: bool,
    /// Local directory root for synced files (used by auto-pull)
    pub sync_root: Option<PathBuf>,
    /// Maximum file age (seconds) before eligible for auto-unsync.
    /// 0 = disabled. Default: 0 (disabled).
    pub auto_unsync_max_age_secs: u64,
    /// How often to run the auto-unsync sweep (seconds). Default: 3600 (hourly).
    pub auto_unsync_interval_secs: u64,
    /// If true, log auto-unsync candidates but don't actually remove them.
    pub auto_unsync_dry_run: bool,
    /// Disk usage threshold (0.0-1.0) that triggers aggressive auto-unsync.
    /// 0.0 = disabled. Example: 0.85 triggers when disk is 85% full.
    pub auto_unsync_disk_pressure_pct: f64,
    /// Maximum files to dehydrate per sweep (prevents long lock holds). 0 = unlimited.
    pub auto_unsync_max_per_sweep: usize,
    /// Global auto-download threshold (bytes) for OnDemand folders.
    /// Files smaller than this are auto-pulled on NATS events. 0 = never auto-download.
    /// Default: 10MB. Per-folder overrides via PolicyStore.download_threshold.
    pub auto_download_threshold: u64,
    /// Enable sync trash (unlink moves to .tcfs-trash/ instead of deleting).
    /// Default: true.
    pub trash_enabled: bool,
    /// Default age threshold used by explicit `tcfs trash purge`.
    /// 0 disables age-based purge unless the operator passes a positive
    /// `--older-than` or explicit `--all`. Default: 2592000 (30 days).
    pub trash_retention_secs: u64,
    /// Periodic reconciliation interval in seconds. 0 = disabled.
    /// Reconciles local sync_root against remote index, applying per-folder policies.
    /// Default: 300 (5 minutes).
    pub reconcile_interval_secs: u64,
    /// Grace period before orphaned remote chunk objects are eligible for cleanup.
    /// 0 = disabled. Default: 86400 (24 hours).
    pub orphan_chunk_cleanup_grace_secs: u64,
    /// Daemon-trusted registry of non-primary sync roots.
    ///
    /// Clients address these entries by stable ID. They never supply a local
    /// state path or remote prefix over RPC; tcfsd resolves both from this
    /// configuration and applies the root policy before opening the cache.
    /// Named paths and prefixes must be component-disjoint from the primary
    /// route and from every registered peer.
    #[serde(default)]
    pub roots: BTreeMap<String, RegisteredRootConfig>,
    /// Trusted parent directory for registered-root state caches.
    ///
    /// When unset, tcfsd uses `<daemon socket parent>/reconcile`, matching the
    /// default per-user socket layout. Deployments that move the socket into a
    /// runtime directory (for example systemd `%t` or `/run`) must set a
    /// persistent directory explicitly.
    #[serde(default)]
    pub root_state_dir: Option<PathBuf>,
}

/// A daemon-enrolled non-primary root.
///
/// This is intentionally a narrow routing record, not the broad root lifecycle
/// model tracked by TIN-1556. TIN-2853 uses it only to make an already scheduled,
/// isolated reconcile cache inspectable and resolvable by stable ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RegisteredRootConfig {
    /// Local filesystem root owned by this identity.
    pub local_root: PathBuf,
    /// Exact object-store prefix used by this root's reconcile cycle.
    pub remote_prefix: String,
    /// Isolated JSON state cache. tcfsd additionally fences this under its
    /// machine-local `reconcile/` state directory.
    pub state_path: PathBuf,
    /// Whether this root is inspection-only or may be explicitly resolved.
    pub policy: RegisteredRootPolicy,
}

impl Default for RegisteredRootConfig {
    fn default() -> Self {
        Self {
            local_root: PathBuf::new(),
            remote_prefix: String::new(),
            state_path: PathBuf::new(),
            policy: RegisteredRootPolicy::InspectOnly,
        }
    }
}

/// Mutation policy for a daemon-enrolled root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegisteredRootPolicy {
    /// Conflict records may be listed and git keep-both may be dry-run, but
    /// execute is rejected server-side.
    #[default]
    InspectOnly,
    /// An authenticated operator may explicitly execute repo-group keep-both.
    Resolve,
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

/// File-key wrap mode (TIN-1417 migration).
///
/// Controls how a regular file's per-file key is wrapped in its manifest. This
/// replaces the legacy boolean `per_device_wrapping`. The three states form an
/// EXPAND / CONTRACT migration ladder:
///
/// - [`WrapMode::Master`] (DEFAULT): wrap ONLY under the shared master key
///   (`encrypted_file_key`). Byte-identical to the legacy default
///   (`per_device_wrapping = false`). Any master/old-binary reader can decrypt.
/// - [`WrapMode::Dual`] (EXPAND / transitional): emit BOTH the master wrap
///   (`encrypted_file_key`, for rollback + master/old-binary readers) AND the
///   per-device wraps (`wrapped_file_keys`). Stays manifest **version 2** and is
///   back-compatible by construction. Safe to roll the fleet through.
/// - [`WrapMode::PerDevice`] (CONTRACT): emit ONLY the per-device wraps and DROP
///   the master wrap — true revocation. Bumps the manifest to **version 3** so
///   pre-per-device binaries fail CLOSED instead of misreading a
///   v2-with-no-`encrypted_file_key` as keyless. The daemon refuses to write
///   PerDevice until a roll-call probe confirms every active (non-revoked)
///   device has a real age recipient; until then it falls back to `Dual` and
///   warns loudly (never silently drops the master wrap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapMode {
    /// Master-only wrap. Legacy default; byte-identical to `per_device_wrapping = false`.
    #[default]
    Master,
    /// Transitional dual wrap: master wrap + per-device wraps (manifest v2).
    Dual,
    /// Per-device-only wrap, drops the master wrap (manifest v3, true revocation).
    PerDevice,
}

/// E2E encryption configuration.
///
/// `Deserialize` is hand-written (rather than derived) so that the legacy
/// boolean `per_device_wrapping` key still parses for back-compat: `true` maps
/// to [`WrapMode::Dual`] (keeps the master fallback — safe), `false`/absent maps
/// to [`WrapMode::Master`]. A present `wrap_mode` key always wins. Going forward
/// `wrap_mode` is canonical and the only key serialized.
#[derive(Debug, Clone, Serialize)]
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
    /// Path to a passphrase file — if set, daemon derives key on startup (auto-unlock)
    pub passphrase_file: Option<PathBuf>,
    /// Hex-encoded 16-byte salt for passphrase-based key derivation.
    /// Generated once per vault. If unset and passphrase_file is used, a random
    /// salt is generated and must be persisted by the caller.
    pub kdf_salt: Option<String>,
    /// File-key wrap mode (TIN-1417). Default [`WrapMode::Master`] (legacy
    /// shared-master). See [`WrapMode`] for the EXPAND/CONTRACT semantics. A
    /// legacy `per_device_wrapping = true` config deserializes to
    /// [`WrapMode::Dual`]; `false`/absent to [`WrapMode::Master`].
    pub wrap_mode: WrapMode,
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
            passphrase_file: None,
            kdf_salt: None,
            wrap_mode: WrapMode::Master,
        }
    }
}

impl<'de> Deserialize<'de> for CryptoConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Shadow struct mirrors the serialized shape but additionally accepts the
        // legacy `per_device_wrapping` boolean. `wrap_mode`, when present, wins.
        #[derive(Deserialize)]
        #[serde(default)]
        struct CryptoConfigShadow {
            enabled: bool,
            argon2_mem_cost_kib: u32,
            argon2_time_cost: u32,
            argon2_parallelism: u32,
            master_key_file: Option<PathBuf>,
            device_identity: Option<PathBuf>,
            passphrase_file: Option<PathBuf>,
            kdf_salt: Option<String>,
            /// Canonical key. `None` when absent so the legacy key can decide.
            wrap_mode: Option<WrapMode>,
            /// Legacy back-compat key (TIN-1417). `true` -> Dual, `false`/absent -> Master.
            per_device_wrapping: Option<bool>,
        }

        impl Default for CryptoConfigShadow {
            fn default() -> Self {
                let base = CryptoConfig::default();
                Self {
                    enabled: base.enabled,
                    argon2_mem_cost_kib: base.argon2_mem_cost_kib,
                    argon2_time_cost: base.argon2_time_cost,
                    argon2_parallelism: base.argon2_parallelism,
                    master_key_file: base.master_key_file,
                    device_identity: base.device_identity,
                    passphrase_file: base.passphrase_file,
                    kdf_salt: base.kdf_salt,
                    wrap_mode: None,
                    per_device_wrapping: None,
                }
            }
        }

        let shadow = CryptoConfigShadow::deserialize(deserializer)?;

        // Precedence: an explicit `wrap_mode` wins. Otherwise map the legacy
        // `per_device_wrapping` boolean (true -> Dual, keeps the master fallback;
        // false/absent -> Master). Default is Master.
        let wrap_mode = match shadow.wrap_mode {
            Some(mode) => mode,
            None => match shadow.per_device_wrapping {
                Some(true) => WrapMode::Dual,
                Some(false) | None => WrapMode::Master,
            },
        };

        Ok(CryptoConfig {
            enabled: shadow.enabled,
            argon2_mem_cost_kib: shadow.argon2_mem_cost_kib,
            argon2_time_cost: shadow.argon2_time_cost,
            argon2_parallelism: shadow.argon2_parallelism,
            master_key_file: shadow.master_key_file,
            device_identity: shadow.device_identity,
            passphrase_file: shadow.passphrase_file,
            kdf_salt: shadow.kdf_salt,
            wrap_mode,
        })
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
        let socket = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join(".local/state")
            })
            .join("tcfsd/tcfsd.sock");
        Self {
            socket,
            fileprovider_socket: None,
            fileprovider_endpoint: None,
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
            remote_prefix: None,
            credentials_file: None,
            enforce_tls: true,
            ca_cert_path: None,
            max_concurrent_ops: 0,
            s3_connect_timeout_secs: 0,
            s3_pool_idle_timeout_secs: 0,
            s3_pool_max_idle_per_host: 0,
            s3_http1_only: false,
            max_upload_bytes_per_sec: 0,
            max_download_bytes_per_sec: 0,
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            nats_url: "nats://localhost:4222".into(),
            nats_tls: true,
            nats_token: None,
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
            sync_symlinks: false,
            sync_empty_dirs: true,
            sync_root: None,
            auto_unsync_max_age_secs: 0,
            auto_unsync_interval_secs: 3600,
            auto_unsync_dry_run: false,
            auto_unsync_disk_pressure_pct: 0.0,
            auto_unsync_max_per_sweep: 100,
            auto_download_threshold: 10 * 1024 * 1024, // 10MB
            trash_enabled: true,
            trash_retention_secs: 30 * 24 * 3600, // 30 days
            reconcile_interval_secs: 300,         // 5 minutes
            orphan_chunk_cleanup_grace_secs: 24 * 3600,
            roots: BTreeMap::new(),
            root_state_dir: None,
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

/// Expand a leading `~/` in `path` to the user's home directory (`HOME`, then
/// `USERPROFILE`). Any other path is returned unchanged.
///
/// Config defaults carry literal `~` paths (e.g. `sync.state_db`) and the
/// config loader performs no normalization, so every consumer that touches the
/// filesystem must expand first — otherwise a `~/…` value resolves to a
/// CWD-relative `./~/…`. Shared here so the CLI and daemon expand identically.
pub fn expand_tilde(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        PathBuf::from(format!("{}/{}", home, rest))
    } else {
        path.to_path_buf()
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    let path = expand_tilde(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("resolving current directory: {error}"))
    }
}

fn lexically_normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("expected an absolute path, got {}", path.display()));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "path escapes its filesystem root during lexical normalization: {}",
                        path.display()
                    ));
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    if !normalized.is_absolute() {
        return Err(format!(
            "path lost its absolute root during normalization: {}",
            path.display()
        ));
    }
    Ok(normalized)
}

/// Resolve symlinks in the longest existing prefix, then append and normalize
/// a missing tail. A dangling symlink is rejected instead of being mistaken
/// for an ordinary not-yet-created component.
fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
    let path = absolute_path(path)?;

    let mut probe = PathBuf::new();
    for component in path.components() {
        probe.push(component.as_os_str());
        match std::fs::symlink_metadata(&probe) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::canonicalize(&probe).map_err(|error| {
                    format!(
                        "refusing unresolved symlink component {} in {}: {error}",
                        probe.display(),
                        path.display()
                    )
                })?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "inspecting path component {} for {}: {error}",
                    probe.display(),
                    path.display()
                ));
            }
        }
    }

    let components = path.components().collect::<Vec<_>>();
    for split in (1..=components.len()).rev() {
        let mut prefix = PathBuf::new();
        for component in &components[..split] {
            prefix.push(component.as_os_str());
        }
        match std::fs::canonicalize(&prefix) {
            Ok(mut resolved) => {
                for component in &components[split..] {
                    resolved.push(component.as_os_str());
                }
                return lexically_normalize_absolute(&resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "resolving path prefix {} for {}: {error}",
                    prefix.display(),
                    path.display()
                ));
            }
        }
    }

    Err(format!(
        "no existing ancestor could be resolved for {}",
        path.display()
    ))
}

/// Return whether `path` is equal to or below `root`.
///
/// Both a lexical comparison and a longest-existing-prefix canonical
/// comparison are required. The lexical check catches configured in-root
/// symlinks even when they currently point elsewhere; the canonical check
/// catches existing aliases that spell the same directory differently.
pub fn path_is_within(path: &Path, root: &Path) -> Result<bool, String> {
    let path_absolute = absolute_path(path)?;
    let root_absolute = absolute_path(root)?;
    let path_lexical = lexically_normalize_absolute(&path_absolute)?;
    let root_lexical = lexically_normalize_absolute(&root_absolute)?;
    if path_lexical == root_lexical || path_lexical.starts_with(&root_lexical) {
        return Ok(true);
    }

    let path_resolved = canonicalize_with_missing_tail(&path_absolute)?;
    let root_resolved = canonicalize_with_missing_tail(&root_absolute)?;
    Ok(path_resolved == root_resolved || path_resolved.starts_with(root_resolved))
}

/// Reject a selected sync path that is the configured master key or contains
/// it. This is a command-level guard for explicit file/tree pushes and manual
/// reconcile roots; the static blacklist remains defense-in-depth for the
/// standard `master.key` and rotation artifact names.
pub fn validate_sync_selection_excludes_master_key(
    config: &TcfsConfig,
    selected_path: &Path,
) -> Result<(), String> {
    let Some(master_key_path) = config.crypto.master_key_file.as_deref() else {
        return Ok(());
    };
    if path_is_within(master_key_path, selected_path)? {
        return Err(format!(
            "selected sync path {} is equal to or contains configured crypto.master_key_file {}",
            selected_path.display(),
            expand_tilde(master_key_path).display()
        ));
    }
    Ok(())
}

/// Reject a selected master-key path that lies in the primary or any named
/// sync root. Callers must run this before creating adjacent rotation state,
/// pending-key, or atomic replacement files.
pub fn validate_master_key_outside_sync_roots(
    config: &TcfsConfig,
    master_key_path: &Path,
) -> Result<(), String> {
    if let Some(primary_root) = config.sync.sync_root.as_deref() {
        if path_is_within(master_key_path, primary_root)? {
            return Err(format!(
                "master key path {} is inside primary sync.sync_root {}",
                expand_tilde(master_key_path).display(),
                expand_tilde(primary_root).display()
            ));
        }
    }

    for (root_id, root) in &config.sync.roots {
        if path_is_within(master_key_path, &root.local_root)? {
            return Err(format!(
                "master key path {} is inside registered root '{root_id}' local_root {}",
                expand_tilde(master_key_path).display(),
                expand_tilde(&root.local_root).display()
            ));
        }
    }
    Ok(())
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
max_concurrent_ops = 8
s3_connect_timeout_secs = 5
s3_pool_idle_timeout_secs = 30
s3_pool_max_idle_per_host = 8
s3_http1_only = true

[secrets]
age_identity = "/home/user/.age/key.txt"

[sync]
nats_url = "tls://nats.example.com:4222"
nats_tls = true
workers = 4
max_retries = 5
sync_root = "/home/user/tcfs"
orphan_chunk_cleanup_grace_secs = 7200

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
        assert_eq!(config.storage.max_concurrent_ops, 8);
        assert_eq!(config.storage.s3_connect_timeout_secs, 5);
        assert_eq!(config.storage.s3_pool_idle_timeout_secs, 30);
        assert_eq!(config.storage.s3_pool_max_idle_per_host, 8);
        assert!(config.storage.s3_http1_only);
        assert!(config.sync.nats_tls);
        assert_eq!(config.sync.workers, 4);
        assert_eq!(
            config.sync.sync_root,
            Some(PathBuf::from("/home/user/tcfs"))
        );
        assert_eq!(config.sync.orphan_chunk_cleanup_grace_secs, 7200);
        assert_eq!(config.fuse.cache_max_mb, 20480);
        assert!(config.crypto.enabled);
        assert_eq!(config.crypto.argon2_mem_cost_kib, 131072);
        assert!(config.config_file_mode_check);
    }

    #[test]
    fn test_parse_defaults() {
        let config: TcfsConfig = toml::from_str("").unwrap();

        assert!(
            config
                .daemon
                .socket
                .to_string_lossy()
                .ends_with("tcfsd/tcfsd.sock"),
            "socket path should end with tcfsd/tcfsd.sock, got: {}",
            config.daemon.socket.display()
        );
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.storage.endpoint, "http://localhost:8333");
        assert!(config.storage.enforce_tls);
        assert_eq!(config.storage.bucket, "tcfs");
        assert_eq!(config.storage.max_concurrent_ops, 0);
        assert_eq!(config.storage.s3_connect_timeout_secs, 0);
        assert_eq!(config.storage.s3_pool_idle_timeout_secs, 0);
        assert_eq!(config.storage.s3_pool_max_idle_per_host, 0);
        assert!(!config.storage.s3_http1_only);
        assert_eq!(config.sync.nats_url, "nats://localhost:4222");
        assert!(config.sync.nats_tls);
        assert_eq!(config.sync.orphan_chunk_cleanup_grace_secs, 24 * 3600);
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
        assert!(
            config.storage.enforce_tls,
            "an HTTP endpoint must not silently disable the TLS default"
        );
        // Defaults
        assert_eq!(config.storage.region, "us-east-1");
        assert_eq!(config.storage.bucket, "tcfs");
        assert_eq!(config.daemon.log_level, "info");
    }

    #[test]
    fn registered_root_config_parses_by_stable_id() {
        let toml_str = r#"
[sync.roots.git-roam-tool-daemon]
local_root = "/home/jess/git/tinyland-tool-daemon"
remote_prefix = "git-roam/tool-daemon"
state_path = "/home/jess/.local/state/tcfsd/reconcile/git-roam-tool-daemon.json"
policy = "resolve"
"#;
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        let root = config
            .sync
            .roots
            .get("git-roam-tool-daemon")
            .expect("registered root");

        assert_eq!(
            root.local_root,
            PathBuf::from("/home/jess/git/tinyland-tool-daemon")
        );
        assert_eq!(root.remote_prefix, "git-roam/tool-daemon");
        assert_eq!(
            root.state_path,
            PathBuf::from("/home/jess/.local/state/tcfsd/reconcile/git-roam-tool-daemon.json")
        );
        assert_eq!(root.policy, RegisteredRootPolicy::Resolve);
        assert_eq!(
            config.storage.enforce_tls,
            TcfsConfig::default().storage.enforce_tls,
            "registered roots inherit the global storage TLS posture; enrollment must not change its default"
        );
    }

    #[test]
    fn registered_root_policy_defaults_to_inspect_only() {
        let config: TcfsConfig = toml::from_str(
            r#"
[sync.roots.docs]
local_root = "/srv/docs"
remote_prefix = "docs"
state_path = "/run/tcfsd/reconcile/docs.json"
"#,
        )
        .unwrap();

        assert_eq!(
            config.sync.roots["docs"].policy,
            RegisteredRootPolicy::InspectOnly
        );
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

    #[test]
    fn auth_require_session_defaults_to_true() {
        let config = AuthConfig::default();
        assert!(
            config.require_session,
            "require_session must default to true for security"
        );
    }

    #[test]
    fn nats_tls_defaults_to_true() {
        let config = SyncConfig::default();
        assert!(
            config.nats_tls,
            "nats_tls must default to true for security"
        );
    }

    #[test]
    fn storage_plaintext_http_requires_explicit_opt_in() {
        let toml_str = r#"
[storage]
endpoint = "http://localhost:8333"
enforce_tls = false
"#;
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.storage.enforce_tls);
    }

    #[test]
    fn auth_bypass_from_toml() {
        let toml_str = r#"
[auth]
require_session = false
"#;
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.auth.require_session);
    }

    #[test]
    fn auth_defaults_when_omitted() {
        let config: TcfsConfig = toml::from_str("").unwrap();
        assert!(config.auth.require_session);
    }

    // ── TIN-1417: crypto.wrap_mode tri-state + legacy back-compat ──────────

    #[test]
    fn wrap_mode_defaults_to_master() {
        let config: TcfsConfig = toml::from_str("").unwrap();
        assert_eq!(
            config.crypto.wrap_mode,
            WrapMode::Master,
            "wrap_mode must default to Master (byte-identical to legacy default)"
        );
        // Empty [crypto] section must also default to Master.
        let config: TcfsConfig = toml::from_str("[crypto]\n").unwrap();
        assert_eq!(config.crypto.wrap_mode, WrapMode::Master);
    }

    #[test]
    fn wrap_mode_explicit_values_parse() {
        for (s, expected) in [
            ("master", WrapMode::Master),
            ("dual", WrapMode::Dual),
            ("per_device", WrapMode::PerDevice),
        ] {
            let toml_str = format!("[crypto]\nwrap_mode = \"{s}\"\n");
            let config: TcfsConfig = toml::from_str(&toml_str).unwrap();
            assert_eq!(config.crypto.wrap_mode, expected, "wrap_mode = {s}");
        }
    }

    #[test]
    fn legacy_per_device_wrapping_true_maps_to_dual() {
        // true -> Dual keeps the master fallback (safe transitional mode).
        let toml_str = "[crypto]\nper_device_wrapping = true\n";
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.crypto.wrap_mode,
            WrapMode::Dual,
            "legacy per_device_wrapping = true must map to Dual (keeps master fallback)"
        );
    }

    #[test]
    fn legacy_per_device_wrapping_false_maps_to_master() {
        let toml_str = "[crypto]\nper_device_wrapping = false\n";
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.crypto.wrap_mode, WrapMode::Master);
    }

    #[test]
    fn explicit_wrap_mode_wins_over_legacy_key() {
        // When both keys are present, wrap_mode is canonical and wins.
        let toml_str = "[crypto]\nwrap_mode = \"per_device\"\nper_device_wrapping = false\n";
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.crypto.wrap_mode,
            WrapMode::PerDevice,
            "explicit wrap_mode must win over a conflicting legacy per_device_wrapping"
        );

        let toml_str = "[crypto]\nwrap_mode = \"master\"\nper_device_wrapping = true\n";
        let config: TcfsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.crypto.wrap_mode, WrapMode::Master);
    }

    #[test]
    fn wrap_mode_serializes_canonically() {
        // Going forward only wrap_mode is emitted (snake_case); never the legacy key.
        let config = CryptoConfig {
            wrap_mode: WrapMode::PerDevice,
            ..Default::default()
        };
        let toml_str = toml::to_string(&config).unwrap();
        assert!(
            toml_str.contains("wrap_mode = \"per_device\""),
            "serialized config must emit canonical wrap_mode: {toml_str}"
        );
        assert!(
            !toml_str.contains("per_device_wrapping"),
            "serialized config must not emit the legacy key: {toml_str}"
        );
    }

    #[test]
    fn sensitive_path_containment_is_lexically_normalized_for_missing_tails() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sync-root");
        std::fs::create_dir(&root).unwrap();

        let missing_key = root.join("secrets/../keys/custom-vault.bin");
        assert!(path_is_within(&missing_key, &root).unwrap());
        assert!(path_is_within(&root, &root).unwrap());
        assert!(!path_is_within(&temp.path().join("sync-root-sibling/key"), &root).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_path_containment_resolves_existing_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real_root = temp.path().join("real-root");
        std::fs::create_dir(&real_root).unwrap();
        let real_key = real_root.join("custom-vault.bin");
        std::fs::write(&real_key, b"secret").unwrap();

        let root_alias = temp.path().join("root-alias");
        symlink(&real_root, &root_alias).unwrap();
        assert!(path_is_within(&real_key, &root_alias).unwrap());

        let key_alias = temp.path().join("key-alias");
        symlink(&real_key, &key_alias).unwrap();
        assert!(path_is_within(&key_alias, &real_root).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_path_containment_rejects_dangling_symlink_ambiguity() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let dangling = temp.path().join("dangling-key");
        symlink(temp.path().join("missing-key"), &dangling).unwrap();

        let error = path_is_within(&dangling, &root)
            .expect_err("an unresolved sensitive-path symlink must fail closed");
        assert!(error.contains("unresolved symlink"), "{error}");
    }

    #[test]
    fn custom_master_key_is_rejected_in_primary_and_registered_roots() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let named = temp.path().join("named");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&named).unwrap();

        let mut config = TcfsConfig::default();
        config.sync.sync_root = Some(primary.clone());
        let primary_key = primary.join("private/custom-key-material.bin");
        let error = validate_master_key_outside_sync_roots(&config, &primary_key)
            .expect_err("custom key inside primary root must be rejected");
        assert!(error.contains("primary sync.sync_root"), "{error}");

        config.sync.sync_root = Some(temp.path().join("other-primary"));
        config.sync.roots.insert(
            "named".into(),
            RegisteredRootConfig {
                local_root: named.clone(),
                remote_prefix: "roots/named".into(),
                state_path: temp.path().join("reconcile/named.json"),
                policy: RegisteredRootPolicy::InspectOnly,
            },
        );
        let named_key = named.join("custom-key-material.bin");
        let error = validate_master_key_outside_sync_roots(&config, &named_key)
            .expect_err("custom key inside named root must be rejected");
        assert!(error.contains("registered root 'named'"), "{error}");
    }

    #[test]
    fn explicit_sync_selection_rejects_direct_key_and_containing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let selected = temp.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let key = selected.join("custom-key-material.bin");
        std::fs::write(&key, b"secret").unwrap();

        let mut config = TcfsConfig::default();
        config.crypto.master_key_file = Some(key.clone());
        assert!(validate_sync_selection_excludes_master_key(&config, &key).is_err());
        assert!(validate_sync_selection_excludes_master_key(&config, &selected).is_err());
        assert!(validate_sync_selection_excludes_master_key(
            &config,
            &temp.path().join("unrelated")
        )
        .is_ok());
    }
}
