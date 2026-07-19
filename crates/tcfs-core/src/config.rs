use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use url::{Host, Url};

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

/// Configuration view safe for operator and diagnostic display surfaces.
///
/// This deliberately does not implement `Deserialize` and does not replace
/// [`TcfsConfig`]'s runtime/persistence serialization. The current config
/// schema has one inline credential, `sync.nats_token`. Endpoint outputs are
/// origin-only (scheme, normalized host/IP, and parsed non-default port) and
/// strip userinfo, path, query, and fragment components. Credential/key paths
/// and the KDF salt are configuration metadata rather than secret contents.
/// Keep each display section allowlisted so future runtime fields do not enter
/// a diagnostic serializer by default.
#[derive(Debug, Serialize)]
pub struct RedactedConfig<'a> {
    daemon: RedactedDaemonConfig<'a>,
    storage: RedactedStorageConfig<'a>,
    secrets: RedactedSecretsConfig<'a>,
    sync: RedactedSyncConfig<'a>,
    fuse: RedactedFuseConfig<'a>,
    crypto: RedactedCryptoConfig<'a>,
    sops: RedactedSopsConfig<'a>,
    auth: RedactedAuthConfig<'a>,
    config_file_mode_check: bool,
}

#[derive(Debug, Serialize)]
struct RedactedDaemonConfig<'a> {
    socket: &'a PathBuf,
    fileprovider_socket: &'a Option<PathBuf>,
    fileprovider_endpoint: Option<String>,
    listen: &'a Option<String>,
    metrics_addr: &'a Option<String>,
    log_level: &'a str,
    log_format: &'a str,
}

#[derive(Debug, Serialize)]
struct RedactedStorageConfig<'a> {
    endpoint: String,
    region: &'a str,
    bucket: &'a str,
    remote_prefix: &'a Option<String>,
    credentials_file: &'a Option<PathBuf>,
    enforce_tls: bool,
    ca_cert_path: &'a Option<PathBuf>,
    max_concurrent_ops: usize,
    s3_connect_timeout_secs: u64,
    s3_pool_idle_timeout_secs: u64,
    s3_pool_max_idle_per_host: usize,
    s3_http1_only: bool,
    max_upload_bytes_per_sec: u64,
    max_download_bytes_per_sec: u64,
}

#[derive(Debug, Serialize)]
struct RedactedSecretsConfig<'a> {
    age_identity: &'a Option<PathBuf>,
    kdbx_path: &'a Option<PathBuf>,
    sops_dir: &'a Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct RedactedSyncConfig<'a> {
    nats_url: String,
    nats_tls: bool,
    nats_token_configured: bool,
    nats_ca_cert: &'a Option<PathBuf>,
    state_db: &'a PathBuf,
    workers: usize,
    max_retries: u32,
    device_identity: &'a Option<PathBuf>,
    device_name: &'a Option<String>,
    conflict_mode: &'a str,
    sync_git_dirs: bool,
    git_sync_mode: &'a str,
    sync_hidden_dirs: bool,
    exclude_patterns: &'a [String],
    sync_symlinks: bool,
    sync_empty_dirs: bool,
    sync_root: &'a Option<PathBuf>,
    auto_unsync_max_age_secs: u64,
    auto_unsync_interval_secs: u64,
    auto_unsync_dry_run: bool,
    auto_unsync_disk_pressure_pct: f64,
    auto_unsync_max_per_sweep: usize,
    auto_download_threshold: u64,
    trash_enabled: bool,
    trash_retention_secs: u64,
    reconcile_interval_secs: u64,
    orphan_chunk_cleanup_grace_secs: u64,
    roots: BTreeMap<&'a str, RedactedRegisteredRootConfig<'a>>,
    root_registry: BTreeMap<&'a str, RedactedRegisteredRootV1Config<'a>>,
    root_state_dir: &'a Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct RedactedRegisteredRootConfig<'a> {
    local_root: &'a PathBuf,
    remote_prefix: &'a str,
    state_path: &'a PathBuf,
    policy: &'a RegisteredRootPolicy,
}

#[derive(Debug, Serialize)]
struct RedactedRegisteredRootV1Config<'a> {
    spec: RedactedRootSpecV1Config<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    binding: Option<RedactedRootBindingV1Config<'a>>,
}

#[derive(Debug, Serialize)]
struct RedactedRootSpecV1Config<'a> {
    version: u32,
    remote_prefix: &'a str,
    profile: &'a RootProfileV1,
    generation: NonZeroU64,
}

#[derive(Debug, Serialize)]
struct RedactedRootBindingV1Config<'a> {
    version: u32,
    local_root: &'a PathBuf,
    state_path: &'a PathBuf,
    lifecycle_policy: &'a RootLifecyclePolicyV1,
    resolution_policy: &'a RegisteredRootPolicy,
}

#[derive(Debug, Serialize)]
struct RedactedFuseConfig<'a> {
    negative_cache_ttl_secs: u64,
    cache_dir: &'a PathBuf,
    cache_max_mb: u64,
}

#[derive(Debug, Serialize)]
struct RedactedCryptoConfig<'a> {
    enabled: bool,
    argon2_mem_cost_kib: u32,
    argon2_time_cost: u32,
    argon2_parallelism: u32,
    master_key_file: &'a Option<PathBuf>,
    device_identity: &'a Option<PathBuf>,
    passphrase_file: &'a Option<PathBuf>,
    kdf_salt: &'a Option<String>,
    wrap_mode: &'a WrapMode,
}

#[derive(Debug, Serialize)]
struct RedactedSopsConfig<'a> {
    enabled: bool,
    sops_dir: &'a PathBuf,
    sync_prefix: &'a str,
    machine_id: &'a Option<String>,
    backup_dir: &'a PathBuf,
    watch: bool,
}

#[derive(Debug, Serialize)]
struct RedactedAuthConfig<'a> {
    enabled: bool,
    require_session: bool,
    session_expiry_hours: u64,
    methods: &'a [String],
    totp: RedactedAuthTotpConfig<'a>,
    webauthn: RedactedAuthWebAuthnConfig<'a>,
    enrollment: RedactedAuthEnrollmentConfig,
    rate_limit: RedactedAuthRateLimitConfig,
}

#[derive(Debug, Serialize)]
struct RedactedAuthTotpConfig<'a> {
    issuer: &'a str,
    digits: u32,
}

#[derive(Debug, Serialize)]
struct RedactedAuthWebAuthnConfig<'a> {
    relying_party_id: &'a str,
    relying_party_name: &'a str,
}

#[derive(Debug, Serialize)]
struct RedactedAuthEnrollmentConfig {
    qr_code: bool,
    auto_discovery: bool,
}

#[derive(Debug, Serialize)]
struct RedactedAuthRateLimitConfig {
    max_attempts: u32,
    lockout_secs: u64,
    backoff_multiplier: u32,
}

const REDACTED_INVALID_ENDPOINT: &str = "<invalid-or-non-base-url:redacted>";

/// Return an HTTP(S) endpoint safe for terminal, log, status, and diagnostic output.
///
/// The result contains only the parsed scheme, normalized host/IP, and
/// non-default port. Invalid or unsupported values become a constant so raw
/// credential-bearing input is never echoed while reporting an error.
pub fn sanitize_http_endpoint_for_display(raw: &str) -> String {
    sanitize_endpoint_for_display(raw, &["http", "https"])
}

/// Return the origin of an absolute HTTP(S) endpoint for functional metadata.
///
/// Unlike [`sanitize_http_endpoint_for_display`], invalid values return
/// `None` instead of a display placeholder. Callers can therefore fail closed
/// before signing or serializing routing metadata that must remain usable.
pub fn http_endpoint_origin(raw: &str) -> Option<String> {
    endpoint_origin(raw, &["http", "https"])
}

/// Return a NATS endpoint safe for terminal, log, status, and diagnostic output.
///
/// The result contains only the parsed scheme, normalized host/IP, and
/// non-default port. Invalid or unsupported values become a constant so raw
/// credential-bearing input is never echoed while reporting an error.
pub fn sanitize_nats_endpoint_for_display(raw: &str) -> String {
    sanitize_endpoint_for_display(raw, &["nats", "tls"])
}

/// Preserve only a URL's scheme, normalized host/IP, and parsed non-default port.
///
/// Userinfo, path, query, and fragment components are intentionally omitted.
/// Unparseable, hostless, opaque, relative, and unsupported-scheme values are
/// represented by a constant because echoing any raw component would recreate
/// the credential leak this display contract prevents.
fn sanitize_endpoint_for_display(raw: &str, allowed_schemes: &[&str]) -> String {
    if raw.is_empty() {
        return String::new();
    }

    endpoint_origin(raw, allowed_schemes).unwrap_or_else(|| REDACTED_INVALID_ENDPOINT.to_owned())
}

fn endpoint_origin(raw: &str, allowed_schemes: &[&str]) -> Option<String> {
    let Ok(endpoint) = Url::parse(raw) else {
        return None;
    };
    if endpoint.cannot_be_a_base() || !allowed_schemes.contains(&endpoint.scheme()) {
        return None;
    }

    let host = match endpoint.host() {
        Some(Host::Domain(host)) => host.to_owned(),
        Some(Host::Ipv4(host)) => host.to_string(),
        Some(Host::Ipv6(host)) => format!("[{host}]"),
        None => return None,
    };
    let port = endpoint
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!("{}://{host}{port}", endpoint.scheme()))
}

impl TcfsConfig {
    /// Borrow a serialization-only view that omits inline credentials.
    pub fn redacted(&self) -> RedactedConfig<'_> {
        // These patterns are intentionally exhaustive: adding a runtime config
        // field must force an explicit decision about diagnostic display.
        let TcfsConfig {
            daemon,
            storage,
            secrets,
            sync,
            fuse,
            crypto,
            sops,
            auth,
            config_file_mode_check,
        } = self;

        let DaemonConfig {
            socket,
            fileprovider_socket,
            fileprovider_endpoint,
            listen,
            metrics_addr,
            log_level,
            log_format,
        } = daemon;
        let StorageConfig {
            endpoint,
            region,
            bucket,
            remote_prefix,
            credentials_file,
            enforce_tls,
            ca_cert_path,
            max_concurrent_ops,
            s3_connect_timeout_secs,
            s3_pool_idle_timeout_secs,
            s3_pool_max_idle_per_host,
            s3_http1_only,
            max_upload_bytes_per_sec,
            max_download_bytes_per_sec,
        } = storage;
        let SecretsConfig {
            age_identity,
            kdbx_path,
            sops_dir: secrets_sops_dir,
        } = secrets;
        let SyncConfig {
            nats_url,
            nats_tls,
            nats_token,
            nats_ca_cert,
            state_db,
            workers,
            max_retries,
            device_identity: sync_device_identity,
            device_name,
            conflict_mode,
            sync_git_dirs,
            git_sync_mode,
            sync_hidden_dirs,
            exclude_patterns,
            sync_symlinks,
            sync_empty_dirs,
            sync_root,
            auto_unsync_max_age_secs,
            auto_unsync_interval_secs,
            auto_unsync_dry_run,
            auto_unsync_disk_pressure_pct,
            auto_unsync_max_per_sweep,
            auto_download_threshold,
            trash_enabled,
            trash_retention_secs,
            reconcile_interval_secs,
            orphan_chunk_cleanup_grace_secs,
            roots,
            root_registry,
            root_state_dir,
        } = sync;
        let FuseConfig {
            negative_cache_ttl_secs,
            cache_dir,
            cache_max_mb,
        } = fuse;
        let CryptoConfig {
            enabled: crypto_enabled,
            argon2_mem_cost_kib,
            argon2_time_cost,
            argon2_parallelism,
            master_key_file,
            device_identity: crypto_device_identity,
            passphrase_file,
            kdf_salt,
            wrap_mode,
        } = crypto;
        let SopsConfig {
            enabled: sops_enabled,
            sops_dir,
            sync_prefix,
            machine_id,
            backup_dir,
            watch,
        } = sops;
        let AuthConfig {
            enabled: auth_enabled,
            require_session,
            session_expiry_hours,
            methods,
            totp,
            webauthn,
            enrollment,
            rate_limit,
        } = auth;
        let AuthTotpConfig { issuer, digits } = totp;
        let AuthWebAuthnConfig {
            relying_party_id,
            relying_party_name,
        } = webauthn;
        let AuthEnrollmentConfig {
            qr_code,
            auto_discovery,
        } = enrollment;
        let AuthRateLimitConfig {
            max_attempts,
            lockout_secs,
            backoff_multiplier,
        } = rate_limit;
        let redacted_roots = roots
            .iter()
            .map(|(root_id, root)| {
                let RegisteredRootConfig {
                    local_root,
                    remote_prefix,
                    state_path,
                    policy,
                } = root;
                (
                    root_id.as_str(),
                    RedactedRegisteredRootConfig {
                        local_root,
                        remote_prefix,
                        state_path,
                        policy,
                    },
                )
            })
            .collect();
        let redacted_root_registry = root_registry
            .iter()
            .map(|(root_id, root)| {
                let RegisteredRootV1Config { spec, binding } = root;
                let RootSpecV1Config {
                    version,
                    remote_prefix,
                    profile,
                    generation,
                } = spec;
                let binding = binding.as_ref().map(|binding| {
                    let RootBindingV1Config {
                        version,
                        local_root,
                        state_path,
                        lifecycle_policy,
                        resolution_policy,
                    } = binding;
                    RedactedRootBindingV1Config {
                        version: *version,
                        local_root,
                        state_path,
                        lifecycle_policy,
                        resolution_policy,
                    }
                });
                (
                    root_id.as_str(),
                    RedactedRegisteredRootV1Config {
                        spec: RedactedRootSpecV1Config {
                            version: *version,
                            remote_prefix,
                            profile,
                            generation: *generation,
                        },
                        binding,
                    },
                )
            })
            .collect();

        RedactedConfig {
            daemon: RedactedDaemonConfig {
                socket,
                fileprovider_socket,
                fileprovider_endpoint: fileprovider_endpoint
                    .as_deref()
                    .map(sanitize_http_endpoint_for_display),
                listen,
                metrics_addr,
                log_level,
                log_format,
            },
            storage: RedactedStorageConfig {
                endpoint: sanitize_http_endpoint_for_display(endpoint),
                region,
                bucket,
                remote_prefix,
                credentials_file,
                enforce_tls: *enforce_tls,
                ca_cert_path,
                max_concurrent_ops: *max_concurrent_ops,
                s3_connect_timeout_secs: *s3_connect_timeout_secs,
                s3_pool_idle_timeout_secs: *s3_pool_idle_timeout_secs,
                s3_pool_max_idle_per_host: *s3_pool_max_idle_per_host,
                s3_http1_only: *s3_http1_only,
                max_upload_bytes_per_sec: *max_upload_bytes_per_sec,
                max_download_bytes_per_sec: *max_download_bytes_per_sec,
            },
            secrets: RedactedSecretsConfig {
                age_identity,
                kdbx_path,
                sops_dir: secrets_sops_dir,
            },
            sync: RedactedSyncConfig {
                nats_url: sanitize_nats_endpoint_for_display(nats_url),
                nats_tls: *nats_tls,
                nats_token_configured: nats_token.is_some(),
                nats_ca_cert,
                state_db,
                workers: *workers,
                max_retries: *max_retries,
                device_identity: sync_device_identity,
                device_name,
                conflict_mode,
                sync_git_dirs: *sync_git_dirs,
                git_sync_mode,
                sync_hidden_dirs: *sync_hidden_dirs,
                exclude_patterns,
                sync_symlinks: *sync_symlinks,
                sync_empty_dirs: *sync_empty_dirs,
                sync_root,
                auto_unsync_max_age_secs: *auto_unsync_max_age_secs,
                auto_unsync_interval_secs: *auto_unsync_interval_secs,
                auto_unsync_dry_run: *auto_unsync_dry_run,
                auto_unsync_disk_pressure_pct: *auto_unsync_disk_pressure_pct,
                auto_unsync_max_per_sweep: *auto_unsync_max_per_sweep,
                auto_download_threshold: *auto_download_threshold,
                trash_enabled: *trash_enabled,
                trash_retention_secs: *trash_retention_secs,
                reconcile_interval_secs: *reconcile_interval_secs,
                orphan_chunk_cleanup_grace_secs: *orphan_chunk_cleanup_grace_secs,
                roots: redacted_roots,
                root_registry: redacted_root_registry,
                root_state_dir,
            },
            fuse: RedactedFuseConfig {
                negative_cache_ttl_secs: *negative_cache_ttl_secs,
                cache_dir,
                cache_max_mb: *cache_max_mb,
            },
            crypto: RedactedCryptoConfig {
                enabled: *crypto_enabled,
                argon2_mem_cost_kib: *argon2_mem_cost_kib,
                argon2_time_cost: *argon2_time_cost,
                argon2_parallelism: *argon2_parallelism,
                master_key_file,
                device_identity: crypto_device_identity,
                passphrase_file,
                kdf_salt,
                wrap_mode,
            },
            sops: RedactedSopsConfig {
                enabled: *sops_enabled,
                sops_dir,
                sync_prefix,
                machine_id,
                backup_dir,
                watch: *watch,
            },
            auth: RedactedAuthConfig {
                enabled: *auth_enabled,
                require_session: *require_session,
                session_expiry_hours: *session_expiry_hours,
                methods,
                totp: RedactedAuthTotpConfig {
                    issuer,
                    digits: *digits,
                },
                webauthn: RedactedAuthWebAuthnConfig {
                    relying_party_id,
                    relying_party_name,
                },
                enrollment: RedactedAuthEnrollmentConfig {
                    qr_code: *qr_code,
                    auto_discovery: *auto_discovery,
                },
                rate_limit: RedactedAuthRateLimitConfig {
                    max_attempts: *max_attempts,
                    lockout_secs: *lockout_secs,
                    backoff_multiplier: *backoff_multiplier,
                },
            },
            config_file_mode_check: *config_file_mode_check,
        }
    }
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
    /// Versioned read-only root inventory.
    ///
    /// This registry is deliberately separate from `roots`, the unversioned
    /// PR #551 conflict-only routing seam. Entries here do not inherit legacy
    /// conflict resolution or any lifecycle mutation authority. TIN-2863
    /// exposes only authorized list/status over these descriptors.
    #[serde(default)]
    pub root_registry: BTreeMap<String, RegisteredRootV1Config>,
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

impl RegisteredRootPolicy {
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::InspectOnly => "inspect-only",
            Self::Resolve => "resolve",
        }
    }
}

/// One strict, versioned root descriptor for read-only inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredRootV1Config {
    pub spec: RootSpecV1Config,
    #[serde(default)]
    pub binding: Option<RootBindingV1Config>,
}

/// Fleet-stable portion of a versioned root identity.
///
/// The containing `sync.root_registry` map key is the authoritative `root_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootSpecV1Config {
    pub version: u32,
    pub remote_prefix: String,
    pub profile: RootProfileV1,
    pub generation: NonZeroU64,
}

/// Host-local binding for one versioned root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootBindingV1Config {
    pub version: u32,
    pub local_root: PathBuf,
    pub state_path: PathBuf,
    pub lifecycle_policy: RootLifecyclePolicyV1,
    pub resolution_policy: RegisteredRootPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootProfileV1 {
    GitRawV1,
    AgentStaticV1,
}

impl RootProfileV1 {
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::GitRawV1 => "git-raw-v1",
            Self::AgentStaticV1 => "agent-static-v1",
        }
    }

    /// Immutable planning policy carried by this profile version.
    ///
    /// Registered-root planning must not inherit mutable primary-root
    /// collection or deletion settings. A profile name therefore expands to
    /// one closed set of settings whose fingerprint is bound into every plan.
    pub fn settings(self) -> RootProfileSettingsV1 {
        match self {
            Self::GitRawV1 => RootProfileSettingsV1 {
                sync_hidden_dirs: true,
                sync_git_dirs: true,
                git_sync_mode: "raw",
                preserve_symlinks: true,
                sync_empty_directories: true,
                delete_local_orphans: false,
                delete_remote_orphans: false,
            },
            Self::AgentStaticV1 => RootProfileSettingsV1 {
                sync_hidden_dirs: true,
                sync_git_dirs: false,
                git_sync_mode: "none",
                preserve_symlinks: true,
                sync_empty_directories: true,
                delete_local_orphans: false,
                delete_remote_orphans: false,
            },
        }
    }

    pub fn settings_fingerprint(self) -> String {
        self.settings().fingerprint(self)
    }
}

/// Closed operational settings for one registered-root profile generation.
///
/// These are deliberately not deserialized from host configuration. Changing
/// any value requires a new profile version so a plan digest cannot retain the
/// same policy identity while silently changing collection or deletion
/// semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootProfileSettingsV1 {
    pub sync_hidden_dirs: bool,
    pub sync_git_dirs: bool,
    pub git_sync_mode: &'static str,
    pub preserve_symlinks: bool,
    pub sync_empty_directories: bool,
    pub delete_local_orphans: bool,
    pub delete_remote_orphans: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootLifecyclePolicyV1 {
    InspectOnly,
    Reconcile,
}

impl RootLifecyclePolicyV1 {
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::InspectOnly => "inspect-only",
            Self::Reconcile => "reconcile",
        }
    }
}

pub fn validate_registered_root_id(root_id: &str) -> Result<(), String> {
    let valid = !root_id.is_empty()
        && root_id.len() <= 64
        && !root_id.eq_ignore_ascii_case("primary")
        && root_id
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        && root_id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.')
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid registered root id '{root_id}': use 1-64 lowercase ASCII letters, digits, '.', '_' or '-' (reserved: primary)"
        ))
    }
}

pub fn validate_registered_remote_prefix(prefix: &str) -> Result<(), String> {
    let valid = !prefix.is_empty()
        && !prefix.starts_with('/')
        && !prefix.ends_with('/')
        && !prefix.contains('\\')
        && prefix
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid registered root remote_prefix '{prefix}': expected a non-empty relative object-key prefix without '.', '..', empty, or backslash segments"
        ))
    }
}

impl RegisteredRootV1Config {
    /// Validate the versioned descriptor without probing host filesystem state.
    pub fn validate_shape(&self, root_id: &str) -> Result<(), String> {
        validate_registered_root_id(root_id)?;
        if self.spec.version != RootSpecV1Config::VERSION {
            return Err(format!(
                "registered root '{root_id}' spec.version must be {}, got {}",
                RootSpecV1Config::VERSION,
                self.spec.version
            ));
        }
        validate_registered_remote_prefix(&self.spec.remote_prefix)?;

        if let Some(binding) = &self.binding {
            if binding.version != RootBindingV1Config::VERSION {
                return Err(format!(
                    "registered root '{root_id}' binding.version must be {}, got {}",
                    RootBindingV1Config::VERSION,
                    binding.version
                ));
            }
            for (field, path) in [
                ("local_root", &binding.local_root),
                ("state_path", &binding.state_path),
            ] {
                let path = expand_tilde(path);
                if !path.is_absolute()
                    || path
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir))
                {
                    return Err(format!(
                        "registered root '{root_id}' binding.{field} must be absolute without '..': {}",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }
}

fn update_root_fingerprint_field(hasher: &mut blake3::Hasher, tag: &str, value: &[u8]) {
    hasher.update(&(tag.len() as u32).to_be_bytes());
    hasher.update(tag.as_bytes());
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn finish_root_fingerprint(hasher: blake3::Hasher) -> String {
    format!("b3v1:{}", hasher.finalize().to_hex())
}

impl RootProfileSettingsV1 {
    fn fingerprint(self, profile: RootProfileV1) -> String {
        let mut hasher =
            blake3::Hasher::new_derive_key("tinyland.tcfs.root-profile-settings.b3v1");
        update_root_fingerprint_field(
            &mut hasher,
            "profile",
            profile.canonical_name().as_bytes(),
        );
        update_root_fingerprint_field(
            &mut hasher,
            "sync_hidden_dirs",
            &[u8::from(self.sync_hidden_dirs)],
        );
        update_root_fingerprint_field(
            &mut hasher,
            "sync_git_dirs",
            &[u8::from(self.sync_git_dirs)],
        );
        update_root_fingerprint_field(
            &mut hasher,
            "git_sync_mode",
            self.git_sync_mode.as_bytes(),
        );
        update_root_fingerprint_field(
            &mut hasher,
            "preserve_symlinks",
            &[u8::from(self.preserve_symlinks)],
        );
        update_root_fingerprint_field(
            &mut hasher,
            "sync_empty_directories",
            &[u8::from(self.sync_empty_directories)],
        );
        update_root_fingerprint_field(
            &mut hasher,
            "delete_local_orphans",
            &[u8::from(self.delete_local_orphans)],
        );
        update_root_fingerprint_field(
            &mut hasher,
            "delete_remote_orphans",
            &[u8::from(self.delete_remote_orphans)],
        );
        finish_root_fingerprint(hasher)
    }
}

impl RootSpecV1Config {
    pub const VERSION: u32 = 1;

    /// Stable identity over the exact validated fleet fields.
    pub fn identity_fingerprint(&self, root_id: &str) -> String {
        let mut hasher = blake3::Hasher::new_derive_key("tinyland.tcfs.root-spec.b3v1");
        update_root_fingerprint_field(&mut hasher, "version", &self.version.to_be_bytes());
        update_root_fingerprint_field(&mut hasher, "root_id", root_id.as_bytes());
        update_root_fingerprint_field(&mut hasher, "remote_prefix", self.remote_prefix.as_bytes());
        update_root_fingerprint_field(
            &mut hasher,
            "profile",
            self.profile.canonical_name().as_bytes(),
        );
        update_root_fingerprint_field(
            &mut hasher,
            "generation",
            &self.generation.get().to_be_bytes(),
        );
        finish_root_fingerprint(hasher)
    }
}

impl RootBindingV1Config {
    pub const VERSION: u32 = 1;

    /// Host-specific identity over runtime-canonical binding paths and policy.
    pub fn binding_fingerprint(
        &self,
        canonical_local_root: &Path,
        canonical_state_path: &Path,
    ) -> Result<String, String> {
        let canonical_local_root = canonical_local_root.to_str().ok_or_else(|| {
            format!(
                "canonical local_root is not valid UTF-8: {}",
                canonical_local_root.display()
            )
        })?;
        let canonical_state_path = canonical_state_path.to_str().ok_or_else(|| {
            format!(
                "canonical state_path is not valid UTF-8: {}",
                canonical_state_path.display()
            )
        })?;

        let mut hasher = blake3::Hasher::new_derive_key("tinyland.tcfs.root-binding.b3v1");
        update_root_fingerprint_field(&mut hasher, "version", &self.version.to_be_bytes());
        update_root_fingerprint_field(
            &mut hasher,
            "canonical_local_root",
            canonical_local_root.as_bytes(),
        );
        update_root_fingerprint_field(
            &mut hasher,
            "canonical_state_path",
            canonical_state_path.as_bytes(),
        );
        update_root_fingerprint_field(
            &mut hasher,
            "lifecycle_policy",
            self.lifecycle_policy.canonical_name().as_bytes(),
        );
        update_root_fingerprint_field(
            &mut hasher,
            "resolution_policy",
            self.resolution_policy.canonical_name().as_bytes(),
        );
        Ok(finish_root_fingerprint(hasher))
    }
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
            root_registry: BTreeMap::new(),
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
    for (root_id, root) in &config.sync.root_registry {
        let Some(binding) = root.binding.as_ref() else {
            continue;
        };
        if path_is_within(master_key_path, &binding.local_root)? {
            return Err(format!(
                "master key path {} is inside versioned registered root '{root_id}' local_root {}",
                expand_tilde(master_key_path).display(),
                expand_tilde(&binding.local_root).display()
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
        assert!(
            config.sync.root_registry.is_empty(),
            "legacy roots must not be reinterpreted as versioned inventory"
        );
    }

    #[test]
    fn versioned_root_registry_parses_strict_spec_and_optional_binding() {
        let config: TcfsConfig = toml::from_str(
            r#"
[sync.root_registry.work.spec]
version = 1
remote_prefix = "roots/work"
profile = "git-raw-v1"
generation = 7

[sync.root_registry.work.binding]
version = 1
local_root = "/srv/work"
state_path = "/var/lib/tcfs/reconcile/work.json"
lifecycle_policy = "inspect-only"
resolution_policy = "inspect-only"

[sync.root_registry.unbound.spec]
version = 1
remote_prefix = "roots/unbound"
profile = "agent-static-v1"
generation = 1
"#,
        )
        .unwrap();

        let work = &config.sync.root_registry["work"];
        work.validate_shape("work").unwrap();
        assert_eq!(work.spec.profile, RootProfileV1::GitRawV1);
        assert_eq!(work.spec.generation.get(), 7);
        assert_eq!(
            work.binding.as_ref().unwrap().lifecycle_policy,
            RootLifecyclePolicyV1::InspectOnly
        );
        assert!(config.sync.root_registry["unbound"].binding.is_none());
        assert!(
            config.sync.roots.is_empty(),
            "versioned inventory must not gain legacy conflict authority"
        );
    }

    #[test]
    fn versioned_root_registry_rejects_invalid_generation_profile_version_and_fields() {
        for invalid in [
            r#"
[sync.root_registry.work.spec]
version = 1
remote_prefix = "roots/work"
profile = "git-raw-v1"
generation = 0
"#,
            r#"
[sync.root_registry.work.spec]
version = 1
remote_prefix = "roots/work"
profile = "unknown-v1"
generation = 1
"#,
            r#"
[sync.root_registry.work.spec]
version = 1
remote_prefix = "roots/work"
profile = "git-raw-v1"
generation = 1
unexpected = "rejected"
"#,
        ] {
            assert!(
                toml::from_str::<TcfsConfig>(invalid).is_err(),
                "invalid V1 descriptor unexpectedly parsed: {invalid}"
            );
        }

        let wrong_version: TcfsConfig = toml::from_str(
            r#"
[sync.root_registry.work.spec]
version = 2
remote_prefix = "roots/work"
profile = "git-raw-v1"
generation = 1
"#,
        )
        .unwrap();
        assert!(wrong_version.sync.root_registry["work"]
            .validate_shape("work")
            .unwrap_err()
            .contains("spec.version must be 1"));

        let wrong_binding_version: TcfsConfig = toml::from_str(
            r#"
[sync.root_registry.work.spec]
version = 1
remote_prefix = "roots/work"
profile = "git-raw-v1"
generation = 1

[sync.root_registry.work.binding]
version = 2
local_root = "/srv/work"
state_path = "/var/lib/tcfs/reconcile/work.json"
lifecycle_policy = "inspect-only"
resolution_policy = "inspect-only"
"#,
        )
        .unwrap();
        assert!(wrong_binding_version.sync.root_registry["work"]
            .validate_shape("work")
            .unwrap_err()
            .contains("binding.version must be 1"));
    }

    #[test]
    fn versioned_root_fingerprints_separate_fleet_spec_from_host_binding() {
        let spec = RootSpecV1Config {
            version: 1,
            remote_prefix: "roots/work".into(),
            profile: RootProfileV1::GitRawV1,
            generation: NonZeroU64::new(3).unwrap(),
        };
        let identity = spec.identity_fingerprint("work");
        assert!(identity.starts_with("b3v1:"));
        assert_eq!(identity.len(), "b3v1:".len() + 64);
        assert_eq!(identity, spec.identity_fingerprint("work"));

        let binding = RootBindingV1Config {
            version: 1,
            local_root: PathBuf::from("/unused/configured/path"),
            state_path: PathBuf::from("/unused/configured/state.json"),
            lifecycle_policy: RootLifecyclePolicyV1::InspectOnly,
            resolution_policy: RegisteredRootPolicy::InspectOnly,
        };
        let neo = binding
            .binding_fingerprint(
                Path::new("/Users/jess/git/work"),
                Path::new("/Users/jess/.local/state/tcfsd/reconcile/work.json"),
            )
            .unwrap();
        let sting = binding
            .binding_fingerprint(
                Path::new("/srv/fast-local/jess/git/work"),
                Path::new("/srv/fast-local/jess/state/tcfsd/reconcile/work.json"),
            )
            .unwrap();
        let local_path_only = binding
            .binding_fingerprint(
                Path::new("/srv/fast-local/jess/git/work"),
                Path::new("/Users/jess/.local/state/tcfsd/reconcile/work.json"),
            )
            .unwrap();
        let state_path_only = binding
            .binding_fingerprint(
                Path::new("/Users/jess/git/work"),
                Path::new("/srv/fast-local/jess/state/tcfsd/reconcile/work.json"),
            )
            .unwrap();

        assert_ne!(neo, sting);
        assert_ne!(neo, local_path_only);
        assert_ne!(neo, state_path_only);
        assert_ne!(local_path_only, state_path_only);
        assert_eq!(identity, spec.identity_fingerprint("work"));
        assert_ne!(identity, spec.identity_fingerprint("other-work"));
    }

    #[test]
    fn versioned_root_profile_settings_are_closed_and_digest_sensitive() {
        let git = RootProfileV1::GitRawV1.settings();
        let agent = RootProfileV1::AgentStaticV1.settings();

        assert!(git.sync_hidden_dirs);
        assert!(git.sync_git_dirs);
        assert_eq!(git.git_sync_mode, "raw");
        assert!(git.preserve_symlinks);
        assert!(git.sync_empty_directories);
        assert!(!git.delete_local_orphans);
        assert!(!git.delete_remote_orphans);

        assert!(agent.sync_hidden_dirs);
        assert!(!agent.sync_git_dirs);
        assert_eq!(agent.git_sync_mode, "none");
        assert!(agent.preserve_symlinks);
        assert!(agent.sync_empty_directories);
        assert!(!agent.delete_local_orphans);
        assert!(!agent.delete_remote_orphans);

        let fingerprint = RootProfileV1::GitRawV1.settings_fingerprint();
        assert!(fingerprint.starts_with("b3v1:"));
        assert_eq!(fingerprint.len(), "b3v1:".len() + 64);
        assert_eq!(
            fingerprint,
            RootProfileV1::GitRawV1.settings_fingerprint()
        );
        assert_ne!(
            fingerprint,
            RootProfileV1::AgentStaticV1.settings_fingerprint()
        );

        let mut changed = git;
        changed.sync_hidden_dirs = false;
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
        );
        let mut changed = git;
        changed.sync_git_dirs = false;
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
        );
        let mut changed = git;
        changed.git_sync_mode = "bundle";
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
        );
        let mut changed = git;
        changed.preserve_symlinks = false;
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
        );
        let mut changed = git;
        changed.sync_empty_directories = false;
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
        );
        let mut changed = git;
        changed.delete_local_orphans = true;
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
        );
        let mut changed = git;
        changed.delete_remote_orphans = true;
        assert_ne!(
            fingerprint,
            changed.fingerprint(RootProfileV1::GitRawV1)
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
    fn raw_config_roundtrip_retains_nats_token_but_redacted_view_omits_it() {
        const TOKEN: &str = "raw-roundtrip-token-sentinel";

        let mut config = TcfsConfig::default();
        config.sync.nats_token = Some(TOKEN.into());

        let raw = toml::to_string(&config).unwrap();
        assert!(raw.contains("nats_token"));
        assert!(raw.contains(TOKEN));

        let parsed: TcfsConfig = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.sync.nats_token.as_deref(), Some(TOKEN));

        let redacted = toml::to_string(&config.redacted()).unwrap();
        assert!(!redacted.contains(TOKEN));
        let redacted: toml::Value = toml::from_str(&redacted).unwrap();
        assert!(redacted["sync"].get("nats_token").is_none());
        assert_eq!(
            redacted["sync"]["nats_token_configured"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn redacted_view_explicitly_serializes_registered_root_metadata() {
        let mut config = TcfsConfig::default();
        config.sync.root_state_dir = Some(PathBuf::from("/var/lib/tcfs/reconcile"));
        config.sync.roots.insert(
            "work-root".into(),
            RegisteredRootConfig {
                local_root: PathBuf::from("/srv/work"),
                remote_prefix: "roots/work".into(),
                state_path: PathBuf::from("/var/lib/tcfs/reconcile/work.json"),
                policy: RegisteredRootPolicy::Resolve,
            },
        );
        config.sync.root_registry.insert(
            "agent-root".into(),
            RegisteredRootV1Config {
                spec: RootSpecV1Config {
                    version: 1,
                    remote_prefix: "roots/agent".into(),
                    profile: RootProfileV1::AgentStaticV1,
                    generation: NonZeroU64::new(2).unwrap(),
                },
                binding: Some(RootBindingV1Config {
                    version: 1,
                    local_root: PathBuf::from("/srv/agent"),
                    state_path: PathBuf::from("/var/lib/tcfs/reconcile/agent-root.json"),
                    lifecycle_policy: RootLifecyclePolicyV1::InspectOnly,
                    resolution_policy: RegisteredRootPolicy::InspectOnly,
                }),
            },
        );

        let toml_rendered = toml::to_string(&config.redacted()).unwrap();
        let toml_value: toml::Value = toml::from_str(&toml_rendered).unwrap();
        let root = &toml_value["sync"]["roots"]["work-root"];
        assert_eq!(root["local_root"].as_str(), Some("/srv/work"));
        assert_eq!(root["remote_prefix"].as_str(), Some("roots/work"));
        assert_eq!(
            root["state_path"].as_str(),
            Some("/var/lib/tcfs/reconcile/work.json")
        );
        assert_eq!(root["policy"].as_str(), Some("resolve"));
        assert_eq!(
            toml_value["sync"]["root_state_dir"].as_str(),
            Some("/var/lib/tcfs/reconcile")
        );
        let versioned = &toml_value["sync"]["root_registry"]["agent-root"];
        assert_eq!(
            versioned["spec"]["remote_prefix"].as_str(),
            Some("roots/agent")
        );
        assert_eq!(
            versioned["spec"]["profile"].as_str(),
            Some("agent-static-v1")
        );
        assert_eq!(versioned["spec"]["generation"].as_integer(), Some(2));
        assert_eq!(
            versioned["binding"]["lifecycle_policy"].as_str(),
            Some("inspect-only")
        );

        let json_value = serde_json::to_value(config.redacted()).unwrap();
        assert_eq!(
            json_value["sync"]["roots"]["work-root"]["remote_prefix"].as_str(),
            Some("roots/work")
        );
        assert_eq!(
            json_value["sync"]["roots"]["work-root"]["local_root"].as_str(),
            Some("/srv/work")
        );
        assert_eq!(
            json_value["sync"]["roots"]["work-root"]["state_path"].as_str(),
            Some("/var/lib/tcfs/reconcile/work.json")
        );
        assert_eq!(
            json_value["sync"]["roots"]["work-root"]["policy"].as_str(),
            Some("resolve")
        );
        assert_eq!(
            json_value["sync"]["root_registry"]["agent-root"]["binding"]["state_path"].as_str(),
            Some("/var/lib/tcfs/reconcile/agent-root.json")
        );
        assert_eq!(
            json_value["sync"]["root_state_dir"].as_str(),
            Some("/var/lib/tcfs/reconcile")
        );
    }

    #[test]
    fn redacted_view_sanitizes_url_credentials_and_unparseable_values() {
        let mut config = TcfsConfig::default();
        config.storage.endpoint =
            "https://s3-user:S3-secret@storage.example.test:8333/S3-path-secret?signature=S3-query#S3-fragment"
                .into();
        config.sync.nats_url =
            "nats://nats-user:NATS-secret@nats.example.test:4222/NATS-path-secret?token=NATS-query#NATS-fragment"
                .into();
        config.daemon.fileprovider_endpoint = Some(
            "https://fp-user:FP-secret@fp.example.test/FP-path-secret?token=FP-query#FP-fragment"
                .into(),
        );
        config.sync.nats_token = Some("inline-token-sentinel".into());

        let rendered = toml::to_string(&config.redacted()).unwrap();
        for forbidden in [
            "s3-user",
            "S3-secret",
            "S3-path-secret",
            "S3-query",
            "S3-fragment",
            "nats-user",
            "NATS-secret",
            "NATS-path-secret",
            "NATS-query",
            "NATS-fragment",
            "fp-user",
            "FP-secret",
            "FP-path-secret",
            "FP-query",
            "FP-fragment",
            "inline-token-sentinel",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "leaked {forbidden}: {rendered}"
            );
        }

        let value: toml::Value = toml::from_str(&rendered).unwrap();
        assert_eq!(
            value["storage"]["endpoint"].as_str(),
            Some("https://storage.example.test:8333")
        );
        assert_eq!(
            value["sync"]["nats_url"].as_str(),
            Some("nats://nats.example.test:4222")
        );
        assert_eq!(
            value["daemon"]["fileprovider_endpoint"].as_str(),
            Some("https://fp.example.test")
        );
        assert_eq!(value["sync"]["nats_token_configured"].as_bool(), Some(true));
        assert!(value["sync"].get("nats_token").is_none());

        config.storage.endpoint = "host-with-secret=S3-unparseable-sentinel".into();
        config.sync.nats_url = "host-with-secret=NATS-unparseable-sentinel".into();
        let rendered = toml::to_string(&config.redacted()).unwrap();
        assert!(!rendered.contains("S3-unparseable-sentinel"));
        assert!(!rendered.contains("NATS-unparseable-sentinel"));
        let value: toml::Value = toml::from_str(&rendered).unwrap();
        assert_eq!(
            value["storage"]["endpoint"].as_str(),
            Some(REDACTED_INVALID_ENDPOINT)
        );
        assert_eq!(
            value["sync"]["nats_url"].as_str(),
            Some(REDACTED_INVALID_ENDPOINT)
        );
    }

    #[test]
    fn endpoint_display_contract_is_origin_only_and_scheme_bounded() {
        assert_eq!(
            sanitize_http_endpoint_for_display(
                "https://user%40name:pass%3Aword@example.test:8333/%50%41%54%48_SECRET?sig=QUERY_SECRET#FRAGMENT_SECRET",
            ),
            "https://example.test:8333"
        );
        assert_eq!(
            sanitize_nats_endpoint_for_display(
                "nats://user:pass@nats.example.test:4222/PATH_SECRET?token=QUERY_SECRET",
            ),
            "nats://nats.example.test:4222"
        );
        assert_eq!(
            sanitize_http_endpoint_for_display("https://[2001:db8::1]:8333/private"),
            "https://[2001:db8::1]:8333"
        );
        assert_eq!(
            sanitize_http_endpoint_for_display("https://safe.example.test:8443/"),
            "https://safe.example.test:8443"
        );
        assert_eq!(
            sanitize_http_endpoint_for_display("/relative/PATH_SECRET?token=QUERY_SECRET"),
            REDACTED_INVALID_ENDPOINT
        );
        assert_eq!(
            sanitize_http_endpoint_for_display("mailto:OPAQUE_SECRET@example.test"),
            REDACTED_INVALID_ENDPOINT
        );
        assert_eq!(
            sanitize_nats_endpoint_for_display("nats:///MISSING_HOST_SECRET"),
            REDACTED_INVALID_ENDPOINT
        );
        assert_eq!(
            sanitize_http_endpoint_for_display("://MALFORMED_SECRET"),
            REDACTED_INVALID_ENDPOINT
        );
        assert_eq!(
            sanitize_http_endpoint_for_display("ftp://user:pass@example.test/DISALLOWED_SECRET",),
            REDACTED_INVALID_ENDPOINT
        );
        assert_eq!(
            http_endpoint_origin(
                "https://user:pass@example.test:8333/private?token=secret#fragment",
            ),
            Some("https://example.test:8333".to_string())
        );
        assert_eq!(http_endpoint_origin("not-an-endpoint"), None);
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

        config.sync.roots.clear();
        config.sync.root_registry.insert(
            "versioned".into(),
            RegisteredRootV1Config {
                spec: RootSpecV1Config {
                    version: 1,
                    remote_prefix: "roots/versioned".into(),
                    profile: RootProfileV1::AgentStaticV1,
                    generation: NonZeroU64::new(1).unwrap(),
                },
                binding: Some(RootBindingV1Config {
                    version: 1,
                    local_root: named.clone(),
                    state_path: temp.path().join("reconcile/versioned.json"),
                    lifecycle_policy: RootLifecyclePolicyV1::InspectOnly,
                    resolution_policy: RegisteredRootPolicy::InspectOnly,
                }),
            },
        );
        let error = validate_master_key_outside_sync_roots(&config, &named_key)
            .expect_err("custom key inside a bound V1 root must be rejected");
        assert!(
            error.contains("versioned registered root 'versioned'"),
            "{error}"
        );
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
