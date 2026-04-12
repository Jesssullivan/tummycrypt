//! tcfs: TummyCrypt filesystem CLI
//!
//! Phase 1 commands:
//!   status              - show daemon status (connects via gRPC Unix socket)
//!   config show         - display current configuration
//!   kdbx resolve <path> - resolve a credential from a KDBX database
//!
//! Phase 2 commands:
//!   push <local> [<prefix>]      - upload file or directory tree to SeaweedFS
//!   pull <manifest> [<local>]    - download file from manifest path
//!   sync-status [<path>]         - show local sync state for a file/dir

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use secrecy::ExposeSecret;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use tonic::transport::{Channel, Endpoint, Uri};
#[cfg(unix)]
use tower::service_fn;

#[cfg(unix)]
use tcfs_core::proto::{tcfs_daemon_client::TcfsDaemonClient, Empty, StatusRequest};

// ── CLI structure ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "tcfs",
    version,
    about = "TummyCrypt filesystem client",
    long_about = "tcfs: manage TummyCrypt FUSE mounts, credentials, and sync operations"
)]
struct Cli {
    /// Path to tcfs.toml configuration file
    #[arg(
        long,
        short = 'c',
        env = "TCFS_CONFIG",
        default_value = "/etc/tcfs/config.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show daemon and storage status
    Status,

    /// Configuration management
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// KDBX credential management (RemoteJuggler bridge)
    Kdbx {
        #[command(subcommand)]
        action: KdbxAction,
    },

    // ── Phase 2 commands ───────────────────────────────────────────────────────
    /// Upload a local file or directory tree to SeaweedFS
    ///
    /// Credentials are read from AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY
    /// environment variables (or set in the config credentials_file via SOPS).
    Push {
        /// Local path (file or directory)
        local: PathBuf,
        /// Remote prefix in the bucket (default: derived from local path name)
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        /// Path to the sync state cache JSON file (overrides config)
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Download a file from SeaweedFS using a manifest path
    ///
    /// The manifest path is in format: {prefix}/manifests/{hash}
    Pull {
        /// Remote manifest path (e.g. mydata/manifests/abc123...)
        manifest: String,
        /// Local destination path (default: current dir + hash basename)
        local: Option<PathBuf>,
        /// Remote prefix to look up chunks (default: derived from manifest path)
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        /// Path to the sync state cache JSON file (overrides config)
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Show local sync state for a file or directory
    #[command(name = "sync-status")]
    SyncStatus {
        /// Path to check (default: current directory)
        path: Option<PathBuf>,
        /// Path to the sync state cache JSON file (overrides config)
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    // ── Phase 3: mount + stub management ──────────────────────────────────────
    /// Mount a remote as a local directory
    Mount {
        /// Remote spec (e.g. seaweedfs://host/bucket[/prefix])
        remote: String,
        /// Local mountpoint
        mountpoint: PathBuf,
        /// Mount read-only
        #[arg(long)]
        read_only: bool,
        /// Use NFS loopback instead of FUSE (no kernel modules required)
        #[arg(long)]
        nfs: bool,
        /// NFS server port (0 = auto-assign, default 0)
        #[arg(long, default_value = "0")]
        nfs_port: u16,
    },

    /// Unmount a tcfs mountpoint
    Unmount {
        /// Local mountpoint to unmount
        mountpoint: PathBuf,
    },

    /// Cache management (stats, clear)
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// Convert hydrated file back to .tc stub, reclaiming disk space
    Unsync {
        /// Path to unsync
        path: PathBuf,
        /// Force unsync even if local changes exist
        #[arg(long)]
        force: bool,
    },

    // ── E2E encryption commands (Sprint 2) ─────────────────────────────────
    /// Initialize tcfs identity and device key (first-time setup)
    Init {
        /// Device name (default: hostname)
        #[arg(long)]
        device_name: Option<String>,
        /// Non-interactive mode (use with --password)
        #[arg(long)]
        non_interactive: bool,
        /// Master passphrase (non-interactive mode only)
        #[arg(long, env = "TCFS_MASTER_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },

    /// Manage enrolled devices
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },

    /// Manage encryption session lock/unlock
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },

    /// Rotate S3 credentials in the SOPS-encrypted credential file
    #[command(name = "rotate-credentials")]
    RotateCredentials {
        /// Path to the SOPS-encrypted credential file (overrides config)
        #[arg(long)]
        cred_file: Option<PathBuf>,
        /// Non-interactive mode (reads new credentials from environment)
        #[arg(long)]
        non_interactive: bool,
    },

    /// Rotate the master encryption key (re-wraps all file keys)
    #[command(name = "rotate-key")]
    RotateKey {
        /// Path to old master key file (default: ~/.config/tcfs/master.key)
        #[arg(long)]
        old_key_file: Option<PathBuf>,
        /// Use passphrase for the new key (instead of generating a mnemonic)
        #[arg(long)]
        password: bool,
        /// Non-interactive mode (generate and print mnemonic without prompt)
        #[arg(long)]
        non_interactive: bool,
    },

    /// Reconcile local directory with remote storage
    ///
    /// Diffs local tree against remote index and shows what would change.
    /// Use --execute to apply the plan (default is dry-run).
    Reconcile {
        /// Local directory to reconcile (default: sync_root from config)
        #[arg(long, short = 'p')]
        path: Option<PathBuf>,
        /// Remote prefix override
        #[arg(long)]
        prefix: Option<String>,
        /// Actually execute the plan (default: dry-run)
        #[arg(long)]
        execute: bool,
        /// Path to the sync state cache JSON file (overrides config)
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Manage per-folder sync policies
    Policy {
        #[command(subcommand)]
        action: PolicyAction,
    },

    /// Delete a file from remote storage and local disk
    ///
    /// Removes the index entry, manifest, and local file. The daemon's file
    /// watcher will detect the local deletion and publish a NATS FileDeleted
    /// event for other devices to process.
    Rm {
        /// Path to the file to delete
        path: PathBuf,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        /// Path to the sync state cache JSON file (overrides config)
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Resolve a sync conflict for a file
    ///
    /// When two devices modify the same file without syncing, a conflict is
    /// detected. Use this command to pick a resolution strategy.
    Resolve {
        /// Path to the conflicted file
        path: PathBuf,
        /// Resolution strategy: keep-local, keep-remote, keep-both, or defer
        #[arg(long, short = 's', value_parser = ["keep-local", "keep-remote", "keep-both", "defer"])]
        strategy: Option<String>,
    },

    /// Manage the sync trash (staged deletes)
    ///
    /// When trash is enabled, deleted files are moved to a .tcfs-trash/ prefix
    /// instead of being permanently removed. Use these subcommands to list,
    /// restore, or purge trashed items.
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },

    /// Migrate S3 index entries from stale/incorrect prefixes
    ///
    /// Fixes double-prefixed entries (data/index/data/*) and orphaned entries
    /// under old prefixes (tcfs/index/*). Run once after upgrading.
    #[command(name = "migrate-prefix")]
    MigratePrefix {
        /// Dry-run mode (show what would be migrated without changing anything)
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
enum PolicyAction {
    /// Set sync mode for a folder (always, on-demand, never)
    Set {
        path: PathBuf,
        #[arg(value_parser = ["always", "on-demand", "never"])]
        mode: String,
    },
    /// Show the effective sync policy for a path (including inherited)
    Get { path: PathBuf },
    /// List all configured policies
    List,
    /// Pin a path (exempt from auto-unsync)
    Pin { path: PathBuf },
    /// Unpin a path
    Unpin { path: PathBuf },
}

#[derive(Subcommand, Debug)]
enum DeviceAction {
    /// Enroll this device in the sync fleet
    Enroll {
        /// Device name (default: hostname)
        #[arg(long)]
        name: Option<String>,
    },
    /// List enrolled devices
    List,
    /// Revoke a device by name
    Revoke {
        /// Device name to revoke
        name: String,
    },
    /// Show this device's identity and status
    Status,
    /// Generate a device enrollment invite (QR code or deep link)
    Invite {
        /// Expiry in hours (default: 24)
        #[arg(long, default_value = "24")]
        expiry_hours: u64,
        /// Render QR code in terminal (compact encoding for phone scanning)
        #[arg(long)]
        qr: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Unlock the encryption session (load master key into daemon)
    Unlock {
        /// Path to master key file (default: ~/.config/tcfs/master.key)
        #[arg(long)]
        key_file: Option<PathBuf>,
        /// Path to a passphrase file (derives key via configured key_derivation method)
        #[arg(long, conflicts_with = "key_file")]
        passphrase_file: Option<PathBuf>,
    },
    /// Lock the encryption session (clear master key from daemon memory)
    Lock,
    /// Show encryption session status
    Status,
    /// Enroll a TOTP authenticator for this device
    Enroll {
        /// Auth method to enroll (default: totp)
        #[arg(long, default_value = "totp")]
        method: String,
    },
    /// Complete a WebAuthn enrollment (submit attestation from authenticator)
    #[command(name = "complete-enroll")]
    CompleteEnroll {
        /// Auth method (default: webauthn)
        #[arg(long, default_value = "webauthn")]
        method: String,
        /// Path to JSON file containing attestation data
        #[arg(long)]
        attestation_file: std::path::PathBuf,
    },
    /// Verify a TOTP code to authenticate
    Verify {
        /// 6-digit TOTP code from authenticator app
        code: String,
    },
    /// Revoke a session (by token or device)
    Revoke {
        /// Session token to revoke
        #[arg(long)]
        token: Option<String>,
        /// Device ID to revoke all sessions for
        #[arg(long)]
        device: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum TrashAction {
    /// List all trashed items
    List {
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
    },
    /// Restore a trashed item back to its original index location
    Restore {
        /// Original path of the trashed file (as shown by `trash list`)
        path: String,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
    },
    /// Permanently delete old trash entries
    Purge {
        /// Delete entries older than N seconds (default: from config trash_retention_secs)
        #[arg(long)]
        older_than: Option<u64>,
        /// Purge ALL trash entries regardless of age
        #[arg(long)]
        all: bool,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the active configuration (merged defaults + config file)
    Show,
}

#[derive(Subcommand, Debug)]
enum CacheAction {
    /// Show cache usage statistics
    Stats,
    /// Clear all cached content
    Clear,
}

#[derive(Subcommand, Debug)]
enum KdbxAction {
    /// Resolve a credential entry by group/title path
    Resolve {
        /// Path in format group/subgroup/entry-title
        /// Example: tummycrypt/tcfs/seaweedfs/admin/access-key
        query: String,

        /// KDBX database file (overrides config kdbx_path)
        #[arg(long, env = "TCFS_KDBX_PATH")]
        kdbx_path: Option<PathBuf>,

        /// Master password (reads from TCFS_KDBX_PASSWORD env var or prompts interactively)
        #[arg(long, env = "TCFS_KDBX_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },

    /// Import credentials from KDBX into SOPS-encrypted credential files (Phase 5)
    Import {
        #[arg(long, env = "TCFS_KDBX_PATH")]
        kdbx_path: Option<PathBuf>,

        /// Master password (reads from TCFS_KDBX_PASSWORD env var or prompts interactively)
        #[arg(long, env = "TCFS_KDBX_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber (respects RUST_LOG env var, default: info)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = load_config(&cli.config).await?;

    match cli.command {
        #[cfg(unix)]
        Commands::Status => cmd_status(&config).await,
        #[cfg(not(unix))]
        Commands::Status => {
            anyhow::bail!("status command requires Unix daemon socket (not available on Windows)")
        }
        Commands::Config {
            action: ConfigAction::Show,
        } => cmd_config_show(&config, &cli.config),
        Commands::Kdbx {
            action:
                KdbxAction::Resolve {
                    query,
                    kdbx_path,
                    password,
                },
        } => {
            let password = resolve_password(password)?;
            cmd_kdbx_resolve(&config, &query, kdbx_path.as_deref(), &password)
        }
        Commands::Kdbx {
            action: KdbxAction::Import { .. },
        } => {
            anyhow::bail!("kdbx import: not yet implemented (Phase 5)")
        }
        Commands::Push {
            local,
            prefix,
            state,
        } => cmd_push(&config, &local, prefix.as_deref(), state.as_deref()).await,
        Commands::Pull {
            manifest,
            local,
            prefix,
            state,
        } => {
            cmd_pull(
                &config,
                &manifest,
                local.as_deref(),
                prefix.as_deref(),
                state.as_deref(),
            )
            .await
        }
        Commands::SyncStatus { path, state } => {
            cmd_sync_status(&config, path.as_deref(), state.as_deref())
        }
        Commands::Cache { action } => match action {
            CacheAction::Stats => cmd_cache_stats(&config).await,
            CacheAction::Clear => cmd_cache_clear(&config).await,
        },
        Commands::Mount {
            remote,
            mountpoint,
            read_only,
            nfs,
            nfs_port,
        } => cmd_mount(&config, &remote, &mountpoint, read_only, nfs, nfs_port).await,
        Commands::Unmount { mountpoint } => cmd_unmount(&mountpoint),
        Commands::Unsync { path, force } => cmd_unsync(&config, &path, force).await,
        Commands::Init {
            device_name,
            non_interactive,
            password,
        } => cmd_init(&config, device_name, non_interactive, password).await,
        Commands::Device { action } => match action {
            DeviceAction::Enroll { name } => cmd_device_enroll(name),
            DeviceAction::List => cmd_device_list(),
            DeviceAction::Revoke { name } => cmd_device_revoke(&name),
            DeviceAction::Status => cmd_device_status(),
            DeviceAction::Invite { expiry_hours, qr } => {
                cmd_device_invite(&config, expiry_hours, qr).await
            }
        },
        Commands::Auth { action } => {
            #[cfg(unix)]
            match action {
                AuthAction::Unlock {
                    key_file,
                    passphrase_file,
                } => {
                    cmd_auth_unlock(&config, key_file.as_deref(), passphrase_file.as_deref()).await
                }
                AuthAction::Lock => cmd_auth_lock(&config).await,
                AuthAction::Status => cmd_auth_status(&config).await,
                AuthAction::Enroll { method } => cmd_auth_enroll(&config, &method).await,
                AuthAction::CompleteEnroll {
                    method,
                    attestation_file,
                } => cmd_auth_complete_enroll(&config, &method, &attestation_file).await,
                AuthAction::Verify { code } => cmd_auth_verify(&config, &code).await,
                AuthAction::Revoke { token, device } => {
                    cmd_auth_revoke(&config, token.as_deref(), device.as_deref()).await
                }
            }
            #[cfg(not(unix))]
            {
                let _ = action;
                anyhow::bail!("auth commands require the daemon (not available on this platform)")
            }
        }
        Commands::RotateCredentials {
            cred_file,
            non_interactive,
        } => cmd_rotate_credentials(&config, cred_file.as_deref(), non_interactive).await,
        Commands::RotateKey {
            old_key_file,
            password,
            non_interactive,
        } => cmd_rotate_key(&config, old_key_file.as_deref(), password, non_interactive).await,
        Commands::Reconcile {
            path,
            prefix,
            execute,
            state,
        } => {
            cmd_reconcile(
                &config,
                path.as_deref(),
                prefix.as_deref(),
                execute,
                state.as_deref(),
            )
            .await
        }
        Commands::Policy { action } => cmd_policy(&config, action).await,
        Commands::Rm {
            path,
            prefix,
            state,
        } => cmd_rm(&config, &path, prefix.as_deref(), state.as_deref()).await,
        Commands::Trash { action } => cmd_trash(&config, action).await,
        Commands::MigratePrefix { dry_run } => cmd_migrate_prefix(&config, dry_run).await,
        Commands::Resolve { path, strategy } => {
            #[cfg(unix)]
            {
                cmd_resolve(&config, &path, strategy.as_deref()).await
            }
            #[cfg(not(unix))]
            {
                let _ = (path, strategy);
                anyhow::bail!(
                    "resolve command requires the daemon (not available on this platform)"
                )
            }
        }
    }
}

// ── Password prompt ──────────────────────────────────────────────────────────

/// Resolve a password: use the provided value, or prompt interactively.
fn resolve_password(password: Option<String>) -> Result<String> {
    match password {
        Some(p) => Ok(p),
        None => rpassword::prompt_password("KDBX master password: ")
            .context("failed to read password from terminal"),
    }
}

// ── Config loading ────────────────────────────────────────────────────────────

async fn load_config(path: &Path) -> Result<tcfs_core::config::TcfsConfig> {
    if path.exists() {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading config: {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("parsing config: {}", path.display()))
    } else {
        Ok(tcfs_core::config::TcfsConfig::default())
    }
}

// ── Storage operator from environment credentials ─────────────────────────────

/// Read a credential from a `*_FILE` env var (the var points to a file path).
fn read_credential_file(env_var: &str) -> Result<String, std::env::VarError> {
    let path = std::env::var(env_var)?;
    std::fs::read_to_string(path.trim())
        .map(|s| s.trim().to_string())
        .map_err(|_| std::env::VarError::NotPresent)
}

/// Read a credential from a SOPS-decrypted JSON or KEY=VALUE file.
fn read_sops_credential(path: &std::path::Path, key: &str) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, String>>(&content) {
        if let Some(val) = map.get(key) {
            return Ok(val.clone());
        }
    }
    for line in content.lines() {
        if let Some(val) = line.strip_prefix(&format!("{}=", key)) {
            return Ok(val.trim().to_string());
        }
    }
    anyhow::bail!("key '{}' not found in {}", key, path.display())
}

/// Build an OpenDAL operator using credentials from environment variables.
///
/// Discovery chain: direct env var -> *_FILE env var -> config credentials_file (SOPS)
fn build_operator_from_env(config: &tcfs_core::config::TcfsConfig) -> Result<opendal::Operator> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID")
        .or_else(|_| std::env::var("TCFS_ACCESS_KEY_ID"))
        .or_else(|_| read_credential_file("TCFS_S3_ACCESS_FILE"))
        .or_else(|_| read_credential_file("AWS_ACCESS_KEY_ID_FILE"))
        .or_else(|_| {
            config
                .storage
                .credentials_file
                .as_ref()
                .and_then(|p| read_sops_credential(p, "access_key_id").ok())
                .ok_or(std::env::VarError::NotPresent)
        })
        .context(
            "S3 credentials not set\n\
             Set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY environment variables,\n\
             or use *_FILE variants pointing to credential files.\n\
             Example:\n\
             \texport AWS_ACCESS_KEY_ID=your-key\n\
             \texport AWS_SECRET_ACCESS_KEY=your-secret",
        )?;
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .or_else(|_| std::env::var("TCFS_SECRET_ACCESS_KEY"))
        .or_else(|_| read_credential_file("TCFS_S3_SECRET_FILE"))
        .or_else(|_| read_credential_file("AWS_SECRET_ACCESS_KEY_FILE"))
        .or_else(|_| {
            config
                .storage
                .credentials_file
                .as_ref()
                .and_then(|p| read_sops_credential(p, "secret_access_key").ok())
                .ok_or(std::env::VarError::NotPresent)
        })
        .context("AWS_SECRET_ACCESS_KEY environment variable not set")?;

    tcfs_storage::operator::build_from_core_config(&config.storage, &access_key, &secret_key)
        .context("building storage operator")
}

/// Expand `~` in path to the user's home directory
fn expand_tilde(path: &Path) -> PathBuf {
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

/// Resolve the state cache path: CLI flag > config > default user data dir
fn resolve_state_path(
    config: &tcfs_core::config::TcfsConfig,
    override_path: Option<&Path>,
) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    // Config uses state_db (designed for RocksDB in Phase 4); for JSON Phase 2
    // we derive a sibling .json file
    let db = expand_tilde(&config.sync.state_db);
    db.with_extension("json")
}

// ── Progress bar helpers ──────────────────────────────────────────────────────

fn make_progress_bar(total: u64, prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{prefix:.bold} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

fn make_spinner(prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{prefix:.bold} {spinner} {msg}").unwrap());
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Load the device_id from the registry, using config for device name and registry path.
fn load_device_id(config: &tcfs_core::config::TcfsConfig) -> String {
    let device_name = config
        .sync
        .device_name
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_device_name);
    let registry_path = config
        .sync
        .device_identity
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);

    match tcfs_secrets::device::DeviceRegistry::load(&registry_path) {
        Ok(mut registry) => {
            match registry.find(&device_name) {
                Some(d) if d.device_id.is_empty() => {
                    // Backfill device_id for entries created before UUID generation
                    let new_id = registry.backfill_device_id(&device_name).unwrap();
                    if let Err(e) = registry.save(&registry_path) {
                        eprintln!("warning: failed to save backfilled device registry: {e}");
                    } else {
                        eprintln!(
                            "Backfilled missing device_id for '{device_name}': {}",
                            &new_id[..8]
                        );
                    }
                    new_id
                }
                Some(d) => d.device_id.clone(),
                None => {
                    eprintln!("warning: device '{device_name}' not enrolled. Run 'tcfs init' or 'tcfs device enroll' for vclock tracking.");
                    String::new()
                }
            }
        }
        Err(_) => {
            eprintln!("warning: no device registry found. Run 'tcfs init' for vclock tracking.");
            String::new()
        }
    }
}

/// Build a CollectConfig from the sync config.
fn collect_config_from_sync(
    config: &tcfs_core::config::TcfsConfig,
) -> tcfs_sync::engine::CollectConfig {
    tcfs_sync::engine::CollectConfig {
        sync_git_dirs: config.sync.sync_git_dirs,
        git_sync_mode: config.sync.git_sync_mode.clone(),
        sync_hidden_dirs: config.sync.sync_hidden_dirs,
        exclude_patterns: config.sync.exclude_patterns.clone(),
        follow_symlinks: false,
        sync_empty_dirs: config.sync.sync_empty_dirs,
    }
}

// ── `tcfs push` ───────────────────────────────────────────────────────────────

async fn cmd_push(
    config: &tcfs_core::config::TcfsConfig,
    local: &Path,
    prefix: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    let op = build_operator_from_env(config)?;
    let state_path = resolve_state_path(config, state_override);
    let mut state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    let device_id = load_device_id(config);
    let collect_cfg = collect_config_from_sync(config);

    // Default prefix: storage.remote_prefix from config, falling back to bucket.
    // This must match the FUSE daemon's mount prefix for cross-host visibility.
    let remote_prefix = prefix
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| config.storage.resolved_prefix().to_string());

    println!(
        "Pushing {} → {}:{} (endpoint: {}{})",
        local.display(),
        config.storage.bucket,
        remote_prefix,
        config.storage.endpoint,
        if device_id.is_empty() {
            String::new()
        } else {
            format!(", device: {}...", &device_id[..8.min(device_id.len())])
        },
    );

    if local.is_file() {
        // Single-file push
        let pb = make_progress_bar(0, "push");
        pb.set_message(format!("{}", local.display()));

        let pb_clone = pb.clone();
        let progress: tcfs_sync::engine::ProgressFn = Box::new(move |done, total, msg| {
            pb_clone.set_length(total);
            pb_clone.set_position(done);
            pb_clone.set_message(msg.to_string());
        });

        let sync_root = config.sync.sync_root.as_deref();
        let rel = tcfs_sync::engine::normalize_rel_path(local, sync_root);

        // Load master key for E2E encryption if configured
        let master_key = config
            .crypto
            .master_key_file
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|k| k.len() == 32)
            .map(|bytes| {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                tcfs_crypto::MasterKey::from_bytes(arr)
            });
        let enc_ctx = master_key
            .as_ref()
            .map(|mk| tcfs_sync::engine::EncryptionContext {
                master_key: mk.clone(),
            });

        let result = tcfs_sync::engine::upload_file_with_device(
            &op,
            local,
            &remote_prefix,
            &mut state,
            Some(&progress),
            &device_id,
            Some(&rel),
            enc_ctx.as_ref(),
        )
        .await
        .with_context(|| format!("uploading {}", local.display()))?;

        state.flush().context("flushing state cache")?;

        // Handle conflict outcomes
        if let Some(ref outcome) = result.outcome {
            match outcome {
                tcfs_sync::conflict::SyncOutcome::Conflict(info) => {
                    eprintln!(
                        "CONFLICT: {} (local device: {}, remote device: {})",
                        info.rel_path, info.local_device, info.remote_device
                    );
                    eprintln!(
                        "  Use 'tcfs device list' to see fleet, resolve with conflict_mode config"
                    );
                }
                tcfs_sync::conflict::SyncOutcome::RemoteNewer => {
                    eprintln!("Remote is newer — run 'tcfs pull' first");
                }
                _ => {}
            }
        }

        if result.skipped {
            pb.finish_with_message(format!(
                "{} (unchanged)",
                local.file_name().unwrap_or_default().to_string_lossy()
            ));
            println!("  skipped (unchanged since last sync)");
        } else {
            // Write index entry for FUSE discoverability (same pattern as push_tree_with_device)
            let index_key = format!("{}/index/{}", remote_prefix.trim_end_matches('/'), &rel);
            let index_entry = format!(
                "manifest_hash={}\nsize={}\nchunks={}\n",
                result.hash, result.bytes, result.chunks
            );
            if let Err(e) = op.write(&index_key, index_entry.into_bytes()).await {
                eprintln!("warning: failed to write index entry: {e}");
            }

            pb.finish_with_message("done".to_string());
            println!("  hash:    {}", &result.hash[..16.min(result.hash.len())]);
            println!("  chunks:  {}", result.chunks);
            println!("  bytes:   {}", fmt_bytes(result.bytes));
            println!("  remote:  {}", result.remote_path);
        }
    } else if local.is_dir() {
        // Directory tree push
        let pb = make_spinner("push");
        pb.set_message("scanning files...");

        let pb_clone = pb.clone();
        let progress: tcfs_sync::engine::ProgressFn = Box::new(move |done, total, msg| {
            if total > 0 {
                pb_clone.set_style(
                    ProgressStyle::with_template(
                        "{prefix:.bold} [{bar:40.cyan/blue}] {pos}/{len} {msg}",
                    )
                    .unwrap()
                    .progress_chars("=>-"),
                );
                pb_clone.set_length(total);
            }
            pb_clone.set_position(done);
            pb_clone.set_message(msg.to_string());
        });

        let (uploaded, skipped, bytes) = tcfs_sync::engine::push_tree_with_device(
            &op,
            local,
            &remote_prefix,
            &mut state,
            Some(&progress),
            &device_id,
            Some(&collect_cfg),
            None,
        )
        .await
        .with_context(|| format!("pushing tree: {}", local.display()))?;

        pb.finish_with_message("done".to_string());
        println!();
        println!("Push complete:");
        println!("  uploaded: {} files ({})", uploaded, fmt_bytes(bytes));
        println!("  skipped:  {} files (unchanged)", skipped);
        println!("  total:    {} files", uploaded + skipped);
    } else {
        anyhow::bail!(
            "path not found or not a file/directory: {}",
            local.display()
        );
    }

    Ok(())
}

// ── `tcfs pull` ───────────────────────────────────────────────────────────────

async fn cmd_pull(
    config: &tcfs_core::config::TcfsConfig,
    manifest_path: &str,
    local: Option<&Path>,
    prefix: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    let op = build_operator_from_env(config)?;
    let device_id = load_device_id(config);

    // Detect whether input looks like a file path vs a manifest path
    let is_file_path = manifest_path.starts_with('/')
        || manifest_path.starts_with('.')
        || std::path::Path::new(manifest_path).exists();

    // Derive the remote prefix from the manifest path if not provided
    // e.g. "devices/A29247/manifests/abc123" → prefix = "devices/A29247"
    let remote_prefix = prefix
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            if !is_file_path {
                // Extract prefix from manifest path: "pfx/manifests/hash" → "pfx"
                manifest_path
                    .rsplit_once("/manifests/")
                    .map(|(pfx, _)| pfx.to_string())
                    .unwrap_or_else(|| {
                        manifest_path
                            .split('/')
                            .next()
                            .unwrap_or("data")
                            .to_string()
                    })
            } else {
                // File path: use config remote_prefix (matches FUSE daemon)
                config
                    .storage
                    .remote_prefix
                    .clone()
                    .unwrap_or_else(|| config.storage.bucket.clone())
            }
        });

    // Resolve file paths to manifest paths via the S3 index
    let sync_root = config.sync.sync_root.as_deref();
    let resolved_manifest =
        tcfs_sync::engine::resolve_manifest_path(&op, manifest_path, &remote_prefix, sync_root)
            .await
            .with_context(|| format!("resolving manifest for: {manifest_path}"))?;

    // Default local path: current dir + manifest hash (last path component)
    let hash_basename = resolved_manifest
        .split('/')
        .next_back()
        .unwrap_or("downloaded");
    let local_path = local
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(hash_basename));

    println!("Pulling {} → {}", manifest_path, local_path.display(),);

    let pb = make_progress_bar(0, "pull");
    pb.set_message("fetching manifest...".to_string());

    let pb_clone = pb.clone();
    let progress: tcfs_sync::engine::ProgressFn = Box::new(move |done, total, msg| {
        pb_clone.set_length(total);
        pb_clone.set_position(done);
        pb_clone.set_message(msg.to_string());
    });

    // Open state cache for vclock merge during pull
    let state_path = resolve_state_path(config, state_override);
    let mut state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    // Load master key for E2E decryption if configured
    let master_key = config
        .crypto
        .master_key_file
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .filter(|k| k.len() == 32)
        .map(|bytes| {
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            tcfs_crypto::MasterKey::from_bytes(key)
        });
    let enc_ctx = master_key
        .as_ref()
        .map(|mk| tcfs_sync::engine::EncryptionContext {
            master_key: mk.clone(),
        });

    let result = tcfs_sync::engine::download_file_with_device(
        &op,
        &resolved_manifest,
        &local_path,
        &remote_prefix,
        Some(&progress),
        &device_id,
        Some(&mut state),
        enc_ctx.as_ref(),
    )
    .await
    .with_context(|| format!("downloading {}", manifest_path))?;

    state.flush().context("flushing state cache")?;

    pb.finish_with_message("done".to_string());
    println!();
    println!("Downloaded:");
    println!("  local:  {}", result.local_path.display());
    println!("  bytes:  {}", fmt_bytes(result.bytes));

    Ok(())
}

// ── `tcfs sync-status` ────────────────────────────────────────────────────────

fn cmd_sync_status(
    config: &tcfs_core::config::TcfsConfig,
    path: Option<&Path>,
    state_override: Option<&Path>,
) -> Result<()> {
    let state_path = resolve_state_path(config, state_override);
    let state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    println!("State cache: {}", state_path.display());
    println!("Tracked files: {}", state.len());

    if let Some(p) = path {
        let canonical =
            std::fs::canonicalize(p).with_context(|| format!("resolving path: {}", p.display()))?;

        match state.get(&canonical) {
            Some(entry) => {
                println!();
                println!("File: {}", canonical.display());
                println!(
                    "  hash:       {}",
                    &entry.blake3[..16.min(entry.blake3.len())]
                );
                println!("  size:       {}", fmt_bytes(entry.size));
                println!("  chunks:     {}", entry.chunk_count);
                println!("  remote:     {}", entry.remote_path);
                println!("  last sync:  {} seconds ago", {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    now.saturating_sub(entry.last_synced)
                });

                // Check if it needs re-sync
                match state.needs_sync(&canonical)? {
                    None => println!("  status:     up to date"),
                    Some(reason) => println!("  status:     needs sync ({reason})"),
                }
            }
            None => {
                println!();
                println!(
                    "File: {} — not in sync state (never pushed)",
                    canonical.display()
                );
            }
        }
    }

    Ok(())
}

// ── `tcfs migrate-prefix` ────────────────────────────────────────────────────

async fn cmd_migrate_prefix(config: &tcfs_core::config::TcfsConfig, dry_run: bool) -> Result<()> {
    let op = build_operator_from_env(config)?;
    let target = config.storage.resolved_prefix();

    println!(
        "Migrating S3 index entries → target prefix: \"{}\"{}\n",
        target,
        if dry_run { " (DRY RUN)" } else { "" }
    );

    let mut migrated = 0u32;
    let mut deleted = 0u32;

    // 1. Fix double-prefixed entries: {target}/index/{target}/* → {target}/index/*
    let double_prefix = format!(
        "{}/index/{}/",
        target.trim_end_matches('/'),
        target.trim_end_matches('/')
    );
    let entries = op
        .list_with(&double_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("listing {double_prefix}"))?;

    for entry in entries {
        let old_key = entry.path().to_string();
        if old_key.ends_with('/') {
            continue;
        }
        let rel = old_key.strip_prefix(&double_prefix).unwrap_or(&old_key);
        let new_key = format!("{}/index/{}", target.trim_end_matches('/'), rel);

        println!("  move: {} → {}", old_key, new_key);
        if !dry_run {
            let data = op.read(&old_key).await?.to_bytes();
            op.write(&new_key, data.to_vec()).await?;
            op.delete(&old_key).await?;
        }
        migrated += 1;
    }

    // 2. Migrate orphan prefixes (e.g., tcfs/index/* when target is "data")
    let bucket = &config.storage.bucket;
    if bucket != target {
        let orphan_prefix = format!("{}/index/", bucket.trim_end_matches('/'));
        let entries = op
            .list_with(&orphan_prefix)
            .recursive(true)
            .await
            .with_context(|| format!("listing {orphan_prefix}"))?;

        for entry in entries {
            let old_key = entry.path().to_string();
            if old_key.ends_with('/') {
                continue;
            }
            let rel = old_key.strip_prefix(&orphan_prefix).unwrap_or(&old_key);
            let new_key = format!("{}/index/{}", target.trim_end_matches('/'), rel);

            // Check if target already has this entry
            let exists = op.read(&new_key).await.is_ok();
            if exists {
                println!("  delete orphan (target exists): {}", old_key);
                if !dry_run {
                    op.delete(&old_key).await?;
                }
                deleted += 1;
            } else {
                println!("  move orphan: {} → {}", old_key, new_key);
                if !dry_run {
                    let data = op.read(&old_key).await?.to_bytes();
                    op.write(&new_key, data.to_vec()).await?;
                    op.delete(&old_key).await?;
                }
                migrated += 1;
            }
        }
    }

    println!(
        "\n{}: migrated={}, orphans_deleted={}",
        if dry_run { "Would process" } else { "Done" },
        migrated,
        deleted
    );
    if dry_run {
        println!("Run without --dry-run to apply changes.");
    } else if migrated > 0 || deleted > 0 {
        println!("Restart tcfsd to re-populate the state cache.");
    }

    Ok(())
}

// ── `tcfs trash` ─────────────────────────────────────────────────────────────

async fn cmd_trash(config: &tcfs_core::config::TcfsConfig, action: TrashAction) -> Result<()> {
    let op = build_operator_from_env(config)?;

    let resolve_prefix = |p: Option<&str>| -> String {
        p.map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| {
                config
                    .storage
                    .remote_prefix
                    .clone()
                    .unwrap_or_else(|| config.storage.bucket.clone())
            })
    };

    match action {
        TrashAction::List { prefix } => {
            let remote_prefix = resolve_prefix(prefix.as_deref());
            let entries = tcfs_vfs::trash::list_trash(&op, &remote_prefix).await?;

            if entries.is_empty() {
                println!("Trash is empty.");
                return Ok(());
            }

            println!("{:<40} {:<20} TRASH KEY", "ORIGINAL PATH", "TRASHED");
            println!("{}", "-".repeat(90));

            for entry in &entries {
                let age = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(entry.trashed_at);
                let age_str = format_duration(age);

                println!(
                    "{:<40} {:<20} {}",
                    truncate_str(&entry.original_path, 39),
                    format!("{} ago", age_str),
                    entry.trash_key,
                );
            }

            println!("\n{} item(s) in trash.", entries.len());
            Ok(())
        }

        TrashAction::Restore { path, prefix } => {
            let remote_prefix = resolve_prefix(prefix.as_deref());
            let entries = tcfs_vfs::trash::list_trash(&op, &remote_prefix).await?;

            // Find matching entry by original path (most recent first)
            let entry = entries
                .iter()
                .find(|e| e.original_path == path)
                .with_context(|| {
                    format!(
                        "no trash entry found for '{}'\nRun `tcfs trash list` to see trashed items.",
                        path
                    )
                })?;

            tcfs_vfs::trash::restore_trash_entry(&op, &remote_prefix, entry).await?;
            println!("Restored: {} → index/{}", path, entry.original_path);
            Ok(())
        }

        TrashAction::Purge {
            older_than,
            all,
            prefix,
        } => {
            let remote_prefix = resolve_prefix(prefix.as_deref());

            let max_age = if all {
                0 // purge everything
            } else {
                older_than.unwrap_or(config.sync.trash_retention_secs)
            };

            if all {
                // List first to confirm count
                let entries = tcfs_vfs::trash::list_trash(&op, &remote_prefix).await?;
                if entries.is_empty() {
                    println!("Trash is already empty.");
                    return Ok(());
                }
                println!("Purging ALL {} trash entries...", entries.len());
            } else {
                println!(
                    "Purging trash entries older than {}...",
                    format_duration(max_age)
                );
            }

            let purged = tcfs_vfs::trash::purge_old_trash(&op, &remote_prefix, max_age).await?;

            if purged > 0 {
                println!("Purged {} entry(ies).", purged);
            } else {
                println!("Nothing to purge.");
            }
            Ok(())
        }
    }
}

/// Format seconds into a human-readable duration string.
fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Truncate a string to max_len, appending "…" if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    }
}

// ── `tcfs rm` ────────────────────────────────────────────────────────────────

async fn cmd_rm(
    config: &tcfs_core::config::TcfsConfig,
    path: &Path,
    prefix: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    let op = build_operator_from_env(config)?;
    let state_path = resolve_state_path(config, state_override);
    let mut state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    let remote_prefix = prefix
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| config.storage.resolved_prefix().to_string());

    let sync_root = config.sync.sync_root.as_deref();
    let rel = tcfs_sync::engine::normalize_rel_path(path, sync_root);

    println!(
        "Deleting {} (remote: {}/index/{})",
        path.display(),
        remote_prefix,
        rel
    );

    // Delete from remote storage (index + manifest)
    tcfs_sync::engine::delete_remote_file(&op, &rel, &remote_prefix, &mut state, sync_root)
        .await
        .with_context(|| format!("deleting remote file: {}", rel))?;

    // Delete local file if it exists
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("deleting local file: {}", path.display()))?;
        println!("  Removed local file: {}", path.display());
    }

    println!("  Removed remote index + manifest");
    println!("Done.");

    Ok(())
}

// ── `tcfs status` ─────────────────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_status(config: &tcfs_core::config::TcfsConfig) -> Result<()> {
    let socket = &config.daemon.socket;

    if !socket.exists() {
        eprintln!("tcfsd: socket not found at {}", socket.display());
        eprintln!("       Is tcfsd running?  Try: tcfsd --config /etc/tcfs/config.toml");
        std::process::exit(1);
    }

    let mut client = connect_daemon(socket).await?;

    // Daemon status
    let status = client
        .status(tonic::Request::new(StatusRequest {}))
        .await
        .context("status RPC failed")?
        .into_inner();

    // Credential status
    let creds = client
        .credential_status(tonic::Request::new(Empty {}))
        .await
        .context("credential_status RPC failed")?
        .into_inner();

    let uptime = format_uptime(status.uptime_secs);

    println!("tcfsd v{}", status.version);
    println!("  uptime:        {uptime}");
    println!("  socket:        {}", socket.display());
    if !status.device_id.is_empty() {
        println!(
            "  device:        {} ({})",
            status.device_name,
            &status.device_id[..8.min(status.device_id.len())]
        );
        println!("  conflict mode: {}", status.conflict_mode);
    }
    println!(
        "  storage:       {} [{}]",
        status.storage_endpoint,
        if status.storage_ok {
            "ok"
        } else {
            "UNREACHABLE"
        }
    );
    println!(
        "  nats:          {}",
        if status.nats_ok {
            "connected"
        } else {
            "not connected"
        }
    );
    println!("  active mounts: {}", status.active_mounts);
    println!(
        "  credentials:   {} (source: {})",
        if creds.loaded { "loaded" } else { "NOT LOADED" },
        creds.source
    );
    if creds.needs_reload {
        println!("  WARNING: credentials need reload");
    }

    // Check for newer version (non-blocking, best-effort)
    check_for_update(&status.version);

    Ok(())
}

/// Check GitHub Releases for a newer tcfs version.
///
/// Results are cached in ~/.cache/tcfs/version-check.json for 24 hours
/// to avoid hitting the API on every invocation. Failures are silently ignored.
fn check_for_update(current_version: &str) {
    let cache_dir = dirs_cache_path();
    let cache_file = cache_dir.join("version-check.json");

    // Try to read cached result first
    if let Some(cached) = read_version_cache(&cache_file) {
        if cached.checked_at + VERSION_CHECK_TTL_SECS > now_epoch() {
            // Cache is still valid
            if let Some(ref latest) = cached.latest_version {
                print_update_notice(current_version, latest);
            }
            return;
        }
    }

    // Fetch the latest release tag from GitHub
    let latest = fetch_latest_version();

    // Cache the result (even on failure, to avoid hammering the API)
    let entry = VersionCacheEntry {
        latest_version: latest.clone(),
        checked_at: now_epoch(),
    };
    let _ = write_version_cache(&cache_file, &entry);

    if let Some(ref latest) = latest {
        print_update_notice(current_version, latest);
    }
}

const VERSION_CHECK_TTL_SECS: u64 = 86400; // 24 hours

#[derive(serde::Serialize, serde::Deserialize)]
struct VersionCacheEntry {
    latest_version: Option<String>,
    checked_at: u64,
}

fn dirs_cache_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    PathBuf::from(home).join(".cache").join("tcfs")
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_version_cache(path: &Path) -> Option<VersionCacheEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_version_cache(path: &Path, entry: &VersionCacheEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating cache dir: {}", parent.display()))?;
    }
    let json = serde_json::to_string(entry).context("serializing version cache")?;
    std::fs::write(path, json).with_context(|| format!("writing cache: {}", path.display()))?;
    Ok(())
}

/// Fetch the latest release version from GitHub using curl.
/// Returns None on any error (network, parse, missing curl, etc.).
fn fetch_latest_version() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "5",
            "-H",
            "Accept: application/vnd.github+json",
            "https://api.github.com/repos/tinyland-inc/tummycrypt/releases/latest",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8(output.stdout).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = json.get("tag_name")?.as_str()?;
    Some(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Compare semver-style versions and print a notice if a newer one is available.
fn print_update_notice(current: &str, latest: &str) {
    // Simple semver comparison: split on '.' and compare numerically
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() >= 3 {
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            ))
        } else {
            None
        }
    };

    if let (Some(cur), Some(lat)) = (parse(current), parse(latest)) {
        if lat > cur {
            println!();
            println!(
                "  A newer version (v{}) is available. You are running v{}.",
                latest, current
            );
            println!("  Update: curl -fsSL https://github.com/tinyland-inc/tummycrypt/releases/latest/download/install.sh | sh");
        }
    }
}

// ── gRPC connection ───────────────────────────────────────────────────────────

#[cfg(unix)]
async fn connect_daemon(socket_path: &Path) -> Result<TcfsDaemonClient<Channel>> {
    let path = socket_path.to_path_buf();

    // tonic over Unix domain socket: use a tower service_fn connector
    let channel = Endpoint::from_static("http://[::]:0")
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(&path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .with_context(|| format!("connecting to tcfsd at {}", socket_path.display()))?;

    Ok(TcfsDaemonClient::new(channel))
}

// ── `tcfs config show` ────────────────────────────────────────────────────────

fn cmd_config_show(config: &tcfs_core::config::TcfsConfig, config_path: &Path) -> Result<()> {
    if config_path.exists() {
        println!("# Configuration from: {}", config_path.display());
    } else {
        println!(
            "# Configuration: defaults (no file at {})",
            config_path.display()
        );
    }
    println!();
    let rendered = toml::to_string_pretty(config).context("serializing config to TOML")?;
    print!("{rendered}");
    Ok(())
}

// ── `tcfs kdbx resolve` ───────────────────────────────────────────────────────

fn cmd_kdbx_resolve(
    config: &tcfs_core::config::TcfsConfig,
    query: &str,
    kdbx_path_override: Option<&Path>,
    password: &str,
) -> Result<()> {
    // Resolve the KDBX path: CLI flag > config > error
    let kdbx_path = kdbx_path_override
        .map(|p| p.to_path_buf())
        .or_else(|| config.secrets.kdbx_path.clone())
        .with_context(|| {
            "no KDBX path provided; use --kdbx-path or set secrets.kdbx_path in config"
        })?;

    if !kdbx_path.exists() {
        anyhow::bail!("KDBX file not found: {}", kdbx_path.display());
    }

    let store = tcfs_secrets::KdbxStore::open(&kdbx_path);
    let cred = store
        .resolve(query, password)
        .with_context(|| format!("resolving '{query}' in {}", kdbx_path.display()))?;

    println!("title:    {}", cred.title);
    if let Some(ref u) = cred.username {
        println!("username: {u}");
    }
    println!("password: {}", cred.password);
    if let Some(ref url) = cred.url {
        println!("url:      {url}");
    }

    Ok(())
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn format_uptime(secs: i64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

// ── `tcfs cache stats` / `tcfs cache clear` ──────────────────────────────────

async fn cmd_cache_stats(config: &tcfs_core::config::TcfsConfig) -> Result<()> {
    let cache_dir = expand_tilde(&config.fuse.cache_dir);
    let cache_max = config.fuse.cache_max_mb * 1024 * 1024;
    let cache = tcfs_vfs::DiskCache::new(cache_dir.clone(), cache_max);

    let stats = cache.stats().await.context("reading cache stats")?;

    println!("Cache: {}", cache_dir.display());
    println!("  entries:  {}", stats.entry_count);
    println!("  shards:   {}", stats.shard_count);
    println!("  used:     {}", fmt_bytes(stats.total_bytes));
    println!("  budget:   {}", fmt_bytes(stats.max_bytes));
    println!(
        "  usage:    {:.1}%",
        if stats.max_bytes > 0 {
            stats.total_bytes as f64 / stats.max_bytes as f64 * 100.0
        } else {
            0.0
        }
    );
    Ok(())
}

async fn cmd_cache_clear(config: &tcfs_core::config::TcfsConfig) -> Result<()> {
    let cache_dir = expand_tilde(&config.fuse.cache_dir);
    if cache_dir.exists() {
        let before = tcfs_vfs::DiskCache::new(cache_dir.clone(), 0)
            .stats()
            .await?;
        tokio::fs::remove_dir_all(&cache_dir)
            .await
            .context("clearing cache directory")?;
        tokio::fs::create_dir_all(&cache_dir)
            .await
            .context("recreating cache directory")?;
        println!(
            "Cleared {} entries ({}).",
            before.entry_count,
            fmt_bytes(before.total_bytes)
        );
    } else {
        println!("Cache directory does not exist: {}", cache_dir.display());
    }
    Ok(())
}

// ── `tcfs mount` ─────────────────────────────────────────────────────────────

async fn cmd_mount(
    config: &tcfs_core::config::TcfsConfig,
    remote: &str,
    mountpoint: &std::path::Path,
    read_only: bool,
    use_nfs: bool,
    nfs_port: u16,
) -> Result<()> {
    // Try daemon-managed mount first
    {
        let socket_path = expand_tilde(&config.daemon.socket);
        let mut options = vec![];
        if use_nfs {
            options.push("nfs".to_string());
        }
        if let Ok(mut client) = connect_daemon(&socket_path).await {
            let resp = client
                .mount(tonic::Request::new(tcfs_core::proto::MountRequest {
                    remote: remote.to_string(),
                    mountpoint: mountpoint.to_string_lossy().to_string(),
                    read_only,
                    options,
                }))
                .await;

            match resp {
                Ok(r) if r.get_ref().success => {
                    println!("Mounted via daemon: {} → {}", remote, mountpoint.display());
                    return Ok(());
                }
                Ok(r) => {
                    eprintln!(
                        "Daemon mount failed: {}, falling back to direct mount",
                        r.into_inner().error
                    );
                }
                Err(e) => {
                    eprintln!("Daemon unavailable: {e}, falling back to direct mount");
                }
            }
        }
    }

    // Direct mount: build operator from remote spec + credentials
    let (endpoint, bucket, prefix) = tcfs_storage::parse_remote_spec(remote)?;

    let access_key = std::env::var("AWS_ACCESS_KEY_ID")
        .or_else(|_| std::env::var("TCFS_ACCESS_KEY_ID"))
        .context("S3 credentials not set — export AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY")?;
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .or_else(|_| std::env::var("TCFS_SECRET_ACCESS_KEY"))
        .context("AWS_SECRET_ACCESS_KEY not set")?;

    let storage_cfg = tcfs_storage::operator::StorageConfig {
        endpoint: endpoint.clone(),
        region: config.storage.region.clone(),
        bucket: bucket.clone(),
        access_key_id: access_key,
        secret_access_key: secret_key,
    };
    let op = tcfs_storage::build_operator(&storage_cfg).context("building storage operator")?;

    let cache_dir = expand_tilde(&config.fuse.cache_dir);
    let neg_ttl = config.fuse.negative_cache_ttl_secs;
    let cache_max = config.fuse.cache_max_mb * 1024 * 1024;

    let backend = if use_nfs { "NFS loopback" } else { "FUSE" };
    println!(
        "Mounting {}:{} (prefix: {}) → {} [{}]",
        endpoint,
        bucket,
        if prefix.is_empty() { "(root)" } else { &prefix },
        mountpoint.display(),
        backend,
    );
    println!(
        "Press Ctrl-C or run `tcfs unmount {}` to stop.",
        mountpoint.display()
    );

    if use_nfs {
        // NFS loopback mount (fallback — use --nfs flag)
        tcfs_nfs::serve_and_mount(tcfs_nfs::NfsMountConfig {
            op,
            prefix,
            mountpoint: mountpoint.to_path_buf(),
            cache_dir,
            cache_max_bytes: cache_max,
            negative_ttl_secs: neg_ttl,
            port: nfs_port,
        })
        .await
        .context("NFS mount failed")
    } else {
        // Connect to NATS for flush events (if configured)
        let device_id = load_device_id(config);
        let on_flush: Option<tcfs_vfs::OnFlushCallback> =
            if config.sync.nats_url != "nats://localhost:4222" {
                match tcfs_sync::nats::NatsClient::connect(
                    &config.sync.nats_url,
                    config.sync.nats_tls,
                    config.sync.nats_token.as_deref(),
                )
                .await
                {
                    Ok(nats) => {
                        let nats = std::sync::Arc::new(tokio::sync::Mutex::new(nats));
                        let dev = device_id.clone();
                        let pfx = prefix.clone();
                        Some(std::sync::Arc::new(
                        move |rel_path: &str,
                              hash: &str,
                              size: u64,
                              _chunks: usize,
                              vclock: &tcfs_sync::conflict::VectorClock| {
                            let event = tcfs_sync::StateEvent::FileSynced {
                                device_id: dev.clone(),
                                rel_path: rel_path.to_string(),
                                blake3: hash.to_string(),
                                size,
                                vclock: vclock.clone(),
                                manifest_path: format!("{}/manifests/{}", pfx, hash),
                                timestamp: tcfs_sync::StateEvent::now(),
                            };
                            let n = nats.clone();
                            tokio::spawn(async move {
                                let client = n.lock().await;
                                if let Err(e) = client.publish_state_event(&event).await {
                                    tracing::warn!("on_flush NATS publish failed: {e}");
                                }
                            });
                        },
                    ))
                    }
                    Err(e) => {
                        tracing::warn!("NATS unavailable for mount callback: {e}");
                        None
                    }
                }
            } else {
                None
            };

        // FUSE3 mount (default — unprivileged via fusermount3)
        tcfs_fuse::mount(
            tcfs_fuse::MountConfig {
                op,
                prefix,
                mountpoint: mountpoint.to_path_buf(),
                cache_dir,
                cache_max_bytes: cache_max,
                negative_ttl_secs: neg_ttl,
                read_only,
                allow_other: false,
                on_flush,
                device_id: std::env::var("HOSTNAME").unwrap_or_else(|_| "cli".to_string()),
                // Load master key from file for FUSE read decryption.
                // The mount process is separate from the daemon, so it can't
                // share the daemon's Arc<Mutex<MasterKey>>. Read the key file directly.
                master_key: {
                    let mk_path = if config.crypto.enabled {
                        config.crypto.master_key_file.as_ref()
                    } else {
                        None
                    };
                    if let Some(path) = mk_path {
                        match std::fs::read(path) {
                            Ok(bytes) if bytes.len() == 32 => {
                                let mut key_bytes = [0u8; 32];
                                key_bytes.copy_from_slice(&bytes);
                                Some(std::sync::Arc::new(tokio::sync::Mutex::new(Some(
                                    tcfs_crypto::MasterKey::from_bytes(key_bytes),
                                ))))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    }
                },
            },
            None,
        )
        .await
        .context("FUSE mount failed")
    }
}

// ── `tcfs unmount` ───────────────────────────────────────────────────────────

fn cmd_unmount(mountpoint: &std::path::Path) -> Result<()> {
    // macOS: use umount directly (works with FUSE, FUSE-T, and NFS mounts)
    // Linux: try fusermount3 first (FUSE), fall back to umount (NFS + FUSE)
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("umount")
            .arg(mountpoint)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("Unmounted: {}", mountpoint.display());
                Ok(())
            }
            Ok(s) => anyhow::bail!(
                "umount exited {}: try `diskutil unmount {}`",
                s,
                mountpoint.display()
            ),
            Err(e) => anyhow::bail!("failed to run umount: {e}"),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let status = std::process::Command::new("fusermount3")
            .args(["-u", &mountpoint.to_string_lossy()])
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("Unmounted: {}", mountpoint.display());
                Ok(())
            }
            Ok(s) => {
                // Fallback: try plain umount (works as root or with FUSE-T)
                let fallback = std::process::Command::new("umount")
                    .arg(mountpoint)
                    .status();
                match fallback {
                    Ok(f) if f.success() => {
                        println!("Unmounted: {}", mountpoint.display());
                        Ok(())
                    }
                    _ => anyhow::bail!(
                        "fusermount3 exited {}: use `fusermount3 -u {}` or `umount {}` manually",
                        s,
                        mountpoint.display(),
                        mountpoint.display()
                    ),
                }
            }
            Err(e) => anyhow::bail!("failed to run fusermount3: {e}"),
        }
    }
}

// ── `tcfs unsync` ─────────────────────────────────────────────────────────────

/// Convert a hydrated file back to a `.tc` stub, reclaiming disk space.
///
/// Reads the file, computes its BLAKE3 hash, looks up the matching index entry,
/// and replaces the file content with a stub. The actual remote data is NOT deleted.
async fn cmd_unsync(
    config: &tcfs_core::config::TcfsConfig,
    path: &std::path::Path,
    force: bool,
) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("path not found: {}", path.display());
    }
    if tcfs_vfs::is_stub_path(path) {
        println!("{} is already a stub — nothing to do.", path.display());
        return Ok(());
    }

    // Read file content and compute hash
    let data = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading: {}", path.display()))?;

    let hash = tcfs_chunks::hash_bytes(&data);
    let hash_hex = tcfs_chunks::hash_to_hex(&hash);
    let size = data.len() as u64;

    if !force {
        let state_path = resolve_state_path(config, None);
        let state = tcfs_sync::state::StateCache::open(&state_path)
            .with_context(|| format!("opening state cache: {}", state_path.display()))?;

        match state.get(path) {
            None => anyhow::bail!(
                "{} is not tracked (never pushed). Use --force to unsync anyway.",
                path.display()
            ),
            Some(entry) if entry.blake3 != hash_hex => anyhow::bail!(
                "{} has local changes (hash mismatch). Use --force to unsync anyway.",
                path.display()
            ),
            _ => {}
        }
    }

    // Build stub at path.tc
    let stub_path = tcfs_vfs::real_to_stub_name(path.file_name().context("path has no filename")?);
    let stub_full = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(stub_path);

    let stub = tcfs_vfs::StubMeta {
        chunks: 0, // unknown without state — leave as 0
        compressed: false,
        fetched: false,
        oid: format!("blake3:{}", hash_hex),
        origin: format!("seaweedfs://{}/{}", config.storage.endpoint, hash_hex),
        size,
    };

    // Write stub then remove original
    tokio::fs::write(&stub_full, stub.to_bytes())
        .await
        .with_context(|| format!("writing stub: {}", stub_full.display()))?;
    tokio::fs::remove_file(path)
        .await
        .with_context(|| format!("removing hydrated file: {}", path.display()))?;

    println!("Unsynced: {} → {}", path.display(), stub_full.display());
    println!("  hash: {}", &hash_hex[..16]);
    println!("  size: {} freed", fmt_bytes(size));

    Ok(())
}

// ── `tcfs init` ──────────────────────────────────────────────────────────────

async fn cmd_init(
    _config: &tcfs_core::config::TcfsConfig,
    device_name: Option<String>,
    non_interactive: bool,
    password: Option<String>,
) -> Result<()> {
    let device_name = device_name.unwrap_or_else(tcfs_secrets::device::default_device_name);

    // Step 1: Check if already initialized (master key file exists)
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        })
        .join("tcfs");
    let master_key_path = config_dir.join("master.key");

    if master_key_path.exists() {
        anyhow::bail!(
            "Already initialized: {} exists. Remove it to re-initialize.",
            master_key_path.display()
        );
    }

    // Step 2-4: Derive or generate master key
    let master_key = if let Some(ref pw) = password {
        // Password provided: derive master key from passphrase via Argon2id
        println!("Deriving master key from passphrase...");
        let salt: [u8; 16] = rand_salt();
        tcfs_crypto::derive_master_key(
            &secrecy::SecretString::from(pw.clone()),
            &salt,
            &tcfs_crypto::kdf::KdfParams::default(),
        )?
    } else if non_interactive {
        // Non-interactive, no password: generate mnemonic, print it, no prompt
        println!("Generating BIP-39 recovery mnemonic...");
        let (mnemonic, master_key) = tcfs_crypto::generate_mnemonic()?;
        println!();
        println!("RECOVERY MNEMONIC (store this securely):");
        println!();
        let words: Vec<&str> = mnemonic.split_whitespace().collect();
        for (i, chunk) in words.chunks(4).enumerate() {
            println!("  {:2}. {}", i * 4 + 1, chunk.join("  "));
        }
        println!();
        master_key
    } else {
        // Interactive, no password: generate mnemonic, display prominently, confirm
        println!("Generating BIP-39 recovery mnemonic...");
        let (mnemonic, master_key) = tcfs_crypto::generate_mnemonic()?;
        println!();
        println!("╔══════════════════════════════════════════════════════════════╗");
        println!("║  RECOVERY MNEMONIC — WRITE THIS DOWN AND STORE IT SAFELY   ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║                                                              ║");
        let words: Vec<&str> = mnemonic.split_whitespace().collect();
        for (i, chunk) in words.chunks(4).enumerate() {
            let line = format!("  {:2}. {}", i * 4 + 1, chunk.join("  "));
            println!("║ {:<60} ║", line);
        }
        println!("║                                                              ║");
        println!("╚══════════════════════════════════════════════════════════════╝");
        println!();
        println!("This mnemonic is the ONLY way to recover your master key.");
        println!("It will NOT be shown again.");
        println!();

        // Ask user to confirm they wrote it down
        let confirmation = rpassword::prompt_password(
            "Type 'yes' to confirm you have written down the mnemonic: ",
        )
        .context("failed to read confirmation")?;
        if confirmation.trim().to_lowercase() != "yes" {
            anyhow::bail!("Initialization aborted. Please run 'tcfs init' again when ready.");
        }
        master_key
    };

    // Step 5: Write master key to ~/.config/tcfs/master.key (raw 32 bytes)
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("creating config dir: {}", config_dir.display()))?;
    std::fs::write(&master_key_path, master_key.as_bytes())
        .with_context(|| format!("writing master key: {}", master_key_path.display()))?;

    // Restrict permissions to owner-only (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&master_key_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on: {}", master_key_path.display()))?;
    }

    // Step 6: Create device registry and enroll this device
    let registry_path = tcfs_secrets::device::default_registry_path();
    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;
    let public_key = format!("age1-device-{}", &blake3_short(&device_name));
    let device_id = registry.enroll(&device_name, &public_key, None);
    registry.save(&registry_path)?;

    // Step 7: Print success message
    println!();
    println!("tcfs initialized successfully.");
    println!();
    println!("  Device name:  {}", device_name);
    println!("  Device ID:    {}", device_id);
    println!("  Master key:   {}", master_key_path.display());
    println!("  Registry:     {}", registry_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Configure storage: tcfs config show");
    println!("  2. Push files: tcfs push /path/to/files");

    Ok(())
}

fn rand_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

fn blake3_short(s: &str) -> String {
    let hash = blake3::hash(s.as_bytes());
    hash.to_hex().as_str()[..8].to_string()
}

// ── `tcfs device list` ───────────────────────────────────────────────────────

fn cmd_device_list() -> Result<()> {
    let registry_path = tcfs_secrets::device::default_registry_path();
    let registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;

    if registry.devices.is_empty() {
        println!("No devices enrolled. Run 'tcfs init' to create an identity.");
        return Ok(());
    }

    println!("Enrolled devices ({}):", registry.devices.len());
    for device in &registry.devices {
        let status = if device.revoked { "REVOKED" } else { "active" };
        let id_short = if device.device_id.len() > 8 {
            &device.device_id[..8]
        } else {
            &device.device_id
        };
        println!(
            "  {} [{}] id={} — enrolled {} — {}",
            device.name, status, id_short, device.enrolled_at, device.public_key
        );
    }

    Ok(())
}

// ── `tcfs device revoke` ─────────────────────────────────────────────────────

fn cmd_device_revoke(name: &str) -> Result<()> {
    let registry_path = tcfs_secrets::device::default_registry_path();
    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;

    if registry.revoke(name) {
        registry.save(&registry_path)?;
        println!("Revoked device: {}", name);
    } else {
        anyhow::bail!("Device '{}' not found", name);
    }

    Ok(())
}

// ── `tcfs device enroll` ──────────────────────────────────────────────────────

fn cmd_device_enroll(name: Option<String>) -> Result<()> {
    let device_name = name.unwrap_or_else(tcfs_secrets::device::default_device_name);
    let registry_path = tcfs_secrets::device::default_registry_path();
    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;

    if registry.find(&device_name).is_some() {
        anyhow::bail!(
            "Device '{}' is already enrolled. Use 'tcfs device list' to see devices.",
            device_name
        );
    }

    let public_key = format!(
        "age1-device-{}",
        &blake3::hash(device_name.as_bytes()).to_hex().as_str()[..8]
    );
    let device_id = registry.enroll(&device_name, &public_key, None);
    registry.save(&registry_path)?;

    println!("Device enrolled:");
    println!("  name:      {}", device_name);
    println!("  device_id: {}", device_id);
    println!("  registry:  {}", registry_path.display());
    println!();
    println!("Next: configure sync in tcfs.toml and run 'tcfs push'");

    Ok(())
}

// ── `tcfs device status` ─────────────────────────────────────────────────────

fn cmd_device_status() -> Result<()> {
    let registry_path = tcfs_secrets::device::default_registry_path();
    let registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;

    let hostname = tcfs_secrets::device::default_device_name();
    match registry.find(&hostname) {
        Some(device) => {
            println!("This device: {}", device.name);
            println!("  device_id:       {}", device.device_id);
            println!("  public_key:      {}", device.public_key);
            println!("  signing_key:     {}", device.signing_key_hash);
            println!("  enrolled_at:     {}", device.enrolled_at);
            println!("  revoked:         {}", device.revoked);
            println!("  last_nats_seq:   {}", device.last_nats_seq);
            if let Some(ref desc) = device.description {
                println!("  description:     {}", desc);
            }
        }
        None => {
            println!("This device ({}) is not enrolled.", hostname);
            println!("Run 'tcfs device enroll' to register it.");
        }
    }

    Ok(())
}

// ── `tcfs auth unlock` / `tcfs auth lock` ────────────────────────────────────

#[cfg(unix)]
async fn cmd_auth_unlock(
    config: &tcfs_core::config::TcfsConfig,
    key_file: Option<&Path>,
    passphrase_file: Option<&Path>,
) -> Result<()> {
    let key_bytes = if let Some(pf) = passphrase_file {
        // Derive key from passphrase file using Argon2id with per-vault salt
        let passphrase = std::fs::read_to_string(pf)
            .with_context(|| format!("reading passphrase file: {}", pf.display()))?;
        let passphrase = passphrase.trim();
        let salt = config
            .crypto
            .kdf_salt
            .as_deref()
            .and_then(|s| {
                (0..s.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .ok()
            })
            .and_then(|b| <[u8; 16]>::try_from(b).ok())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "crypto.kdf_salt not configured — required for passphrase-based key derivation"
                )
            })?;
        let mk = tcfs_crypto::recovery::derive_from_passphrase(passphrase, &salt)
            .context("deriving key from passphrase")?;
        mk.as_bytes().to_vec()
    } else {
        // Resolve master key file path
        let key_path = key_file
            .map(|p| p.to_path_buf())
            .or_else(|| config.crypto.master_key_file.clone())
            .unwrap_or_else(|| {
                tcfs_secrets::device::default_registry_path()
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("master.key")
            });

        let bytes = std::fs::read(&key_path)
            .with_context(|| format!("reading master key: {}", key_path.display()))?;

        if bytes.len() != tcfs_crypto::KEY_SIZE {
            anyhow::bail!(
                "master key file has wrong size: {} bytes (expected {})",
                bytes.len(),
                tcfs_crypto::KEY_SIZE
            );
        }
        bytes
    };

    // Send to daemon via gRPC
    let mut client = connect_daemon(&config.daemon.socket).await?;
    let resp = client
        .auth_unlock(tcfs_core::proto::AuthUnlockRequest {
            master_key: key_bytes,
        })
        .await
        .context("auth_unlock RPC failed")?
        .into_inner();

    if resp.success {
        println!("Encryption unlocked. Master key loaded into daemon.");
        println!("Run 'tcfs auth lock' to clear it from memory.");
    } else {
        anyhow::bail!("unlock failed: {}", resp.error);
    }

    Ok(())
}

#[cfg(unix)]
async fn cmd_auth_lock(config: &tcfs_core::config::TcfsConfig) -> Result<()> {
    // Clear from daemon
    let mut client = connect_daemon(&config.daemon.socket).await?;
    let resp = client
        .auth_lock(tcfs_core::proto::Empty {})
        .await
        .context("auth_lock RPC failed")?
        .into_inner();

    if !resp.success {
        anyhow::bail!("lock failed: {}", resp.error);
    }

    // Clear from platform keychain too
    let _ = tcfs_secrets::keychain::delete_secret(tcfs_secrets::keychain::keys::SESSION_TOKEN);
    let _ = tcfs_secrets::keychain::delete_secret(tcfs_secrets::keychain::keys::MASTER_KEY);

    println!("Session locked. Master key cleared from daemon and keychain.");
    Ok(())
}

#[cfg(unix)]
async fn cmd_auth_status(config: &tcfs_core::config::TcfsConfig) -> Result<()> {
    let mut client = connect_daemon(&config.daemon.socket).await?;
    let resp = client
        .auth_status(tcfs_core::proto::Empty {})
        .await
        .context("auth_status RPC failed")?
        .into_inner();

    if resp.crypto_enabled {
        if resp.unlocked {
            println!("Encryption: ACTIVE (master key loaded in daemon)");
        } else {
            println!("Encryption: LOCKED (configured but key not loaded)");
            println!("Run 'tcfs auth unlock' to load the master key.");
        }
    } else {
        println!("Encryption: DISABLED (crypto.enabled = false in config)");
    }

    // Show auth method and available methods
    if !resp.auth_method.is_empty() {
        println!("Auth method: {}", resp.auth_method);
    }
    if !resp.available_methods.is_empty() {
        println!("Available methods: {}", resp.available_methods.join(", "));
    }
    if !resp.session_device_id.is_empty() {
        println!("Device: {}", resp.session_device_id);
    }

    // Show session requirement from config
    if config.auth.require_session {
        println!("Session required: YES (protected RPCs need 'tcfs auth verify')");
    } else {
        println!("Session required: no (alpha bypass mode)");
    }

    Ok(())
}

// ── `tcfs auth enroll` ────────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_auth_enroll(config: &tcfs_core::config::TcfsConfig, method: &str) -> Result<()> {
    let mut client = connect_daemon(&config.daemon.socket).await?;

    // Get device ID from daemon status
    let status = client
        .status(tonic::Request::new(tcfs_core::proto::StatusRequest {}))
        .await
        .context("status RPC failed")?
        .into_inner();

    let resp = client
        .auth_enroll(tcfs_core::proto::AuthEnrollRequest {
            device_id: status.device_id.clone(),
            method: method.to_string(),
        })
        .await
        .context("auth_enroll RPC failed")?
        .into_inner();

    if !resp.success {
        anyhow::bail!("enrollment failed: {}", resp.error);
    }

    // Parse registration data (JSON with secret, qr_uri, qr_svg)
    if let Ok(reg) = serde_json::from_slice::<serde_json::Value>(&resp.registration_data) {
        if let Some(uri) = reg.get("qr_uri").and_then(|v| v.as_str()) {
            println!("TOTP enrolled for device '{}'", status.device_id);
            println!();
            println!("Scan this URI with your authenticator app:");
            println!("  {uri}");
            println!();
            println!("Or add the secret manually:");
            if let Some(secret) = reg.get("secret").and_then(|v| v.as_str()) {
                println!("  Secret: {secret}");
            }
        }
    }

    if !resp.instructions.is_empty() {
        println!();
        println!("{}", resp.instructions);
    }

    println!();
    println!("Verify enrollment: tcfs auth verify <6-digit-code>");
    Ok(())
}

// ── `tcfs auth complete-enroll` ───────────────────────────────────────────

#[cfg(unix)]
async fn cmd_auth_complete_enroll(
    config: &tcfs_core::config::TcfsConfig,
    method: &str,
    attestation_file: &std::path::Path,
) -> Result<()> {
    let attestation_data = std::fs::read(attestation_file).with_context(|| {
        format!(
            "failed to read attestation file: {}",
            attestation_file.display()
        )
    })?;

    let mut client = connect_daemon(&config.daemon.socket).await?;
    let resp = client
        .auth_complete_enroll(tcfs_core::proto::AuthCompleteEnrollRequest {
            device_id: String::new(), // daemon uses its own device_id
            method: method.to_string(),
            attestation_data,
        })
        .await
        .context("auth_complete_enroll RPC failed")?
        .into_inner();

    if resp.success {
        println!("Enrollment completed successfully for method '{method}'.");
    } else {
        anyhow::bail!("enrollment completion failed: {}", resp.error);
    }

    Ok(())
}

// ── `tcfs auth verify` ───────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_auth_verify(config: &tcfs_core::config::TcfsConfig, code: &str) -> Result<()> {
    let mut client = connect_daemon(&config.daemon.socket).await?;

    // Get device ID
    let status = client
        .status(tonic::Request::new(tcfs_core::proto::StatusRequest {}))
        .await
        .context("status RPC failed")?
        .into_inner();

    // Request challenge (TOTP challenges are time-based, so data is empty)
    let challenge = client
        .auth_challenge(tcfs_core::proto::AuthChallengeRequest {
            device_id: status.device_id.clone(),
            method: "totp".into(),
        })
        .await
        .context("auth_challenge RPC failed")?
        .into_inner();

    // Submit verification
    let resp = client
        .auth_verify(tcfs_core::proto::AuthVerifyRequest {
            challenge_id: challenge.challenge_id,
            device_id: status.device_id.clone(),
            data: code.as_bytes().to_vec(),
        })
        .await
        .context("auth_verify RPC failed")?
        .into_inner();

    if resp.success {
        println!("Authentication successful.");
        println!(
            "Session token: {}...",
            &resp.session_token[..8.min(resp.session_token.len())]
        );
    } else {
        anyhow::bail!("verification failed: {}", resp.error);
    }

    Ok(())
}

// ── `tcfs auth revoke` ───────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_auth_revoke(
    config: &tcfs_core::config::TcfsConfig,
    token: Option<&str>,
    device: Option<&str>,
) -> Result<()> {
    let mut client = connect_daemon(&config.daemon.socket).await?;
    let resp = client
        .auth_revoke(tcfs_core::proto::AuthRevokeRequest {
            session_token: token.unwrap_or_default().to_string(),
            device_id: device.unwrap_or_default().to_string(),
        })
        .await
        .context("auth_revoke RPC failed")?
        .into_inner();

    if resp.success {
        if let Some(t) = token {
            println!("Session {}... revoked.", &t[..8.min(t.len())]);
        } else if let Some(d) = device {
            println!("All sessions for device '{d}' revoked.");
        }
    } else {
        anyhow::bail!("revocation failed: {}", resp.error);
    }

    Ok(())
}

// ── `tcfs device invite` ─────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_device_invite(
    config: &tcfs_core::config::TcfsConfig,
    expiry_hours: u64,
    render_qr: bool,
) -> Result<()> {
    use tcfs_auth::enrollment::EnrollmentInvite;
    use tcfs_auth::session::DevicePermissions;

    // Get device ID from daemon
    let mut client = connect_daemon(&config.daemon.socket).await?;
    let status = client
        .status(tonic::Request::new(tcfs_core::proto::StatusRequest {}))
        .await
        .context("status RPC failed")?
        .into_inner();

    // Load master key for signing
    let key_path = config.crypto.master_key_file.clone().unwrap_or_else(|| {
        tcfs_secrets::device::default_registry_path()
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("master.key")
    });

    let signing_key = if key_path.exists() {
        let key_bytes = std::fs::read(&key_path)
            .with_context(|| format!("reading master key: {}", key_path.display()))?;
        if key_bytes.len() != tcfs_crypto::KEY_SIZE {
            anyhow::bail!(
                "master key has wrong size: {} bytes (expected {})",
                key_bytes.len(),
                tcfs_crypto::KEY_SIZE,
            );
        }
        *blake3::hash(&key_bytes).as_bytes()
    } else {
        eprintln!(
            "Warning: master key not found at {}, using placeholder signing key",
            key_path.display()
        );
        *blake3::hash(b"tcfs-fleet-invite-key").as_bytes()
    };

    let mut invite = EnrollmentInvite::new(
        &status.device_id,
        &signing_key,
        expiry_hours,
        DevicePermissions::default(),
    );

    // Include storage credentials for credential brokering
    invite.storage_endpoint = Some(config.storage.endpoint.clone());
    invite.storage_bucket = Some(config.storage.bucket.clone());
    invite.remote_prefix = Some(String::from("default"));

    // Load S3 credentials from environment (sops-nix populates these)
    if let Ok(access_key) = std::env::var("AWS_ACCESS_KEY_ID").or_else(|_| {
        std::env::var("TCFS_S3_ACCESS_KEY_FILE")
            .and_then(|f| std::fs::read_to_string(f).map_err(|_| std::env::VarError::NotPresent))
    }) {
        invite.storage_access_key = Some(access_key);
    }
    if let Ok(secret_key) = std::env::var("AWS_SECRET_ACCESS_KEY").or_else(|_| {
        std::env::var("TCFS_S3_SECRET_KEY_FILE")
            .and_then(|f| std::fs::read_to_string(f).map_err(|_| std::env::VarError::NotPresent))
    }) {
        invite.storage_secret_key = Some(secret_key);
    }

    // Include encryption config if enabled
    if config.crypto.enabled {
        if let Ok(passphrase) = std::env::var("TCFS_ENCRYPTION_KEY_FILE")
            .and_then(|f| std::fs::read_to_string(f).map_err(|_| std::env::VarError::NotPresent))
        {
            invite.encryption_passphrase = Some(passphrase);
        }
    }

    // Use compact encoding (short keys + zstd) for QR-friendly payloads
    let compact = invite
        .encode_compact()
        .context("failed to compact-encode invite")?;
    let full = invite.encode().context("failed to encode invite")?;
    let deep_link = format!("tcfs://enroll?data={compact}");

    println!("Device enrollment invite created");
    println!();
    println!("Expires: {} hours from now", expiry_hours);
    println!(
        "Storage: {} (bucket: {})",
        config.storage.endpoint, config.storage.bucket
    );
    if invite.storage_access_key.is_some() {
        println!("Credentials: included (S3 access key brokered)");
    } else {
        println!("Credentials: NOT included (set AWS_ACCESS_KEY_ID or TCFS_S3_ACCESS_KEY_FILE)");
    }
    println!(
        "Payload: {} bytes compact, {} bytes full",
        compact.len(),
        full.len()
    );
    println!();

    if render_qr {
        use qrcode::{render::unicode::Dense1x2, QrCode};
        let code = QrCode::new(deep_link.as_bytes())
            .context("QR code generation failed (payload may still be too large)")?;
        let qr_string = code
            .render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .build();
        println!("{qr_string}");
        println!();
        println!("Scan the QR code above with the TCFS iOS app.");
        println!("Deep link: {deep_link}");
    } else {
        println!("Share this invite data with the new device:");
        println!("  {compact}");
        println!();
        println!("Or use this deep link (iOS/macOS):");
        println!("  {deep_link}");
        println!();
        println!("Tip: use --qr to render a scannable QR code in the terminal.");
    }
    println!();
    println!("On the new device, run:");
    println!("  tcfs device enroll --invite <invite-data>");

    Ok(())
}

// ── `tcfs rotate-key` ─────────────────────────────────────────────────────

async fn cmd_rotate_key(
    config: &tcfs_core::config::TcfsConfig,
    old_key_file: Option<&Path>,
    use_password: bool,
    non_interactive: bool,
) -> Result<()> {
    use tcfs_crypto::{MasterKey, KEY_SIZE};

    // Step 1: Load old master key
    let key_path = old_key_file
        .map(|p| p.to_path_buf())
        .or_else(|| config.crypto.master_key_file.clone())
        .unwrap_or_else(|| {
            tcfs_secrets::device::default_registry_path()
                .parent()
                .unwrap_or(Path::new("."))
                .join("master.key")
        });

    let old_bytes = std::fs::read(&key_path)
        .with_context(|| format!("reading old master key: {}", key_path.display()))?;
    if old_bytes.len() != KEY_SIZE {
        anyhow::bail!(
            "old master key has wrong size: {} bytes (expected {})",
            old_bytes.len(),
            KEY_SIZE
        );
    }
    let mut old_key_bytes = [0u8; KEY_SIZE];
    old_key_bytes.copy_from_slice(&old_bytes);
    let old_master = MasterKey::from_bytes(old_key_bytes);

    println!("Old master key loaded from: {}", key_path.display());

    // Step 2: Generate new master key
    let new_master = if use_password {
        let passphrase =
            rpassword::prompt_password("New master passphrase: ").context("reading passphrase")?;
        let confirm =
            rpassword::prompt_password("Confirm passphrase: ").context("reading confirmation")?;
        if passphrase != confirm {
            anyhow::bail!("passphrases do not match");
        }

        println!("Deriving new master key from passphrase...");
        let salt: [u8; 16] = rand::random();
        tcfs_crypto::derive_master_key(
            &secrecy::SecretString::from(passphrase),
            &salt,
            &tcfs_crypto::kdf::KdfParams::default(),
        )?
    } else {
        let (mnemonic, master_key) = tcfs_crypto::generate_mnemonic()?;

        if non_interactive {
            println!("\nNew BIP-39 recovery mnemonic:");
            println!("{mnemonic}");
        } else {
            println!("\n{}", "=".repeat(60));
            println!("NEW RECOVERY MNEMONIC (write this down!):");
            println!("{}", "=".repeat(60));
            println!("\n  {mnemonic}\n");
            println!("{}", "=".repeat(60));
            println!("This mnemonic is the ONLY way to recover your new master key.");
            println!("Store it securely and NEVER share it.\n");

            let confirm = rpassword::prompt_password("Type 'ROTATE' to confirm key rotation: ")
                .context("reading confirmation")?;
            if confirm != "ROTATE" {
                anyhow::bail!("key rotation cancelled");
            }
        }
        master_key
    };

    // Step 3: Connect to storage and enumerate manifests
    let cred_store = tcfs_secrets::CredStore::load(&config.secrets, &config.storage)
        .await
        .context("loading credentials for S3 access")?;

    let s3 = cred_store
        .s3
        .as_ref()
        .context("no S3 credentials available")?;

    let op = tcfs_storage::operator::build_from_core_config(
        &config.storage,
        &s3.access_key_id,
        s3.secret_access_key.expose_secret(),
    )?;

    let manifest_prefix = format!("{}/manifests/", config.storage.bucket);
    println!("Scanning manifests at: {manifest_prefix}");

    let entries = op
        .list(&manifest_prefix)
        .await
        .context("listing manifests from S3")?;

    let mut rotated = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    for entry in entries {
        let path = entry.path().to_string();
        if entry.metadata().is_dir() {
            continue;
        }

        // Read manifest
        let data = match op.read(&path).await {
            Ok(d) => d.to_bytes(),
            Err(e) => {
                eprintln!("  WARN: failed to read {path}: {e}");
                errors += 1;
                continue;
            }
        };

        let mut manifest: tcfs_sync::manifest::SyncManifest =
            match tcfs_sync::manifest::SyncManifest::from_bytes(&data) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("  WARN: failed to parse {path}: {e}");
                    errors += 1;
                    continue;
                }
            };

        // Only rotate manifests that have wrapped file keys
        let wrapped_b64 = match &manifest.encrypted_file_key {
            Some(k) => k.clone(),
            None => {
                skipped += 1;
                continue;
            }
        };

        // Unwrap with old key, re-wrap with new key
        let wrapped_bytes = base64::engine::general_purpose::STANDARD
            .decode(&wrapped_b64)
            .context("decoding wrapped file key")?;

        let file_key = match tcfs_crypto::unwrap_key(&old_master, &wrapped_bytes) {
            Ok(fk) => fk,
            Err(e) => {
                eprintln!("  WARN: unwrap failed for {path}: {e}");
                errors += 1;
                continue;
            }
        };

        let new_wrapped = tcfs_crypto::wrap_key(&new_master, &file_key)?;
        let new_wrapped_b64 = base64::engine::general_purpose::STANDARD.encode(&new_wrapped);

        manifest.encrypted_file_key = Some(new_wrapped_b64);

        // Write back
        let new_data = serde_json::to_vec(&manifest).context("serializing rotated manifest")?;
        op.write(&path, new_data)
            .await
            .with_context(|| format!("writing rotated manifest: {path}"))?;

        rotated += 1;
    }

    // Step 4: Write new master key file
    std::fs::write(&key_path, new_master.as_bytes())
        .with_context(|| format!("writing new master key: {}", key_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }

    println!("\nKey rotation complete:");
    println!("  Manifests rotated: {rotated}");
    println!("  Manifests skipped (plaintext): {skipped}");
    if errors > 0 {
        println!("  Errors: {errors}");
    }
    println!("  New master key: {}", key_path.display());

    // Step 5: Notify daemon to reload if running
    #[cfg(unix)]
    if let Ok(mut client) = connect_daemon(&config.daemon.socket).await {
        let key_bytes = std::fs::read(&key_path)?;
        let _ = client
            .auth_unlock(tcfs_core::proto::AuthUnlockRequest {
                master_key: key_bytes,
            })
            .await;
        println!("  Daemon notified with new key.");
    }

    Ok(())
}

// ── `tcfs rotate-credentials` ─────────────────────────────────────────────

async fn cmd_rotate_credentials(
    config: &tcfs_core::config::TcfsConfig,
    cred_file_override: Option<&Path>,
    non_interactive: bool,
) -> Result<()> {
    // Resolve the credential file path
    let cred_file = cred_file_override
        .map(|p| p.to_path_buf())
        .or_else(|| config.storage.credentials_file.clone())
        .context(
            "No credential file configured.\n\
             Use --cred-file or set storage.credentials_file in config.toml",
        )?;

    if !cred_file.exists() {
        anyhow::bail!("credential file not found: {}", cred_file.display());
    }

    // Get new credentials
    let (new_access_key, new_secret_key) = if non_interactive {
        let ak = std::env::var("AWS_ACCESS_KEY_ID")
            .or_else(|_| std::env::var("TCFS_NEW_ACCESS_KEY"))
            .context(
                "Non-interactive mode requires AWS_ACCESS_KEY_ID or TCFS_NEW_ACCESS_KEY env var",
            )?;
        let sk = std::env::var("AWS_SECRET_ACCESS_KEY")
            .or_else(|_| std::env::var("TCFS_NEW_SECRET_KEY"))
            .context(
                "Non-interactive mode requires AWS_SECRET_ACCESS_KEY or TCFS_NEW_SECRET_KEY env var",
            )?;
        (ak, sk)
    } else {
        println!("Rotating S3 credentials in: {}", cred_file.display());
        println!();
        let ak = rpassword::prompt_password("New Access Key ID: ")
            .context("failed to read access key")?;
        let sk = rpassword::prompt_password("New Secret Access Key: ")
            .context("failed to read secret key")?;

        if ak.is_empty() || sk.is_empty() {
            anyhow::bail!("Access key and secret key must not be empty");
        }
        (ak, sk)
    };

    println!("Rotating credentials...");

    let result = tcfs_secrets::rotate::rotate_s3_credentials(
        &cred_file,
        &new_access_key,
        &new_secret_key,
        None, // No watcher channel in CLI mode
    )
    .await
    .context("credential rotation failed")?;

    println!();
    println!("Credentials rotated successfully.");
    println!("  file:     {}", result.path.display());
    println!("  time:     {}", result.rotated_at);
    if result.backup_created {
        println!(
            "  backup:   {}.bak.{}",
            result.path.display(),
            result.rotated_at
        );
    }
    println!();
    println!("Next steps:");
    println!("  1. Verify tcfsd reloaded: journalctl -u tcfsd --since '1 min ago' | grep reload");
    println!("  2. Test storage: tcfs status");
    println!("  3. Deactivate old credentials on the S3/SeaweedFS admin console");

    Ok(())
}

// ── Interactive conflict resolver ──────────────────────────────────────────

// ── `tcfs policy` ────────────────────────────────────────────────────────────

async fn cmd_policy(config: &tcfs_core::config::TcfsConfig, action: PolicyAction) -> Result<()> {
    let policy_path = config
        .sync
        .sync_root
        .as_ref()
        .map(|r| r.join(".tcfs-policy.json"))
        .unwrap_or_else(|| PathBuf::from(".tcfs-policy.json"));

    let mut store = tcfs_sync::policy::PolicyStore::open(&policy_path).unwrap_or_default();

    match action {
        PolicyAction::Set { path, mode } => {
            let abs = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let sync_mode = match mode.as_str() {
                "always" => tcfs_sync::policy::SyncMode::Always,
                "never" => tcfs_sync::policy::SyncMode::Never,
                _ => tcfs_sync::policy::SyncMode::OnDemand,
            };
            let mut policy = store.get(&abs).cloned().unwrap_or_default();
            policy.sync_mode = sync_mode;
            store.set(&abs, policy);
            store.flush().context("saving policy")?;
            println!("Policy set: {} → {}", abs.display(), mode);
        }
        PolicyAction::Get { path } => {
            let abs = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            match store.get(&abs) {
                Some(policy) => {
                    println!("Policy for {}:", abs.display());
                    println!("  sync_mode: {:?}", policy.sync_mode);
                    if let Some(threshold) = policy.download_threshold {
                        println!("  download_threshold: {} bytes", threshold);
                    }
                    println!("  auto_unsync_exempt: {}", policy.auto_unsync_exempt);
                }
                None => println!(
                    "No policy set for {} (inherits default: on-demand)",
                    abs.display()
                ),
            }
        }
        PolicyAction::List => {
            let all = store.all();
            if all.is_empty() {
                println!("No policies configured.");
            } else {
                for (path, policy) in all {
                    println!(
                        "  {} → {:?}{}{}",
                        path,
                        policy.sync_mode,
                        if policy.auto_unsync_exempt {
                            " [pinned]"
                        } else {
                            ""
                        },
                        policy
                            .download_threshold
                            .map(|t| format!(" [threshold: {}B]", t))
                            .unwrap_or_default()
                    );
                }
            }
        }
        PolicyAction::Pin { path } => {
            let abs = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let mut policy = store.get(&abs).cloned().unwrap_or_default();
            policy.auto_unsync_exempt = true;
            store.set(&abs, policy);
            store.flush().context("saving policy")?;
            println!("Pinned: {} (exempt from auto-unsync)", abs.display());
        }
        PolicyAction::Unpin { path } => {
            let abs = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let mut policy = store.get(&abs).cloned().unwrap_or_default();
            policy.auto_unsync_exempt = false;
            store.set(&abs, policy);
            store.flush().context("saving policy")?;
            println!("Unpinned: {}", abs.display());
        }
    }
    Ok(())
}

// ── `tcfs reconcile` ─────────────────────────────────────────────────────────

async fn cmd_reconcile(
    config: &tcfs_core::config::TcfsConfig,
    path: Option<&Path>,
    prefix: Option<&str>,
    execute: bool,
    state_override: Option<&Path>,
) -> Result<()> {
    let op = build_operator_from_env(config)?;
    let device_id = load_device_id(config);

    let local_root = path
        .map(|p| p.to_path_buf())
        .or_else(|| config.sync.sync_root.clone())
        .ok_or_else(|| anyhow::anyhow!("no path specified and no sync_root in config"))?;

    let remote_prefix = prefix.map(|s| s.to_string()).unwrap_or_else(|| {
        config
            .storage
            .remote_prefix
            .clone()
            .unwrap_or_else(|| config.storage.bucket.clone())
    });

    let state_path = resolve_state_path(config, state_override);
    let state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    let blacklist = tcfs_sync::blacklist::Blacklist::from_sync_config(&config.sync);
    let reconcile_config = tcfs_sync::reconcile::ReconcileConfig::default();

    println!(
        "Reconciling {} ↔ {}:{}/",
        local_root.display(),
        config.storage.endpoint,
        remote_prefix
    );

    let plan = tcfs_sync::reconcile::reconcile(
        &op,
        &local_root,
        &remote_prefix,
        &state,
        &device_id,
        &blacklist,
        &reconcile_config,
    )
    .await
    .context("reconciliation failed")?;

    // Display plan
    println!();
    println!(
        "Plan: {} push, {} pull, {} delete-local, {} delete-remote, {} conflict, {} up-to-date",
        plan.summary.pushes,
        plan.summary.pulls,
        plan.summary.local_deletes,
        plan.summary.remote_deletes,
        plan.summary.conflicts,
        plan.summary.up_to_date
    );

    if plan.actions.is_empty() {
        println!("Nothing to do — local and remote are in sync.");
        return Ok(());
    }

    for action in &plan.actions {
        match action {
            tcfs_sync::reconcile::ReconcileAction::Push {
                rel_path, reason, ..
            } => println!("  → push  {rel_path}  ({reason:?})"),
            tcfs_sync::reconcile::ReconcileAction::Pull {
                rel_path,
                reason,
                size,
                ..
            } => println!("  ← pull  {rel_path}  ({reason:?}, {size} bytes)"),
            tcfs_sync::reconcile::ReconcileAction::DeleteLocal { rel_path, .. } => {
                println!("  ✗ delete-local  {rel_path}")
            }
            tcfs_sync::reconcile::ReconcileAction::DeleteRemote { rel_path } => {
                println!("  ✗ delete-remote  {rel_path}")
            }
            tcfs_sync::reconcile::ReconcileAction::Conflict { rel_path, info } => {
                println!(
                    "  ! conflict  {rel_path}  (local: {}, remote: {})",
                    info.local_device, info.remote_device
                )
            }
            tcfs_sync::reconcile::ReconcileAction::UpToDate { rel_path } => {
                println!("  = up-to-date  {rel_path}")
            }
        }
    }

    if !execute {
        println!();
        println!("Dry run — no changes made. Use --execute to apply.");
        return Ok(());
    }

    // Execute the plan
    println!();
    println!("Executing plan...");

    let mut state = tcfs_sync::state::StateCache::open(&state_path)?;

    let master_key = config
        .crypto
        .master_key_file
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .filter(|k| k.len() == 32)
        .map(|bytes| {
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            tcfs_crypto::MasterKey::from_bytes(key)
        });
    let enc_ctx = master_key
        .as_ref()
        .map(|mk| tcfs_sync::engine::EncryptionContext {
            master_key: mk.clone(),
        });

    let result = tcfs_sync::reconcile::execute_plan(
        &plan,
        &op,
        &local_root,
        &remote_prefix,
        &mut state,
        &device_id,
        enc_ctx.as_ref(),
        None,
    )
    .await
    .context("executing reconciliation plan")?;

    state.flush().context("flushing state cache")?;

    println!(
        "Done: {} pushed, {} pulled, {} deleted, {} conflicts, {} errors",
        result.pushed,
        result.pulled,
        result.deleted_local + result.deleted_remote,
        result.conflicts_recorded,
        result.errors.len()
    );

    for (path, err) in &result.errors {
        eprintln!("  error: {path}: {err}");
    }

    Ok(())
}

// ── `tcfs resolve` ───────────────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_resolve(
    config: &tcfs_core::config::TcfsConfig,
    path: &Path,
    strategy: Option<&str>,
) -> Result<()> {
    let resolution = match strategy {
        Some(s) => s.replace('-', "_"),
        None => {
            // Interactive mode: reuse the existing interactive resolver
            let dummy_info = tcfs_sync::conflict::ConflictInfo {
                rel_path: path.to_string_lossy().to_string(),
                local_blake3: String::new(),
                remote_blake3: String::new(),
                local_device: "local".to_string(),
                remote_device: "remote".to_string(),
                local_vclock: tcfs_sync::conflict::VectorClock::new(),
                remote_vclock: tcfs_sync::conflict::VectorClock::new(),
                detected_at: 0,
            };
            match resolve_conflict_interactive(&dummy_info) {
                tcfs_sync::conflict::Resolution::KeepLocal => "keep_local".to_string(),
                tcfs_sync::conflict::Resolution::KeepRemote => "keep_remote".to_string(),
                tcfs_sync::conflict::Resolution::KeepBoth => "keep_both".to_string(),
                tcfs_sync::conflict::Resolution::Defer => {
                    println!("Conflict deferred.");
                    return Ok(());
                }
            }
        }
    };

    // Call daemon's ResolveConflict gRPC
    let mut client = connect_daemon(&config.daemon.socket).await?;
    let resp = client
        .resolve_conflict(tonic::Request::new(
            tcfs_core::proto::ResolveConflictRequest {
                path: path.to_string_lossy().to_string(),
                resolution: resolution.clone(),
            },
        ))
        .await
        .context("resolve_conflict RPC failed")?
        .into_inner();

    if resp.success {
        println!("Conflict resolved ({}): {}", resolution, path.display());
        if !resp.resolved_path.is_empty() && resp.resolved_path != path.to_string_lossy() {
            println!("  Conflict copy: {}", resp.resolved_path);
        }
    } else {
        anyhow::bail!("resolution failed: {}", resp.error);
    }

    Ok(())
}

/// Prompt the user to resolve a sync conflict interactively.
#[cfg(unix)]
fn resolve_conflict_interactive(
    info: &tcfs_sync::conflict::ConflictInfo,
) -> tcfs_sync::conflict::Resolution {
    println!();
    println!("CONFLICT DETECTED: {}", info.rel_path);
    println!("  Local device:    {}", info.local_device);
    println!(
        "  Local hash:      {}",
        &info.local_blake3[..16.min(info.local_blake3.len())]
    );
    println!("  Remote device:   {}", info.remote_device);
    println!(
        "  Remote hash:     {}",
        &info.remote_blake3[..16.min(info.remote_blake3.len())]
    );
    println!();
    println!("  [K]eep local / [R]emote / [B]oth / [D]efer?");

    loop {
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            return tcfs_sync::conflict::Resolution::Defer;
        }
        match input.trim().to_lowercase().as_str() {
            "k" | "keep" | "local" => return tcfs_sync::conflict::Resolution::KeepLocal,
            "r" | "remote" => return tcfs_sync::conflict::Resolution::KeepRemote,
            "b" | "both" => return tcfs_sync::conflict::Resolution::KeepBoth,
            "d" | "defer" => return tcfs_sync::conflict::Resolution::Defer,
            _ => {
                println!("  Please enter K, R, B, or D:");
            }
        }
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────

fn fmt_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
