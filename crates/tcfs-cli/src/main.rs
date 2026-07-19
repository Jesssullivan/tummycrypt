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
//!   index inspect <path>         - inspect one remote index entry read-only

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use secrecy::ExposeSecret;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tcfs_core::config::sanitize_http_endpoint_for_display;

#[cfg(unix)]
use tonic::metadata::MetadataValue;
#[cfg(unix)]
use tonic::service::{interceptor::InterceptedService, Interceptor};
#[cfg(unix)]
use tonic::transport::{Channel, Endpoint, Uri};
#[cfg(unix)]
use tower::service_fn;

#[cfg(unix)]
use tcfs_core::proto::{tcfs_daemon_client::TcfsDaemonClient, Empty, StatusRequest};

#[cfg(unix)]
type DaemonClient = TcfsDaemonClient<InterceptedService<Channel, SessionTokenInterceptor>>;

#[cfg(unix)]
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const DAEMON_RPC_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const SESSION_TOKEN_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
#[derive(Clone, Debug)]
struct SessionTokenInterceptor {
    token: Option<String>,
}

#[cfg(unix)]
impl Interceptor for SessionTokenInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(token) = self.token.as_deref().filter(|token| !token.is_empty()) {
            let value = format!("Bearer {token}")
                .parse::<MetadataValue<_>>()
                .map_err(|_| {
                    tonic::Status::unauthenticated(
                        "stored TCFS session token is not valid metadata",
                    )
                })?;
            request.metadata_mut().insert("authorization", value);
        }
        Ok(request)
    }
}

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
        /// Path to the sync state cache JSON file (overrides config).
        /// `.db` paths are normalized to their `.json` sibling — the file the
        /// daemon owns — so the CLI and daemon always act on the same cache.
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
        /// Path to the sync state cache JSON file (overrides config).
        /// `.db` paths are normalized to their `.json` sibling — the file the
        /// daemon owns — so the CLI and daemon always act on the same cache.
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Show local sync state for a file or directory
    #[command(name = "sync-status")]
    SyncStatus {
        /// Path to check (default: current directory)
        path: Option<PathBuf>,
        /// Path to the sync state cache JSON file (overrides config).
        /// `.db` paths are normalized to their `.json` sibling — the file the
        /// daemon owns — so the CLI and daemon always act on the same cache.
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Inspect remote index entries without changing storage
    Index {
        #[command(subcommand)]
        action: IndexAction,
    },

    /// Storage posture checks
    Storage {
        #[command(subcommand)]
        action: StorageAction,
    },

    // ── Phase 3: mount + stub management ──────────────────────────────────────
    /// Mount a remote as a local directory
    Mount {
        /// Remote spec (e.g. seaweedfs://host/bucket[/prefix] or seaweedfs+https://host/bucket[/prefix])
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
        /// Check whether first-run identity and user config are present
        #[arg(long)]
        check: bool,
        /// Do not write ~/.config/tcfs/config.toml
        #[arg(long)]
        skip_config: bool,
        /// Overwrite an existing init config file
        #[arg(long)]
        force_config: bool,
        /// Config path to write/check (default: ~/.config/tcfs/config.toml)
        #[arg(long)]
        config_out: Option<PathBuf>,
        /// Optional FileProvider bootstrap JSON path to write for macOS HostApp provisioning
        #[arg(long)]
        fileprovider_config_out: Option<PathBuf>,
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
        /// Path to a file holding the EXACT new master key: 32 raw bytes, or
        /// 64 hex chars (optional trailing newline). Bypasses mnemonic/passphrase
        /// generation — for externally derived keys (TIN-2856).
        #[arg(long, conflicts_with = "password")]
        new_key_file: Option<PathBuf>,
        /// Non-interactive mode (generate and print mnemonic without prompt)
        #[arg(long)]
        non_interactive: bool,
    },

    /// Scoped, per-device-aware FileKey rotation (TIN-1899 / B2 forward secrecy)
    ///
    /// SEPARATE from `rotate-key` (which rotates the shared MASTER key and only
    /// re-wraps master-wrapped manifests). `key rotate <prefix>` generates a
    /// FRESH FileKey for every manifest under <prefix>, re-encrypts the content
    /// under new BLAKE3 content addresses, and re-wraps to the CURRENT
    /// (post-revocation) recipient set — so a revoked device that is absent from
    /// the recipient set can no longer decrypt the re-keyed content.
    Key {
        #[command(subcommand)]
        action: KeyAction,
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
        /// Path to the sync state cache JSON file (overrides config).
        /// `.db` paths are normalized to their `.json` sibling — the file the
        /// daemon owns — so the CLI and daemon always act on the same cache.
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
        /// Path to the sync state cache JSON file (overrides config).
        /// `.db` paths are normalized to their `.json` sibling — the file the
        /// daemon owns — so the CLI and daemon always act on the same cache.
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Resolve a sync conflict for a file or git repo group
    ///
    /// Repository-group Git keep-both is available through a daemon-owned
    /// root. Ordinary per-file mutation is retired until it has equivalent
    /// root and manifest identity; `defer` remains a no-op.
    Resolve {
        /// Path to the conflicted file, or a git repo root for repo-group keep-both
        path: PathBuf,
        /// Stable daemon-enrolled root identity. The daemon selects and fences
        /// this root's local path, remote prefix, state cache, and policy.
        /// Named-root dry-run requires pull permission; execute requires both
        /// pull and push. Inspect-only roots permit dry-run but reject execute.
        #[arg(long)]
        root: Option<String>,
        /// Resolution strategy: keep-both for a Git repo, or defer for an
        /// ordinary file. Repo dry-run requires pull; execute requires pull
        /// and push. Ordinary-file mutation is disabled fail-closed.
        #[arg(long, short = 's', value_parser = ["keep-both", "defer"])]
        strategy: Option<String>,
        /// Execute repo-group git keep-both. Without this flag, repo mode is a
        /// dry-run. Execute requires both pull and push permission.
        #[arg(long)]
        execute: bool,
    },

    /// List recorded sync conflicts (read-only)
    ///
    /// Primary/legacy inspection reads the local cache directly. `--root`
    /// asks the daemon to select the enrolled root's isolated cache. Neither
    /// mode mutates conflict state. `.git`-internal conflicts are grouped by
    /// their enclosing repository; non-`.git` conflicts are listed flat.
    Conflicts {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
        /// Stable daemon-enrolled root identity. Named-root inspection uses the
        /// daemon RPC so it reads the same isolated cache as `tcfs resolve`.
        #[arg(long, conflicts_with = "state")]
        root: Option<String>,
        /// Path to the sync state cache JSON file (overrides config).
        /// `.db` paths are normalized to their `.json` sibling — the file the
        /// daemon owns — so the CLI and daemon always act on the same cache.
        /// This legacy option is read-only; named roots never accept a client
        /// state path.
        #[arg(long, env = "TCFS_STATE_PATH")]
        state: Option<PathBuf>,
    },

    /// Manage the sync trash (staged deletes)
    ///
    /// When trash is enabled, deletion writes an immutable safety copy and
    /// conditionally tombstones the live index. Purge is logical: evidence is
    /// retained for reachability-safe GC.
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },

    /// Migrate S3 index entries from stale/incorrect prefixes
    ///
    /// Copies double-prefixed entries (data/index/data/*) and orphaned entries
    /// under old prefixes (tcfs/index/*). Double-prefixed sources are logically
    /// tombstoned after an exact copy; orphan-prefix sources are retained.
    #[command(name = "migrate-prefix")]
    MigratePrefix {
        /// Dry-run mode (show what would be migrated without changing anything)
        #[arg(long)]
        dry_run: bool,
        /// Assert that every old and new TCFS writer for these prefixes is stopped
        #[arg(long)]
        writers_quiesced: bool,
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
enum IndexAction {
    /// Inspect one logical path in the remote index
    Inspect {
        /// Logical relative path under the remote prefix
        rel_path: String,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum StorageAction {
    /// Write, read, delete, and verify one scoped canary object
    Canary {
        /// Remote prefix override (default: storage.remote_prefix or bucket)
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        /// Prefix that must reject canary writes with PermissionDenied
        #[arg(long, value_name = "PREFIX")]
        expect_deny_prefix: Option<String>,
        /// Per-operation timeout in seconds
        #[arg(long, default_value = "5")]
        timeout_secs: u64,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize)]
struct StorageCanaryReport {
    endpoint: String,
    bucket: String,
    prefix: String,
    key: String,
    list_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_deny: Option<StorageCanaryScopeDenyReport>,
    bytes: usize,
    write_ms: u128,
    list_ms: u128,
    list_count: usize,
    read_ms: u128,
    delete_ms: u128,
    verify_delete_ms: u128,
    listed: bool,
    deleted: bool,
    endpoint_tls: bool,
    enforce_tls: bool,
}

#[derive(Debug, Serialize)]
struct StorageCanaryScopeDenyReport {
    prefix: String,
    key: String,
    write_ms: u128,
    error_kind: String,
    denied: bool,
}

#[derive(Subcommand, Debug)]
enum DeviceAction {
    /// Enroll this device in the sync fleet
    Enroll {
        /// Device name (default: hostname)
        #[arg(long)]
        name: Option<String>,
        /// Replace an existing placeholder/legacy public key with a real age key
        #[arg(long)]
        repair_placeholder: bool,
        /// Merge the local registry with the storage-backed fleet registry
        #[arg(long)]
        sync_remote: bool,
        /// TIN-1417 B4 migration escape hatch: explicitly accept and re-sign an
        /// UNSIGNED (legacy) remote registry during `--sync-remote`. Without this
        /// flag an unsigned remote is REFUSED on the merge path, because merging
        /// then re-signing it with the local master would launder an
        /// attacker-injected recipient into a validly-signed registry. Only pass
        /// this when you trust the remote object store has not been tampered with.
        #[arg(long, requires = "sync_remote")]
        accept_unsigned_remote: bool,
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
enum KeyAction {
    /// Rotate FileKeys for a legacy manifest-only prefix (forward secrecy).
    ///
    /// Without `--rotate-keys` this is a dry-run that reports the projected
    /// bytes-to-rewrite. With `--rotate-keys` it generates fresh FileKeys,
    /// re-encrypts content under new BLAKE3 addresses, re-wraps to the current
    /// recipient set, publishes new manifests, and submits orphaned old chunks
    /// to generation-pinned GC. Unversioned backends retain those chunks.
    Rotate {
        /// Legacy remote prefix under which to rotate (e.g. `projects/secret`).
        /// Relative to the configured storage prefix; legacy manifests are
        /// scanned under `<storage_prefix>/manifests/<prefix>`. Indexed roots
        /// fail closed until index-first copy-on-write rotation is available.
        prefix: String,
        /// Actually perform the rotation. Without this flag the command only
        /// projects the bytes-to-rewrite and exits (safe dry-run).
        #[arg(long)]
        rotate_keys: bool,
        /// Resume an interrupted rotation from its `.rotate-state.json`.
        #[arg(long)]
        resume: bool,
        /// Skip the interactive confirmation prompt (for automation). Has no
        /// effect without `--rotate-keys`.
        #[arg(long)]
        non_interactive: bool,
        /// Make orphaned old chunks immediately eligible for GC (grace=0)
        /// instead of honoring `orphan_chunk_cleanup_grace_secs`. Exact
        /// generation-pinned deletion is still required; unversioned backends
        /// retain the chunks. Disabled for indexed/multi-writer roots. Use only
        /// on a proven single-writer legacy root when no reader can still need
        /// an old chunk.
        #[arg(long)]
        gc_immediate: bool,
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
        /// Exact generation key from `trash list` (required when generations are ambiguous)
        #[arg(long)]
        trash_key: Option<String>,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
    },
    /// Logically purge old trash entries while retaining recovery evidence
    Purge {
        /// Mark entries older than N seconds purged (default: trash_retention_secs)
        #[arg(long, conflicts_with = "all")]
        older_than: Option<u64>,
        /// Logically purge ALL visible trash entries regardless of age
        #[arg(long, conflicts_with = "older_than")]
        all: bool,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print a redacted diagnostic view (not suitable for config reuse)
    Show,
    /// Render the macOS FileProvider bootstrap JSON from the active config
    Fileprovider {
        /// Path to write (default: ~/.config/tcfs/fileprovider/config.json)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Device ID to place in the FileProvider bootstrap JSON
        #[arg(long)]
        device_id: Option<String>,
        /// Master key file path to hand to the HostApp for Keychain enrichment
        #[arg(long)]
        master_key_file: Option<PathBuf>,
        /// Overwrite an existing FileProvider config JSON
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum CacheAction {
    /// Show cache usage statistics
    Stats,
    /// Clear all cached content
    Clear,
    /// Evict one remote-backed file from the local hydrated-content cache
    Evict {
        /// Logical relative path under the remote prefix
        rel_path: String,
        /// Remote prefix override
        #[arg(long, short = 'p')]
        prefix: Option<String>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
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
        Commands::Config {
            action:
                ConfigAction::Fileprovider {
                    out,
                    device_id,
                    master_key_file,
                    force,
                },
        } => {
            cmd_config_fileprovider(
                &config,
                out.as_deref(),
                device_id.as_deref(),
                master_key_file.as_deref(),
                force,
            )
            .await
        }
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
        Commands::Index { action } => cmd_index(&config, action).await,
        Commands::Storage { action } => cmd_storage(&config, action).await,
        Commands::Cache { action } => match action {
            CacheAction::Stats => cmd_cache_stats(&config).await,
            CacheAction::Clear => cmd_cache_clear(&config).await,
            CacheAction::Evict {
                rel_path,
                prefix,
                json,
            } => cmd_cache_evict(&config, &rel_path, prefix.as_deref(), json).await,
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
            check,
            skip_config,
            force_config,
            config_out,
            fileprovider_config_out,
            non_interactive,
            password,
        } => {
            cmd_init(
                &config,
                InitOptions {
                    device_name,
                    check,
                    skip_config,
                    force_config,
                    config_out: config_out.as_deref(),
                    fileprovider_config_out: fileprovider_config_out.as_deref(),
                    non_interactive,
                    password,
                },
            )
            .await
        }
        Commands::Device { action } => match action {
            DeviceAction::Enroll {
                name,
                repair_placeholder,
                sync_remote,
                accept_unsigned_remote,
            } => {
                cmd_device_enroll(
                    &config,
                    name,
                    repair_placeholder,
                    sync_remote,
                    accept_unsigned_remote,
                )
                .await
            }
            DeviceAction::List => cmd_device_list(),
            DeviceAction::Revoke { name } => cmd_device_revoke(&config, &name),
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
            new_key_file,
            non_interactive,
        } => {
            cmd_rotate_key(
                &config,
                old_key_file.as_deref(),
                password,
                new_key_file.as_deref(),
                non_interactive,
            )
            .await
        }
        Commands::Key { action } => match action {
            KeyAction::Rotate {
                prefix,
                rotate_keys,
                resume,
                non_interactive,
                gc_immediate,
            } => {
                cmd_key_rotate(
                    &config,
                    &prefix,
                    rotate_keys,
                    resume,
                    non_interactive,
                    gc_immediate,
                )
                .await
            }
        },
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
        Commands::MigratePrefix {
            dry_run,
            writers_quiesced,
        } => cmd_migrate_prefix(&config, dry_run, writers_quiesced).await,
        Commands::Resolve {
            path,
            root,
            strategy,
            execute,
        } => {
            #[cfg(unix)]
            {
                cmd_resolve(
                    &config,
                    &path,
                    root.as_deref(),
                    strategy.as_deref(),
                    execute,
                )
                .await
            }
            #[cfg(not(unix))]
            {
                let _ = (path, root, strategy, execute);
                anyhow::bail!(
                    "resolve command requires the daemon (not available on this platform)"
                )
            }
        }
        Commands::Conflicts { json, root, state } => {
            cmd_conflicts(&config, json, root.as_deref(), state.as_deref()).await
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
        toml::from_str(&content).map_err(|_| {
            anyhow::anyhow!(
                "parsing config {} failed; check TOML syntax and field types",
                path.display()
            )
        })
    } else {
        Ok(tcfs_core::config::TcfsConfig::default())
    }
}

// ── Storage operator from unified credential discovery ───────────────────────

/// Build an OpenDAL operator using the unified credential discovery chain.
///
/// Delegates to `tcfs_secrets::CredStore::load()` which tries (in order):
///   1. SOPS-encrypted credential file
///   2. RemoteJuggler KDBX store
///   3. TCFS-specific env vars (TCFS_S3_ACCESS/SECRET)
///   4. AWS env vars (with warning)
///   5. Legacy SeaweedFS env vars
///   6. File-pointer env vars (*_FILE)
///   7. AWS shared credentials file (~/.aws/credentials)
async fn build_operator(config: &tcfs_core::config::TcfsConfig) -> Result<opendal::Operator> {
    let cred_store = tcfs_secrets::CredStore::load(&config.secrets, &config.storage)
        .await
        .context("credential discovery failed")?;

    let s3 = cred_store.s3.context(
        "S3 credentials not found.\n\
         Set TCFS_S3_ACCESS and TCFS_S3_SECRET environment variables,\n\
         or configure storage.credentials_file in tcfs.toml,\n\
         or use ~/.aws/credentials file.\n\
         Example:\n\
         \texport TCFS_S3_ACCESS=your-key\n\
         \texport TCFS_S3_SECRET=your-secret",
    )?;

    tracing::info!(source = %cred_store.source, "CLI credentials loaded");

    tcfs_storage::operator::build_from_core_config(
        &config.storage,
        &s3.access_key_id,
        s3.secret_access_key.expose_secret(),
    )
    .context("building storage operator")
}

// `~`-expansion lives in tcfs-core so the CLI and daemon expand identically
// (TIN-2657 adversarial gate, Fix B).
use tcfs_core::config::expand_tilde;

/// Resolve the state cache path: CLI flag > config > default user data dir
fn resolve_state_path(
    config: &tcfs_core::config::TcfsConfig,
    override_path: Option<&Path>,
) -> PathBuf {
    if let Some(p) = override_path {
        // Normalize the override to the canonical `.json` sibling so a
        // `--state …/state.db` (or `TCFS_STATE_PATH`) input resolves to the
        // exact file the daemon owns. `.with_extension("json")` is idempotent
        // for `.json` inputs, so explicit `.json` overrides are unchanged.
        return expand_tilde(p).with_extension("json");
    }
    // Config uses state_db (designed for RocksDB in Phase 4); for JSON Phase 2
    // we derive a sibling .json file
    let db = expand_tilde(&config.sync.state_db);
    db.with_extension("json")
}

/// Serialize explicit state-cache mutations with daemon-side registered-root
/// operations. Default-primary commands retain their existing daemon/process
/// coordination; only an operator-selected state path can alias a registered
/// root cache.
fn lock_explicit_state_for_mutation(
    state_path: &Path,
    state_override: Option<&Path>,
) -> Result<Option<tcfs_sync::state::StateFileLock>> {
    state_override
        .is_some()
        .then(|| tcfs_sync::state::StateFileLock::acquire(state_path))
        .transpose()
        .with_context(|| format!("locking explicit state cache: {}", state_path.display()))
}

fn validate_sync_selection_excludes_master_key(
    config: &tcfs_core::config::TcfsConfig,
    selected_path: &Path,
) -> Result<()> {
    tcfs_core::config::validate_sync_selection_excludes_master_key(config, selected_path)
        .map_err(anyhow::Error::msg)
}

fn validate_push_selection(
    config: &tcfs_core::config::TcfsConfig,
    selected_path: &Path,
) -> Result<()> {
    validate_sync_selection_excludes_master_key(config, selected_path)?;
    let fixed = tcfs_sync::blacklist::Blacklist::default();
    if let Some(reason) = fixed.check_fixed_ingress_path_components(selected_path) {
        anyhow::bail!(
            "selected push path {} is blocked by the fixed security deny-set: {reason}",
            selected_path.display()
        );
    }
    Ok(())
}

fn pull_input_is_file_path(manifest_path: &str) -> bool {
    manifest_path.starts_with('/')
        || manifest_path.starts_with('.')
        || Path::new(manifest_path).exists()
}

fn validate_pull_path(path: &Path, description: &str) -> Result<()> {
    let fixed = tcfs_sync::blacklist::Blacklist::default();
    if let Some(reason) = fixed.check_fixed_ingress_path_components(path) {
        anyhow::bail!(
            "{description} {} is blocked by the fixed security deny-set: {reason}",
            path.display()
        );
    }
    Ok(())
}

fn validate_pull_destination(
    config: &tcfs_core::config::TcfsConfig,
    destination: &Path,
) -> Result<()> {
    validate_pull_path(destination, "pull destination")?;
    validate_sync_selection_excludes_master_key(config, destination)
}

/// Validate every pull input whose meaning is known without consulting
/// storage. A manifest hash may still determine the default output basename;
/// that target is checked again immediately after resolution and before the
/// state cache is opened.
fn validate_pull_preflight(
    config: &tcfs_core::config::TcfsConfig,
    manifest_path: &str,
    local: Option<&Path>,
) -> Result<()> {
    validate_pull_path(Path::new(manifest_path), "pull logical path")?;
    if let Some(destination) = local {
        validate_pull_destination(config, destination)?;
    } else if pull_input_is_file_path(manifest_path) {
        validate_pull_destination(config, Path::new(manifest_path))?;
    }
    Ok(())
}

fn validate_rotation_master_key_path(
    config: &tcfs_core::config::TcfsConfig,
    master_key_path: &Path,
) -> Result<()> {
    tcfs_core::config::validate_master_key_outside_sync_roots(config, master_key_path)
        .map_err(anyhow::Error::msg)
}
/// Resolve the daemon-owned per-folder policy store.
fn policy_store_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tcfsd")
        .join("folder-policies.json")
}

// ── Progress bar helpers ──────────────────────────────────────────────────────

fn make_progress_bar(total: u64, prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{prefix:.bold} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .expect("hard-coded progress template")
            .progress_chars("=>-"),
    );
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

fn make_spinner(prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{prefix:.bold} {spinner} {msg}")
            .expect("hard-coded spinner template"),
    );
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
                    let new_id = registry
                        .backfill_device_id(&device_name)
                        .expect("backfill_device_id with valid device name");
                    // TIN-1417 B4: keep the signature valid after a backfill write.
                    if let Err(e) =
                        save_registry_signed_or_warn(&mut registry, &registry_path, config)
                    {
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
        preserve_symlinks: config.sync.sync_symlinks,
        sync_empty_dirs: config.sync.sync_empty_dirs,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SyncStatusReport {
    state_path: PathBuf,
    tracked_files: usize,
    file: Option<SyncStatusPathReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncStatusPathReport {
    Tracked {
        canonical: PathBuf,
        hash_prefix: String,
        size: u64,
        chunk_count: usize,
        remote_path: String,
        last_synced_age_secs: u64,
        sync_status: tcfs_sync::state::FileSyncStatus,
        needs_sync_reason: Option<String>,
    },
    Untracked {
        canonical: PathBuf,
    },
}

fn build_sync_status_report(
    config: &tcfs_core::config::TcfsConfig,
    path: Option<&Path>,
    state_override: Option<&Path>,
) -> Result<SyncStatusReport> {
    let state_path = resolve_state_path(config, state_override);
    let state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    let file = if let Some(p) = path {
        let canonical = resolve_sync_status_lookup_path(p)
            .with_context(|| format!("resolving path: {}", p.display()))?;

        match state.get(&canonical) {
            Some(entry) => Some(SyncStatusPathReport::Tracked {
                canonical: canonical.clone(),
                hash_prefix: entry.blake3[..16.min(entry.blake3.len())].to_string(),
                size: entry.size,
                chunk_count: entry.chunk_count,
                remote_path: entry.remote_path.clone(),
                last_synced_age_secs: now_epoch().saturating_sub(entry.last_synced),
                sync_status: entry.status,
                needs_sync_reason: if entry.status == tcfs_sync::state::FileSyncStatus::NotSynced
                    || !canonical.exists()
                {
                    None
                } else {
                    state.needs_sync(&canonical)?
                },
            }),
            None => Some(SyncStatusPathReport::Untracked { canonical }),
        }
    } else {
        None
    };

    Ok(SyncStatusReport {
        state_path,
        tracked_files: state.len(),
        file,
    })
}

fn resolve_sync_status_lookup_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        if tcfs_vfs::is_stub_path(path) {
            let stub = std::fs::canonicalize(path)?;
            let real_name =
                tcfs_vfs::stub_to_real_name(stub.file_name().context("stub path has no filename")?)
                    .context("invalid stub filename")?;
            let parent = stub.parent().context("stub path has no parent")?;
            return Ok(parent.join(real_name));
        }
        return std::fs::canonicalize(path).map_err(Into::into);
    }

    if !tcfs_vfs::is_stub_path(path) {
        let stub_candidate =
            path.parent()
                .unwrap_or(Path::new("."))
                .join(tcfs_vfs::real_to_stub_name(
                    path.file_name().context("path has no filename")?,
                ));

        if stub_candidate.exists() {
            let stub = std::fs::canonicalize(&stub_candidate)?;
            let parent = stub.parent().context("stub path has no parent")?;
            return Ok(parent.join(path.file_name().context("path has no filename")?));
        }
    }

    std::fs::canonicalize(path).map_err(Into::into)
}

/// Build an `EncryptionContext` honoring `crypto.wrap_mode` (TIN-1417).
///
/// Mirrors the daemon's `build_encryption_context`:
/// - `Master` (default): legacy shared-master wrap (byte-identical to before).
/// - `Dual`: master wrap + per-device wraps (requires a real recipient set).
/// - `PerDevice`: per-device-only wrap, gated behind a roll-call probe — refused
///   (and downgraded to `Dual` + a loud warning) unless EVERY active device has
///   a real age recipient.
///
/// Falls back to master (logging why) whenever the registry can't be loaded, has
/// no real recipients, or this device's age secret is missing — never producing
/// content this device cannot read back.
fn build_encryption_context(
    config: &tcfs_core::config::TcfsConfig,
    device_id: &str,
    master_key: &tcfs_crypto::MasterKey,
) -> tcfs_sync::engine::EncryptionContext {
    use tcfs_core::config::WrapMode;
    use tcfs_sync::engine::{DeviceUnwrapIdentity, EncryptionContext};

    let base = EncryptionContext::new(master_key.clone());
    let requested = config.crypto.wrap_mode;
    if requested == WrapMode::Master {
        return base;
    }
    let registry_path = config
        .sync
        .device_identity
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_registry_path);
    // TIN-1417 B4: build recipients only from a signature-VERIFIED registry;
    // an unsigned/tampered registry falls back to the shared master wrap.
    let registry = match tcfs_secrets::device::DeviceRegistry::load_verified(
        &registry_path,
        master_key.as_bytes(),
    ) {
        Ok((r, tcfs_secrets::device::RegistryTrust::Signed)) => r,
        Ok((_, tcfs_secrets::device::RegistryTrust::UnsignedLegacy)) => {
            tracing::warn!(
                "wrap_mode={requested:?}: device registry is UNSIGNED (legacy); refusing \
                 per-device recipients from an unverified registry — using master wrap."
            );
            return base;
        }
        Err(e) => {
            tracing::warn!(
                "wrap_mode={requested:?}: device registry FAILED signature verification ({e}); \
                 refusing per-device recipients — using master wrap (fail-closed)"
            );
            return base;
        }
    };
    let recipients: Vec<tcfs_crypto::AgeFileKeyRecipient> = registry
        .active_devices()
        .filter(|d| tcfs_secrets::device::is_real_age_public_key(&d.public_key))
        .map(|d| tcfs_crypto::AgeFileKeyRecipient {
            device_id: d.device_id.clone(),
            recipient: d.public_key.clone(),
        })
        .collect();
    if recipients.is_empty() {
        tracing::warn!(
            "wrap_mode={requested:?} enabled but no active age recipients; using master wrap"
        );
        return base;
    }
    let secret_path = tcfs_secrets::device::device_secret_key_path(&registry_path, device_id);
    let identity = match std::fs::read_to_string(&secret_path) {
        Ok(s) => DeviceUnwrapIdentity {
            device_id: device_id.to_string(),
            secret: s.trim().to_string(),
        },
        Err(e) => {
            tracing::warn!(
                "wrap_mode={requested:?}: local device secret unreadable ({e}); using master wrap"
            );
            return base;
        }
    };
    let effective = resolve_wrap_mode_with_roll_call(requested, &registry);
    base.with_wrap_mode(effective, recipients, Some(identity))
}

/// Apply the roll-call gate to a requested wrap mode (CLI mirror of the daemon's
/// gate). `PerDevice` downgrades to `Dual` (with a loud warning) unless every
/// active device carries a real age recipient.
fn resolve_wrap_mode_with_roll_call(
    requested: tcfs_core::config::WrapMode,
    registry: &tcfs_secrets::device::DeviceRegistry,
) -> tcfs_core::config::WrapMode {
    use tcfs_core::config::WrapMode;
    if requested != WrapMode::PerDevice {
        return requested;
    }
    let roll_call = registry.roll_call();
    if roll_call.all_capable() {
        return WrapMode::PerDevice;
    }
    tracing::warn!(
        active = roll_call.active,
        capable = roll_call.capable,
        blockers = ?roll_call.incapable_devices,
        "wrap_mode=PerDevice REFUSED by roll-call gate: not every active device has a real \
         age recipient; falling back to Dual (keeping the master wrap)."
    );
    WrapMode::Dual
}

// ── `tcfs push` ───────────────────────────────────────────────────────────────

async fn cmd_push_with_operator(
    config: &tcfs_core::config::TcfsConfig,
    op: &opendal::Operator,
    local: &Path,
    prefix: Option<&str>,
    state_path: &Path,
    device_id: &str,
) -> Result<()> {
    validate_push_selection(config, local)?;

    let mut state = tcfs_sync::state::StateCache::open(state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;
    let collect_cfg = collect_config_from_sync(config);

    // Default prefix: storage.remote_prefix from config, falling back to bucket.
    // This must match the FUSE daemon's mount prefix for cross-host visibility.
    let remote_prefix = prefix
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| config.storage.resolved_prefix().to_string());
    let storage_endpoint_display = sanitize_http_endpoint_for_display(&config.storage.endpoint);

    println!(
        "Pushing {} → {}:{} (endpoint: {}{})",
        local.display(),
        config.storage.bucket,
        remote_prefix,
        storage_endpoint_display,
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
            .map(|mk| build_encryption_context(config, device_id, mk));

        let result = tcfs_sync::engine::upload_file_with_device(
            op,
            local,
            &remote_prefix,
            &mut state,
            Some(&progress),
            device_id,
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
            // Path publication is handled inside upload_file_with_device so the
            // manifest/index sequence remains crash-aware.
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
                    .expect("hard-coded progress template")
                    .progress_chars("=>-"),
                );
                pb_clone.set_length(total);
            }
            pb_clone.set_position(done);
            pb_clone.set_message(msg.to_string());
        });

        let (uploaded, skipped, bytes) = tcfs_sync::engine::push_tree_with_device(
            op,
            local,
            &remote_prefix,
            &mut state,
            Some(&progress),
            device_id,
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

async fn cmd_push(
    config: &tcfs_core::config::TcfsConfig,
    local: &Path,
    prefix: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    // Reject before credential discovery or storage access. Keep the same
    // check in `cmd_push_with_operator` so embedded/test callers cannot bypass
    // the command preflight.
    validate_push_selection(config, local)?;
    let state_path = resolve_state_path(config, state_override);
    let _state_lock = lock_explicit_state_for_mutation(&state_path, state_override)?;
    let op = build_operator(config).await?;
    let device_id = load_device_id(config);
    cmd_push_with_operator(config, &op, local, prefix, &state_path, &device_id).await
}

// ── `tcfs pull` ───────────────────────────────────────────────────────────────

async fn cmd_pull_with_operator(
    config: &tcfs_core::config::TcfsConfig,
    op: &opendal::Operator,
    manifest_path: &str,
    local: Option<&Path>,
    prefix: Option<&str>,
    state_path: &Path,
    device_id: &str,
) -> Result<()> {
    validate_pull_preflight(config, manifest_path, local)?;

    // Detect whether input looks like a file path vs a manifest path
    let is_file_path = pull_input_is_file_path(manifest_path);

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
        tcfs_sync::engine::resolve_manifest_reference(op, manifest_path, &remote_prefix, sync_root)
            .await
            .with_context(|| format!("resolving manifest for: {manifest_path}"))?;

    if let Some(resolved_rel_path) = resolved_manifest.rel_path() {
        validate_pull_path(Path::new(resolved_rel_path), "resolved pull logical path")?;
    }

    // Explicit `.../manifests/<id>` compatibility reads have no index rel to
    // preflight here. The shared download boundary validates their parsed
    // manifest-bound rel_path before local or cache mutation, and cannot
    // redirect this command away from the independently validated destination.

    // Default local destination:
    // - an explicit `local` always wins;
    // - if the user pulled by file path, write back to that path (not a
    //   hash-named file in the current directory);
    // - otherwise (a remote manifest reference) fall back to the manifest hash
    //   basename in the current directory.
    let local_path = match local {
        Some(p) => p.to_path_buf(),
        None if is_file_path => PathBuf::from(manifest_path),
        None => {
            let hash_basename = resolved_manifest
                .manifest_path()
                .split('/')
                .next_back()
                .unwrap_or("downloaded");
            PathBuf::from(hash_basename)
        }
    };
    validate_pull_destination(config, &local_path)?;

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
    let mut state = tcfs_sync::state::StateCache::open(state_path)
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
        .map(|mk| build_encryption_context(config, device_id, mk));

    let result = tcfs_sync::engine::download_resolved_file_with_device(
        op,
        &resolved_manifest,
        &local_path,
        &remote_prefix,
        Some(&progress),
        device_id,
        Some(&mut state),
        enc_ctx.as_ref(),
    )
    .await
    .with_context(|| format!("downloading {}", manifest_path))?;

    state.flush().context("flushing state cache")?;
    remove_adjacent_stub_after_pull(&result.local_path).await?;
    restore_git_history_after_pull(&result.local_path).await?;

    pb.finish_with_message("done".to_string());
    println!();
    println!("Downloaded:");
    println!("  local:  {}", result.local_path.display());
    println!("  bytes:  {}", fmt_bytes(result.bytes));

    Ok(())
}

async fn remove_adjacent_stub_after_pull(local_path: &Path) -> Result<()> {
    if tcfs_vfs::is_stub_path(local_path) {
        return Ok(());
    }

    let file_name = match local_path.file_name() {
        Some(name) => name,
        None => return Ok(()),
    };
    let stub_path = local_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(tcfs_vfs::real_to_stub_name(file_name));

    let stub_bytes = match tokio::fs::read(&stub_path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading adjacent stub: {}", stub_path.display()));
        }
    };
    let Ok(stub_text) = String::from_utf8(stub_bytes) else {
        return Ok(());
    };
    if tcfs_vfs::StubMeta::parse(&stub_text).is_err() {
        return Ok(());
    }

    match tokio::fs::remove_file(&stub_path).await {
        Ok(()) => {
            println!("  removed stub: {}", stub_path.display());
            Ok(())
        }
        Err(err) => {
            Err(err).with_context(|| format!("removing stale stub: {}", stub_path.display()))
        }
    }
}

/// If the pulled file is a TCFS git bundle (`.git-tcfs-bundle`), reconstruct
/// the repo's `.git` metadata in place so the rehydrated working tree has
/// working history (`git log` / `git status` / `git fetch`).
///
/// The bundle artifact is intentionally left in place: it is a synced object,
/// so deleting it locally would propagate as a deletion to peers on the next
/// reconcile. Artifact cleanup (gitignore / post-restore prune) is tracked as
/// a follow-up.
async fn restore_git_history_after_pull(local_path: &Path) -> Result<()> {
    let is_bundle = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == tcfs_sync::git_safety::GIT_BUNDLE_REL_PATH)
        .unwrap_or(false);
    if !is_bundle {
        return Ok(());
    }
    let Some(repo_root) = local_path.parent().map(|p| p.to_path_buf()) else {
        return Ok(());
    };
    let bundle = local_path.to_path_buf();
    // git invokes blocking subprocesses; keep them off the async runtime.
    tokio::task::spawn_blocking(move || {
        tcfs_sync::git_safety::restore_git_bundle_into(&bundle, &repo_root)
    })
    .await
    .context("joining git bundle restore task")?
    .with_context(|| {
        format!(
            "restoring git history from bundle: {}",
            local_path.display()
        )
    })?;
    println!("  restored git history from bundle");
    Ok(())
}

async fn cmd_pull(
    config: &tcfs_core::config::TcfsConfig,
    manifest_path: &str,
    local: Option<&Path>,
    prefix: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    // Reject known-sensitive paths before state locking, credential discovery,
    // or storage construction. The operator-backed seam repeats this check.
    validate_pull_preflight(config, manifest_path, local)?;
    let state_path = resolve_state_path(config, state_override);
    let _state_lock = lock_explicit_state_for_mutation(&state_path, state_override)?;
    let op = build_operator(config).await?;
    let device_id = load_device_id(config);
    cmd_pull_with_operator(
        config,
        &op,
        manifest_path,
        local,
        prefix,
        &state_path,
        &device_id,
    )
    .await
}

// ── `tcfs sync-status` ────────────────────────────────────────────────────────

fn cmd_sync_status(
    config: &tcfs_core::config::TcfsConfig,
    path: Option<&Path>,
    state_override: Option<&Path>,
) -> Result<()> {
    let report = build_sync_status_report(config, path, state_override)?;

    println!("State cache: {}", report.state_path.display());
    println!("Tracked files: {}", report.tracked_files);

    if let Some(file) = report.file {
        println!();
        match file {
            SyncStatusPathReport::Tracked {
                canonical,
                hash_prefix,
                size,
                chunk_count,
                remote_path,
                last_synced_age_secs,
                sync_status,
                needs_sync_reason,
            } => {
                println!("File: {}", canonical.display());
                println!("  hash:       {}", hash_prefix);
                println!("  size:       {}", fmt_bytes(size));
                println!("  chunks:     {}", chunk_count);
                println!("  remote:     {}", remote_path);
                println!("  last sync:  {} seconds ago", last_synced_age_secs);
                println!("  sync state: {}", sync_status);
                match needs_sync_reason {
                    None => println!("  sync check: up to date"),
                    Some(reason) => println!("  sync check: needs sync ({reason})"),
                }
            }
            SyncStatusPathReport::Untracked { canonical } => {
                println!(
                    "File: {} — not in sync state (never pushed)",
                    canonical.display()
                );
            }
        }
    }

    Ok(())
}

// ── `tcfs index` ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct IndexInspectReport {
    rel_path: String,
    remote_prefix: String,
    index_key: String,
    index_exists: bool,
    status: String,
    parse_error: Option<String>,
    entry_state: Option<String>,
    visible_entry: Option<IndexInspectVisibleEntry>,
    pending_entry: Option<IndexInspectPendingEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct IndexInspectVisibleEntry {
    manifest_hash: String,
    manifest_key: String,
    manifest_exists: bool,
    size: u64,
    chunks: usize,
    kind: tcfs_sync::index_entry::RemoteEntryKind,
    symlink_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct IndexInspectPendingEntry {
    manifest_hash: String,
    manifest_key: String,
    manifest_exists: bool,
    staged_manifest_key: String,
    staged_manifest_exists: bool,
    size: u64,
    chunks: usize,
    kind: tcfs_sync::index_entry::RemoteEntryKind,
    symlink_target: Option<String>,
}

fn normalize_index_rel_path(path: &str) -> Result<String> {
    let trimmed = path.trim().trim_start_matches('/');
    anyhow::ensure!(!trimmed.is_empty(), "index path must not be empty");
    anyhow::ensure!(
        !trimmed.ends_with('/'),
        "index inspect expects a file or marker path, not a directory"
    );

    let mut parts = Vec::new();
    for component in trimmed.split('/') {
        anyhow::ensure!(
            !component.is_empty() && component != "." && component != "..",
            "index path must be normalized: {path}"
        );
        parts.push(component);
    }

    Ok(parts.join("/"))
}

async fn inspect_index_entry_with_operator(
    op: &opendal::Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<IndexInspectReport> {
    let rel_path = normalize_index_rel_path(rel_path)?;
    let remote_prefix = remote_prefix.trim_end_matches('/').to_string();
    anyhow::ensure!(!remote_prefix.is_empty(), "remote prefix must not be empty");

    let index_key = format!("{remote_prefix}/index/{rel_path}");
    let manifest_prefix = format!("{remote_prefix}/manifests");

    let raw = match op.read(&index_key).await {
        Ok(bytes) => bytes.to_vec(),
        Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
            return Ok(IndexInspectReport {
                rel_path,
                remote_prefix,
                index_key,
                index_exists: false,
                status: "missing_index".to_string(),
                parse_error: None,
                entry_state: None,
                visible_entry: None,
                pending_entry: None,
            });
        }
        Err(e) => {
            return Err(anyhow::anyhow!(e))
                .with_context(|| format!("reading index entry: {index_key}"));
        }
    };

    let parsed = match tcfs_sync::index_entry::parse_index_entry_record(&raw) {
        Ok(parsed) => parsed,
        Err(e) => {
            return Ok(IndexInspectReport {
                rel_path,
                remote_prefix,
                index_key,
                index_exists: true,
                status: "parse_error".to_string(),
                parse_error: Some(format!("{e:#}")),
                entry_state: None,
                visible_entry: None,
                pending_entry: None,
            });
        }
    };

    let entry_state = Some(format!("{:?}", parsed.state()).to_lowercase());

    let visible_entry = if let Some(entry) = parsed.visible_entry() {
        let manifest_key =
            tcfs_sync::index_entry::manifest_key(&manifest_prefix, &entry.manifest_hash);
        let manifest_exists = op
            .exists(&manifest_key)
            .await
            .with_context(|| format!("checking visible manifest: {manifest_key}"))?;
        Some(IndexInspectVisibleEntry {
            manifest_hash: entry.manifest_hash.clone(),
            manifest_key,
            manifest_exists,
            size: entry.size,
            chunks: entry.chunks,
            kind: entry.kind,
            symlink_target: entry.symlink_target.clone(),
        })
    } else {
        None
    };

    let pending_entry = if let Some(entry) = parsed.pending_entry() {
        let manifest_key =
            tcfs_sync::index_entry::manifest_key(&manifest_prefix, &entry.manifest_hash);
        let manifest_exists = op
            .exists(&manifest_key)
            .await
            .with_context(|| format!("checking pending manifest: {manifest_key}"))?;
        tcfs_sync::index_entry::validate_staged_manifest_key(&manifest_prefix, entry)?;
        let staged_manifest_exists =
            op.exists(&entry.staged_manifest_key)
                .await
                .with_context(|| {
                    format!(
                        "checking pending staged manifest: {}",
                        entry.staged_manifest_key
                    )
                })?;
        Some(IndexInspectPendingEntry {
            manifest_hash: entry.manifest_hash.clone(),
            manifest_key,
            manifest_exists,
            staged_manifest_key: entry.staged_manifest_key.clone(),
            staged_manifest_exists,
            size: entry.size,
            chunks: entry.chunks,
            kind: entry.kind,
            symlink_target: entry.symlink_target.clone(),
        })
    } else {
        None
    };

    let status = match &visible_entry {
        Some(entry) if entry.manifest_exists => "visible",
        Some(_) => "missing_manifest",
        None if pending_entry.is_some() => "preparing_only",
        None => "no_visible_entry",
    }
    .to_string();

    Ok(IndexInspectReport {
        rel_path,
        remote_prefix,
        index_key,
        index_exists: true,
        status,
        parse_error: None,
        entry_state,
        visible_entry,
        pending_entry,
    })
}

async fn cmd_index(config: &tcfs_core::config::TcfsConfig, action: IndexAction) -> Result<()> {
    match action {
        IndexAction::Inspect {
            rel_path,
            prefix,
            json,
        } => {
            let op = build_operator(config).await?;
            let remote_prefix = prefix
                .as_deref()
                .unwrap_or_else(|| config.storage.resolved_prefix());
            let report = inspect_index_entry_with_operator(&op, &rel_path, remote_prefix).await?;

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).context("serializing index report")?
                );
            } else {
                print_index_inspect_report(&report);
            }
        }
    }

    Ok(())
}

fn print_index_inspect_report(report: &IndexInspectReport) {
    println!("Remote prefix: {}", report.remote_prefix);
    println!("Relative path: {}", report.rel_path);
    println!("Index key:     {}", report.index_key);
    println!("Status:        {}", report.status);
    if let Some(error) = &report.parse_error {
        println!("Parse error:   {error}");
    }
    if let Some(entry) = &report.visible_entry {
        println!("Manifest key:  {}", entry.manifest_key);
        println!(
            "Manifest:      {}",
            if entry.manifest_exists {
                "ok"
            } else {
                "missing"
            }
        );
        println!("Size:          {}", fmt_bytes(entry.size));
        println!("Chunks:        {}", entry.chunks);
        println!("Kind:          {:?}", entry.kind);
        if let Some(target) = &entry.symlink_target {
            println!("Symlink:       {target}");
        }
    }
    if let Some(entry) = &report.pending_entry {
        println!("Pending key:   {}", entry.manifest_key);
        println!(
            "Pending:       {}",
            if entry.manifest_exists {
                "ok"
            } else {
                "missing"
            }
        );
        println!("Staged key:    {}", entry.staged_manifest_key);
        println!(
            "Staged:        {}",
            if entry.staged_manifest_exists {
                "ok"
            } else {
                "missing"
            }
        );
    }
}

// ── `tcfs storage` ───────────────────────────────────────────────────────────

async fn cmd_storage(config: &tcfs_core::config::TcfsConfig, action: StorageAction) -> Result<()> {
    match action {
        StorageAction::Canary {
            prefix,
            expect_deny_prefix,
            timeout_secs,
            json,
        } => {
            let op = build_operator(config).await?;
            let remote_prefix = prefix
                .map(|s| s.trim_matches('/').to_string())
                .unwrap_or_else(|| {
                    config
                        .storage
                        .resolved_prefix()
                        .trim_matches('/')
                        .to_string()
                });
            let expect_deny_prefix = expect_deny_prefix
                .map(|s| s.trim_matches('/').to_string())
                .filter(|s| !s.is_empty());
            let timeout = Duration::from_secs(timeout_secs.max(1));
            let nonce = new_storage_canary_nonce();
            let report = run_storage_canary_with_operator(
                config,
                &op,
                &remote_prefix,
                expect_deny_prefix.as_deref(),
                &nonce,
                timeout,
            )
            .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Storage canary passed:");
                println!("  endpoint: {}", report.endpoint);
                println!("  bucket:   {}", report.bucket);
                println!("  prefix:   {}", report.prefix);
                println!("  key:      {}", report.key);
                println!("  bytes:    {}", report.bytes);
                println!("  write:    {} ms", report.write_ms);
                println!(
                    "  list:     {} ms ({} entries at {})",
                    report.list_ms, report.list_count, report.list_prefix
                );
                println!("  read:     {} ms", report.read_ms);
                println!("  delete:   {} ms", report.delete_ms);
                println!("  verify:   {} ms", report.verify_delete_ms);
                if let Some(scope_deny) = &report.scope_deny {
                    println!(
                        "  scope:    deny write to {} ({}, {} ms)",
                        scope_deny.key, scope_deny.error_kind, scope_deny.write_ms
                    );
                }
                println!(
                    "  tls:      endpoint={}, enforce_tls={}",
                    report.endpoint_tls, report.enforce_tls
                );
            }
        }
    }

    Ok(())
}

fn new_storage_canary_nonce() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{}", now.as_secs(), std::process::id())
}

fn storage_canary_key(prefix: &str, nonce: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        format!(".tcfs-canary/{nonce}.txt")
    } else {
        format!("{prefix}/.tcfs-canary/{nonce}.txt")
    }
}

fn storage_canary_list_prefix(prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        "/".to_string()
    } else {
        format!("{prefix}/")
    }
}

async fn run_storage_canary_with_operator(
    config: &tcfs_core::config::TcfsConfig,
    op: &opendal::Operator,
    prefix: &str,
    expect_deny_prefix: Option<&str>,
    nonce: &str,
    timeout: Duration,
) -> Result<StorageCanaryReport> {
    let key = storage_canary_key(prefix, nonce);
    let list_prefix = storage_canary_list_prefix(prefix);
    let endpoint_display = sanitize_http_endpoint_for_display(&config.storage.endpoint);
    let payload = format!(
        "tcfs storage canary\nendpoint={}\nbucket={}\nprefix={}\nkey={}\nnonce={}\n",
        endpoint_display, config.storage.bucket, prefix, key, nonce
    )
    .into_bytes();

    let write_start = std::time::Instant::now();
    tokio::time::timeout(timeout, op.write(&key, payload.clone()))
        .await
        .map_err(|_| anyhow::anyhow!("storage canary write timed out after {timeout:?}: {key}"))?
        .with_context(|| format!("storage canary write failed: {key}"))?;
    let write_ms = write_start.elapsed().as_millis();

    let list_start = std::time::Instant::now();
    let list_entries = tokio::time::timeout(timeout, op.list(&list_prefix))
        .await
        .map_err(|_| {
            anyhow::anyhow!("storage canary list timed out after {timeout:?}: {list_prefix}")
        })?
        .with_context(|| format!("storage canary list failed: {list_prefix}"))?;
    let list_ms = list_start.elapsed().as_millis();
    let list_count = list_entries.len();

    let read_start = std::time::Instant::now();
    let read_back = tokio::time::timeout(timeout, op.read(&key))
        .await
        .map_err(|_| anyhow::anyhow!("storage canary read timed out after {timeout:?}: {key}"))?
        .with_context(|| format!("storage canary read failed: {key}"))?
        .to_bytes();
    let read_ms = read_start.elapsed().as_millis();

    anyhow::ensure!(
        read_back == payload.as_slice(),
        "storage canary readback mismatch: {key}"
    );

    let delete_start = std::time::Instant::now();
    tokio::time::timeout(timeout, op.delete(&key))
        .await
        .map_err(|_| anyhow::anyhow!("storage canary delete timed out after {timeout:?}: {key}"))?
        .with_context(|| format!("storage canary delete failed: {key}"))?;
    let delete_ms = delete_start.elapsed().as_millis();

    let verify_start = std::time::Instant::now();
    let exists_after_delete = tokio::time::timeout(timeout, op.exists(&key))
        .await
        .map_err(|_| {
            anyhow::anyhow!("storage canary delete verification timed out after {timeout:?}: {key}")
        })?
        .with_context(|| format!("storage canary delete verification failed: {key}"))?;
    let verify_delete_ms = verify_start.elapsed().as_millis();

    anyhow::ensure!(
        !exists_after_delete,
        "storage canary delete verification failed; object still exists: {key}"
    );

    let scope_deny = if let Some(deny_prefix) = expect_deny_prefix {
        Some(run_storage_canary_scope_deny_probe(op, prefix, deny_prefix, nonce, timeout).await?)
    } else {
        None
    };

    Ok(StorageCanaryReport {
        endpoint: endpoint_display,
        bucket: config.storage.bucket.clone(),
        prefix: prefix.to_string(),
        key,
        list_prefix,
        scope_deny,
        bytes: payload.len(),
        write_ms,
        list_ms,
        list_count,
        read_ms,
        delete_ms,
        verify_delete_ms,
        listed: true,
        deleted: !exists_after_delete,
        endpoint_tls: config.storage.endpoint.starts_with("https://"),
        enforce_tls: config.storage.enforce_tls,
    })
}

async fn run_storage_canary_scope_deny_probe(
    op: &opendal::Operator,
    allowed_prefix: &str,
    deny_prefix: &str,
    nonce: &str,
    timeout: Duration,
) -> Result<StorageCanaryScopeDenyReport> {
    let key = storage_canary_key(deny_prefix, nonce);
    anyhow::ensure!(
        key != storage_canary_key(allowed_prefix, nonce),
        "--expect-deny-prefix resolves to the same canary key as --prefix: {key}"
    );

    let payload = format!(
        "tcfs storage canary forbidden-scope probe\nallowed_prefix={allowed_prefix}\ndeny_prefix={deny_prefix}\nkey={key}\nnonce={nonce}\n",
    )
    .into_bytes();

    let write_start = std::time::Instant::now();
    let write_result = tokio::time::timeout(timeout, op.write(&key, payload)).await;
    let write_ms = write_start.elapsed().as_millis();

    match write_result {
        Err(_) => {
            anyhow::bail!("storage canary deny-scope write timed out after {timeout:?}: {key}")
        }
        Ok(Err(err)) if err.kind() == opendal::ErrorKind::PermissionDenied => {
            Ok(StorageCanaryScopeDenyReport {
                prefix: deny_prefix.to_string(),
                key,
                write_ms,
                error_kind: err.kind().to_string(),
                denied: true,
            })
        }
        Ok(Err(err)) => anyhow::bail!(
            "storage canary deny-scope write failed with {}, expected PermissionDenied: {key}",
            err.kind()
        ),
        Ok(Ok(_)) => {
            let _ = tokio::time::timeout(timeout, op.delete(&key)).await;
            anyhow::bail!(
                "storage canary deny-scope write unexpectedly succeeded; credentials are not scoped away from {key}"
            )
        }
    }
}

// ── `tcfs migrate-prefix` ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationInstallOutcome {
    Created,
    AlreadyExact,
    SourceAlreadyRetired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationDryRunOutcome {
    WouldCopy,
    AlreadyExact,
    DestinationConflict,
    SourceAlreadyRetired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationSourceKind {
    IndexEntry,
    DirectoryMarker,
    AlreadyRetired,
}

fn canonical_migration_prefix<'a>(value: &'a str, description: &str) -> Result<&'a str> {
    anyhow::ensure!(
        !value.is_empty() && value == value.trim_matches('/'),
        "{description} must be non-empty without leading or trailing slashes: {value:?}"
    );
    anyhow::ensure!(
        !value.contains('\\') && !value.chars().any(char::is_control),
        "{description} contains an unsafe storage-key character: {value:?}"
    );
    for component in value.split('/') {
        anyhow::ensure!(
            !component.is_empty() && component != "." && component != "..",
            "{description} contains an unsafe empty/dot component: {value:?}"
        );
    }
    Ok(value)
}

fn migration_index_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        "index/".to_string()
    } else {
        format!("{prefix}/index/")
    }
}

fn migration_index_key(prefix: &str, rel_path: &str) -> String {
    format!("{}{rel_path}", migration_index_prefix(prefix))
}

fn migration_rel_from_key<'a>(key: &'a str, index_prefix: &str) -> Result<&'a str> {
    let rel_path = key
        .strip_prefix(index_prefix)
        .filter(|rel_path| !rel_path.is_empty())
        .with_context(|| {
            format!("migration source escaped listed index prefix {index_prefix:?}: {key:?}")
        })?;
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)
        .with_context(|| format!("validating canonical migration path: {rel_path:?}"))?;
    Ok(rel_path)
}

fn migration_namespace_claim(
    rel_path: &str,
) -> Result<Option<(String, tcfs_sync::index_entry::PortableNamespaceRole)>> {
    use tcfs_sync::index_entry::PortableNamespaceRole;

    if rel_path == ".tcfs_dir" {
        return Ok(None);
    }
    if let Some(parent) = rel_path.strip_suffix("/.tcfs_dir") {
        tcfs_sync::index_entry::validate_canonical_rel_path(parent)?;
        return Ok(Some((parent.to_string(), PortableNamespaceRole::Directory)));
    }
    tcfs_sync::index_entry::validate_canonical_rel_path(rel_path)?;
    Ok(Some((rel_path.to_string(), PortableNamespaceRole::File)))
}

fn validate_migration_source(rel_path: &str, source_bytes: &[u8]) -> Result<MigrationSourceKind> {
    if rel_path.ends_with("/.tcfs_dir") {
        if source_bytes == tcfs_sync::index_entry::DIRECTORY_MARKER_BYTES {
            return Ok(MigrationSourceKind::DirectoryMarker);
        }
        let parsed = tcfs_sync::index_entry::parse_index_entry_record(source_bytes)
            .context("validating logically retired migration directory marker")?;
        anyhow::ensure!(
            parsed.state() == tcfs_sync::index_entry::IndexEntryState::Deleted,
            "migration directory marker is neither canonical nor logically retired"
        );
        return Ok(MigrationSourceKind::AlreadyRetired);
    }

    let parsed = tcfs_sync::index_entry::parse_index_entry_record(source_bytes)
        .context("validating migration source index entry")?;
    if parsed.state() == tcfs_sync::index_entry::IndexEntryState::Deleted {
        Ok(MigrationSourceKind::AlreadyRetired)
    } else {
        Ok(MigrationSourceKind::IndexEntry)
    }
}

/// Prove that every manifest pointer copied into the target index is already
/// valid in the target root. Prefix repair deliberately does not rewrite or
/// relocate immutable manifests: legacy entries whose path binding or object
/// placement differs require a dedicated rewrite protocol and fail closed.
async fn validate_migration_source_bindings(
    op: &opendal::Operator,
    remote_prefix: &str,
    rel_path: &str,
    source_kind: MigrationSourceKind,
    source_bytes: &[u8],
) -> Result<()> {
    if source_kind != MigrationSourceKind::IndexEntry {
        return Ok(());
    }

    let record = tcfs_sync::index_entry::parse_index_entry_record(source_bytes)
        .context("parsing migration index record for manifest binding")?;
    let manifest_prefix = format!("{remote_prefix}/manifests");

    if let Some(current) = record.visible_entry() {
        let manifest_key =
            tcfs_sync::index_entry::manifest_key(&manifest_prefix, &current.manifest_hash);
        let manifest_bytes = op
            .read(&manifest_key)
            .await
            .with_context(|| format!("reading target-root migration manifest: {manifest_key}"))?
            .to_vec();
        tcfs_sync::engine::validate_indexed_manifest_entry_binding(
            &manifest_bytes,
            &current.manifest_hash,
            current,
            rel_path,
        )
        .with_context(|| {
            format!("validating target-root migration manifest binding for {rel_path:?}")
        })?;
    }

    if let Some(pending) = record.pending_entry() {
        tcfs_sync::index_entry::validate_staged_manifest_key(&manifest_prefix, pending)
            .with_context(|| {
                format!("validating target-root staged migration key for {rel_path:?}")
            })?;
        let staged_bytes = op
            .read(&pending.staged_manifest_key)
            .await
            .with_context(|| {
                format!(
                    "reading target-root staged migration manifest: {}",
                    pending.staged_manifest_key
                )
            })?
            .to_vec();
        tcfs_sync::engine::validate_indexed_manifest_entry_binding(
            &staged_bytes,
            &pending.manifest_hash,
            &pending.as_remote_entry(),
            rel_path,
        )
        .with_context(|| {
            format!("validating target-root staged manifest binding for {rel_path:?}")
        })?;
    }

    Ok(())
}

#[cfg(test)]
fn memory_migration_install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn prove_migration_object_bytes(
    op: &opendal::Operator,
    key: &str,
    expected: &[u8],
) -> Result<()> {
    let observed = op
        .read(key)
        .await
        .with_context(|| format!("proving exact migration object bytes: {key}"))?
        .to_vec();
    anyhow::ensure!(
        observed == expected,
        "migration destination contains different bytes; preserving source: {key}"
    );
    Ok(())
}

/// Atomically install a migration destination or accept an existing object
/// only when a live read proves it is byte-for-byte identical.
async fn install_migration_destination(
    op: &opendal::Operator,
    new_key: &str,
    source_bytes: &[u8],
) -> Result<MigrationInstallOutcome> {
    if op.info().full_capability().write_with_if_not_exists {
        let outcome = match op
            .write_with(new_key, source_bytes.to_vec())
            .if_not_exists(true)
            .await
        {
            Ok(_) => MigrationInstallOutcome::Created,
            Err(write_error) => match op.read(new_key).await {
                Ok(observed) if observed.to_vec() == source_bytes => {
                    MigrationInstallOutcome::AlreadyExact
                }
                Ok(_) => {
                    anyhow::bail!(
                        "migration destination already exists with different bytes; preserving source: {new_key}"
                    )
                }
                Err(read_error) if read_error.kind() == opendal::ErrorKind::NotFound => {
                    return Err(anyhow::Error::new(write_error)).with_context(|| {
                        format!("atomically creating absent migration destination: {new_key}")
                    });
                }
                Err(read_error) => {
                    return Err(anyhow::Error::new(read_error)).with_context(|| {
                        format!(
                            "checking migration destination after conditional write failed: {new_key}"
                        )
                    });
                }
            },
        };
        prove_migration_object_bytes(op, new_key, source_bytes).await?;
        return Ok(outcome);
    }

    // OpenDAL Memory has no external endpoint and no conditional-write
    // capability. Keep this process-local emulation confined to unit tests.
    #[cfg(test)]
    if tcfs_storage::memory_conditional_write_emulation_is_registered_for_tests(op)? {
        let _guard = memory_migration_install_lock().lock().await;
        let outcome = match op.read(new_key).await {
            Ok(observed) if observed.to_vec() == source_bytes => {
                MigrationInstallOutcome::AlreadyExact
            }
            Ok(_) => {
                anyhow::bail!(
                    "migration destination already exists with different bytes; preserving source: {new_key}"
                )
            }
            Err(error) if error.kind() == opendal::ErrorKind::NotFound => {
                op.write(new_key, source_bytes.to_vec())
                    .await
                    .with_context(|| {
                        format!("creating guarded test migration destination: {new_key}")
                    })?;
                MigrationInstallOutcome::Created
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)).with_context(|| {
                    format!("reading guarded test migration destination: {new_key}")
                });
            }
        };
        prove_migration_object_bytes(op, new_key, source_bytes).await?;
        return Ok(outcome);
    }

    anyhow::bail!(
        "prefix migration requires atomic absent-object creation; refusing unsafe destination write: {new_key}"
    )
}

async fn migrate_bound_index_entry(
    op: &opendal::Operator,
    old_key: &str,
    new_key: &str,
    source_bytes: &[u8],
) -> Result<MigrationInstallOutcome> {
    anyhow::ensure!(
        old_key != new_key,
        "migration source and destination must differ: {old_key}"
    );
    let outcome = install_migration_destination(op, new_key, source_bytes).await?;

    // OpenDAL 0.55 has no ETag-conditional delete. Revalidate both objects so
    // the command reports a raced source. The caller may then retain the source
    // or replace an in-root bogus source with an exact-CAS logical tombstone.
    prove_migration_object_bytes(op, old_key, source_bytes)
        .await
        .with_context(|| format!("revalidating retained migration source: {old_key}"))?;
    prove_migration_object_bytes(op, new_key, source_bytes)
        .await
        .with_context(|| format!("revalidating installed migration destination: {new_key}"))?;
    Ok(outcome)
}

async fn migrate_index_entry(
    op: &opendal::Operator,
    remote_prefix: &str,
    rel_path: &str,
    old_key: &str,
    new_key: &str,
    retire_source: bool,
) -> Result<MigrationInstallOutcome> {
    let source_bytes = op
        .read(old_key)
        .await
        .with_context(|| format!("reading migration source: {old_key}"))?
        .to_vec();
    let source_kind = validate_migration_source(rel_path, &source_bytes)
        .with_context(|| format!("validating migration source: {old_key}"))?;
    // A tombstone is deletion evidence scoped to its existing root, not an
    // object to publish into a different target namespace. Treat it as an
    // idempotently retired source in both the double-prefix and orphan lanes.
    if source_kind == MigrationSourceKind::AlreadyRetired {
        return Ok(MigrationInstallOutcome::SourceAlreadyRetired);
    }
    validate_migration_source_bindings(op, remote_prefix, rel_path, source_kind, &source_bytes)
        .await
        .with_context(|| format!("binding migration source before publication: {old_key}"))?;
    if let Some((logical_path, role)) = migration_namespace_claim(rel_path)? {
        tcfs_sync::index_entry::admit_portable_namespace_entry(
            op,
            remote_prefix,
            &logical_path,
            role,
        )
        .await
        .with_context(|| format!("reserving portable namespace for migration: {rel_path:?}"))?;
    }
    let outcome = migrate_bound_index_entry(op, old_key, new_key, &source_bytes).await?;

    if retire_source {
        match source_kind {
            MigrationSourceKind::IndexEntry => {
                tcfs_sync::index_entry::tombstone_index_entry_if_exact(
                    op,
                    remote_prefix,
                    old_key,
                    &source_bytes,
                )
                .await
                .with_context(|| {
                    format!("logically retiring migrated double-prefix source: {old_key}")
                })?;
            }
            MigrationSourceKind::DirectoryMarker => {
                tcfs_sync::index_entry::tombstone_directory_marker_if_exact(
                    op,
                    remote_prefix,
                    old_key,
                    &source_bytes,
                )
                .await
                .with_context(|| {
                    format!("logically retiring migrated double-prefix directory marker: {old_key}")
                })?;
            }
            MigrationSourceKind::AlreadyRetired => unreachable!("handled before publication"),
        }
    }

    Ok(outcome)
}

async fn inspect_migration_destination(
    op: &opendal::Operator,
    remote_prefix: &str,
    rel_path: &str,
    old_key: &str,
    new_key: &str,
) -> Result<MigrationDryRunOutcome> {
    let source = op
        .read(old_key)
        .await
        .with_context(|| format!("reading migration source: {old_key}"))?
        .to_vec();
    let source_kind = validate_migration_source(rel_path, &source)
        .with_context(|| format!("validating migration source: {old_key}"))?;
    if source_kind == MigrationSourceKind::AlreadyRetired {
        return Ok(MigrationDryRunOutcome::SourceAlreadyRetired);
    }
    validate_migration_source_bindings(op, remote_prefix, rel_path, source_kind, &source)
        .await
        .with_context(|| format!("binding dry-run migration source: {old_key}"))?;
    match op.read(new_key).await {
        Ok(destination) if destination.to_vec() == source => {
            Ok(MigrationDryRunOutcome::AlreadyExact)
        }
        Ok(_) => Ok(MigrationDryRunOutcome::DestinationConflict),
        Err(error) if error.kind() == opendal::ErrorKind::NotFound => {
            Ok(MigrationDryRunOutcome::WouldCopy)
        }
        Err(error) => Err(anyhow::Error::new(error))
            .with_context(|| format!("checking migration destination: {new_key}")),
    }
}

fn require_migration_writers_quiesced(dry_run: bool, writers_quiesced: bool) -> Result<()> {
    anyhow::ensure!(
        dry_run || writers_quiesced,
        "executing prefix migration requires --writers-quiesced after stopping every old and new TCFS writer for the source and target prefixes"
    );
    Ok(())
}

async fn cmd_migrate_prefix(
    config: &tcfs_core::config::TcfsConfig,
    dry_run: bool,
    writers_quiesced: bool,
) -> Result<()> {
    require_migration_writers_quiesced(dry_run, writers_quiesced)?;
    let target =
        canonical_migration_prefix(config.storage.resolved_prefix(), "migration target prefix")?;
    let op = build_operator(config).await?;

    if !dry_run {
        tcfs_storage::ensure_conditional_write_semantics(&op, target)
            .await
            .context("verifying conditional writes before executing prefix migration")?;
    }

    println!(
        "Migrating S3 index entries → target prefix: \"{}\"{}\n",
        target,
        if dry_run { " (DRY RUN)" } else { "" }
    );

    let mut copied = 0u32;
    let mut exact_sources_retained = 0u32;
    let mut sources_logically_retired = 0u32;
    let mut sources_already_retired = 0u32;
    let mut conflicts = 0u32;

    // 1. Fix double-prefixed entries: {target}/index/{target}/* → {target}/index/*
    let double_prefix = format!("{}{target}/", migration_index_prefix(target));
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
        let rel = migration_rel_from_key(&old_key, &double_prefix)?;
        let new_key = migration_index_key(target, rel);

        if dry_run {
            match inspect_migration_destination(&op, target, rel, &old_key, &new_key).await? {
                MigrationDryRunOutcome::WouldCopy => {
                    println!(
                        "  copy + logically retire source: {} → {}",
                        old_key, new_key
                    );
                    copied += 1;
                    sources_logically_retired += 1;
                }
                MigrationDryRunOutcome::AlreadyExact => {
                    println!("  exact destination + logically retire source: {}", old_key);
                    sources_logically_retired += 1;
                }
                MigrationDryRunOutcome::DestinationConflict => {
                    println!(
                        "  conflict (destination differs; preserve source): {} → {}",
                        old_key, new_key
                    );
                    conflicts += 1;
                }
                MigrationDryRunOutcome::SourceAlreadyRetired => {
                    println!("  source already logically retired: {}", old_key);
                    sources_already_retired += 1;
                }
            }
        } else {
            match migrate_index_entry(&op, target, rel, &old_key, &new_key, true).await? {
                MigrationInstallOutcome::Created => {
                    println!(
                        "  copied + source logically retired: {} → {}",
                        old_key, new_key
                    );
                    copied += 1;
                    sources_logically_retired += 1;
                }
                MigrationInstallOutcome::AlreadyExact => {
                    println!(
                        "  exact destination + source logically retired: {}",
                        old_key
                    );
                    sources_logically_retired += 1;
                }
                MigrationInstallOutcome::SourceAlreadyRetired => {
                    println!("  source already logically retired: {}", old_key);
                    sources_already_retired += 1;
                }
            }
        }
    }

    // 2. Migrate orphan prefixes (e.g., tcfs/index/* when target is "data")
    let bucket = canonical_migration_prefix(&config.storage.bucket, "legacy bucket prefix")?;
    if bucket != target {
        let orphan_prefix = migration_index_prefix(bucket);
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
            let rel = migration_rel_from_key(&old_key, &orphan_prefix)?;
            let new_key = migration_index_key(target, rel);

            if dry_run {
                match inspect_migration_destination(&op, target, rel, &old_key, &new_key).await? {
                    MigrationDryRunOutcome::AlreadyExact => {
                        println!("  exact orphan destination (retain source): {}", old_key);
                        exact_sources_retained += 1;
                    }
                    MigrationDryRunOutcome::DestinationConflict => {
                        println!(
                            "  conflict (destination differs; preserve source): {} → {}",
                            old_key, new_key
                        );
                        conflicts += 1;
                    }
                    MigrationDryRunOutcome::WouldCopy => {
                        println!("  copy orphan (retain source): {} → {}", old_key, new_key);
                        copied += 1;
                    }
                    MigrationDryRunOutcome::SourceAlreadyRetired => {
                        println!("  orphan source already logically retired: {}", old_key);
                        sources_already_retired += 1;
                    }
                }
            } else {
                match migrate_index_entry(&op, target, rel, &old_key, &new_key, false).await? {
                    MigrationInstallOutcome::Created => {
                        println!(
                            "  copied orphan (source retained): {} → {}",
                            old_key, new_key
                        );
                        copied += 1;
                    }
                    MigrationInstallOutcome::AlreadyExact => {
                        println!("  exact orphan destination (source retained): {}", old_key);
                        exact_sources_retained += 1;
                    }
                    MigrationInstallOutcome::SourceAlreadyRetired => {
                        println!("  orphan source already logically retired: {}", old_key);
                        sources_already_retired += 1;
                    }
                }
            }
        }
    }

    println!(
        "\n{}: copied={}, exact_sources_retained={}, sources_logically_retired={}, sources_already_retired={}, conflicts={}",
        if dry_run { "Would process" } else { "Done" },
        copied,
        exact_sources_retained,
        sources_logically_retired,
        sources_already_retired,
        conflicts
    );
    if dry_run {
        println!("Run without --dry-run to apply changes.");
    } else if copied > 0 {
        println!("Restart tcfsd to re-populate the state cache.");
    }

    Ok(())
}

// ── `tcfs trash` ─────────────────────────────────────────────────────────────

fn select_trash_entry<'a>(
    entries: &'a [tcfs_vfs::trash::TrashEntry],
    original_path: &str,
    exact_trash_key: Option<&str>,
) -> Result<&'a tcfs_vfs::trash::TrashEntry> {
    if let Some(exact_trash_key) = exact_trash_key {
        return entries
            .iter()
            .find(|entry| {
                entry.original_path == original_path && entry.trash_key == exact_trash_key
            })
            .with_context(|| {
                format!(
                    "no visible trash generation for {original_path:?} has exact key {exact_trash_key:?}"
                )
            });
    }

    let matching: Vec<_> = entries
        .iter()
        .filter(|entry| entry.original_path == original_path)
        .collect();
    match matching.as_slice() {
        [] => anyhow::bail!(
            "no trash entry found for {original_path:?}\nRun `tcfs trash list` to see trashed items."
        ),
        [entry] => Ok(*entry),
        _ => {
            let keys = matching
                .iter()
                .map(|entry| entry.trash_key.as_str())
                .collect::<Vec<_>>()
                .join("\n  ");
            anyhow::bail!(
                "multiple trash generations exist for {original_path:?}; retry with one exact `--trash-key` from `tcfs trash list`:\n  {keys}"
            )
        }
    }
}

fn trash_purge_max_age(all: bool, older_than: Option<u64>, configured: u64) -> Result<u64> {
    anyhow::ensure!(
        !(all && older_than.is_some()),
        "--all conflicts with --older-than"
    );
    if all {
        return Ok(0);
    }
    let max_age = older_than.unwrap_or(configured);
    anyhow::ensure!(
        max_age > 0,
        "trash retention is disabled (0 seconds); pass --older-than with a positive duration or explicitly use --all"
    );
    Ok(max_age)
}

async fn cmd_trash(config: &tcfs_core::config::TcfsConfig, action: TrashAction) -> Result<()> {
    let op = build_operator(config).await?;

    let resolve_prefix = |p: Option<&str>| -> String {
        p.map(str::to_owned).unwrap_or_else(|| {
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
            let report = tcfs_vfs::trash::scan_trash(&op, &remote_prefix).await?;
            let entries = &report.entries;

            if entries.is_empty() && report.issues.is_empty() {
                println!("Trash is empty.");
                return Ok(());
            }

            if !entries.is_empty() {
                println!(
                    "{:<40} {:<20} {:<14} TRASH KEY",
                    "ORIGINAL PATH", "TRASHED", "STATE"
                );
                println!("{}", "-".repeat(105));

                for entry in entries {
                    let age = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_sub(entry.trashed_at);
                    let age_str = format_duration(age);

                    println!(
                        "{:<40} {:<20} {:<14} {}",
                        truncate_str(&entry.original_path, 39),
                        format!("{} ago", age_str),
                        entry.generation_state.as_str(),
                        entry.trash_key,
                    );
                }

                println!("\n{} valid item(s) in trash.", entries.len());
            }

            if !report.issues.is_empty() {
                eprintln!(
                    "Warning: retained {} unreadable trash object(s); use an exact --trash-key for known-good recovery.",
                    report.issues.len()
                );
                for issue in &report.issues {
                    eprintln!("  {:?}: {:?}", issue.trash_key, issue.error);
                }
            }
            Ok(())
        }

        TrashAction::Restore {
            path,
            trash_key,
            prefix,
        } => {
            let remote_prefix = resolve_prefix(prefix.as_deref());
            let entry = if let Some(trash_key) = trash_key.as_deref() {
                tcfs_vfs::trash::read_exact_trash_entry(&op, &remote_prefix, &path, trash_key)
                    .await?
                    .with_context(|| {
                        format!(
                            "no visible trash generation for {path:?} has exact key {trash_key:?}"
                        )
                    })?
            } else {
                let entries = tcfs_vfs::trash::list_trash(&op, &remote_prefix).await?;
                select_trash_entry(&entries, &path, None)?.clone()
            };

            tcfs_vfs::trash::restore_trash_entry(&op, &remote_prefix, &entry).await?;
            println!("Restored: {} → index/{}", path, entry.original_path);
            Ok(())
        }

        TrashAction::Purge {
            older_than,
            all,
            prefix,
        } => {
            let remote_prefix = resolve_prefix(prefix.as_deref());

            let max_age = trash_purge_max_age(all, older_than, config.sync.trash_retention_secs)?;

            if all {
                println!(
                    "Logically purging ALL independently valid trash entries (evidence retained; malformed entries retained)..."
                );
            } else {
                println!(
                    "Logically purging trash entries older than {} (evidence retained)...",
                    format_duration(max_age)
                );
            }

            let report = tcfs_vfs::trash::purge_old_trash(&op, &remote_prefix, max_age).await?;

            if report.purged > 0 {
                println!(
                    "Logically purged {} entry(ies); evidence retained.",
                    report.purged
                );
            } else {
                println!("Nothing to purge.");
            }
            if !report.issues.is_empty() {
                eprintln!(
                    "Warning: retained {} trash object(s) that could not be safely purged:",
                    report.issues.len()
                );
                for issue in &report.issues {
                    eprintln!("  {:?}: {:?}", issue.trash_key, issue.error);
                }
                anyhow::bail!(
                    "trash purge completed partially: {} purged, {} retained with issues",
                    report.purged,
                    report.issues.len()
                );
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
    if s.chars().count() <= max_len {
        return s.to_string();
    }
    if max_len == 0 {
        return String::new();
    }
    let prefix: String = s.chars().take(max_len - 1).collect();
    format!("{prefix}…")
}

// ── `tcfs rm` ────────────────────────────────────────────────────────────────

async fn cmd_rm(
    config: &tcfs_core::config::TcfsConfig,
    path: &Path,
    prefix: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    let state_path = resolve_state_path(config, state_override);
    let _state_lock = lock_explicit_state_for_mutation(&state_path, state_override)?;
    let op = build_operator(config).await?;
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

    let mut client = connect_daemon_without_session(socket).await?;

    // Daemon status
    let status = tokio::time::timeout(
        DAEMON_RPC_TIMEOUT,
        client.status(tonic::Request::new(StatusRequest {})),
    )
    .await
    .context("status RPC timed out")?
    .context("status RPC failed")?
    .into_inner();

    // Credential status
    let creds = tokio::time::timeout(
        DAEMON_RPC_TIMEOUT,
        client.credential_status(tonic::Request::new(Empty {})),
    )
    .await
    .context("credential_status RPC timed out")?
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
        sanitize_http_endpoint_for_display(&status.storage_endpoint),
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
            "https://api.github.com/repos/Jesssullivan/tummycrypt/releases/latest",
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
            println!("  Update: curl -fsSL https://github.com/Jesssullivan/tummycrypt/releases/latest/download/install.sh | sh");
        }
    }
}

// ── gRPC connection ───────────────────────────────────────────────────────────

#[cfg(unix)]
async fn load_session_token() -> Option<String> {
    if let Ok(token) = std::env::var("TCFS_SESSION_TOKEN") {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let lookup = tokio::task::spawn_blocking(|| {
        match tcfs_secrets::keychain::get_secret(tcfs_secrets::keychain::keys::SESSION_TOKEN) {
            Ok(Some(secret)) => Some(secret.expose_secret().to_string()),
            Ok(None) => None,
            Err(err) => {
                tracing::debug!("failed to read TCFS session token from keychain: {err}");
                None
            }
        }
    });

    match tokio::time::timeout(SESSION_TOKEN_LOOKUP_TIMEOUT, lookup).await {
        Ok(Ok(token)) => token,
        Ok(Err(err)) => {
            tracing::debug!("TCFS session token keychain lookup task failed: {err}");
            None
        }
        Err(_) => {
            tracing::debug!("TCFS session token keychain lookup timed out");
            None
        }
    }
}

#[cfg(unix)]
fn store_session_token(token: &str) -> Result<()> {
    if token.trim().is_empty() {
        anyhow::bail!("refusing to store an empty session token");
    }

    let secret = secrecy::SecretString::from(token.to_string());
    tcfs_secrets::keychain::store_secret(tcfs_secrets::keychain::keys::SESSION_TOKEN, &secret)
        .context("storing TCFS session token in keychain")
}

#[cfg(unix)]
async fn connect_daemon(socket_path: &Path) -> Result<DaemonClient> {
    let token = load_session_token().await;
    connect_daemon_with_token(socket_path, token).await
}

#[cfg(unix)]
async fn connect_daemon_without_session(socket_path: &Path) -> Result<DaemonClient> {
    connect_daemon_with_token(socket_path, None).await
}

#[cfg(unix)]
async fn connect_daemon_with_token(
    socket_path: &Path,
    token: Option<String>,
) -> Result<DaemonClient> {
    let path = socket_path.to_path_buf();

    // tonic over Unix domain socket: use a tower service_fn connector
    let endpoint = Endpoint::from_static("http://[::]:0");
    let connect = endpoint.connect_with_connector(service_fn(move |_: Uri| {
        let path = path.clone();
        async move {
            let stream = tokio::time::timeout(
                DAEMON_CONNECT_TIMEOUT,
                tokio::net::UnixStream::connect(&path),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out connecting to tcfsd",
                )
            })??;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }
    }));

    let channel = tokio::time::timeout(DAEMON_CONNECT_TIMEOUT, connect)
        .await
        .with_context(|| format!("timed out connecting to tcfsd at {}", socket_path.display()))?
        .with_context(|| format!("connecting to tcfsd at {}", socket_path.display()))?;

    Ok(TcfsDaemonClient::with_interceptor(
        channel,
        SessionTokenInterceptor { token },
    ))
}

// ── `tcfs config show` ────────────────────────────────────────────────────────

fn cmd_config_show(config: &tcfs_core::config::TcfsConfig, config_path: &Path) -> Result<()> {
    print!("{}", render_config_show(config, config_path)?);
    Ok(())
}

fn render_config_show(
    config: &tcfs_core::config::TcfsConfig,
    config_path: &Path,
) -> Result<String> {
    let source = if config_path.exists() {
        format!("# Configuration from: {}", config_path.display())
    } else {
        format!(
            "# Configuration: defaults (no file at {})",
            config_path.display()
        )
    };
    let rendered =
        toml::to_string_pretty(&config.redacted()).context("serializing config to TOML")?;
    Ok(format!(
        "# Redacted diagnostic view; not suitable for reuse as configuration\n{source}\n\n{rendered}"
    ))
}

async fn cmd_config_fileprovider(
    config: &tcfs_core::config::TcfsConfig,
    out: Option<&Path>,
    device_id: Option<&str>,
    master_key_file: Option<&Path>,
    force: bool,
) -> Result<()> {
    let config_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(default_fileprovider_config_path);
    let device_id = resolve_fileprovider_device_id(config, device_id)?;
    let master_key_path = resolve_fileprovider_master_key_path(config, master_key_file)?;

    write_fileprovider_init_config(&config_path, config, &master_key_path, &device_id, force)
        .await?;
    println!("FileProvider config: {}", config_path.display());
    Ok(())
}

fn default_fileprovider_config_path() -> PathBuf {
    default_user_config_dir().join("fileprovider/config.json")
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CacheEvictReport {
    rel_path: String,
    remote_prefix: String,
    manifest_hash: String,
    bytes_freed: u64,
    was_cached: bool,
}

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

async fn evict_cache_entry_with_operator(
    config: &tcfs_core::config::TcfsConfig,
    op: &opendal::Operator,
    rel_path: &str,
    remote_prefix: &str,
) -> Result<CacheEvictReport> {
    let report = inspect_index_entry_with_operator(op, rel_path, remote_prefix).await?;
    let status = report.status.clone();
    let visible = report.visible_entry.with_context(|| {
        format!(
            "cannot evict cache for {}: remote index status is {status}",
            report.rel_path
        )
    })?;
    anyhow::ensure!(
        visible.manifest_exists,
        "cannot evict cache for {}: manifest {} is missing",
        report.rel_path,
        visible.manifest_key
    );

    let cache_dir = expand_tilde(&config.fuse.cache_dir);
    let cache_max = config.fuse.cache_max_mb * 1024 * 1024;
    let cache = tcfs_vfs::DiskCache::new(cache_dir, cache_max);
    let bytes_freed = cache.evict(&visible.manifest_hash).await?;

    Ok(CacheEvictReport {
        rel_path: report.rel_path,
        remote_prefix: report.remote_prefix,
        manifest_hash: visible.manifest_hash,
        bytes_freed,
        was_cached: bytes_freed > 0,
    })
}

async fn cmd_cache_evict(
    config: &tcfs_core::config::TcfsConfig,
    rel_path: &str,
    prefix: Option<&str>,
    json: bool,
) -> Result<()> {
    let op = build_operator(config).await?;
    let remote_prefix = prefix.unwrap_or_else(|| config.storage.resolved_prefix());
    let report = evict_cache_entry_with_operator(config, &op, rel_path, remote_prefix).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serializing cache evict report")?
        );
    } else {
        println!("Evicted cache entry: {}", report.rel_path);
        println!("  remote prefix: {}", report.remote_prefix);
        println!("  manifest:      {}", report.manifest_hash);
        println!("  freed:         {}", fmt_bytes(report.bytes_freed));
        println!(
            "  result:        {}",
            if report.was_cached {
                "evicted"
            } else {
                "not cached"
            }
        );
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
                    println!(
                        "Mounted via daemon: {} → {}",
                        mount_remote_endpoint_for_display(remote),
                        mountpoint.display()
                    );
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
        allow_insecure_http: !config.storage.enforce_tls,
        s3_connect_timeout_secs: config.storage.s3_connect_timeout_secs,
        s3_pool_idle_timeout_secs: config.storage.s3_pool_idle_timeout_secs,
        s3_pool_max_idle_per_host: config.storage.s3_pool_max_idle_per_host,
        s3_http1_only: config.storage.s3_http1_only,
        ca_cert_path: config.storage.ca_cert_path.clone(),
    };
    let op = tcfs_storage::operator::build_operator_with_limits(
        &storage_cfg,
        config.storage.max_concurrent_ops,
    )
    .context("building storage operator")?;

    let cache_dir = expand_tilde(&config.fuse.cache_dir);
    let neg_ttl = config.fuse.negative_cache_ttl_secs;
    let cache_max = config.fuse.cache_max_mb * 1024 * 1024;

    let backend = if use_nfs { "NFS loopback" } else { "FUSE" };
    println!(
        "Mounting {} → {} [{}]",
        sanitize_http_endpoint_for_display(&endpoint),
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
                              file_hash: &str,
                              manifest_object_id: &str,
                              size: u64,
                              _chunks: usize,
                              vclock: &tcfs_sync::conflict::VectorClock| {
                            let rel_path =
                                match tcfs_vfs::virtual_path_to_canonical_rel_path(rel_path) {
                                    Ok(rel_path) => rel_path.to_string(),
                                    Err(error) => {
                                        tracing::warn!(
                                            path = %rel_path,
                                            %error,
                                            "refusing to publish invalid VFS flush path"
                                        );
                                        return;
                                    }
                                };
                            let event = tcfs_sync::StateEvent::FileSynced {
                                device_id: dev.clone(),
                                rel_path,
                                blake3: file_hash.to_string(),
                                size,
                                vclock: vclock.clone(),
                                manifest_path: format!("{}/manifests/{}", pfx, manifest_object_id),
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
                encryption_required: config.crypto.enabled,
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

fn mount_remote_endpoint_for_display(remote: &str) -> String {
    tcfs_storage::parse_remote_spec(remote)
        .map(|(endpoint, _, _)| sanitize_http_endpoint_for_display(&endpoint))
        .unwrap_or_else(|_| sanitize_http_endpoint_for_display(remote))
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

#[derive(Debug, Clone)]
struct UnsyncTarget {
    path: PathBuf,
    tracked: tcfs_sync::state::SyncState,
}

#[derive(Debug, Clone)]
struct UnsyncConversion {
    path: PathBuf,
    stub_full: PathBuf,
    tracked: tcfs_sync::state::SyncState,
    local_size: u64,
}

#[derive(Debug, Clone)]
struct UnsyncSkip {
    path: PathBuf,
    reason: String,
}

#[derive(Debug, Clone)]
struct DirtyUnsyncPath {
    path: PathBuf,
    reason: String,
}

#[derive(Debug, Default)]
struct UnsyncPlan {
    conversions: Vec<UnsyncConversion>,
    skipped: Vec<UnsyncSkip>,
    dirty: Vec<DirtyUnsyncPath>,
}

impl UnsyncPlan {
    fn has_work(&self) -> bool {
        !self.conversions.is_empty() || !self.skipped.is_empty()
    }
}

/// Convert hydrated file(s) back to `.tc` stubs, reclaiming disk space.
///
/// File input preserves the original one-file behavior. Directory input walks
/// tracked descendants, refuses dirty files unless `--force` is set, flips
/// state to `NotSynced`, then writes stubs and removes hydrated files.
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

    let state_path = resolve_state_path(config, None);

    if path.is_dir() {
        cmd_unsync_directory(config, path, force, &state_path).await
    } else {
        cmd_unsync_file(config, path, force, &state_path).await
    }
}

async fn cmd_unsync_file(
    config: &tcfs_core::config::TcfsConfig,
    path: &std::path::Path,
    force: bool,
    state_path: &std::path::Path,
) -> Result<()> {
    let state = tcfs_sync::state::StateCache::open(state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;
    let tracked = state
        .get(path)
        .cloned()
        .with_context(|| format!("{} is not tracked (never pushed)", path.display()))?;
    drop(state);

    let target = UnsyncTarget {
        path: path.to_path_buf(),
        tracked,
    };
    let plan = build_unsync_plan(state_path, vec![target], force)?;
    if let Some(dirty) = plan.dirty.first() {
        anyhow::bail!(
            "{} has local changes ({}). Use --force to unsync anyway.",
            dirty.path.display(),
            dirty.reason
        );
    }

    flush_unsync_state_first(
        state_path,
        plan.conversions.iter().map(|c| c.path.as_path()),
    )?;

    let conversion = plan
        .conversions
        .first()
        .context("tracked file had no unsync conversion")?;
    apply_unsync_conversion(config, conversion).await?;

    println!(
        "Unsynced: {} → {}",
        conversion.path.display(),
        conversion.stub_full.display()
    );
    println!(
        "  hash: {}",
        &conversion.tracked.blake3[..16.min(conversion.tracked.blake3.len())]
    );
    println!("  size: {} freed", fmt_bytes(conversion.local_size));

    Ok(())
}

async fn cmd_unsync_directory(
    config: &tcfs_core::config::TcfsConfig,
    path: &std::path::Path,
    force: bool,
    state_path: &std::path::Path,
) -> Result<()> {
    let state = tcfs_sync::state::StateCache::open(state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;
    let mut targets: Vec<UnsyncTarget> = state
        .children_with_prefix(path)
        .into_iter()
        .map(|(key, tracked)| UnsyncTarget {
            path: PathBuf::from(key),
            tracked: tracked.clone(),
        })
        .collect();
    targets.sort_by(|a, b| a.path.cmp(&b.path));
    drop(state);

    if targets.is_empty() {
        anyhow::bail!(
            "{} has no tracked descendants in {}",
            path.display(),
            state_path.display()
        );
    }

    let plan = build_unsync_plan(state_path, targets, force)?;
    if !plan.dirty.is_empty() {
        print_dirty_unsync_paths(path, &plan.dirty);
        anyhow::bail!(
            "{} dirty descendant(s) with local changes. Use --force to unsync anyway.",
            plan.dirty.len()
        );
    }

    if !plan.has_work() {
        println!(
            "{} has no hydrated tracked descendants to unsync.",
            path.display()
        );
        return Ok(());
    }

    flush_unsync_state_first(
        state_path,
        plan.conversions
            .iter()
            .map(|c| c.path.as_path())
            .chain(plan.skipped.iter().map(|s| s.path.as_path())),
    )?;

    for conversion in &plan.conversions {
        apply_unsync_conversion(config, conversion).await?;
    }

    println!("Unsynced directory: {}", path.display());
    if !plan.conversions.is_empty() {
        println!("  converted:");
        for conversion in &plan.conversions {
            println!(
                "    {} → {}",
                conversion.path.display(),
                conversion.stub_full.display()
            );
        }
    }
    if !plan.skipped.is_empty() {
        println!("  skipped:");
        for skipped in &plan.skipped {
            println!("    {} ({})", skipped.path.display(), skipped.reason);
        }
    }
    println!(
        "  summary: {} converted, {} skipped, 0 dirty",
        plan.conversions.len(),
        plan.skipped.len()
    );

    Ok(())
}

fn build_unsync_plan(
    state_path: &std::path::Path,
    targets: Vec<UnsyncTarget>,
    force: bool,
) -> Result<UnsyncPlan> {
    let state = tcfs_sync::state::StateCache::open(state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;
    let mut plan = UnsyncPlan::default();

    for target in targets {
        if target.path.is_dir() {
            plan.skipped.push(UnsyncSkip {
                path: target.path,
                reason: "tracked directory marker".to_string(),
            });
            continue;
        }

        if target.path.exists() {
            match state.needs_sync(&target.path) {
                Ok(Some(reason)) if !force => {
                    plan.dirty.push(DirtyUnsyncPath {
                        path: target.path,
                        reason,
                    });
                    continue;
                }
                Ok(_) => {}
                Err(e) if !force => {
                    plan.dirty.push(DirtyUnsyncPath {
                        path: target.path,
                        reason: e.to_string(),
                    });
                    continue;
                }
                Err(_) => {}
            }

            let metadata = std::fs::metadata(&target.path)
                .with_context(|| format!("stat: {}", target.path.display()))?;
            if !metadata.is_file() {
                plan.skipped.push(UnsyncSkip {
                    path: target.path,
                    reason: "not a regular file".to_string(),
                });
                continue;
            }

            let stub_name = tcfs_vfs::real_to_stub_name(
                target
                    .path
                    .file_name()
                    .context("tracked path has no filename")?,
            );
            let stub_full = target
                .path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(stub_name);

            plan.conversions.push(UnsyncConversion {
                path: target.path,
                stub_full,
                tracked: target.tracked,
                local_size: metadata.len(),
            });
            continue;
        }

        let stub_candidate = target
            .path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(tcfs_vfs::real_to_stub_name(
                target
                    .path
                    .file_name()
                    .context("tracked path has no filename")?,
            ));
        let reason = if stub_candidate.exists() {
            "already stubbed".to_string()
        } else {
            "hydrated file missing".to_string()
        };
        plan.skipped.push(UnsyncSkip {
            path: target.path,
            reason,
        });
    }

    Ok(plan)
}

fn flush_unsync_state_first<'a>(
    state_path: &std::path::Path,
    paths: impl IntoIterator<Item = &'a std::path::Path>,
) -> Result<()> {
    // Flip persisted state to NotSynced BEFORE destructive fs ops.
    //
    // If a stub write or original removal fails below, the on-disk state
    // already reflects reality (NotSynced, possibly with a missing stub)
    // and a re-hydration pass can recover. The previous ordering could
    // leave a stub on disk, the original gone, and status still Synced —
    // which would make the CLI lie to the daemon.
    let mut state = tcfs_sync::state::StateCache::open(state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;
    for path in paths {
        state.set_status(path, tcfs_sync::state::FileSyncStatus::NotSynced);
    }
    state.flush().with_context(|| {
        format!(
            "flushing state cache before unsync: {}",
            state_path.display()
        )
    })?;
    drop(state);
    Ok(())
}

async fn apply_unsync_conversion(
    config: &tcfs_core::config::TcfsConfig,
    conversion: &UnsyncConversion,
) -> Result<()> {
    let sync_root = config.sync.sync_root.as_deref();
    let rel_path = tcfs_sync::engine::normalize_rel_path(&conversion.path, sync_root);
    let stub = tcfs_vfs::StubMeta::for_upload(
        &conversion.tracked.blake3,
        conversion.tracked.size,
        conversion.tracked.chunk_count,
        config.storage.resolved_prefix(),
        &rel_path,
    );

    // Now safe: any fs failure below leaves a recoverable
    // NotSynced-with-possibly-missing-stub state.
    tokio::fs::write(&conversion.stub_full, stub.to_bytes())
        .await
        .with_context(|| format!("writing stub: {}", conversion.stub_full.display()))?;
    tokio::fs::remove_file(&conversion.path)
        .await
        .with_context(|| format!("removing hydrated file: {}", conversion.path.display()))?;

    Ok(())
}

fn print_dirty_unsync_paths(root: &std::path::Path, dirty: &[DirtyUnsyncPath]) {
    eprintln!(
        "Refusing to unsync directory with dirty descendants: {}",
        root.display()
    );
    eprintln!("  dirty:");
    for entry in dirty {
        eprintln!("    {} ({})", entry.path.display(), entry.reason);
    }
    eprintln!("  summary: 0 converted, 0 skipped, {} dirty", dirty.len());
}

// ── `tcfs init` ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct InitOptions<'a> {
    device_name: Option<String>,
    check: bool,
    skip_config: bool,
    force_config: bool,
    config_out: Option<&'a Path>,
    fileprovider_config_out: Option<&'a Path>,
    non_interactive: bool,
    password: Option<String>,
}

async fn cmd_init(config: &tcfs_core::config::TcfsConfig, options: InitOptions<'_>) -> Result<()> {
    let InitOptions {
        device_name,
        check,
        skip_config,
        force_config,
        config_out,
        fileprovider_config_out,
        non_interactive,
        password,
    } = options;

    let device_name = device_name.unwrap_or_else(tcfs_secrets::device::default_device_name);
    let init_paths = InitPaths::resolve(config_out);
    let config_path = init_paths.config_path.clone();
    let master_key_path = init_paths.master_key_path.clone();
    let registry_path = init_paths.registry_path.clone();

    if check {
        return cmd_init_check(&init_paths);
    }

    // Step 1: Check if already initialized (master key file exists)
    if master_key_path.exists() {
        anyhow::bail!(
            "Already initialized: {} exists. Remove it to re-initialize.",
            master_key_path.display()
        );
    }
    if !skip_config && config_path.exists() && !force_config {
        anyhow::bail!(
            "Config already exists: {}. Pass --force-config to overwrite it or --skip-config to leave it unchanged.",
            config_path.display()
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
    std::fs::create_dir_all(&init_paths.config_dir)
        .with_context(|| format!("creating config dir: {}", init_paths.config_dir.display()))?;
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
    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;
    let (device_id, device_key) = registry.enroll_local(&device_name, None);
    let device_key_path = tcfs_secrets::device::device_secret_key_path(&registry_path, &device_id);
    tcfs_secrets::device::save_device_secret_key(&device_key_path, &device_key.secret_key, false)?;
    // TIN-1417 B4: sign the registry with the freshly written master key so the
    // very first registry on disk is signed (no migration window for new setups).
    registry.save_signed(&registry_path, master_key.as_bytes())?;

    let init_config = build_init_config(config, &master_key_path, &registry_path, &device_name);
    if !skip_config {
        write_init_config(&config_path, &init_config, force_config)?;
    }
    if let Some(fileprovider_config_path) = fileprovider_config_out {
        write_fileprovider_init_config(
            fileprovider_config_path,
            &init_config,
            &master_key_path,
            &device_id,
            force_config,
        )
        .await?;
    }

    // Step 7: Print success message
    println!();
    println!("tcfs initialized successfully.");
    println!();
    println!("  Device name:  {}", device_name);
    println!("  Device ID:    {}", device_id);
    println!("  Device key:   {}", device_key_path.display());
    println!("  Master key:   {}", master_key_path.display());
    println!("  Registry:     {}", registry_path.display());
    if !skip_config {
        println!("  Config:       {}", config_path.display());
    }
    if let Some(fileprovider_config_path) = fileprovider_config_out {
        println!("  FileProvider: {}", fileprovider_config_path.display());
    }
    println!();
    println!("Next steps:");
    if skip_config {
        println!("  1. Write a config.toml or re-run tcfs init without --skip-config");
        println!("  2. Start tcfsd with that config, then run tcfs status");
    } else {
        println!(
            "  1. Review configuration: tcfs --config {} config show",
            config_path.display()
        );
        println!(
            "  2. Start tcfsd with that config, then run: tcfs --config {} status",
            config_path.display()
        );
        println!(
            "  3. Push files: tcfs --config {} push /path/to/files",
            config_path.display()
        );
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitPaths {
    config_dir: PathBuf,
    config_path: PathBuf,
    master_key_path: PathBuf,
    registry_path: PathBuf,
}

impl InitPaths {
    fn resolve(config_out: Option<&Path>) -> Self {
        let config_path = config_out.map(Path::to_path_buf).unwrap_or_else(|| {
            let config_dir = default_user_config_dir();
            config_dir.join("config.toml")
        });
        let config_dir = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let master_key_path = config_dir.join("master.key");
        let registry_path = config_dir.join("devices.json");
        Self {
            config_dir,
            config_path,
            master_key_path,
            registry_path,
        }
    }
}

fn default_user_config_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        })
        .join("tcfs")
}

fn cmd_init_check(paths: &InitPaths) -> Result<()> {
    let mut missing = Vec::new();
    if !paths.config_path.exists() {
        missing.push(format!("config {}", paths.config_path.display()));
    }
    if !paths.master_key_path.exists() {
        missing.push(format!("master key {}", paths.master_key_path.display()));
    }
    if !paths.registry_path.exists() {
        missing.push(format!("device registry {}", paths.registry_path.display()));
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "tcfs is not initialized; missing {}. Run 'tcfs init'.",
            missing.join(", ")
        );
    }
    let registry = tcfs_secrets::device::DeviceRegistry::load(&paths.registry_path)?;
    if registry.devices.is_empty() {
        anyhow::bail!(
            "tcfs is not initialized; device registry {} has no enrolled devices. Run 'tcfs init'.",
            paths.registry_path.display()
        );
    }
    let configured_device_name = std::fs::read_to_string(&paths.config_path)
        .ok()
        .and_then(|content| toml::from_str::<tcfs_core::config::TcfsConfig>(&content).ok())
        .and_then(|config| config.sync.device_name);
    let active_devices: Vec<_> = registry.active_devices().collect();
    let local_device = configured_device_name
        .as_deref()
        .and_then(|name| {
            active_devices
                .iter()
                .copied()
                .find(|device| device.name == name)
        })
        .or_else(|| {
            if active_devices.len() == 1 {
                active_devices.first().copied()
            } else {
                None
            }
        })
        .with_context(|| {
            let expected = configured_device_name
                .as_deref()
                .unwrap_or("<unset; registry has multiple active devices>");
            format!(
                "tcfs is not initialized; local device '{expected}' was not found in {}",
                paths.registry_path.display()
            )
        })?;

    if !tcfs_secrets::device::is_real_age_public_key(&local_device.public_key) {
        anyhow::bail!(
            "tcfs is not initialized with a real device key; '{}' has a placeholder public key. Run 'tcfs init' with a fresh config or migrate the device registry.",
            local_device.name
        );
    }
    let key_path =
        tcfs_secrets::device::device_secret_key_path(&paths.registry_path, &local_device.device_id);
    if !key_path.exists() {
        anyhow::bail!(
            "tcfs is not initialized; missing device private key for '{}' ({}). Run 'tcfs init' with a fresh config or restore the device key backup.",
            local_device.name,
            key_path.display()
        );
    }

    println!("tcfs init check [ok]");
    println!("  Config:     {}", paths.config_path.display());
    println!("  Master key: {}", paths.master_key_path.display());
    println!("  Registry:   {}", paths.registry_path.display());
    Ok(())
}

fn build_init_config(
    base: &tcfs_core::config::TcfsConfig,
    master_key_path: &Path,
    registry_path: &Path,
    device_name: &str,
) -> tcfs_core::config::TcfsConfig {
    let mut config = base.clone();
    config.crypto.enabled = true;
    config.crypto.master_key_file = Some(master_key_path.to_path_buf());
    config.sync.device_identity = Some(registry_path.to_path_buf());
    config.sync.device_name = Some(device_name.to_string());
    config
}

fn resolve_fileprovider_device_id(
    config: &tcfs_core::config::TcfsConfig,
    explicit: Option<&str>,
) -> Result<String> {
    if let Some(device_id) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(device_id.to_string());
    }

    if let Some(registry_path) = &config.sync.device_identity {
        if let Ok(registry) = tcfs_secrets::device::DeviceRegistry::load(registry_path) {
            let active_devices: Vec<_> = registry.active_devices().collect();
            if let Some(device_name) = config.sync.device_name.as_deref() {
                if let Some(device) = active_devices
                    .iter()
                    .copied()
                    .find(|device| device.name == device_name)
                {
                    if !device.device_id.is_empty() {
                        return Ok(device.device_id.clone());
                    }
                }
            }
            if active_devices.len() == 1 && !active_devices[0].device_id.is_empty() {
                return Ok(active_devices[0].device_id.clone());
            }
        }
    }

    if let Some(device_name) = config
        .sync
        .device_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(device_name.to_string());
    }

    anyhow::bail!(
        "FileProvider config requires a device id; pass --device-id or configure sync.device_name/device_identity"
    )
}

fn resolve_fileprovider_master_key_path(
    config: &tcfs_core::config::TcfsConfig,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| config.crypto.master_key_file.clone())
        .context(
            "FileProvider config requires a master key file; pass --master-key-file or configure crypto.master_key_file",
        )
}

fn write_init_config(
    config_path: &Path,
    config: &tcfs_core::config::TcfsConfig,
    force: bool,
) -> Result<()> {
    if config_path.exists() && !force {
        anyhow::bail!(
            "Config already exists: {}. Pass --force-config to overwrite it.",
            config_path.display()
        );
    }
    if let Some(parent) = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir: {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(config).context("serializing init config to TOML")?;
    std::fs::write(config_path, rendered)
        .with_context(|| format!("writing config: {}", config_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on: {}", config_path.display()))?;
    }
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct FileProviderInitConfig {
    s3_endpoint: String,
    /// Explicit compatibility opt-in for isolated plaintext development S3.
    allow_insecure_http: bool,
    s3_bucket: String,
    s3_access: String,
    s3_secret: String,
    remote_prefix: String,
    device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_socket: Option<String>,
    master_key_file: String,
    /// File-key wrap mode (TIN-1417). Mirrors `crypto.wrap_mode` from the active
    /// config. The FileProvider extension reads this key to decide whether to
    /// build a device-aware `EncryptionContext` (see `tcfs-file-provider`
    /// `device_ctx`). Omitted from the rendered JSON when `Master` (the default)
    /// so the default-off output stays byte-identical to the legacy master-only
    /// bootstrap.
    #[serde(skip_serializing_if = "is_master_wrap_mode")]
    wrap_mode: tcfs_core::config::WrapMode,
    /// Path to the device registry (`devices.json`) the FileProvider must read
    /// to resolve active age recipients + this device's secret
    /// (`device-<id>.age` alongside it). Only emitted when `wrap_mode` is not
    /// `Master`; the extension otherwise falls back to its own default registry
    /// path.
    #[serde(skip_serializing_if = "Option::is_none")]
    device_registry_path: Option<String>,
}

/// `skip_serializing_if` predicate so `wrap_mode` is omitted from the rendered
/// FileProvider JSON when it is the default `Master`, keeping default-off output
/// byte-identical to the legacy bootstrap.
fn is_master_wrap_mode(value: &tcfs_core::config::WrapMode) -> bool {
    *value == tcfs_core::config::WrapMode::Master
}

fn build_fileprovider_init_config(
    config: &tcfs_core::config::TcfsConfig,
    s3: &tcfs_secrets::S3Credentials,
    master_key_path: &Path,
    device_id: &str,
) -> FileProviderInitConfig {
    use tcfs_core::config::WrapMode;
    let wrap_mode = config.crypto.wrap_mode;
    // Only surface the device registry path when per-device wrapping is involved
    // (Dual/PerDevice), so the default-off rendered JSON is byte-identical to the
    // legacy bootstrap.
    let device_registry_path = if wrap_mode == WrapMode::Master {
        None
    } else {
        Some(
            resolve_fileprovider_registry_path(config)
                .to_string_lossy()
                .into_owned(),
        )
    };
    FileProviderInitConfig {
        s3_endpoint: config.storage.endpoint.clone(),
        allow_insecure_http: !config.storage.enforce_tls,
        s3_bucket: config.storage.bucket.clone(),
        s3_access: s3.access_key_id.clone(),
        s3_secret: s3.secret_access_key.expose_secret().to_string(),
        remote_prefix: config.storage.resolved_prefix().to_string(),
        device_id: device_id.to_string(),
        daemon_endpoint: config.daemon.fileprovider_endpoint.clone(),
        daemon_socket: config
            .daemon
            .fileprovider_socket
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        master_key_file: master_key_path.to_string_lossy().into_owned(),
        wrap_mode,
        device_registry_path,
    }
}

/// Resolve the device-registry (`devices.json`) path the FileProvider should
/// read for per-device unwrap. Mirrors `resolve_fileprovider_device_id`: prefer
/// the configured `sync.device_identity` registry path, falling back to the
/// shared default. The device secret key (`device-<id>.age`) lives alongside
/// this file and is derived by the extension via
/// `tcfs_secrets::device::device_secret_key_path`.
fn resolve_fileprovider_registry_path(config: &tcfs_core::config::TcfsConfig) -> PathBuf {
    config
        .sync
        .device_identity
        .clone()
        .unwrap_or_else(tcfs_secrets::device::default_registry_path)
}

async fn write_fileprovider_init_config(
    config_path: &Path,
    config: &tcfs_core::config::TcfsConfig,
    master_key_path: &Path,
    device_id: &str,
    force: bool,
) -> Result<()> {
    if config_path.exists() && !force {
        anyhow::bail!(
            "FileProvider config already exists: {}. Pass --force-config to overwrite it.",
            config_path.display()
        );
    }
    let cred_store = tcfs_secrets::CredStore::load(&config.secrets, &config.storage)
        .await
        .context("credential discovery failed for FileProvider init config")?;
    let s3 = cred_store.s3.context(
        "S3 credentials not found for FileProvider init config.\n\
         Set TCFS_S3_ACCESS and TCFS_S3_SECRET environment variables,\n\
         or configure storage.credentials_file in tcfs.toml,\n\
         or use ~/.aws/credentials file.",
    )?;
    tracing::info!(source = %cred_store.source, "FileProvider init credentials loaded");

    let rendered = serde_json::to_string_pretty(&build_fileprovider_init_config(
        config,
        &s3,
        master_key_path,
        device_id,
    ))
    .context("serializing FileProvider init config to JSON")?;
    write_fileprovider_config_file(config_path, &rendered, force)
}

fn write_fileprovider_config_file(config_path: &Path, rendered: &str, force: bool) -> Result<()> {
    if config_path.exists() && !force {
        anyhow::bail!(
            "FileProvider config already exists: {}. Pass --force-config to overwrite it.",
            config_path.display()
        );
    }
    if let Some(parent) = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating FileProvider config dir: {}", parent.display()))?;
    }

    let parent = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let temp_path = parent.join(format!(".{filename}.{}.tmp", std::process::id()));
    let write_result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(&temp_path).with_context(|| {
            format!(
                "creating FileProvider config temp file: {}",
                temp_path.display()
            )
        })?;
        file.write_all(rendered.as_bytes()).with_context(|| {
            format!(
                "writing FileProvider config temp file: {}",
                temp_path.display()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "syncing FileProvider config temp file: {}",
                temp_path.display()
            )
        })?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on: {}", temp_path.display()))?;
    }
    if config_path.exists() && !force {
        let _ = std::fs::remove_file(&temp_path);
        anyhow::bail!(
            "FileProvider config already exists: {}. Pass --force-config to overwrite it.",
            config_path.display()
        );
    }

    #[cfg(windows)]
    if force && config_path.exists() {
        std::fs::remove_file(config_path)
            .with_context(|| format!("replacing FileProvider config: {}", config_path.display()))?;
    }

    if let Err(error) = std::fs::rename(&temp_path, config_path)
        .with_context(|| format!("installing FileProvider config: {}", config_path.display()))
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }

    Ok(())
}

fn rand_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut salt);
    salt
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

fn cmd_device_revoke(config: &tcfs_core::config::TcfsConfig, name: &str) -> Result<()> {
    let registry_path = tcfs_secrets::device::default_registry_path();
    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;

    // Capture the public key for the forward-secrecy notice before mutating.
    let recipient = registry.find(name).map(|d| d.public_key.clone());

    if registry.revoke(name) {
        // TIN-1899: recipient-set REMOVAL is the DEFAULT, cheap, immediate action.
        // `revoke()` drops the device from `active_devices()`, so every recipient
        // set built afterwards (CLI/daemon/FileProvider via load_verified) excludes
        // it: NO new content is wrapped to the revoked device. We re-sign so the
        // signed envelope records `revoked + revoked_at` and the removal is
        // trustworthy (TIN-1417 B4).
        save_registry_signed_or_warn(&mut registry, &registry_path, config)?;
        println!("Revoked device: {name}");
        println!("  Dropped from the recipient set (immediate): no NEW content will be wrapped to this device.");
        if let Some(recipient) = recipient {
            println!("  Removed age recipient: {recipient}");
        }

        // LOUD forward-secrecy warning: recipient-set removal alone does NOT
        // re-key content the device could already read.
        eprintln!();
        eprintln!(
            "  WARNING (forward secrecy): the revoked device RETAINS read access to content it \
             already pulled AND to any content that has not yet been re-keyed (its old FileKey \
             wraps and cached chunks are unchanged)."
        );
        eprintln!(
            "  To achieve forward secrecy, re-key the affected content (expensive):\n      \
             tcfs key rotate <prefix> --rotate-keys\n  This generates fresh FileKeys, re-encrypts \
             content under new addresses, and re-wraps ONLY to the current recipient set."
        );
    } else {
        anyhow::bail!("Device '{name}' not found");
    }

    Ok(())
}

// ── `tcfs device enroll` ──────────────────────────────────────────────────────

async fn cmd_device_enroll(
    config: &tcfs_core::config::TcfsConfig,
    name: Option<String>,
    repair_placeholder: bool,
    sync_remote: bool,
    accept_unsigned_remote: bool,
) -> Result<()> {
    let device_name = name.unwrap_or_else(tcfs_secrets::device::default_device_name);
    let registry_path = tcfs_secrets::device::default_registry_path();
    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;

    let mut enrolled_or_repaired = false;
    let device_id: String;
    let public_key: String;
    let mut device_key_path: Option<PathBuf> = None;

    if let Some(device) = registry.find(&device_name) {
        if !tcfs_secrets::device::is_real_age_public_key(&device.public_key) {
            if !repair_placeholder {
                anyhow::bail!(
                    "Device '{}' is already enrolled with a placeholder/legacy public key. Re-run with --repair-placeholder to generate a real age device key.",
                    device_name
                );
            }
            let key_path =
                repair_placeholder_device_key(&mut registry, &registry_path, &device_name)?;
            device_key_path = Some(key_path);
            enrolled_or_repaired = true;
        } else if !sync_remote {
            anyhow::bail!(
                "Device '{}' is already enrolled. Use 'tcfs device list' to see devices.",
                device_name
            );
        }
        let device = registry.find(&device_name).with_context(|| {
            format!(
                "device '{}' disappeared while preparing enrollment output",
                device_name
            )
        })?;
        device_id = device.device_id.clone();
        public_key = device.public_key.clone();
    } else {
        let (new_device_id, device_key) = registry.enroll_local(&device_name, None);
        let key_path = tcfs_secrets::device::device_secret_key_path(&registry_path, &new_device_id);
        tcfs_secrets::device::save_device_secret_key(&key_path, &device_key.secret_key, false)?;
        device_id = new_device_id;
        public_key = device_key.public_key;
        device_key_path = Some(key_path);
        enrolled_or_repaired = true;
    }

    save_registry_signed_or_warn(&mut registry, &registry_path, config)?;

    if sync_remote {
        let op = build_operator(config).await?;
        let meta_prefix = config.storage.resolved_prefix();
        // TIN-1417 B4: verify the remote registry before merging. The remote object
        // store is the primary tamper surface, so a signature-PRESENT-but-invalid
        // remote is hard-rejected by `load_remote_verified`. An UNSIGNED (legacy)
        // remote verifies as `UnsignedLegacy` and its trust MUST be bound here: if
        // we merged it and then re-signed the result with our real master, an
        // attacker who stripped the signature and injected a recipient would have
        // their entry LAUNDERED into a validly-signed registry. So we refuse to
        // merge an unsigned remote unless the operator explicitly opts in with
        // `--accept-unsigned-remote` (loud warning below).
        let remote = match master_key_for_registry_signing(config) {
            Some(mk) => {
                let (remote, trust) = tcfs_secrets::device::DeviceRegistry::load_remote_verified(
                    &op,
                    meta_prefix,
                    mk.as_bytes(),
                )
                .await?;
                enforce_remote_merge_trust(trust, accept_unsigned_remote)?;
                remote
            }
            None => {
                // No master key available: we cannot verify the remote at all, and
                // we will write the merged result UNSIGNED anyway (no laundering
                // into a signed registry is possible). Preserve legacy behaviour.
                tcfs_secrets::device::DeviceRegistry::load_remote(&op, meta_prefix).await?
            }
        };
        merge_device_registry(&mut registry, &remote)?;
        match master_key_for_registry_signing(config) {
            Some(mk) => {
                registry
                    .sync_to_remote_signed(&op, meta_prefix, mk.as_bytes())
                    .await?
            }
            None => {
                tcfs_secrets::device::DeviceRegistry::sync_to_remote(&registry, &op, meta_prefix)
                    .await?
            }
        }
        save_registry_signed_or_warn(&mut registry, &registry_path, config)?;
    }

    if enrolled_or_repaired {
        println!("Device enrolled:");
    } else {
        println!("Device already enrolled:");
    }
    println!("  name:      {}", device_name);
    println!("  device_id: {}", device_id);
    println!("  public_key: {}", public_key);
    if let Some(path) = device_key_path {
        println!("  key:       {}", path.display());
    }
    println!("  registry:  {}", registry_path.display());
    if sync_remote {
        println!(
            "  remote:    {}/tcfs-meta/devices.json",
            config.storage.resolved_prefix().trim_end_matches('/')
        );
    }
    println!();
    if sync_remote {
        println!("Next: run the same command on peer devices to pull the merged registry.");
    } else {
        println!("Next: configure sync in tcfs.toml and run 'tcfs push'");
    }

    Ok(())
}

/// TIN-1417 B4 — close the unsigned-remote LAUNDERING bypass on the enroll
/// `--sync-remote` path.
///
/// `load_remote_verified` already HARD-REJECTS a signature-present-but-invalid
/// remote (tampering). The remaining hole is `RegistryTrust::UnsignedLegacy`: an
/// attacker can strip the signature off the remote `devices.json` and inject a
/// hostile recipient; the stripped registry verifies as "unsigned" rather than
/// "tampered". If we merged that into our local registry and then re-signed the
/// result with the real master key, the injected recipient would be laundered
/// into a validly-signed registry — defeating B4 entirely.
///
/// So we REFUSE to merge an unsigned remote by default. The operator may opt in
/// with `--accept-unsigned-remote` (e.g. a genuine one-time migration of a
/// trusted legacy fleet), which logs a loud warning and proceeds.
///
/// MIGRATION WINDOW (TODO TIN-1417 B5, hard-reject by 2026-09-01): once all
/// fleets have re-signed at least once, drop `--accept-unsigned-remote` and make
/// an unsigned remote on the merge path an unconditional hard error. Track the
/// last-seen-unsigned timestamp in fleet telemetry to confirm the window can
/// close.
fn enforce_remote_merge_trust(
    trust: tcfs_secrets::device::RegistryTrust,
    accept_unsigned_remote: bool,
) -> Result<()> {
    match trust {
        tcfs_secrets::device::RegistryTrust::Signed => Ok(()),
        tcfs_secrets::device::RegistryTrust::UnsignedLegacy => {
            if accept_unsigned_remote {
                tracing::warn!(
                    "TIN-1417 B4: merging an UNSIGNED (legacy) remote device registry because \
                     --accept-unsigned-remote was passed. Its recipient set is UNVERIFIED; you \
                     are about to RE-SIGN it with this device's master key. Only proceed if you \
                     trust the remote object store has not been tampered with."
                );
                eprintln!(
                    "WARNING: accepting and re-signing an UNSIGNED remote device registry \
                     (--accept-unsigned-remote). Its recipients are UNVERIFIED."
                );
                Ok(())
            } else {
                anyhow::bail!(
                    "TIN-1417 B4: refusing to merge an UNSIGNED (legacy) remote device registry \
                     on the enroll --sync-remote path. Merging then re-signing it with this \
                     device's master key would launder any attacker-injected recipient into a \
                     validly-signed registry. Re-save the remote with a master-key-holding \
                     command to sign it, or (only if you trust the remote store) re-run with \
                     --accept-unsigned-remote to explicitly accept and re-sign it."
                );
            }
        }
    }
}

fn repair_placeholder_device_key(
    registry: &mut tcfs_secrets::device::DeviceRegistry,
    registry_path: &Path,
    device_name: &str,
) -> Result<PathBuf> {
    let needs_device_id = registry
        .find(device_name)
        .map(|device| device.device_id.is_empty())
        .unwrap_or(false);
    if needs_device_id {
        registry.backfill_device_id(device_name);
    }

    let device = registry
        .devices
        .iter_mut()
        .find(|device| device.name == device_name)
        .with_context(|| format!("device '{device_name}' not found in registry"))?;

    if tcfs_secrets::device::is_real_age_public_key(&device.public_key) {
        anyhow::bail!("device '{device_name}' already has a real age public key");
    }

    let key = tcfs_secrets::device::generate_local_device_key();
    device.public_key = key.public_key.clone();
    device.signing_key_hash =
        blake3::hash(device.public_key.as_bytes()).to_hex().as_str()[..16].to_string();
    device.revoked = false;

    let key_path = tcfs_secrets::device::device_secret_key_path(registry_path, &device.device_id);
    tcfs_secrets::device::save_device_secret_key(&key_path, &key.secret_key, false)?;
    Ok(key_path)
}

fn merge_device_registry(
    local: &mut tcfs_secrets::device::DeviceRegistry,
    incoming: &tcfs_secrets::device::DeviceRegistry,
) -> Result<usize> {
    let mut changed = 0usize;
    for incoming_device in &incoming.devices {
        let existing = local.devices.iter_mut().find(|device| {
            (!device.device_id.is_empty() && device.device_id == incoming_device.device_id)
                || device.name == incoming_device.name
        });

        if let Some(existing_device) = existing {
            if merge_device_entry(existing_device, incoming_device)? {
                changed += 1;
            }
        } else {
            local.devices.push(incoming_device.clone());
            changed += 1;
        }
    }
    Ok(changed)
}

fn merge_device_entry(
    existing: &mut tcfs_secrets::device::DeviceIdentity,
    incoming: &tcfs_secrets::device::DeviceIdentity,
) -> Result<bool> {
    let existing_real = tcfs_secrets::device::is_real_age_public_key(&existing.public_key);
    let incoming_real = tcfs_secrets::device::is_real_age_public_key(&incoming.public_key);

    if existing_real
        && incoming_real
        && existing.public_key != incoming.public_key
        && (existing.device_id == incoming.device_id || existing.name == incoming.name)
    {
        anyhow::bail!(
            "registry conflict for device '{}' ({}): two real public keys differ",
            existing.name,
            existing.device_id
        );
    }

    if incoming_real && !existing_real {
        *existing = incoming.clone();
        return Ok(true);
    }

    let mut changed = false;
    if existing.device_id.is_empty() && !incoming.device_id.is_empty() {
        existing.device_id = incoming.device_id.clone();
        changed = true;
    }
    if existing.signing_key_hash.is_empty() && !incoming.signing_key_hash.is_empty() {
        existing.signing_key_hash = incoming.signing_key_hash.clone();
        changed = true;
    }
    if existing.description.is_none() && incoming.description.is_some() {
        existing.description = incoming.description.clone();
        changed = true;
    }
    if existing.revoked != incoming.revoked && incoming.revoked {
        existing.revoked = true;
        changed = true;
    }
    if incoming.last_nats_seq > existing.last_nats_seq {
        existing.last_nats_seq = incoming.last_nats_seq;
        changed = true;
    }
    Ok(changed)
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
    let mut client = connect_daemon_without_session(&config.daemon.socket).await?;
    let resp = tokio::time::timeout(
        DAEMON_RPC_TIMEOUT,
        client.auth_unlock(tcfs_core::proto::AuthUnlockRequest {
            master_key: key_bytes,
        }),
    )
    .await
    .context("auth_unlock RPC timed out")?
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
    let mut client = connect_daemon_without_session(&config.daemon.socket).await?;
    let resp = tokio::time::timeout(
        DAEMON_RPC_TIMEOUT,
        client.auth_lock(tcfs_core::proto::Empty {}),
    )
    .await
    .context("auth_lock RPC timed out")?
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
    let mut client = connect_daemon_without_session(&config.daemon.socket).await?;
    let resp = tokio::time::timeout(
        DAEMON_RPC_TIMEOUT,
        client.auth_status(tcfs_core::proto::Empty {}),
    )
    .await
    .context("auth_status RPC timed out")?
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
        let saved = match store_session_token(&resp.session_token) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("Warning: failed to save session token to platform keychain: {err:#}");
                false
            }
        };
        println!("Authentication successful.");
        if saved {
            println!("Session token saved to platform keychain.");
        } else {
            println!("Session token was not saved; set TCFS_SESSION_TOKEN to use it manually.");
        }
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
fn populate_invite_routing_metadata(
    invite: &mut tcfs_auth::enrollment::EnrollmentInvite,
    config: &tcfs_core::config::TcfsConfig,
) -> Result<()> {
    invite.storage_endpoint = Some(
        tcfs_core::config::http_endpoint_origin(&config.storage.endpoint).ok_or_else(|| {
            anyhow::anyhow!(
                "storage endpoint must be an absolute HTTP(S) URL before creating a device invite"
            )
        })?,
    );
    invite.storage_bucket = Some(config.storage.bucket.clone());
    invite.remote_prefix = Some(config.storage.resolved_prefix().to_string());
    Ok(())
}

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
        anyhow::bail!(
            "cannot create a device invite without a master key at {}; run tcfs init or configure crypto.master_key_file",
            key_path.display(),
        );
    };

    let mut invite = EnrollmentInvite::new(
        &status.device_id,
        &signing_key,
        expiry_hours,
        DevicePermissions::default(),
    );

    // Include non-secret routing metadata. Secret bootstrap material is brokered
    // by tcfsd during DeviceEnroll and wrapped to the joining device public key.
    populate_invite_routing_metadata(&mut invite, config)?;

    invite.refresh_signature(&signing_key);

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
        sanitize_http_endpoint_for_display(&config.storage.endpoint),
        config.storage.bucket
    );
    println!("Credentials: not embedded in invite; daemon wraps bootstrap during enrollment");
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum KeyRotationStatus {
    RewritingManifests,
    ReadyToSwap,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct KeyRotationState {
    version: u32,
    started_at: u64,
    manifest_prefix: String,
    pending_key_path: String,
    status: KeyRotationStatus,
    rotated_manifests: u64,
    already_rotated_manifests: u64,
    /// Manifests that carry NO wrapped FileKey at all (genuinely plaintext /
    /// unencrypted content). The master rotate has nothing to re-wrap for these,
    /// so they are skipped. Renamed from the old `skipped_plaintext_manifests`
    /// (TIN-1899): that name conflated two very different cases. This counter now
    /// means *only* "no key material present". Per-device-only manifests are
    /// tracked separately by `skipped_per_device_manifests`.
    skipped_keyless_manifests: u64,
    /// Manifests that carry ONLY per-device wraps (`encrypted_file_key == None`,
    /// `wrapped_file_keys` non-empty) and therefore cannot be re-wrapped by the
    /// MASTER rotate (TIN-1899). The scoped `tcfs key rotate <prefix>` command is
    /// the only path that re-keys these; the master rotate records them here and
    /// loudly tells the operator to run the scoped command for forward secrecy.
    #[serde(default)]
    skipped_per_device_manifests: u64,
    error_count: u64,
    last_manifest_path: Option<String>,
}

impl KeyRotationState {
    fn new(manifest_prefix: &str, pending_key_path: &Path) -> Self {
        Self {
            version: 1,
            started_at: now_epoch(),
            manifest_prefix: manifest_prefix.to_string(),
            pending_key_path: pending_key_path.display().to_string(),
            status: KeyRotationStatus::RewritingManifests,
            rotated_manifests: 0,
            already_rotated_manifests: 0,
            skipped_keyless_manifests: 0,
            skipped_per_device_manifests: 0,
            error_count: 0,
            last_manifest_path: None,
        }
    }

    fn reset_scan_progress(&mut self) {
        self.status = KeyRotationStatus::RewritingManifests;
        self.rotated_manifests = 0;
        self.already_rotated_manifests = 0;
        self.skipped_keyless_manifests = 0;
        self.skipped_per_device_manifests = 0;
        self.error_count = 0;
        self.last_manifest_path = None;
    }
}

#[derive(Debug, Clone)]
struct KeyRotationPaths {
    state_path: PathBuf,
    pending_key_path: PathBuf,
}

#[derive(Debug)]
struct PreparedKeyRotation {
    old_master: tcfs_crypto::MasterKey,
    new_master: tcfs_crypto::MasterKey,
    state: KeyRotationState,
    paths: KeyRotationPaths,
    resumed: bool,
}

fn key_rotation_paths(key_path: &Path) -> KeyRotationPaths {
    let parent = atomic_write_parent(key_path);
    let file_name = key_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    KeyRotationPaths {
        state_path: parent.join(format!(".{file_name}.rotate-state.json")),
        pending_key_path: parent.join(format!(".{file_name}.rotate-pending")),
    }
}

const ATOMIC_WRITE_TEMP_ATTEMPTS: usize = 16;

fn atomic_write_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn atomic_write_temp_path(path: &Path, nonce: u128) -> PathBuf {
    atomic_write_parent(path).join(format!(
        ".{}.tmp.{nonce:032x}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<()> {
    std::fs::File::open(parent)
        .with_context(|| format!("opening parent directory for sync: {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("syncing parent directory: {}", parent.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    Ok(())
}

fn atomic_write_bytes(path: &Path, data: &[u8], mode: Option<u32>) -> Result<()> {
    atomic_write_bytes_with_nonce_source(path, data, mode, rand::random::<u128>)
}

fn atomic_write_bytes_with_nonce_source(
    path: &Path,
    data: &[u8],
    mode: Option<u32>,
    mut next_nonce: impl FnMut() -> u128,
) -> Result<()> {
    use std::fs::OpenOptions;

    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = atomic_write_parent(path);
    for _ in 0..ATOMIC_WRITE_TEMP_ATTEMPTS {
        let tmp_path = atomic_write_temp_path(path, next_nonce());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(mode.unwrap_or(0o600));

        let mut file = match options.open(&tmp_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating temp file: {}", tmp_path.display()));
            }
        };

        let persist_result = (|| -> Result<()> {
            #[cfg(unix)]
            if let Some(mode) = mode {
                file.set_permissions(std::fs::Permissions::from_mode(mode))
                    .with_context(|| format!("setting permissions on: {}", tmp_path.display()))?;
            }

            file.write_all(data)
                .with_context(|| format!("writing temp file: {}", tmp_path.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing temp file: {}", tmp_path.display()))?;
            drop(file);

            std::fs::rename(&tmp_path, path).with_context(|| {
                format!("renaming {} to {}", tmp_path.display(), path.display())
            })?;
            sync_parent_directory(parent)?;
            Ok(())
        })();

        if persist_result.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        return persist_result;
    }

    anyhow::bail!(
        "unable to allocate a unique temp file for {} after {} attempts",
        path.display(),
        ATOMIC_WRITE_TEMP_ATTEMPTS
    )
}

fn write_rotation_state(path: &Path, state: &KeyRotationState) -> Result<()> {
    let data = serde_json::to_vec_pretty(state).context("serializing key rotation state")?;
    atomic_write_bytes(path, &data, Some(0o600))
}

fn read_rotation_state(path: &Path) -> Result<KeyRotationState> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading key rotation state: {}", path.display()))?;
    serde_json::from_slice(&data).context("parsing key rotation state")
}

/// Best-effort load of the master key for *signing the device registry*
/// (TIN-1417 B4). Returns `None` (with a loud warning) when no master key file is
/// configured/readable, so registry mutations still succeed but leave the
/// registry unsigned for the migration window instead of hard-failing.
fn master_key_for_registry_signing(
    config: &tcfs_core::config::TcfsConfig,
) -> Option<tcfs_crypto::MasterKey> {
    let path = config.crypto.master_key_file.as_ref()?;
    match read_master_key(path) {
        Ok(k) => Some(k),
        Err(e) => {
            tracing::warn!(
                "TIN-1417 B4: cannot read master key for registry signing ({e}); the device \
                 registry will be written UNSIGNED. Per-device wrapping will refuse to trust it."
            );
            None
        }
    }
}

/// Sign (if a master key is available) and save the registry to disk. Falls back
/// to an unsigned save with a warning when no master key is configured.
fn save_registry_signed_or_warn(
    registry: &mut tcfs_secrets::device::DeviceRegistry,
    path: &Path,
    config: &tcfs_core::config::TcfsConfig,
) -> Result<()> {
    match master_key_for_registry_signing(config) {
        Some(mk) => registry.save_signed(path, mk.as_bytes()),
        None => registry.save(path),
    }
}

fn read_master_key(path: &Path) -> Result<tcfs_crypto::MasterKey> {
    use tcfs_crypto::KEY_SIZE;

    let bytes =
        std::fs::read(path).with_context(|| format!("reading master key: {}", path.display()))?;
    if bytes.len() != KEY_SIZE {
        anyhow::bail!(
            "master key has wrong size: {} bytes (expected {})",
            bytes.len(),
            KEY_SIZE
        );
    }

    let mut key_bytes = [0u8; KEY_SIZE];
    key_bytes.copy_from_slice(&bytes);
    Ok(tcfs_crypto::MasterKey::from_bytes(key_bytes))
}

fn write_master_key(path: &Path, key: &tcfs_crypto::MasterKey) -> Result<()> {
    atomic_write_bytes(path, key.as_bytes(), Some(0o600))
        .with_context(|| format!("writing master key: {}", path.display()))
}

fn cleanup_rotation_artifacts(paths: &KeyRotationPaths) {
    cleanup_rotation_artifacts_with_sync(paths, sync_parent_directory);
}

fn cleanup_rotation_artifacts_with_sync(
    paths: &KeyRotationPaths,
    mut sync_parent: impl FnMut(&Path) -> Result<()>,
) {
    // Remove the state first. A crash between removals then leaves only an
    // ignorable pending key; leaving state without its pending key would make
    // the next invocation fail before it can observe the already-installed key.
    let state_removed = match std::fs::remove_file(&paths.state_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            eprintln!(
                "  WARN: failed to remove {}: {error}",
                paths.state_path.display()
            );
            false
        }
    };

    if !state_removed {
        return;
    }

    let parent = atomic_write_parent(&paths.state_path);
    if let Err(error) = sync_parent(parent) {
        eprintln!(
            "  WARN: failed to durably remove rotation state in {}: {error}",
            parent.display()
        );
        return;
    }

    let pending_removed = match std::fs::remove_file(&paths.pending_key_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            eprintln!(
                "  WARN: failed to remove {}: {error}",
                paths.pending_key_path.display()
            );
            false
        }
    };
    if pending_removed {
        if let Err(error) = sync_parent(parent) {
            eprintln!(
                "  WARN: failed to durably remove pending rotation key in {}: {error}",
                parent.display()
            );
        }
    }
}

fn finalize_key_rotation(
    key_path: &Path,
    new_master: &tcfs_crypto::MasterKey,
    paths: &KeyRotationPaths,
) -> Result<()> {
    // `write_master_key` returns only after syncing both the replacement file
    // and its parent directory. Keep the resume artifacts until that durable
    // point so any interrupted final swap remains recoverable.
    write_master_key(key_path, new_master)?;
    cleanup_rotation_artifacts(paths);
    Ok(())
}

/// Read the EXACT new master key from a file (TIN-2856): either exactly 32 raw
/// bytes, or 64 hex chars with optional trailing whitespace (e.g. a newline).
/// Used when the operator pre-derives the key externally (the fleet unlock
/// wrapper re-derives the daemon key as SHA-256 of the passphrase file on every
/// unlock, so rotate-key must adopt that exact key instead of minting its own).
fn read_exact_new_master_key(path: &Path) -> Result<tcfs_crypto::MasterKey> {
    use tcfs_crypto::KEY_SIZE;

    let bytes = std::fs::read(path)
        .with_context(|| format!("reading new master key: {}", path.display()))?;

    if bytes.len() == KEY_SIZE {
        let mut key_bytes = [0u8; KEY_SIZE];
        key_bytes.copy_from_slice(&bytes);
        return Ok(tcfs_crypto::MasterKey::from_bytes(key_bytes));
    }

    // Not raw-sized: accept a hex digest (`sha256sum` column, `echo <hex>`).
    let hex = std::str::from_utf8(&bytes)
        .ok()
        .map(str::trim_end)
        .filter(|s| s.len() == KEY_SIZE * 2 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let Some(hex) = hex else {
        anyhow::bail!(
            "new master key file {} must be exactly {} raw bytes or {} hex chars \
             (optionally newline-terminated); got {} bytes",
            path.display(),
            KEY_SIZE,
            KEY_SIZE * 2,
            bytes.len()
        );
    };

    let mut key_bytes = [0u8; KEY_SIZE];
    for (i, byte) in key_bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).context("parsing hex master key")?;
    }
    Ok(tcfs_crypto::MasterKey::from_bytes(key_bytes))
}

fn generate_new_master_key(
    use_password: bool,
    non_interactive: bool,
) -> Result<tcfs_crypto::MasterKey> {
    if use_password {
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
        )
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

        Ok(master_key)
    }
}

fn prepare_key_rotation(
    key_path: &Path,
    manifest_prefix: &str,
    use_password: bool,
    non_interactive: bool,
    new_key_file: Option<&Path>,
) -> Result<Option<PreparedKeyRotation>> {
    let paths = key_rotation_paths(key_path);

    if paths.state_path.exists() {
        let mut state = read_rotation_state(&paths.state_path)?;
        if state.manifest_prefix != manifest_prefix {
            anyhow::bail!(
                "pending key rotation targets {} but current config resolved to {}",
                state.manifest_prefix,
                manifest_prefix
            );
        }

        let new_master = read_master_key(&paths.pending_key_path).with_context(|| {
            format!(
                "reading pending rotation key: {}",
                paths.pending_key_path.display()
            )
        })?;

        // TIN-2856: resuming with an explicit --new-key-file must target the
        // SAME key the pending rotation already committed to; anything else
        // would silently rotate the fleet to a key the operator did not supply.
        if let Some(new_key_file) = new_key_file {
            let supplied = read_exact_new_master_key(new_key_file)?;
            if supplied.as_bytes() != new_master.as_bytes() {
                anyhow::bail!(
                    "pending rotation key {} does not match --new-key-file {}; \
                     finish the pending rotation (or remove its state/pending files) \
                     before rotating to a different key",
                    paths.pending_key_path.display(),
                    new_key_file.display()
                );
            }
        }

        let current_master = read_master_key(key_path)?;

        if current_master.as_bytes() == new_master.as_bytes() {
            cleanup_rotation_artifacts(&paths);
            return Ok(None);
        }

        state.reset_scan_progress();
        write_rotation_state(&paths.state_path, &state)?;

        return Ok(Some(PreparedKeyRotation {
            old_master: current_master,
            new_master,
            state,
            paths,
            resumed: true,
        }));
    }

    let old_master = read_master_key(key_path)?;
    let new_master = match new_key_file {
        Some(path) => {
            let key = read_exact_new_master_key(path)?;
            println!("New master key loaded exactly from: {}", path.display());
            key
        }
        None => generate_new_master_key(use_password, non_interactive)?,
    };
    // Persist the key first, including its directory entry, then the state that
    // makes it authoritative. `cmd_rotate_key` cannot rewrite remote data until
    // this function returns, so every remote rewrite has durable recovery data.
    write_master_key(&paths.pending_key_path, &new_master)?;

    let state = KeyRotationState::new(manifest_prefix, &paths.pending_key_path);
    write_rotation_state(&paths.state_path, &state)?;

    Ok(Some(PreparedKeyRotation {
        old_master,
        new_master,
        state,
        paths,
        resumed: false,
    }))
}

/// Capability proving that an in-place manifest rewrite is confined to a
/// legacy manifest-only root.
///
/// New TCFS manifests are immutable byte-addressed objects selected through
/// `index/<relative-path>`. Rewriting one at the same object key invalidates
/// its address, while selecting `manifests/<scope>` confuses object identity
/// with path identity. Keep the old rotation implementation callable only
/// after the storage layout has been checked and found unambiguously legacy.
#[derive(Debug)]
struct LegacyManifestMutationPermit {
    storage_prefix: String,
}

impl LegacyManifestMutationPermit {
    fn authorize_manifest_prefix(&self, manifest_prefix: &str) -> Result<()> {
        let expected = rotation_child_prefix(&self.storage_prefix, "manifests");
        anyhow::ensure!(
            manifest_prefix.starts_with(&expected),
            "legacy rotation permit for root {:?} cannot authorize manifest namespace {:?}",
            self.storage_prefix,
            manifest_prefix
        );
        Ok(())
    }

    fn authorize_remote_prefix(&self, remote_prefix: &str) -> Result<()> {
        anyhow::ensure!(
            normalize_rotation_storage_prefix(remote_prefix) == self.storage_prefix,
            "legacy rotation permit for root {:?} cannot authorize root {:?}",
            self.storage_prefix,
            remote_prefix
        );
        Ok(())
    }
}

fn normalize_rotation_storage_prefix(storage_prefix: &str) -> String {
    storage_prefix.trim_matches('/').to_string()
}

fn rotation_child_prefix(storage_prefix: &str, child: &str) -> String {
    let storage_prefix = normalize_rotation_storage_prefix(storage_prefix);
    if storage_prefix.is_empty() {
        format!("{child}/")
    } else {
        format!("{storage_prefix}/{child}/")
    }
}

fn immutable_rotation_error(
    storage_prefix: &str,
    operation: &str,
    evidence: &str,
    gc_immediate: bool,
) -> anyhow::Error {
    let root = if storage_prefix.is_empty() {
        "<bucket-root>"
    } else {
        storage_prefix
    };

    if gc_immediate {
        anyhow::anyhow!(
            "--gc-immediate is disabled for indexed/multi-writer root `{root}` ({evidence}). \
             Refusing {operation} before changing any manifest, chunk, index, key, or rotation \
             state. Readers can still hold old manifest and chunk addresses. Indexed rotation \
             requires an index-first copy-on-write protocol: write a new byte-addressed \
             manifest, commit/repoint the index, retain old objects through the reader grace \
             period, then garbage-collect."
        )
    } else {
        anyhow::anyhow!(
            "refusing {operation} for root `{root}`: {evidence}. The legacy rotation path \
             mutates `manifests/*` in place and scopes by `manifests/<scope>`, which is invalid \
             for immutable byte-addressed, path-indexed manifests. No manifest, chunk, index, \
             key, or rotation state was changed. Indexed rotation requires an index-first \
             copy-on-write protocol: write a new byte-addressed manifest, commit/repoint the \
             index, retain old objects through the reader grace period, then garbage-collect."
        )
    }
}

/// Prove that a root is safe for the legacy in-place rotation implementation.
///
/// A root is rejected when it has any live path-index object. We also reject a
/// flat manifest object whose key is the content-derived manifest object ID,
/// even if its index was lost or has not yet been published. Storage listing or
/// reads fail closed: inability to prove the legacy layout never grants the
/// mutation capability.
async fn legacy_manifest_mutation_permit(
    op: &opendal::Operator,
    storage_prefix: &str,
    operation: &str,
    gc_immediate: bool,
) -> Result<LegacyManifestMutationPermit> {
    let storage_prefix = normalize_rotation_storage_prefix(storage_prefix);
    let index_prefix = rotation_child_prefix(&storage_prefix, "index");
    let index_entries = op
        .list_with(&index_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("checking rotation index authority under {index_prefix}"))?;
    if index_entries
        .iter()
        .any(|entry| !entry.metadata().is_dir() && !entry.path().ends_with('/'))
    {
        return Err(immutable_rotation_error(
            &storage_prefix,
            operation,
            "live path-index entries select manifest object IDs",
            gc_immediate,
        ));
    }

    let manifest_prefix = rotation_child_prefix(&storage_prefix, "manifests");
    let manifest_entries = op
        .list_with(&manifest_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("checking rotation manifest layout under {manifest_prefix}"))?;
    for entry in manifest_entries {
        if entry.metadata().is_dir() || entry.path().ends_with('/') {
            continue;
        }
        let entry_path = entry.path().to_string();
        let Some(object_id) = entry_path.strip_prefix(&manifest_prefix) else {
            return Err(anyhow::anyhow!(
                "storage returned manifest object outside requested namespace: {}",
                entry_path
            ));
        };
        let object_id = object_id.to_string();
        // Byte-addressed manifests are flat. Nested names are the historical
        // path-shaped layout and remain eligible for the bounded legacy path.
        if object_id.contains('/') {
            continue;
        }
        let bytes = op
            .read(&entry_path)
            .await
            .with_context(|| format!("verifying legacy manifest identity: {entry_path}"))?
            .to_bytes();
        if tcfs_sync::index_entry::manifest_object_id(&bytes) == object_id {
            return Err(immutable_rotation_error(
                &storage_prefix,
                operation,
                "a manifest key is its immutable byte-derived object ID",
                gc_immediate,
            ));
        }
    }

    Ok(LegacyManifestMutationPermit { storage_prefix })
}

#[allow(clippy::too_many_arguments)]
async fn rotate_manifests_with_resume(
    op: &opendal::Operator,
    permit: &LegacyManifestMutationPermit,
    manifest_prefix: &str,
    old_master: &tcfs_crypto::MasterKey,
    new_master: &tcfs_crypto::MasterKey,
    state: &mut KeyRotationState,
    state_path: &Path,
    max_rotations: Option<u64>,
) -> Result<()> {
    // This check happens before even the local resume ledger is updated.
    permit.authorize_manifest_prefix(manifest_prefix)?;
    state.reset_scan_progress();
    write_rotation_state(state_path, state)?;

    let entries = op
        .list(manifest_prefix)
        .await
        .with_context(|| format!("listing manifests from storage: {manifest_prefix}"))?;

    for entry in entries {
        let path = entry.path().to_string();
        if entry.metadata().is_dir() {
            continue;
        }

        let data = match op.read(&path).await {
            Ok(d) => d.to_bytes(),
            Err(e) => {
                eprintln!("  WARN: failed to read {path}: {e}");
                state.error_count += 1;
                state.last_manifest_path = Some(path.clone());
                write_rotation_state(state_path, state)?;
                continue;
            }
        };

        let mut manifest: tcfs_sync::manifest::SyncManifest =
            match tcfs_sync::manifest::SyncManifest::from_bytes(&data) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("  WARN: failed to parse {path}: {e}");
                    state.error_count += 1;
                    state.last_manifest_path = Some(path.clone());
                    write_rotation_state(state_path, state)?;
                    continue;
                }
            };

        let wrapped_b64 = match &manifest.encrypted_file_key {
            Some(k) => k.clone(),
            None => {
                // TIN-1899: distinguish genuinely-keyless (plaintext) manifests
                // from per-device-only (v3) manifests. The MASTER rotate cannot
                // re-wrap per-device manifests (no master wrap to read) and must
                // NOT silently treat them as plaintext — that was the old
                // forward-secrecy gap. Count them separately and tell the
                // operator to run the scoped `tcfs key rotate <prefix>`.
                if manifest.wrapped_file_keys.is_empty() {
                    state.skipped_keyless_manifests += 1;
                } else {
                    state.skipped_per_device_manifests += 1;
                }
                state.last_manifest_path = Some(path.clone());
                write_rotation_state(state_path, state)?;
                continue;
            }
        };

        let wrapped_bytes = match base64::engine::general_purpose::STANDARD.decode(&wrapped_b64) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("  WARN: base64 decode failed for {path}: {e}");
                state.error_count += 1;
                state.last_manifest_path = Some(path.clone());
                write_rotation_state(state_path, state)?;
                continue;
            }
        };

        let needs_rotation = match tcfs_crypto::unwrap_key(old_master, &wrapped_bytes) {
            Ok(file_key) => Some(file_key),
            Err(old_err) => match tcfs_crypto::unwrap_key(new_master, &wrapped_bytes) {
                Ok(_) => {
                    state.already_rotated_manifests += 1;
                    state.last_manifest_path = Some(path.clone());
                    write_rotation_state(state_path, state)?;
                    None
                }
                Err(new_err) => {
                    eprintln!(
                        "  WARN: unwrap failed for {path}: old_key={old_err}; new_key={new_err}"
                    );
                    state.error_count += 1;
                    state.last_manifest_path = Some(path.clone());
                    write_rotation_state(state_path, state)?;
                    None
                }
            },
        };

        let Some(file_key) = needs_rotation else {
            continue;
        };

        let new_wrapped = tcfs_crypto::wrap_key(new_master, &file_key)?;
        let new_wrapped_b64 = base64::engine::general_purpose::STANDARD.encode(&new_wrapped);
        manifest.encrypted_file_key = Some(new_wrapped_b64);

        let new_data = serde_json::to_vec(&manifest).context("serializing rotated manifest")?;
        if let Err(e) = op.write(&path, new_data).await {
            eprintln!("  WARN: failed to write {path}: {e}");
            state.error_count += 1;
            state.last_manifest_path = Some(path.clone());
            write_rotation_state(state_path, state)?;
            continue;
        }

        state.rotated_manifests += 1;
        state.last_manifest_path = Some(path.clone());
        write_rotation_state(state_path, state)?;

        if let Some(limit) = max_rotations {
            if state.rotated_manifests >= limit {
                anyhow::bail!("simulated interruption after {limit} manifest rotations");
            }
        }
    }

    if state.error_count > 0 {
        anyhow::bail!(
            "key rotation incomplete: {} manifest errors remain; resume after fixing the failures",
            state.error_count
        );
    }

    state.status = KeyRotationStatus::ReadyToSwap;
    write_rotation_state(state_path, state)?;
    Ok(())
}

async fn cmd_rotate_key(
    config: &tcfs_core::config::TcfsConfig,
    old_key_file: Option<&Path>,
    use_password: bool,
    new_key_file: Option<&Path>,
    non_interactive: bool,
) -> Result<()> {
    let key_path = old_key_file
        .map(|p| p.to_path_buf())
        .or_else(|| config.crypto.master_key_file.clone())
        .unwrap_or_else(|| {
            tcfs_secrets::device::default_registry_path()
                .parent()
                .unwrap_or(Path::new("."))
                .join("master.key")
        });
    let key_path = expand_tilde(&key_path);

    // This must precede credential discovery and, critically, every call that
    // can create `.rotate-pending`, `.rotate-state.json`, or atomic temp
    // siblings next to the selected key.
    validate_rotation_master_key_path(config, &key_path)?;

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

    // Freeze immutable/indexed roots before prepare_key_rotation writes a
    // pending key or resume ledger. The legacy implementation below rewrites
    // manifest bytes at the same key and therefore needs this capability.
    let legacy_permit = legacy_manifest_mutation_permit(
        &op,
        config.storage.resolved_prefix(),
        "master-key rotation",
        false,
    )
    .await?;

    let manifest_prefix = rotation_child_prefix(config.storage.resolved_prefix(), "manifests");
    let Some(mut rotation) = prepare_key_rotation(
        &key_path,
        &manifest_prefix,
        use_password,
        non_interactive,
        new_key_file,
    )?
    else {
        println!(
            "Key rotation was already finalized; cleaned stale resume state for {}",
            key_path.display()
        );
        return Ok(());
    };

    if rotation.resumed {
        println!(
            "Resuming key rotation using pending key: {}",
            rotation.paths.pending_key_path.display()
        );
    } else {
        println!("Old master key loaded from: {}", key_path.display());
        println!(
            "Prepared pending new master key at: {}",
            rotation.paths.pending_key_path.display()
        );
    }

    println!("Scanning manifests at: {manifest_prefix}");
    if let Err(e) = rotate_manifests_with_resume(
        &op,
        &legacy_permit,
        &manifest_prefix,
        &rotation.old_master,
        &rotation.new_master,
        &mut rotation.state,
        &rotation.paths.state_path,
        None,
    )
    .await
    {
        println!(
            "\nKey rotation paused with resumable state preserved:\n  Resume state: {}\n  Pending key:  {}",
            rotation.paths.state_path.display(),
            rotation.paths.pending_key_path.display()
        );
        return Err(e);
    }

    finalize_key_rotation(&key_path, &rotation.new_master, &rotation.paths)?;

    println!("\nKey rotation complete:");
    println!("  Manifests rotated: {}", rotation.state.rotated_manifests);
    println!(
        "  Already rotated on resume: {}",
        rotation.state.already_rotated_manifests
    );
    println!(
        "  Manifests skipped (keyless/plaintext): {}",
        rotation.state.skipped_keyless_manifests
    );
    println!(
        "  Manifests skipped (per-device only): {}",
        rotation.state.skipped_per_device_manifests
    );
    println!("  New master key: {}", key_path.display());

    if rotation.state.skipped_per_device_manifests > 0 {
        eprintln!();
        eprintln!(
            "  WARNING: {} per-device-only (v3) manifest(s) were NOT re-keyed by this MASTER \
             rotation.\n  The master rotate cannot re-wrap content that carries no master wrap. \
             To rotate FileKeys for per-device content (forward secrecy on device revoke), run:\n    \
             tcfs key rotate <prefix> --rotate-keys",
            rotation.state.skipped_per_device_manifests
        );
    }

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

// ── `tcfs key rotate <prefix>` (TIN-1899 / B2 forward secrecy) ─────────────
//
// Scoped, per-device-aware FileKey rotation. SEPARATE from the master
// `rotate-key` above (which re-wraps master-wrapped manifests under a new master
// key). This command:
//   1. Decrypts each manifest's current FileKey via the existing read path
//      (master OR per-device, per the manifest's wrap shape).
//   2. Generates a FRESH random FileKey; re-encrypts every chunk under it and
//      uploads the new ciphertext under its NEW BLAKE3 content address.
//   3. Re-wraps the new FileKey to the CURRENT (post-revocation) recipient set
//      resolved from the VERIFIED device registry, honoring `crypto.wrap_mode`.
//      A device absent from that set gets NO wrap -> cannot decrypt the re-key.
//   4. Publishes the new manifest, then submits orphaned old chunks to
//      generation-pinned GC once no live manifest references them. Backends
//      without usable object versions retain the orphans rather than risk loss.
//
// Resumable via `.rotate-state.json`: a kill mid-run resumes, skipping manifests
// already published.

/// Resumable state for the scoped per-device key rotation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ScopedRotationState {
    version: u32,
    started_at: u64,
    /// Full manifest prefix scanned (resolved storage prefix + scope).
    manifest_prefix: String,
    /// Manifest object keys that have been fully re-keyed AND published.
    /// Re-reading these on resume is a no-op (idempotent skip).
    done_manifests: Vec<String>,
    /// Count of manifests re-keyed this run (cumulative across resumes).
    rotated_manifests: u64,
    /// Manifests skipped because they carry NO FileKey at all (plaintext).
    skipped_keyless_manifests: u64,
    /// Manifests skipped on resume because they were already published.
    already_done_manifests: u64,
    /// Total plaintext bytes re-encrypted (for reporting).
    bytes_rewritten: u64,
    /// Set once every in-scope manifest has been published; gates GC.
    all_published: bool,
}

impl ScopedRotationState {
    fn new(manifest_prefix: &str) -> Self {
        Self {
            version: 1,
            started_at: now_epoch(),
            manifest_prefix: manifest_prefix.to_string(),
            done_manifests: Vec::new(),
            rotated_manifests: 0,
            skipped_keyless_manifests: 0,
            already_done_manifests: 0,
            bytes_rewritten: 0,
            all_published: false,
        }
    }

    fn is_done(&self, path: &str) -> bool {
        self.done_manifests.iter().any(|p| p == path)
    }

    fn mark_done(&mut self, path: &str) {
        if !self.is_done(path) {
            self.done_manifests.push(path.to_string());
        }
    }
}

/// Resolve the scoped rotation's state-file path (adjacent to the sync state).
fn scoped_rotation_state_path(config: &tcfs_core::config::TcfsConfig, scope: &str) -> PathBuf {
    let base = resolve_state_path(config, None);
    let parent = base.parent().unwrap_or(Path::new(".")).to_path_buf();
    // Encode the scope so distinct prefixes get distinct resume files.
    let tag: String = scope
        .trim_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    parent.join(format!(".key-rotate-{tag}.rotate-state.json"))
}

fn write_scoped_rotation_state(path: &Path, state: &ScopedRotationState) -> Result<()> {
    let data = serde_json::to_vec_pretty(state).context("serializing scoped rotation state")?;
    atomic_write_bytes(path, &data, Some(0o600))
}

fn read_scoped_rotation_state(path: &Path) -> Result<ScopedRotationState> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading scoped rotation state: {}", path.display()))?;
    serde_json::from_slice(&data).context("parsing scoped rotation state")
}

/// Load the master key (required to verify the signed registry and to read/write
/// master-wrapped manifests). Mirrors how the other CLI crypto paths resolve it.
fn load_master_key_for_rotation(
    config: &tcfs_core::config::TcfsConfig,
) -> Result<tcfs_crypto::MasterKey> {
    let key_path = config.crypto.master_key_file.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no master key configured (crypto.master_key_file); key rotation requires it to \
             verify the signed device registry and read existing wraps"
        )
    })?;
    read_master_key(&key_path)
        .with_context(|| format!("reading master key: {}", key_path.display()))
}

/// Build the current (post-revocation) recipient set + this device's unwrap
/// identity, honoring `crypto.wrap_mode`. Returns the `EncryptionContext` used
/// for BOTH the read (unwrap) and write (re-wrap) sides of the rotation.
fn rotation_encryption_context(
    config: &tcfs_core::config::TcfsConfig,
    master_key: &tcfs_crypto::MasterKey,
) -> tcfs_sync::engine::EncryptionContext {
    let device_id = load_device_id(config);
    // Reuse the canonical builder: it loads the VERIFIED registry (post-B4
    // load_verified), filters to active+real recipients, and applies the
    // roll-call gate. A revoked device is dropped from active_devices() and thus
    // from the recipient set -- exactly the forward-secrecy requirement.
    build_encryption_context(config, &device_id, master_key)
}

/// Unwrap the FileKey carried by a manifest using the rotation context.
///
/// Mirrors the engine read path: prefer the per-device wrap (using this device's
/// age identity), fall back to the master wrap when one is present (Dual/v2).
/// Returns `Ok(None)` for a genuinely keyless (plaintext) manifest.
fn unwrap_manifest_file_key(
    manifest: &tcfs_sync::manifest::SyncManifest,
    ctx: &tcfs_sync::engine::EncryptionContext,
    manifest_path: &str,
) -> Result<Option<tcfs_crypto::FileKey>> {
    if !manifest.wrapped_file_keys.is_empty() {
        let per_device: Result<tcfs_crypto::FileKey> = (|| {
            let identity = ctx.device_identity.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "manifest {manifest_path} is per-device encrypted but this device has no age \
                     identity to unwrap it (configure wrap_mode + device secret)"
                )
            })?;
            let age_wraps: Vec<tcfs_crypto::AgeWrappedFileKey> = manifest
                .wrapped_file_keys
                .iter()
                .map(|w| tcfs_crypto::AgeWrappedFileKey {
                    recipient_device_id: w.recipient_device_id.clone(),
                    recipient: w.recipient.clone(),
                    algorithm: w.algorithm.clone(),
                    wrapped_key: w.wrapped_key.clone(),
                })
                .collect();
            tcfs_crypto::unwrap_file_key_with_age_identity(
                &age_wraps,
                &identity.secret,
                Some(&identity.device_id),
            )
            .with_context(|| format!("unwrapping per-device file key for {manifest_path}"))
        })();

        match per_device {
            Ok(fk) => return Ok(Some(fk)),
            Err(per_device_err) => {
                if let Some(ref wrapped_b64) = manifest.encrypted_file_key {
                    let wrapped = base64::engine::general_purpose::STANDARD
                        .decode(wrapped_b64)
                        .context("decoding master-wrapped file key")?;
                    return Ok(Some(
                        tcfs_crypto::unwrap_key(&ctx.master_key, &wrapped).with_context(|| {
                            format!("unwrapping master file key for {manifest_path}")
                        })?,
                    ));
                }
                return Err(per_device_err);
            }
        }
    }

    if let Some(ref wrapped_b64) = manifest.encrypted_file_key {
        let wrapped = base64::engine::general_purpose::STANDARD
            .decode(wrapped_b64)
            .context("decoding master-wrapped file key")?;
        return Ok(Some(
            tcfs_crypto::unwrap_key(&ctx.master_key, &wrapped)
                .with_context(|| format!("unwrapping master file key for {manifest_path}"))?,
        ));
    }

    Ok(None)
}

/// Build the master wrap + per-device wraps for a fresh FileKey, honoring the
/// context's `wrap_mode`. Returns `(encrypted_file_key, wrapped_file_keys,
/// manifest_version)` mirroring the engine write path exactly.
fn wrap_rotated_file_key(
    ctx: &tcfs_sync::engine::EncryptionContext,
    file_key: &tcfs_crypto::FileKey,
) -> Result<(
    Option<String>,
    Vec<tcfs_sync::manifest::WrappedFileKey>,
    u32,
)> {
    use tcfs_sync::engine::WrapMode;

    let master_wrap = || -> Result<String> {
        let wrapped = tcfs_crypto::wrap_key(&ctx.master_key, file_key)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(&wrapped))
    };
    let device_wraps = || -> Result<Vec<tcfs_sync::manifest::WrappedFileKey>> {
        let wraps =
            tcfs_crypto::wrap_file_key_for_age_recipients(file_key, &ctx.device_recipients)?;
        Ok(wraps
            .into_iter()
            .map(|w| tcfs_sync::manifest::WrappedFileKey {
                recipient_device_id: w.recipient_device_id,
                recipient: w.recipient,
                algorithm: w.algorithm,
                wrapped_key: w.wrapped_key,
            })
            .collect())
    };

    match ctx.wrap_mode {
        WrapMode::Master => Ok((Some(master_wrap()?), Vec::new(), 2)),
        WrapMode::Dual => {
            if ctx.device_recipients.is_empty() {
                anyhow::bail!(
                    "wrap_mode=Dual requires per-device recipients but none are configured"
                );
            }
            Ok((Some(master_wrap()?), device_wraps()?, 2))
        }
        WrapMode::PerDevice => {
            if ctx.device_recipients.is_empty() {
                anyhow::bail!(
                    "wrap_mode=PerDevice requires per-device recipients but none are configured"
                );
            }
            // v3: per-device-only, NO master wrap -> a revoked device absent from
            // the recipient set has no path to the FileKey.
            Ok((None, device_wraps()?, 3))
        }
    }
}

enum RekeyOutcome {
    Rotated { bytes_rewritten: u64 },
    Keyless,
}

/// Re-key ONE manifest: decrypt the old FileKey, generate a fresh one,
/// re-encrypt every chunk under it (new BLAKE3 addresses), re-wrap to the
/// current recipient set, and publish the new manifest. The OLD chunks are
/// intentionally left in place -- they are swept by the post-publish GC once no
/// live manifest references them.
async fn rekey_one_manifest(
    op: &opendal::Operator,
    permit: &LegacyManifestMutationPermit,
    remote_prefix: &str,
    manifest_path: &str,
    ctx: &tcfs_sync::engine::EncryptionContext,
) -> Result<RekeyOutcome> {
    // Hold the layout capability before writing either replacement chunks or
    // the legacy in-place manifest object.
    permit.authorize_remote_prefix(remote_prefix)?;
    let data = op
        .read(manifest_path)
        .await
        .map_err(|e| anyhow::anyhow!("reading manifest {manifest_path}: {e}"))?
        .to_bytes();
    let mut manifest = tcfs_sync::manifest::SyncManifest::from_bytes(&data)
        .with_context(|| format!("parsing manifest {manifest_path}"))?;

    let Some(old_file_key) = unwrap_manifest_file_key(&manifest, ctx, manifest_path)? else {
        return Ok(RekeyOutcome::Keyless);
    };

    // file_id (AAD) is BLAKE3 of the plaintext file_hash. Plaintext is unchanged
    // by re-keying, so file_id and file_hash stay identical -- only the FileKey
    // (and therefore the ciphertext + its content address) changes.
    let file_id: [u8; 32] = {
        let hash = tcfs_chunks::hash_from_hex(&manifest.file_hash)
            .with_context(|| format!("parsing file_hash for {manifest_path}"))?;
        *hash.as_bytes()
    };

    let new_file_key = tcfs_crypto::generate_file_key();
    let mut new_chunk_hashes: Vec<String> = Vec::with_capacity(manifest.chunks.len());
    let mut bytes_rewritten: u64 = 0;

    for (i, old_hash) in manifest.chunks.iter().enumerate() {
        let old_chunk_key = format!("{remote_prefix}/chunks/{old_hash}");
        let ciphertext = op
            .read(&old_chunk_key)
            .await
            .map_err(|e| anyhow::anyhow!("reading chunk {old_chunk_key}: {e}"))?
            .to_vec();
        let plaintext = tcfs_crypto::decrypt_chunk(&old_file_key, i as u64, &file_id, &ciphertext)
            .with_context(|| format!("decrypting chunk {i} of {manifest_path}"))?;
        bytes_rewritten += plaintext.len() as u64;

        let new_ciphertext =
            tcfs_crypto::encrypt_chunk(&new_file_key, i as u64, &file_id, &plaintext)
                .with_context(|| format!("re-encrypting chunk {i} of {manifest_path}"))?;
        let new_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&new_ciphertext));
        let new_chunk_key = format!("{remote_prefix}/chunks/{new_hash}");

        // Content-addressed: if the new ciphertext already exists (idempotent
        // resume / dedupe), the write is a harmless overwrite of identical bytes.
        op.write(&new_chunk_key, new_ciphertext)
            .await
            .map_err(|e| anyhow::anyhow!("writing re-encrypted chunk {new_chunk_key}: {e}"))?;
        new_chunk_hashes.push(new_hash);
    }

    // Swap in the new content addresses + new wraps, then publish.
    manifest.chunks = new_chunk_hashes;
    let (encrypted_file_key, wrapped_file_keys, version) =
        wrap_rotated_file_key(ctx, &new_file_key)?;
    manifest.encrypted_file_key = encrypted_file_key;
    manifest.wrapped_file_keys = wrapped_file_keys;
    manifest.version = version;

    let new_data = manifest
        .to_bytes()
        .with_context(|| format!("serializing rotated manifest {manifest_path}"))?;
    op.write(manifest_path, new_data)
        .await
        .map_err(|e| anyhow::anyhow!("publishing rotated manifest {manifest_path}: {e}"))?;

    Ok(RekeyOutcome::Rotated { bytes_rewritten })
}

/// List the in-scope manifest object keys (non-directory) under the prefix.
async fn list_scoped_manifests(
    op: &opendal::Operator,
    manifest_prefix: &str,
) -> Result<Vec<String>> {
    let entries = op
        .list_with(manifest_prefix)
        .recursive(true)
        .await
        .with_context(|| format!("listing manifests under {manifest_prefix}"))?;
    let mut paths: Vec<String> = entries
        .into_iter()
        .filter(|e| !e.metadata().is_dir() && !e.path().ends_with('/'))
        .map(|e| e.path().to_string())
        .collect();
    paths.sort();
    Ok(paths)
}

/// Whether the closing message after a rotation may truthfully claim per-device
/// forward secrecy.
///
/// Forward secrecy from re-keying only holds when the re-wrapped content carries
/// NO path back to a shared, unchanged secret. That is exactly
/// [`WrapMode::PerDevice`] with a non-empty recipient set (manifest v3, no master
/// wrap). Under the DEFAULT [`WrapMode::Master`] (and [`WrapMode::Dual`]) the
/// re-keyed FileKey is re-wrapped to the UNCHANGED shared master key, so a
/// revoked master-key holder STILL decrypts the re-keyed content — claiming
/// forward secrecy there would be false and dangerous.
fn rotation_grants_forward_secrecy(ctx: &tcfs_sync::engine::EncryptionContext) -> bool {
    use tcfs_sync::engine::WrapMode;
    ctx.wrap_mode == WrapMode::PerDevice && !ctx.device_recipients.is_empty()
}

/// The closing forward-secrecy summary for a completed rotation, rendered as
/// lines to print. Gated on [`rotation_grants_forward_secrecy`]: only the
/// per-device path earns the reassurance; Master/Dual gets a LOUD warning that
/// no per-device forward secrecy was gained.
fn forward_secrecy_summary_lines(ctx: &tcfs_sync::engine::EncryptionContext) -> Vec<String> {
    if rotation_grants_forward_secrecy(ctx) {
        vec![
            "Forward secrecy: devices absent from the current recipient set can no longer \
             decrypt the re-keyed content (per-device wrap, no master wrap). (They may still \
             hold content they previously pulled.)"
                .to_string(),
        ]
    } else {
        vec![
            "WARNING: NO per-device forward secrecy was gained by this rotation.".to_string(),
            format!(
                "  wrap_mode={:?}: the re-keyed content was re-wrapped to the UNCHANGED shared \
                 master key.",
                ctx.wrap_mode
            ),
            "  A revoked device that still holds the master key can STILL decrypt the re-keyed \
             content."
                .to_string(),
            "  Per-device forward secrecy requires wrap_mode=PerDevice (per-device-only wraps, \
             no master wrap) with a real recipient set."
                .to_string(),
        ]
    }
}

async fn cmd_key_rotate(
    config: &tcfs_core::config::TcfsConfig,
    scope: &str,
    rotate_keys: bool,
    resume: bool,
    non_interactive: bool,
    gc_immediate: bool,
) -> Result<()> {
    let storage_prefix = normalize_rotation_storage_prefix(config.storage.resolved_prefix());
    let scope_clean = scope.trim_matches('/');
    let root_manifest_prefix = rotation_child_prefix(&storage_prefix, "manifests");
    let manifest_prefix = if scope_clean.is_empty() {
        root_manifest_prefix
    } else {
        format!("{root_manifest_prefix}{scope_clean}/")
    };
    let remote_prefix = storage_prefix.clone();

    let master_key = load_master_key_for_rotation(config)?;
    let ctx = rotation_encryption_context(config, &master_key);

    let op = build_operator(config).await?;

    // This must precede `list_scoped_manifests`: path scope lives in index
    // keys on modern roots, not beneath the flat byte-addressed manifest
    // namespace. It also precedes creation or mutation of the resume ledger.
    let legacy_permit = legacy_manifest_mutation_permit(
        &op,
        &storage_prefix,
        "scoped FileKey rotation",
        gc_immediate,
    )
    .await?;

    println!("Scanning manifests under: {manifest_prefix}");
    let manifests = list_scoped_manifests(&op, &manifest_prefix).await?;
    if manifests.is_empty() {
        println!("No manifests found under that prefix. Nothing to rotate.");
        return Ok(());
    }

    // Project bytes-to-rewrite by summing manifest file sizes (cheap; reads
    // manifests only, not chunks).
    let mut projected_bytes: u64 = 0;
    let mut encrypted_count: u64 = 0;
    for path in &manifests {
        if let Ok(data) = op.read(path).await {
            if let Ok(m) = tcfs_sync::manifest::SyncManifest::from_bytes(&data.to_bytes()) {
                let has_key = m.encrypted_file_key.is_some() || !m.wrapped_file_keys.is_empty();
                if has_key {
                    projected_bytes += m.file_size;
                    encrypted_count += 1;
                }
            }
        }
    }

    println!(
        "  Manifests in scope: {} ({} encrypted)",
        manifests.len(),
        encrypted_count
    );
    println!(
        "  Recipient set (post-revocation): {} device(s), wrap_mode={:?}",
        ctx.device_recipients.len(),
        ctx.wrap_mode
    );
    for r in &ctx.device_recipients {
        println!("    - {} ({})", r.device_id, r.recipient);
    }
    println!(
        "  Projected bytes to re-encrypt: {} ({:.2} MiB)",
        projected_bytes,
        projected_bytes as f64 / (1024.0 * 1024.0)
    );

    if !rotate_keys {
        println!();
        println!(
            "Dry run -- no changes made. Re-run with --rotate-keys to perform the rotation. \
             Legacy content is re-encrypted under fresh FileKeys without re-chunking \
             (file_hash/file_id stay valid), re-wrapped to the current recipient set, and the \
             orphaned old chunks become eligible for generation-pinned GC. Unversioned backends \
             retain them. Indexed roots require index-first copy-on-write rotation and are \
             rejected before this scan."
        );
        return Ok(());
    }

    // Confirmation prompt before the expensive rewrite.
    if !non_interactive {
        println!();
        println!(
            "This will re-encrypt {projected_bytes} bytes across {encrypted_count} file(s), \
             upload new chunks, publish new manifests, and submit orphaned old chunks to \
             generation-pinned GC."
        );
        let confirm =
            rpassword::prompt_password("Type 'ROTATE' to confirm scoped FileKey rotation: ")
                .context("reading confirmation")?;
        if confirm != "ROTATE" {
            anyhow::bail!("key rotation cancelled");
        }
    }

    let state_path = scoped_rotation_state_path(config, scope);
    let mut state = if resume && state_path.exists() {
        let existing = read_scoped_rotation_state(&state_path)?;
        if existing.manifest_prefix != manifest_prefix {
            anyhow::bail!(
                "resume state targets {} but this invocation resolved to {}",
                existing.manifest_prefix,
                manifest_prefix
            );
        }
        println!(
            "Resuming scoped rotation: {} manifest(s) already published.",
            existing.done_manifests.len()
        );
        existing
    } else {
        if state_path.exists() && !resume {
            anyhow::bail!(
                "a scoped rotation is already in progress for this prefix ({}). Pass --resume to \
                 continue it, or remove the state file to start over.",
                state_path.display()
            );
        }
        let s = ScopedRotationState::new(&manifest_prefix);
        write_scoped_rotation_state(&state_path, &s)?;
        s
    };

    println!();
    println!("Re-keying manifests...");
    for path in &manifests {
        if state.is_done(path) {
            state.already_done_manifests += 1;
            continue;
        }
        match rekey_one_manifest(&op, &legacy_permit, &remote_prefix, path, &ctx).await {
            Ok(RekeyOutcome::Rotated { bytes_rewritten }) => {
                state.rotated_manifests += 1;
                state.bytes_rewritten += bytes_rewritten;
                state.mark_done(path);
                write_scoped_rotation_state(&state_path, &state)?;
                println!("  re-keyed: {path}");
            }
            Ok(RekeyOutcome::Keyless) => {
                state.skipped_keyless_manifests += 1;
                state.mark_done(path);
                write_scoped_rotation_state(&state_path, &state)?;
                println!("  skipped (keyless/plaintext): {path}");
            }
            Err(e) => {
                // Persist progress and surface a resumable error. Already-published
                // manifests are in done_manifests; old chunks are NOT yet GC'd, so
                // nothing referenced by a live manifest can be lost.
                write_scoped_rotation_state(&state_path, &state)?;
                println!(
                    "\nScoped rotation paused with resumable state preserved:\n  Resume state: {}\n  \
                     Re-run with --resume after fixing the failure.",
                    state_path.display()
                );
                return Err(e).with_context(|| format!("re-keying manifest {path}"));
            }
        }
    }

    // All in-scope manifests are now published with NEW chunk addresses. The old
    // chunks are orphaned (no live manifest references them) and safe to sweep.
    state.all_published = true;
    write_scoped_rotation_state(&state_path, &state)?;

    // GC grace: default to the configured orphan_chunk_cleanup_grace_secs so a
    // concurrent reader in a multi-writer fleet that still holds an old chunk
    // address won't 404 mid-flight. `--gc-immediate` opts into grace=0 (the old
    // hardcoded behavior). Either way only chunks unreferenced by ANY live
    // manifest are eligible, and physical deletion still requires an exact
    // object version so an unversioned backend remains fail-closed.
    let gc_grace = if gc_immediate {
        Duration::from_secs(0)
    } else {
        Duration::from_secs(config.sync.orphan_chunk_cleanup_grace_secs)
    };
    println!();
    if gc_immediate {
        println!(
            "GC: sweeping chunks no longer referenced by ANY live manifest under {remote_prefix} \
             (grace=0, immediately eligible; exact-version delete still required) ..."
        );
    } else {
        println!(
            "GC: sweeping chunks no longer referenced by ANY live manifest under {remote_prefix} \
             (grace={}s; older orphans only — on a proven single-writer legacy root, pass \
             --gc-immediate for grace=0) ...",
            config.sync.orphan_chunk_cleanup_grace_secs
        );
    }
    let cleanup = tcfs_sync::reconcile::cleanup_legacy_orphaned_chunks(
        &op,
        &remote_prefix,
        gc_grace,
        SystemTime::now(),
    )
    .await
    .context("GC of orphaned chunks after rotation")?;

    let deferred_orphans = cleanup.skipped_within_grace.len()
        + cleanup.skipped_missing_last_modified.len()
        + cleanup.skipped_without_atomic_delete.len();
    println!(
        "  GC: {} orphaned, {} deleted, {} deferred, {} referenced (kept), {} errors",
        cleanup.orphaned_chunks_found,
        cleanup.deleted_chunks.len(),
        deferred_orphans,
        cleanup.referenced_chunks,
        cleanup.delete_errors.len()
    );
    if !cleanup.skipped_within_grace.is_empty() && !gc_immediate {
        println!(
            "       {} orphaned chunk(s) are within the grace window and will be \
             swept by a later GC (or, on a proven single-writer legacy root, with \
             --gc-immediate).",
            cleanup.skipped_within_grace.len()
        );
    }
    if !cleanup.skipped_missing_last_modified.is_empty() {
        println!(
            "       {} orphaned chunk(s) lack last-modified metadata and were retained.",
            cleanup.skipped_missing_last_modified.len()
        );
    }
    if !cleanup.skipped_without_atomic_delete.is_empty() {
        println!(
            "       {} orphaned chunk(s) lack generation-pinned delete support and were retained \
             to avoid racing a concurrent publisher.",
            cleanup.skipped_without_atomic_delete.len()
        );
    }
    for (hash, err) in &cleanup.delete_errors {
        eprintln!("    GC error: {hash}: {err}");
    }

    // Rotation complete: drop the resume artifact.
    if let Err(e) = std::fs::remove_file(&state_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "  WARN: failed to remove resume state {}: {e}",
                state_path.display()
            );
        }
    }

    println!();
    println!("Scoped FileKey rotation complete:");
    println!("  Manifests re-keyed:      {}", state.rotated_manifests);
    println!(
        "  Skipped (keyless):       {}",
        state.skipped_keyless_manifests
    );
    println!(
        "  Already done (resume):   {}",
        state.already_done_manifests
    );
    println!("  Bytes re-encrypted:      {}", state.bytes_rewritten);
    println!(
        "  Old chunks GC'd:         {}",
        cleanup.deleted_chunks.len()
    );
    println!();
    for line in forward_secrecy_summary_lines(&ctx) {
        println!("{line}");
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

async fn cmd_policy(_config: &tcfs_core::config::TcfsConfig, action: PolicyAction) -> Result<()> {
    let policy_path = policy_store_path();
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
    let local_root = path
        .map(|p| p.to_path_buf())
        .or_else(|| config.sync.sync_root.clone())
        .ok_or_else(|| anyhow::anyhow!("no path specified and no sync_root in config"))?;
    // Reconcile scans before it executes, so even a dry-run must not admit a
    // directory containing the configured key into a generated plan.
    validate_sync_selection_excludes_master_key(config, &local_root)?;

    let state_path = resolve_state_path(config, state_override);
    let _state_lock = execute
        .then(|| lock_explicit_state_for_mutation(&state_path, state_override))
        .transpose()?
        .flatten();
    let op = build_operator(config).await?;
    let device_id = load_device_id(config);

    let remote_prefix = prefix.map(|s| s.to_string()).unwrap_or_else(|| {
        config
            .storage
            .remote_prefix
            .clone()
            .unwrap_or_else(|| config.storage.bucket.clone())
    });

    let state = tcfs_sync::state::StateCache::open(&state_path)
        .with_context(|| format!("opening state cache: {}", state_path.display()))?;

    let blacklist = tcfs_sync::blacklist::Blacklist::from_sync_config(&config.sync);
    // Enable `.git`-aware fast-forward conflict resolution for raw git-dir sync.
    let reconcile_config = tcfs_sync::reconcile::ReconcileConfig {
        git_sync_mode: blacklist.git_sync_mode().to_string(),
        git_ff_resolution: blacklist.allows_git_dirs() && blacklist.git_sync_mode() == "raw",
        ..Default::default()
    };
    let orphan_chunk_cleanup_grace =
        Duration::from_secs(config.sync.orphan_chunk_cleanup_grace_secs);

    // Build the encryption context (if a master key is configured) before the
    // reconcile pass: the `.git` fast-forward check reads remote ref blobs, which
    // are encrypted when a master key is set.
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
        .map(|mk| build_encryption_context(config, &device_id, mk));

    println!(
        "Reconciling {} ↔ {}:{}/",
        local_root.display(),
        sanitize_http_endpoint_for_display(&config.storage.endpoint),
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
        enc_ctx.as_ref(),
    )
    .await
    .context("reconciliation failed")?;

    // Display plan
    println!();
    println!(
        "Plan: {} push, {} pull, {} create-dir, {} delete-local, {} delete-remote, {} conflict, {} up-to-date",
        plan.summary.pushes,
        plan.summary.pulls,
        plan.summary.directories,
        plan.summary.local_deletes,
        plan.summary.remote_deletes,
        plan.summary.conflicts,
        plan.summary.up_to_date
    );

    if plan.actions.is_empty() {
        println!("Nothing to do — local and remote are in sync.");
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
            tcfs_sync::reconcile::ReconcileAction::CreateDirectory { rel_path } => {
                println!("  + create-dir  {rel_path}")
            }
            tcfs_sync::reconcile::ReconcileAction::UpToDate { rel_path } => {
                println!("  = up-to-date  {rel_path}")
            }
        }
    }

    if !execute {
        println!();
        println!("Dry run — no changes made. Use --execute to apply.");
        if !orphan_chunk_cleanup_grace.is_zero() {
            if plan_may_orphan_remote_chunks(&plan) {
                println!(
                    "Orphan chunk cleanup runs during execute with a {} second grace period.",
                    config.sync.orphan_chunk_cleanup_grace_secs
                );
            } else {
                println!(
                    "Orphan chunk cleanup will be skipped during execute; this plan does not overwrite or delete remote data."
                );
            }
        }
        return Ok(());
    }

    if !plan.actions.is_empty() {
        println!();
        println!("Executing plan...");

        let mut state = tcfs_sync::state::StateCache::open(&state_path)?;

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
            "Done: {} pushed, {} pulled, {} dirs-created, {} deleted, {} conflicts, {} errors",
            result.pushed,
            result.pulled,
            result.directories_created,
            result.deleted_local + result.deleted_remote,
            result.conflicts_recorded,
            result.errors.len()
        );

        for (path, err) in &result.errors {
            eprintln!("  error: {path}: {err}");
        }

        if !result.deferred_git_refs.is_empty() {
            println!(
                "  {} git ref action(s) deferred (objects-before-refs barrier); \
                 they will re-plan next cycle:",
                result.deferred_git_refs.len()
            );
            for path in &result.deferred_git_refs {
                println!("    deferred: {path}");
            }
        }
    }

    if !orphan_chunk_cleanup_grace.is_zero() && plan_may_orphan_remote_chunks(&plan) {
        println!();
        println!(
            "Sweeping orphaned remote chunks older than {} seconds...",
            config.sync.orphan_chunk_cleanup_grace_secs
        );

        let cleanup = tcfs_sync::reconcile::cleanup_orphaned_chunks(
            &op,
            &remote_prefix,
            orphan_chunk_cleanup_grace,
            SystemTime::now(),
        )
        .await
        .context("cleaning orphaned remote chunks")?;

        println!(
            "Orphan cleanup: {} found, {} deleted, {} within grace, {} missing timestamps, {} without atomic delete, {} errors",
            cleanup.orphaned_chunks_found,
            cleanup.deleted_chunks.len(),
            cleanup.skipped_within_grace.len(),
            cleanup.skipped_missing_last_modified.len(),
            cleanup.skipped_without_atomic_delete.len(),
            cleanup.delete_errors.len()
        );

        for (chunk, err) in &cleanup.delete_errors {
            eprintln!("  orphan cleanup error: {chunk}: {err}");
        }
    } else if execute && !orphan_chunk_cleanup_grace.is_zero() {
        println!();
        println!(
            "Skipping orphan chunk cleanup; this plan did not overwrite or delete remote data."
        );
    }

    Ok(())
}

fn plan_may_orphan_remote_chunks(plan: &tcfs_sync::reconcile::ReconcilePlan) -> bool {
    plan.actions.iter().any(|action| {
        matches!(
            action,
            tcfs_sync::reconcile::ReconcileAction::Push {
                reason: tcfs_sync::reconcile::PushReason::LocalNewer
                    | tcfs_sync::reconcile::PushReason::GitFastForward { .. },
                ..
            } | tcfs_sync::reconcile::ReconcileAction::DeleteRemote { .. }
        )
    })
}

// ── `tcfs resolve` ───────────────────────────────────────────────────────────

#[cfg(unix)]
async fn cmd_resolve(
    config: &tcfs_core::config::TcfsConfig,
    path: &Path,
    root_id: Option<&str>,
    strategy: Option<&str>,
    execute: bool,
) -> Result<()> {
    let is_git_repo = path.join(".git").is_dir();
    if root_id.is_some() && !is_git_repo {
        anyhow::bail!(
            "--root currently supports a registered git repository root only: {}",
            path.display()
        );
    }
    let requested = strategy.map(|s| s.replace('-', "_"));

    let resolution = match (is_git_repo, requested.as_deref()) {
        (true, None) | (true, Some("keep_both")) => {
            if execute {
                "git_keep_both_execute".to_string()
            } else {
                "git_keep_both_dry_run".to_string()
            }
        }
        (true, Some(other)) => {
            anyhow::bail!(
                "repo-group conflict resolution for {} supports keep-both only, got {other}",
                path.display()
            );
        }
        (false, Some(_)) if execute => {
            anyhow::bail!("--execute is only valid for repo-group git keep-both resolution");
        }
        (false, Some("defer")) => "defer".to_string(),
        (false, Some(other)) => {
            anyhow::bail!(
                "ordinary-file resolution strategy '{other}' is retired; inspect with `tcfs conflicts` or use --strategy defer"
            );
        }
        (false, None) => {
            anyhow::bail!(
                "ordinary-file mutation is disabled until it is root- and manifest-bound; inspect with `tcfs conflicts` or pass --strategy defer"
            );
        }
    };

    // Named roots use a dedicated RPC. An older daemon returns Unimplemented;
    // it cannot ignore a new request field and accidentally run the recognized
    // git resolution against its primary cache. Dry-run is authenticated read
    // access (pull); execute performs the same reads and also requires push.
    let mut client = connect_daemon(&config.daemon.socket).await?;
    if let Some(root_id) = root_id {
        let requested = std::fs::canonicalize(path)
            .with_context(|| format!("canonicalizing repo root: {}", path.display()))?;
        let mode = if execute {
            tcfs_core::proto::RegisteredRootResolveMode::GitKeepBothExecute
        } else {
            tcfs_core::proto::RegisteredRootResolveMode::GitKeepBothDryRun
        };
        let response = client
            .resolve_registered_root(tonic::Request::new(
                tcfs_core::proto::ResolveRegisteredRootRequest {
                    root_id: root_id.to_string(),
                    path: requested.display().to_string(),
                    mode: mode.into(),
                    operator_cli: true,
                },
            ))
            .await
            .with_context(|| {
                format!(
                    "resolving registered root '{root_id}' (daemon must support the dedicated stable-root RPC)"
                )
            })?
            .into_inner();
        anyhow::ensure!(
            response.root_id == root_id,
            "daemon selected root '{}' while '{}' was requested; refusing resolution evidence",
            response.root_id,
            root_id
        );
        let routed = std::fs::canonicalize(&response.local_root).with_context(|| {
            format!(
                "canonicalizing daemon-selected local_root for '{root_id}': {}",
                response.local_root
            )
        })?;
        anyhow::ensure!(
            requested == routed,
            "requested repo {} does not match daemon-selected root '{}' local_root {}",
            requested.display(),
            root_id,
            routed.display()
        );
        anyhow::ensure!(
            !response.remote_prefix.is_empty() && !response.state_path.is_empty(),
            "daemon omitted atomic route evidence for registered root '{root_id}'"
        );
        if !response.success {
            anyhow::bail!("resolution failed: {}", response.error);
        }

        println!("Root: {}", response.root_id);
        println!("Local root: {}", response.local_root);
        println!("Remote prefix: {}", response.remote_prefix);
        println!("State cache: {}", response.state_path);
        if !response.error.is_empty() {
            println!("{}", response.error);
        }
        if !execute {
            println!(
                "Dry-run only (pull-authorized inspection). Re-run with --execute using a pull+push session and a resolve-policy root to mutate refs and clear conflicts."
            );
        }
        return Ok(());
    }

    // Primary repository-group conflict resolution remains on the original
    // RPC. Ordinary-file mutation is rejected locally above and fail-closed by
    // the daemon for older clients.
    let resp = client
        .resolve_conflict(tonic::Request::new(
            tcfs_core::proto::ResolveConflictRequest {
                path: path.to_string_lossy().to_string(),
                resolution: resolution.clone(),
                // Explicit operator intent from the human-driven CLI. This is a
                // client-supplied defense-in-depth hint, not attestation; tcfsd
                // separately applies strategy-specific capability checks to
                // this legacy primary-cache RPC: repo dry-run requires pull
                // and execute requires pull+push, matching the named-root RPC.
                operator_cli: true,
            },
        ))
        .await
        .context("resolve_conflict RPC failed")?
        .into_inner();

    if resp.success {
        let is_repo_mode = resolution.starts_with("git_keep_both_");
        if is_repo_mode {
            if !resp.error.is_empty() {
                println!("{}", resp.error);
            }
            if !execute {
                println!("Dry-run only. Re-run with --execute to mutate refs and clear conflicts.");
            }
        } else if resolution == "defer" {
            println!(
                "Conflict deferred; ordinary-file mutation is disabled until it is root- and manifest-bound."
            );
        } else {
            unreachable!("ordinary-file mutation strategies are rejected before the RPC");
        }
        if !is_repo_mode
            && !resp.resolved_path.is_empty()
            && resp.resolved_path != path.to_string_lossy()
        {
            println!("  Conflict copy: {}", resp.resolved_path);
        }
    } else {
        anyhow::bail!("resolution failed: {}", resp.error);
    }

    Ok(())
}

// ── `tcfs conflicts` (read-only) ────────────────────────────────────────────

/// One conflicting path inside a group, as rendered for `tcfs conflicts`.
#[derive(Debug, Clone, serde::Serialize)]
struct ConflictPathReport {
    /// Repo-relative path of the conflicting file.
    rel_path: String,
    /// True when the path is `.git`-internal.
    git_internal: bool,
    /// The head ref (`refs/heads/<branch>`) when this is a head-ref conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    head_ref: Option<String>,
    local_device: String,
    remote_device: String,
    /// Unix timestamp of first detection (preserved across re-records).
    detected_at: u64,
    /// Number of reconcile cycles that re-recorded this conflict.
    times_recorded: u64,
}

/// A group of conflicts sharing one enclosing `.git` repo, or the flat bucket
/// of non-`.git` conflicts (`repo_root == None`).
#[derive(Debug, Clone, serde::Serialize)]
struct ConflictGroup {
    /// Absolute repo root for a `.git` group; `None` for the non-git bucket.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_root: Option<String>,
    /// True for a `.git`-internal repo group.
    is_git: bool,
    paths: Vec<ConflictPathReport>,
}

/// Derive the absolute enclosing `.git` repo root for a conflicted cache entry,
/// or `None` when the path is not git-internal.
///
/// The cache key is `<canonical local root>/<rel_path>`; stripping the
/// repo-relative suffix recovers the sync root, then
/// [`repo_root_for_git_path`](tcfs_sync::git_safety::repo_root_for_git_path)
/// locates the `.git` boundary.
fn conflict_repo_root(cache_key: &str, rel_path: &str) -> Option<PathBuf> {
    let rel_path = rel_path.replace('\\', "/");
    let cache_key = cache_key.replace('\\', "/");
    let local_root = cache_key
        .strip_suffix(&rel_path)
        .map(|s| s.trim_end_matches('/'))
        .unwrap_or("");
    tcfs_sync::git_safety::repo_root_for_git_path(Path::new(local_root), &rel_path)
}

/// Group recorded conflicts by their enclosing git repo (for `.git`-internal
/// paths) or into a single flat bucket (non-`.git`). Pure — no disk or network,
/// so it is directly unit-testable. Git groups are ordered by repo root; the
/// flat non-git bucket (if any) sorts last.
fn group_conflicts(items: &[(String, tcfs_sync::conflict::ConflictInfo)]) -> Vec<ConflictGroup> {
    use std::collections::BTreeMap;
    let mut git_groups: BTreeMap<String, ConflictGroup> = BTreeMap::new();
    let mut flat: Vec<ConflictPathReport> = Vec::new();

    for (key, info) in items {
        let rel = info.rel_path.as_str();
        let repo_root = conflict_repo_root(key, rel);
        let report = ConflictPathReport {
            rel_path: rel.to_string(),
            git_internal: repo_root.is_some(),
            head_ref: tcfs_sync::git_safety::head_ref_for_git_path(rel),
            local_device: info.local_device.clone(),
            remote_device: info.remote_device.clone(),
            detected_at: info.detected_at,
            times_recorded: info.times_recorded,
        };
        match repo_root {
            Some(root) => {
                let root_str = root.to_string_lossy().to_string();
                git_groups
                    .entry(root_str.clone())
                    .or_insert_with(|| ConflictGroup {
                        repo_root: Some(root_str),
                        is_git: true,
                        paths: Vec::new(),
                    })
                    .paths
                    .push(report);
            }
            None => flat.push(report),
        }
    }

    let mut out: Vec<ConflictGroup> = git_groups.into_values().collect();
    if !flat.is_empty() {
        out.push(ConflictGroup {
            repo_root: None,
            is_git: false,
            paths: flat,
        });
    }
    out
}

/// Resolve a repo's current HEAD to `<shortsha> <summary>` via
/// `git log --oneline -1`, or `None` when the repo/HEAD cannot be read.
///
/// Spawned through the tcfs-sync sanitizer so repository-local configuration
/// (hooks, `log.showSignature` + `gpg.program`) synced from another device can
/// never execute code on the machine listing conflicts (TIN-2853).
fn git_head_oneline(repo_root: &Path) -> Option<String> {
    let out = tcfs_sync::git_safety::sanitized_git_readonly_command()
        .args(["-C", &repo_root.to_string_lossy(), "log", "--oneline", "-1"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Age of a conflict as a coarse human string ("3d", "5h", "12m", "<1m").
fn conflict_age(detected_at: u64) -> String {
    if detected_at == 0 {
        return "unknown".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now.saturating_sub(detected_at);
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        "<1m".to_string()
    }
}

/// `tcfs conflicts` — list recorded conflicts, grouped by repo for
/// `.git`-internal paths. Named roots use the daemon-selected cache; the
/// primary/legacy path stays an offline read.
async fn cmd_conflicts(
    config: &tcfs_core::config::TcfsConfig,
    json: bool,
    root_id: Option<&str>,
    state_override: Option<&Path>,
) -> Result<()> {
    let (state_path, routed_local_root, routed_remote_prefix, items) =
        if let Some(root_id) = root_id {
            let mut client = connect_daemon(&config.daemon.socket).await?;
            let response = client
                .list_conflicts(tonic::Request::new(
                    tcfs_core::proto::ListConflictsRequest {
                        root_id: root_id.to_string(),
                    },
                ))
                .await
                .with_context(|| format!("listing conflicts for registered root '{root_id}'"))?
                .into_inner();
            anyhow::ensure!(
                response.root_id == root_id,
                "daemon returned root '{}' while '{}' was requested; refusing inspection",
                response.root_id,
                root_id
            );
            let items: Vec<(String, tcfs_sync::conflict::ConflictInfo)> = response
                .conflicts
                .into_iter()
                .map(|record| {
                    (
                        record.cache_key,
                        tcfs_sync::conflict::ConflictInfo {
                            rel_path: record.rel_path,
                            local_blake3: String::new(),
                            remote_blake3: String::new(),
                            local_device: record.local_device,
                            remote_device: record.remote_device,
                            local_vclock: tcfs_sync::conflict::VectorClock::new(),
                            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
                            detected_at: record.detected_at,
                            times_recorded: record.times_recorded,
                            remote_manifest_key: None,
                        },
                    )
                })
                .collect();
            (
                PathBuf::from(response.state_path),
                Some(response.local_root),
                Some(response.remote_prefix),
                items,
            )
        } else {
            let state_path = resolve_state_path(config, state_override);
            let state = tcfs_sync::state::StateCache::open(&state_path)
                .with_context(|| format!("opening state cache: {}", state_path.display()))?;
            let items: Vec<(String, tcfs_sync::conflict::ConflictInfo)> = state
                .conflicts()
                .into_iter()
                .filter_map(|(key, state)| {
                    state
                        .conflict
                        .as_ref()
                        .map(|conflict| (key.to_string(), conflict.clone()))
                })
                .collect();
            (state_path, None, None, items)
        };

    let groups = group_conflicts(&items);

    if json {
        // Render each group with resolvable HEAD evidence for `.git` groups.
        // The remote HEAD commit SHA is NOT carried in the offline state cache
        // (only a content hash + device); we surface honest degradation rather
        // than fetch it here — the resolve verb (PR-3) performs that fetch.
        let rendered: Vec<serde_json::Value> = groups
            .iter()
            .map(|g| {
                let local_head = g
                    .repo_root
                    .as_deref()
                    .map(Path::new)
                    .and_then(git_head_oneline);
                serde_json::json!({
                    "repo_root": g.repo_root,
                    "is_git": g.is_git,
                    "local_head": local_head,
                    "remote_head": serde_json::Value::Null,
                    "remote_head_note": if g.is_git {
                        Some("remote commit SHA not in offline state cache (objects not local yet / requires resolve-verb ref fetch)")
                    } else {
                        None
                    },
                    "paths": g.paths,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "root_id": root_id,
                "local_root": routed_local_root.as_deref(),
                "remote_prefix": routed_remote_prefix.as_deref(),
                "state_path": state_path.to_string_lossy(),
                "conflict_count": items.len(),
                "groups": rendered,
            }))?
        );
        return Ok(());
    }

    if items.is_empty() {
        if let Some(root_id) = root_id {
            println!("Root: {root_id}");
        }
        println!("No recorded conflicts.");
        return Ok(());
    }

    if let Some(root_id) = root_id {
        println!("Root: {root_id}");
        println!(
            "Route: {} ↔ {}/ (state: {})",
            routed_local_root.as_deref().unwrap_or("?"),
            routed_remote_prefix.as_deref().unwrap_or("?"),
            state_path.display()
        );
    }

    println!(
        "{} conflict(s) across {} group(s):",
        items.len(),
        groups.len()
    );
    for g in &groups {
        println!();
        match &g.repo_root {
            Some(root) => {
                println!("repo: {}", root);
                match git_head_oneline(Path::new(root)) {
                    Some(head) => println!("  local HEAD:  {}", head),
                    None => println!("  local HEAD:  <unreadable> ({})", root),
                }
                // Remote HEAD is not available offline (state cache carries a
                // content hash + device, not the remote commit SHA).
                let remote_device = g
                    .paths
                    .first()
                    .map(|p| p.remote_device.as_str())
                    .unwrap_or("?");
                println!(
                    "  remote HEAD: <objects not local yet> (from {}; resolve to fetch)",
                    remote_device
                );
                for p in &g.paths {
                    let kind = match &p.head_ref {
                        Some(r) => format!("head {}", r),
                        None => "git-internal".to_string(),
                    };
                    println!(
                        "  - {} [{}] age={} recorded={}x",
                        p.rel_path,
                        kind,
                        conflict_age(p.detected_at),
                        p.times_recorded
                    );
                }
                println!(
                    "  resolve: tcfs resolve {} --strategy keep-both --execute{}   (repo-group)",
                    root,
                    root_id
                        .map(|root_id| format!(" --root {root_id}"))
                        .unwrap_or_default()
                );
            }
            None => {
                println!("non-git conflicts:");
                for p in &g.paths {
                    println!(
                        "  - {} age={} recorded={}x (local={} remote={})",
                        p.rel_path,
                        conflict_age(p.detected_at),
                        p.times_recorded,
                        p.local_device,
                        p.remote_device
                    );
                    if root_id.is_some() {
                        println!(
                            "    resolve: named-root per-file resolution is outside the Strategy-A seam"
                        );
                    } else {
                        println!(
                            "    resolve: ordinary-file mutation is disabled; inspect or defer only"
                        );
                    }
                }
            }
        }
    }

    Ok(())
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
#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;
    use opendal::Operator;

    fn memory_op() -> Operator {
        let op = Operator::new(Memory::default()).unwrap().finish();
        tcfs_sync::index_entry::register_memory_index_emulation_for_tests(&op).unwrap();
        op
    }

    async fn seed_migration_manifest(
        op: &Operator,
        prefix: &str,
        manifest_hash: &str,
        rel_path: &str,
        size: u64,
        chunks: usize,
    ) {
        let manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: manifest_hash.to_string(),
            file_size: size,
            chunks: (0..chunks).map(|index| format!("chunk-{index}")).collect(),
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "migration-test".into(),
            written_at: 0,
            rel_path: Some(rel_path.to_string()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        op.write(
            &format!("{prefix}/manifests/{manifest_hash}"),
            manifest.to_bytes().unwrap(),
        )
        .await
        .unwrap();
    }

    fn master_key(fill: u8) -> tcfs_crypto::MasterKey {
        tcfs_crypto::MasterKey::from_bytes([fill; tcfs_crypto::KEY_SIZE])
    }

    #[test]
    fn migration_prefix_rejects_empty_or_root_like_scope() {
        for prefix in ["", "/", "data/", "/data"] {
            assert!(
                canonical_migration_prefix(prefix, "test migration prefix").is_err(),
                "unexpectedly accepted migration prefix {prefix:?}"
            );
        }
        assert_eq!(
            canonical_migration_prefix("data/nested", "test migration prefix").unwrap(),
            "data/nested"
        );
    }

    #[test]
    fn executing_migration_requires_explicit_writer_quiescence() {
        assert!(require_migration_writers_quiesced(true, false).is_ok());
        assert!(require_migration_writers_quiesced(false, true).is_ok());
        let error = require_migration_writers_quiesced(false, false).unwrap_err();
        assert!(format!("{error:#}").contains("--writers-quiesced"));

        let parsed = Cli::try_parse_from(["tcfs", "migrate-prefix", "--writers-quiesced"])
            .expect("writers-quiesced flag must parse");
        assert!(matches!(
            parsed.command,
            Commands::MigratePrefix {
                dry_run: false,
                writers_quiesced: true
            }
        ));
    }

    fn listed_trash_entry(path: &str, key: &str) -> tcfs_vfs::trash::TrashEntry {
        tcfs_vfs::trash::TrashEntry {
            original_path: path.to_string(),
            trashed_at: 1,
            trash_key: key.to_string(),
            index_content: String::new(),
            generation_state: tcfs_vfs::trash::TrashGenerationState::Completed,
        }
    }

    #[test]
    fn trash_restore_requires_exact_key_for_ambiguous_generations() {
        let first = "data/.tcfs-trash/1-00000000-0000-4000-8000-000000000001/doc.txt";
        let second = "data/.tcfs-trash/1-00000000-0000-4000-8000-000000000002/doc.txt";
        let entries = vec![
            listed_trash_entry("doc.txt", first),
            listed_trash_entry("doc.txt", second),
        ];

        let error = select_trash_entry(&entries, "doc.txt", None)
            .expect_err("path-only restore must reject ambiguous generations");
        assert!(format!("{error:#}").contains("--trash-key"));
        assert_eq!(
            select_trash_entry(&entries, "doc.txt", Some(second))
                .unwrap()
                .trash_key,
            second
        );

        let parsed =
            Cli::try_parse_from(["tcfs", "trash", "restore", "doc.txt", "--trash-key", second])
                .expect("exact trash generation selector must parse");
        assert!(matches!(
            parsed.command,
            Commands::Trash {
                action: TrashAction::Restore {
                    trash_key: Some(key),
                    ..
                }
            } if key == second
        ));
    }

    #[test]
    fn trash_retention_zero_requires_explicit_all() {
        assert_eq!(trash_purge_max_age(true, None, 0).unwrap(), 0);
        assert_eq!(trash_purge_max_age(false, Some(60), 0).unwrap(), 60);
        assert!(trash_purge_max_age(true, Some(60), 0).is_err());
        let error = trash_purge_max_age(false, None, 0).unwrap_err();
        assert!(format!("{error:#}").contains("explicitly use --all"));

        let parse_error =
            Cli::try_parse_from(["tcfs", "trash", "purge", "--all", "--older-than", "60"])
                .expect_err("--all and --older-than must conflict at the CLI boundary");
        assert!(parse_error.to_string().contains("cannot be used with"));
    }

    #[tokio::test]
    async fn migration_destination_race_never_overwrites_or_deletes_source() {
        let op = memory_op();
        let old_key = "legacy/index/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let source = b"manifest_hash=source\nsize=10\nchunks=1".to_vec();
        let competing = b"manifest_hash=competing\nsize=20\nchunks=2".to_vec();
        op.write(old_key, source.clone()).await.unwrap();

        // Bind the source, then deterministically inject a competing publisher
        // in the exact window before the migration's absent-object create.
        let bound = op.read(old_key).await.unwrap().to_vec();
        op.write(new_key, competing.clone()).await.unwrap();
        let error = migrate_bound_index_entry(&op, old_key, new_key, &bound)
            .await
            .expect_err("different destination bytes must stop migration");

        assert!(
            format!("{error:#}").contains("different bytes"),
            "unexpected migration error: {error:#}"
        );
        assert_eq!(op.read(old_key).await.unwrap().to_vec(), source);
        assert_eq!(op.read(new_key).await.unwrap().to_vec(), competing);
    }

    #[tokio::test]
    async fn migration_accepts_exact_destination_and_retains_source() {
        let op = memory_op();
        let old_key = "legacy/index/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let source = b"manifest_hash=source\nsize=10\nchunks=1".to_vec();
        seed_migration_manifest(&op, "data", "source", "docs/report.txt", 10, 1).await;
        op.write(old_key, source.clone()).await.unwrap();
        op.write(new_key, source.clone()).await.unwrap();

        let outcome = migrate_index_entry(&op, "data", "docs/report.txt", old_key, new_key, false)
            .await
            .unwrap();

        assert_eq!(outcome, MigrationInstallOutcome::AlreadyExact);
        assert_eq!(op.read(old_key).await.unwrap().to_vec(), source);
        assert_eq!(op.read(new_key).await.unwrap().to_vec(), source);
    }

    #[tokio::test]
    async fn double_prefix_migration_logically_retires_source_and_is_idempotent() {
        let op = memory_op();
        let old_key = "data/index/data/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let source = b"manifest_hash=source\nsize=10\nchunks=1".to_vec();
        seed_migration_manifest(&op, "data", "source", "docs/report.txt", 10, 1).await;
        op.write(old_key, source.clone()).await.unwrap();

        let outcome = migrate_index_entry(&op, "data", "docs/report.txt", old_key, new_key, true)
            .await
            .unwrap();

        assert_eq!(outcome, MigrationInstallOutcome::Created);
        assert_eq!(op.read(new_key).await.unwrap().to_vec(), source);
        let retired = tcfs_sync::index_entry::read_index_entry_record_from_store(&op, old_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retired.state(),
            tcfs_sync::index_entry::IndexEntryState::Deleted
        );

        let retry = migrate_index_entry(&op, "data", "docs/report.txt", old_key, new_key, true)
            .await
            .unwrap();
        assert_eq!(retry, MigrationInstallOutcome::SourceAlreadyRetired);
        assert_eq!(op.read(new_key).await.unwrap().to_vec(), source);
    }

    #[tokio::test]
    async fn migration_rejects_unbound_manifest_before_destination_publication() {
        let op = memory_op();
        let old_key = "data/index/data/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let source = b"manifest_hash=source\nsize=10\nchunks=1".to_vec();
        seed_migration_manifest(&op, "data", "source", "data/docs/report.txt", 10, 1).await;
        op.write(old_key, source.clone()).await.unwrap();

        let error = migrate_index_entry(&op, "data", "docs/report.txt", old_key, new_key, true)
            .await
            .expect_err("path-mismatched manifest must fail before destination publication");

        assert!(format!("{error:#}").contains("manifest rel_path mismatch"));
        assert!(!op.exists(new_key).await.unwrap());
        assert_eq!(op.read(old_key).await.unwrap().to_vec(), source);
    }

    #[tokio::test]
    async fn migration_rejects_pending_staged_manifest_outside_target_root() {
        let op = memory_op();
        let old_key = "data/index/data/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let pending = tcfs_sync::index_entry::PendingIndexEntry::new(
            "source",
            10,
            1,
            "other/staging/manifests/00000000-0000-4000-8000-000000000000-source.json",
        );
        let source = tcfs_sync::index_entry::VersionedIndexEntry::preparing(None, pending)
            .to_json_bytes()
            .unwrap();
        op.write(old_key, source.clone()).await.unwrap();

        let error = migrate_index_entry(&op, "data", "docs/report.txt", old_key, new_key, true)
            .await
            .expect_err("cross-root pending staging key must fail before publication");

        assert!(format!("{error:#}").contains("escapes its root staging namespace"));
        assert!(!op.exists(new_key).await.unwrap());
        assert_eq!(op.read(old_key).await.unwrap().to_vec(), source);
    }

    #[tokio::test]
    async fn orphan_tombstone_is_not_republished_into_target_root() {
        let op = memory_op();
        let old_key = "legacy/index/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let tombstone = tcfs_sync::index_entry::VersionedIndexEntry::deleted()
            .to_json_bytes()
            .unwrap();
        op.write(old_key, tombstone.clone()).await.unwrap();

        let inspected =
            inspect_migration_destination(&op, "data", "docs/report.txt", old_key, new_key)
                .await
                .unwrap();
        assert_eq!(inspected, MigrationDryRunOutcome::SourceAlreadyRetired);
        assert!(!op.exists(new_key).await.unwrap());

        let outcome = migrate_index_entry(&op, "data", "docs/report.txt", old_key, new_key, false)
            .await
            .unwrap();
        assert_eq!(outcome, MigrationInstallOutcome::SourceAlreadyRetired);
        assert_eq!(op.read(old_key).await.unwrap().to_vec(), tombstone);
        assert!(!op.exists(new_key).await.unwrap());
    }

    #[tokio::test]
    async fn double_prefix_directory_marker_migrates_and_logically_retires_source() {
        let op = memory_op();
        let old_key = "data/index/data/empty/.tcfs_dir";
        let new_key = "data/index/empty/.tcfs_dir";
        op.write(
            old_key,
            tcfs_sync::index_entry::DIRECTORY_MARKER_BYTES.to_vec(),
        )
        .await
        .unwrap();

        let outcome = migrate_index_entry(&op, "data", "empty/.tcfs_dir", old_key, new_key, true)
            .await
            .unwrap();

        assert_eq!(outcome, MigrationInstallOutcome::Created);
        assert!(
            tcfs_sync::index_entry::directory_marker_is_visible(&op, new_key)
                .await
                .unwrap()
        );
        assert!(
            !tcfs_sync::index_entry::directory_marker_is_visible(&op, old_key)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn unregistered_memory_accessor_cannot_emulate_migration_absent_write() {
        let op = Operator::new(Memory::default()).unwrap().finish();
        let error = install_migration_destination(
            &op,
            "data/index/docs/report.txt",
            b"manifest_hash=source\nsize=10\nchunks=1",
        )
        .await
        .expect_err("scheme alone must not enable migration write emulation");
        assert!(format!("{error:#}").contains("requires atomic absent-object creation"));
    }

    #[tokio::test]
    async fn migration_revalidation_preserves_source_changed_after_snapshot() {
        let op = memory_op();
        let old_key = "legacy/index/docs/report.txt";
        let new_key = "data/index/docs/report.txt";
        let snapshot = b"manifest_hash=old\nsize=10\nchunks=1".to_vec();
        let concurrent = b"manifest_hash=new\nsize=20\nchunks=2".to_vec();
        op.write(old_key, snapshot.clone()).await.unwrap();

        let bound = op.read(old_key).await.unwrap().to_vec();
        op.write(old_key, concurrent.clone()).await.unwrap();
        let error = migrate_bound_index_entry(&op, old_key, new_key, &bound)
            .await
            .expect_err("changed source must not be deleted");

        assert!(
            format!("{error:#}").contains("different bytes"),
            "unexpected migration error: {error:#}"
        );
        assert_eq!(op.read(old_key).await.unwrap().to_vec(), concurrent);
        assert_eq!(op.read(new_key).await.unwrap().to_vec(), snapshot);
    }

    #[test]
    fn mount_flush_events_publish_only_canonical_relative_paths() {
        assert_eq!(
            tcfs_vfs::virtual_path_to_canonical_rel_path("/notes/todo.txt").unwrap(),
            "notes/todo.txt"
        );
        for invalid in [
            "notes/todo.txt",
            "///nested/file",
            "/../outside",
            "/.GIT/config",
        ] {
            assert!(
                tcfs_vfs::virtual_path_to_canonical_rel_path(invalid).is_err(),
                "flush path must be rejected: {invalid:?}"
            );
        }
    }

    fn mk_conflict(rel: &str) -> tcfs_sync::conflict::ConflictInfo {
        tcfs_sync::conflict::ConflictInfo {
            rel_path: rel.to_string(),
            local_vclock: tcfs_sync::conflict::VectorClock::new(),
            remote_vclock: tcfs_sync::conflict::VectorClock::new(),
            local_blake3: "aaaa".into(),
            remote_blake3: "bbbb".into(),
            local_device: "neo".into(),
            remote_device: "honey".into(),
            detected_at: 1_700_000_000,
            times_recorded: 3,
            remote_manifest_key: None,
        }
    }

    #[test]
    fn stable_root_flags_parse_and_conflicts_reject_state_mix() {
        use clap::CommandFactory;

        let cli = Cli::try_parse_from([
            "tcfs",
            "resolve",
            "/repo",
            "--root",
            "git-roam-tool-daemon",
            "--strategy",
            "keep-both",
            "--execute",
        ])
        .expect("parse registered-root resolve");
        let Commands::Resolve {
            root,
            strategy,
            execute,
            ..
        } = cli.command
        else {
            panic!("expected resolve command");
        };
        assert_eq!(root.as_deref(), Some("git-roam-tool-daemon"));
        assert_eq!(strategy.as_deref(), Some("keep-both"));
        assert!(execute);

        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("resolve")
            .expect("resolve subcommand")
            .render_long_help()
            .to_string();
        let normalized_help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for required in [
            "Named-root dry-run requires pull permission",
            "execute requires both pull and push",
            "Inspect-only roots permit dry-run but reject execute",
            "keep-both for a Git repo",
            "Ordinary-file mutation is disabled fail-closed",
        ] {
            assert!(
                normalized_help.contains(required),
                "missing `{required}` from:\n{help}"
            );
        }

        for retired in ["keep-local", "keep-remote"] {
            assert!(
                Cli::try_parse_from(["tcfs", "resolve", "/ordinary-file", "--strategy", retired,])
                    .is_err(),
                "retired strategy {retired} must not remain in shipped CLI help/parser"
            );
        }

        assert!(
            Cli::try_parse_from([
                "tcfs",
                "conflicts",
                "--root",
                "git-roam-tool-daemon",
                "--state",
                "/tmp/root.json",
            ])
            .is_err(),
            "a named root must never be combined with a client state path"
        );
    }

    #[test]
    fn config_show_never_serializes_nats_token_content() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "# display source marker\n").unwrap();

        let mut config = tcfs_core::config::TcfsConfig::default();
        let token = "TIN2860-left-sentinel.middle-sentinel.right-sentinel";
        config.sync.nats_token = Some(token.into());
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
        let output = render_config_show(&config, &config_path).unwrap();

        for forbidden in [
            token,
            "TIN2860-left-sentinel",
            "middle-sentinel",
            "right-sentinel",
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
        ] {
            assert!(
                !output.contains(forbidden),
                "CLI config output leaked token material: {output}"
            );
        }

        let (_, rendered) = output
            .split_once("\n\n")
            .expect("config output must separate its source header from TOML");
        let value: toml::Value =
            toml::from_str(rendered).expect("CLI config output must remain valid TOML");
        assert_eq!(value["sync"]["nats_token_configured"].as_bool(), Some(true));
        assert!(value["sync"].get("nats_token").is_none());
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
        assert!(
            output.contains("# Redacted diagnostic view; not suitable for reuse as configuration")
        );
    }

    #[test]
    fn mount_display_is_origin_only() {
        let remote = "seaweedfs+https://mount-user:MOUNT-secret@storage.example.test:8333/bucket/MOUNT-path?token=MOUNT-query#MOUNT-fragment";
        let rendered = mount_remote_endpoint_for_display(remote);
        assert_eq!(rendered, "https://storage.example.test:8333");
        for forbidden in [
            "mount-user",
            "MOUNT-secret",
            "bucket",
            "MOUNT-path",
            "MOUNT-query",
            "MOUNT-fragment",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "mount display leaked {forbidden}: {rendered}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn device_invite_routing_metadata_is_origin_only() {
        use tcfs_auth::enrollment::EnrollmentInvite;
        use tcfs_auth::session::DevicePermissions;

        let signing_key = [7_u8; tcfs_crypto::KEY_SIZE];
        let mut config = tcfs_core::config::TcfsConfig::default();
        config.storage.endpoint =
            "https://invite-user:INVITE-secret@storage.example.test:8333/INVITE-path?token=INVITE-query#INVITE-fragment"
                .into();
        config.storage.bucket = "invite-bucket".into();
        config.storage.remote_prefix = Some("invite-prefix".into());

        let mut invite = EnrollmentInvite::new(
            "source-device",
            &signing_key,
            1,
            DevicePermissions::default(),
        );
        populate_invite_routing_metadata(&mut invite, &config).unwrap();
        invite.refresh_signature(&signing_key);

        let encoded = invite.encode_compact().unwrap();
        let decoded = EnrollmentInvite::decode_compact(&encoded).unwrap();
        assert_eq!(
            decoded.storage_endpoint.as_deref(),
            Some("https://storage.example.test:8333")
        );
        assert_eq!(decoded.storage_bucket.as_deref(), Some("invite-bucket"));
        assert_eq!(decoded.remote_prefix.as_deref(), Some("invite-prefix"));
        for forbidden in [
            "invite-user",
            "INVITE-secret",
            "INVITE-path",
            "INVITE-query",
            "INVITE-fragment",
        ] {
            assert!(
                !decoded
                    .storage_endpoint
                    .as_deref()
                    .unwrap()
                    .contains(forbidden),
                "decoded invite leaked {forbidden}: {decoded:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn device_invite_rejects_invalid_endpoint_without_echoing_input() {
        use tcfs_auth::enrollment::EnrollmentInvite;
        use tcfs_auth::session::DevicePermissions;

        let signing_key = [7_u8; tcfs_crypto::KEY_SIZE];
        for endpoint in [
            "not-an-endpoint-with-INVALID-secret?token=INVALID-query",
            "ftp://invite-user:FTP-secret@storage.example.test/FTP-path?token=FTP-query",
        ] {
            let mut config = tcfs_core::config::TcfsConfig::default();
            config.storage.endpoint = endpoint.into();
            let mut invite = EnrollmentInvite::new(
                "source-device",
                &signing_key,
                1,
                DevicePermissions::default(),
            );

            let rendered = populate_invite_routing_metadata(&mut invite, &config)
                .unwrap_err()
                .to_string();
            assert!(invite.storage_endpoint.is_none());
            assert!(invite.storage_bucket.is_none());
            assert!(invite.remote_prefix.is_none());
            for forbidden in [
                "INVALID-secret",
                "INVALID-query",
                "invite-user",
                "FTP-secret",
                "FTP-path",
                "FTP-query",
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "invite validation error leaked {forbidden}: {rendered}"
                );
            }
        }
    }

    #[tokio::test]
    async fn load_config_parse_error_never_echoes_offending_source_line() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let malformed = r#"
[sync]
nats_token = ["TIN2860-malformed-left", "malformed-middle", "malformed-right"]
"#;
        std::fs::write(&config_path, malformed).unwrap();

        let error = load_config(&config_path)
            .await
            .expect_err("malformed token type must fail config loading");
        let rendered = format!("{error:#}");

        for forbidden in [
            "TIN2860-malformed-left",
            "malformed-middle",
            "malformed-right",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "CLI parse error leaked config source material: {rendered}"
            );
        }
        assert!(rendered.contains(&config_path.display().to_string()));
        assert!(rendered.contains("check TOML syntax and field types"));
    }

    #[test]
    fn conflicts_group_by_repo_and_classify_git_paths() {
        // keep-both PR-1 (piece 3): `.git`-internal conflicts group by their
        // enclosing repo; non-`.git` conflicts fall into a flat bucket.
        // Cache keys are `<sync root>/<rel_path>`.
        let items = vec![
            (
                "/sync/repoA/.git/refs/heads/main".to_string(),
                mk_conflict("repoA/.git/refs/heads/main"),
            ),
            (
                "/sync/repoA/.git/index".to_string(),
                mk_conflict("repoA/.git/index"),
            ),
            (
                "/sync/repoB/.git/HEAD".to_string(),
                mk_conflict("repoB/.git/HEAD"),
            ),
            (
                "/sync/repoC/.git/refs/heads/main".to_string(),
                mk_conflict("repoC\\.git\\refs\\heads\\main"),
            ),
            (
                "/sync/notes/todo.txt".to_string(),
                mk_conflict("notes/todo.txt"),
            ),
        ];

        let groups = group_conflicts(&items);

        // Three git repo groups + one flat non-git bucket.
        assert_eq!(
            groups.len(),
            4,
            "expected repoA, repoB, repoC, and a flat bucket"
        );

        let repo_a = groups
            .iter()
            .find(|g| g.repo_root.as_deref() == Some("/sync/repoA"))
            .expect("repoA group present");
        assert!(repo_a.is_git);
        assert_eq!(repo_a.paths.len(), 2, "both repoA paths in one group");
        // The head-ref path is classified with its ref name; index is not.
        let head = repo_a
            .paths
            .iter()
            .find(|p| p.rel_path.ends_with("refs/heads/main"))
            .unwrap();
        assert_eq!(head.head_ref.as_deref(), Some("refs/heads/main"));
        assert!(head.git_internal);
        assert_eq!(head.times_recorded, 3);
        let index = repo_a
            .paths
            .iter()
            .find(|p| p.rel_path.ends_with(".git/index"))
            .unwrap();
        assert!(index.head_ref.is_none());
        assert!(index.git_internal);

        let repo_b = groups
            .iter()
            .find(|g| g.repo_root.as_deref() == Some("/sync/repoB"))
            .expect("repoB group present");
        assert_eq!(repo_b.paths.len(), 1);

        let repo_c = groups
            .iter()
            .find(|g| g.repo_root.as_deref() == Some("/sync/repoC"))
            .expect("repoC group present");
        assert_eq!(repo_c.paths.len(), 1);
        assert!(repo_c.paths[0].git_internal);
        assert_eq!(repo_c.paths[0].head_ref.as_deref(), Some("refs/heads/main"));

        // The non-git conflict is flat (no repo_root), and not git-internal.
        let flat = groups
            .iter()
            .find(|g| g.repo_root.is_none())
            .expect("flat bucket present");
        assert!(!flat.is_git);
        assert_eq!(flat.paths.len(), 1);
        assert!(!flat.paths[0].git_internal);
        assert_eq!(flat.paths[0].rel_path, "notes/todo.txt");
    }

    fn test_config(sync_root: &Path) -> tcfs_core::config::TcfsConfig {
        let mut config = tcfs_core::config::TcfsConfig::default();
        config.storage.bucket = "test-bucket".into();
        config.storage.remote_prefix = Some("data".into());
        config.sync.sync_root = Some(sync_root.to_path_buf());
        config.sync.state_db = sync_root.join("state.db");
        config
    }

    #[tokio::test]
    async fn push_preflight_rejects_direct_custom_key_and_containing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let selected_root = dir.path().join("selected");
        std::fs::create_dir(&selected_root).unwrap();
        let key_path = selected_root.join("custom-key-material.bin");
        std::fs::write(&key_path, [7u8; tcfs_crypto::KEY_SIZE]).unwrap();

        let mut config = test_config(&selected_root);
        config.crypto.master_key_file = Some(key_path.clone());
        let op = memory_op();
        let state_path = dir.path().join("must-not-open.json");

        for selected in [&key_path, &selected_root] {
            let error =
                cmd_push_with_operator(&config, &op, selected, None, &state_path, "test-device")
                    .await
                    .expect_err("push must reject a selected path containing the configured key");
            assert!(
                error.to_string().contains("crypto.master_key_file"),
                "{error:#}"
            );
        }
        assert!(
            !state_path.exists(),
            "preflight must run before opening the state cache"
        );
    }

    #[tokio::test]
    async fn push_preflight_applies_fixed_key_artifact_denies_without_configured_key() {
        let dir = tempfile::tempdir().unwrap();
        let config = tcfs_core::config::TcfsConfig::default();
        let op = memory_op();
        let state_path = dir.path().join("must-not-open.json");

        for name in [
            "master.key",
            ".custom-key.rotate-pending",
            ".custom-key.rotate-state.json",
            ".custom-key.tmp.0123456789abcdef0123456789abcdef",
        ] {
            let selected = dir.path().join(name);
            std::fs::write(&selected, b"sensitive").unwrap();
            let error =
                cmd_push_with_operator(&config, &op, &selected, None, &state_path, "test-device")
                    .await
                    .expect_err("direct fixed-deny push must fail before state or storage access");
            assert!(
                error.to_string().contains("fixed security deny-set"),
                "{name}: {error:#}"
            );
        }
        assert!(!state_path.exists());
    }

    #[tokio::test]
    async fn pull_preflight_rejects_fixed_logical_paths_without_touching_bytes_or_state() {
        let dir = tempfile::tempdir().unwrap();
        let config = tcfs_core::config::TcfsConfig::default();
        let op = memory_op();
        let destination = dir.path().join("unchanged.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();

        for (index, remote_path) in ["master.key", ".rotate-pending", ".env"]
            .into_iter()
            .enumerate()
        {
            let state_path = dir.path().join(format!("must-not-open-{index}.json"));
            let error = cmd_pull_with_operator(
                &config,
                &op,
                remote_path,
                Some(&destination),
                None,
                &state_path,
                "test-device",
            )
            .await
            .expect_err("fixed-deny pull must fail before state or storage access");
            assert!(
                error.to_string().contains("fixed security deny-set"),
                "{remote_path}: {error:#}"
            );
            assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
            assert!(!state_path.exists());
        }

        let command_state = dir.path().join("command-state.db");
        let error = cmd_pull(
            &config,
            ".env",
            Some(&destination),
            None,
            Some(&command_state),
        )
        .await
        .expect_err("command wrapper must reject before locking state or building storage");
        assert!(error.to_string().contains("fixed security deny-set"));
        assert!(!command_state.with_extension("json").exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
    }

    #[tokio::test]
    async fn pull_preflight_rejects_custom_master_key_and_containing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let key_path = sync_root.join("custom-key-material.bin");
        std::fs::write(&key_path, b"keep-key-bytes").unwrap();
        let mut config = test_config(&sync_root);
        config.crypto.master_key_file = Some(key_path.clone());
        let state_override = dir.path().join("must-not-lock.db");

        for destination in [&key_path, &sync_root] {
            let error = cmd_pull(
                &config,
                "data/manifests/safe-reference",
                Some(destination),
                None,
                Some(&state_override),
            )
            .await
            .expect_err("master-key destination must fail before state or storage access");
            assert!(
                error.to_string().contains("crypto.master_key_file"),
                "{error:#}"
            );
            assert_eq!(std::fs::read(&key_path).unwrap(), b"keep-key-bytes");
            assert!(!state_override.with_extension("json").exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_preflight_rejects_symlink_alias_of_custom_master_key() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        std::fs::create_dir_all(&sync_root).unwrap();
        let key_path = sync_root.join("custom-key-material.bin");
        let alias_path = sync_root.join("key-alias.bin");
        std::fs::write(&key_path, b"keep-key-bytes").unwrap();
        std::os::unix::fs::symlink(&key_path, &alias_path).unwrap();
        let mut config = test_config(&sync_root);
        config.crypto.master_key_file = Some(key_path.clone());
        let state_path = dir.path().join("must-not-open.json");

        let error = cmd_pull_with_operator(
            &config,
            &memory_op(),
            "data/manifests/safe-reference",
            Some(&alias_path),
            None,
            &state_path,
            "test-device",
        )
        .await
        .expect_err("symlink alias of the master key must fail before storage access");
        assert!(
            error.to_string().contains("crypto.master_key_file"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), b"keep-key-bytes");
        assert!(!state_path.exists());
    }

    #[tokio::test]
    async fn reconcile_preflight_rejects_root_containing_custom_key() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        std::fs::create_dir(&sync_root).unwrap();
        let key_path = sync_root.join("custom-key-material.bin");
        std::fs::write(&key_path, [7u8; tcfs_crypto::KEY_SIZE]).unwrap();

        let mut config = test_config(&sync_root);
        config.crypto.master_key_file = Some(key_path);
        let error = cmd_reconcile(&config, Some(&sync_root), None, false, None)
            .await
            .expect_err("reconcile must reject before credential discovery");

        assert!(
            error.to_string().contains("crypto.master_key_file"),
            "{error:#}"
        );
        assert!(!resolve_state_path(&config, None).exists());
    }

    #[tokio::test]
    async fn master_key_rotation_guard_precedes_artifact_and_storage_access() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let named = dir.path().join("named");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&named).unwrap();

        for (root_kind, root) in [("primary", &primary), ("named", &named)] {
            let key_path = root.join(format!("{root_kind}-custom-key.bin"));
            std::fs::write(&key_path, [7u8; tcfs_crypto::KEY_SIZE]).unwrap();
            let paths = key_rotation_paths(&key_path);
            let mut config = tcfs_core::config::TcfsConfig::default();
            if root_kind == "primary" {
                config.sync.sync_root = Some(root.to_path_buf());
            } else {
                config.sync.roots.insert(
                    "named".into(),
                    tcfs_core::config::RegisteredRootConfig {
                        local_root: root.to_path_buf(),
                        remote_prefix: "roots/named".into(),
                        state_path: dir.path().join("reconcile/named.json"),
                        policy: tcfs_core::config::RegisteredRootPolicy::InspectOnly,
                    },
                );
            }

            let error = cmd_rotate_key(&config, Some(&key_path), false, None, true)
                .await
                .expect_err("rotation must reject before credential discovery");
            assert!(error.to_string().contains("master key path"), "{error:#}");
            assert!(!paths.pending_key_path.exists());
            assert!(!paths.state_path.exists());
            assert!(
                atomic_write_temp_path(&key_path, 0).parent() == key_path.parent(),
                "test must cover the same adjacent artifact directory"
            );
        }
    }

    // ── TIN-2657: CLI/daemon state-path convergence ──────────────────────────

    #[test]
    fn resolve_state_path_override_db_equals_default() {
        // THE TIN-2657 regression guard: a `--state …/state.db` override must
        // resolve to the *same* file as the config default (the daemon-owned
        // `state.json`). On the pre-fix `return p.to_path_buf()` this fails,
        // because the override stayed `…/state.db` while the default derived
        // `…/state.json` — the split that orphaned writes and hid conflicts.
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        let config = test_config(&sync_root); // state_db = sync_root/state.db
        let db_literal = config.sync.state_db.clone(); // …/state.db

        let via_override = resolve_state_path(&config, Some(&db_literal));
        let via_default = resolve_state_path(&config, None);

        assert_eq!(
            via_override, via_default,
            "override `.db` must resolve to the same file as the default"
        );
        assert_eq!(
            via_default.extension().and_then(|e| e.to_str()),
            Some("json"),
            "canonical state path is always `.json`"
        );
    }

    #[test]
    fn resolve_state_path_json_override_idempotent_and_tilde_expands() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("tree"));

        // An explicit `.json` override is idempotent under normalization.
        let json_override = dir.path().join("reconcile/root-a.json");
        assert_eq!(
            resolve_state_path(&config, Some(&json_override)),
            json_override,
            "`.json` override must pass through unchanged"
        );

        // A `~`-prefixed override is expanded, and `.db` still normalizes to
        // `.json`. Read HOME rather than mutate it, so parallel tests can't race.
        let home = std::env::var("HOME").expect("HOME set in test env");
        let tilde = std::path::PathBuf::from("~/tcfs-tin2657/state.db");
        assert_eq!(
            resolve_state_path(&config, Some(&tilde)),
            std::path::PathBuf::from(format!("{home}/tcfs-tin2657/state.json")),
            "tilde expands and `.db` normalizes to `.json`"
        );
    }

    #[tokio::test]
    async fn explicit_state_writers_fail_before_operator_work_when_locked() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let local = sync_root.join("doc.txt");
        std::fs::write(&local, b"local-only test data").unwrap();

        let mut config = test_config(&sync_root);
        config.storage.endpoint = "https://should-never-be-contacted.invalid".into();
        let state_override = dir.path().join("registered-root.json");
        let state_path = resolve_state_path(&config, Some(&state_override));
        let _held_lock = tcfs_sync::state::StateFileLock::acquire(&state_path).unwrap();

        let push_error = cmd_push(&config, &local, None, Some(&state_override))
            .await
            .unwrap_err();
        let pull_error = cmd_pull(
            &config,
            "data/manifests/should-not-be-read",
            Some(&local),
            None,
            Some(&state_override),
        )
        .await
        .unwrap_err();
        let rm_error = cmd_rm(&config, &local, None, Some(&state_override))
            .await
            .unwrap_err();

        for error in [push_error, pull_error, rm_error] {
            let chain = format!("{error:#}");
            assert!(
                chain.contains("locking explicit state cache")
                    && chain.contains("is locked by another process"),
                "explicit state writer must fail at the lock before operator/network work: {chain}"
            );
            assert!(
                !chain.contains("credential discovery")
                    && !chain.contains("building storage operator"),
                "lock contention must precede operator construction: {chain}"
            );
        }
    }

    #[test]
    fn cli_write_via_db_override_visible_to_daemon_json_read() {
        // CLI writes through a `--state …/state.db` override; the daemon reads
        // at `config.sync.state_db.with_extension("json")`. Post-fix they are
        // the same file, so the write is visible.
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let file = sync_root.join("doc.txt");
        std::fs::write(&file, b"hello").unwrap();
        let config = test_config(&sync_root);

        let cli_path = resolve_state_path(&config, Some(&config.sync.state_db));
        let mut cli_state = tcfs_sync::state::StateCache::open(&cli_path).unwrap();
        seed_tracked_file(&mut cli_state, &file, "data/index/doc.txt");
        cli_state.flush().unwrap();

        let daemon_path = config.sync.state_db.with_extension("json");
        assert_eq!(cli_path, daemon_path, "CLI and daemon resolve one file");
        let daemon_state = tcfs_sync::state::StateCache::open(&daemon_path).unwrap();
        assert!(
            daemon_state.get(&file).is_some(),
            "CLI write must be visible to the daemon read"
        );
    }

    #[test]
    fn daemon_write_json_visible_to_cli_db_override_read() {
        // The reverse: the daemon writes at the canonical `.json`; a CLI
        // invocation using `--state …/state.db` must still see it.
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let file = sync_root.join("doc.txt");
        std::fs::write(&file, b"hello").unwrap();
        let config = test_config(&sync_root);

        let daemon_path = config.sync.state_db.with_extension("json");
        let mut daemon_state = tcfs_sync::state::StateCache::open(&daemon_path).unwrap();
        seed_tracked_file(&mut daemon_state, &file, "data/index/doc.txt");
        daemon_state.flush().unwrap();

        let cli_path = resolve_state_path(&config, Some(&config.sync.state_db));
        let cli_state = tcfs_sync::state::StateCache::open(&cli_path).unwrap();
        assert!(
            cli_state.get(&file).is_some(),
            "daemon write must be visible to a `--state …/state.db` CLI read"
        );
    }

    #[test]
    fn init_paths_use_config_out_parent_for_master_key() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("nested/config.toml");
        let paths = InitPaths::resolve(Some(&config_path));

        assert_eq!(paths.config_path, config_path);
        assert_eq!(paths.config_dir, dir.path().join("nested"));
        assert_eq!(paths.master_key_path, dir.path().join("nested/master.key"));
        assert_eq!(paths.registry_path, dir.path().join("nested/devices.json"));
    }

    #[test]
    fn init_paths_use_current_dir_for_relative_config_out() {
        let paths = InitPaths::resolve(Some(Path::new("config.toml")));

        assert_eq!(paths.config_path, PathBuf::from("config.toml"));
        assert_eq!(paths.config_dir, PathBuf::from("."));
        assert_eq!(paths.master_key_path, PathBuf::from(".").join("master.key"));
        assert_eq!(paths.registry_path, PathBuf::from(".").join("devices.json"));
    }

    #[test]
    fn build_init_config_enables_crypto_and_device_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut base = test_config(dir.path());
        base.storage.endpoint = "https://s3.example.test".into();
        base.crypto.enabled = false;
        base.crypto.master_key_file = None;
        base.sync.device_name = None;

        let master_key_path = dir.path().join("master.key");
        let registry_path = dir.path().join("devices.json");
        let config = build_init_config(&base, &master_key_path, &registry_path, "laptop");

        assert!(config.crypto.enabled);
        assert_eq!(
            config.crypto.master_key_file.as_deref(),
            Some(master_key_path.as_path())
        );
        assert_eq!(
            config.sync.device_identity.as_deref(),
            Some(registry_path.as_path())
        );
        assert_eq!(config.sync.device_name.as_deref(), Some("laptop"));
        assert_eq!(config.storage.endpoint, "https://s3.example.test");
    }

    #[test]
    fn write_init_config_refuses_existing_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "existing = true\n").unwrap();
        let config = tcfs_core::config::TcfsConfig::default();

        let err = write_init_config(&config_path, &config, false).unwrap_err();
        assert!(err.to_string().contains("Config already exists"));

        write_init_config(&config_path, &config, true).unwrap();
        let reparsed: tcfs_core::config::TcfsConfig =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(reparsed.storage.bucket, config.storage.bucket);
    }

    #[test]
    fn build_fileprovider_init_config_emits_hostapp_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.storage.endpoint = "https://s3.example.test".into();
        config.storage.bucket = "tcfs-smoke".into();
        config.storage.remote_prefix = Some("devices/neo".into());
        config.daemon.fileprovider_endpoint = Some("http://127.0.0.1:19101".into());
        config.daemon.fileprovider_socket = Some(dir.path().join("tcfsd-fileprovider.sock"));

        let s3 = tcfs_secrets::S3Credentials {
            access_key_id: "access-key".into(),
            secret_access_key: secrecy::SecretString::from("secret-key".to_string()),
            endpoint: config.storage.endpoint.clone(),
            region: config.storage.region.clone(),
        };
        let master_key_path = dir.path().join("master.key");
        let rendered = build_fileprovider_init_config(&config, &s3, &master_key_path, "device-1");

        assert_eq!(rendered.s3_endpoint, "https://s3.example.test");
        assert!(!rendered.allow_insecure_http);
        assert_eq!(rendered.s3_bucket, "tcfs-smoke");
        assert_eq!(rendered.s3_access, "access-key");
        assert_eq!(rendered.s3_secret, "secret-key");
        assert_eq!(rendered.remote_prefix, "devices/neo");
        assert_eq!(rendered.device_id, "device-1");
        assert_eq!(
            rendered.daemon_endpoint.as_deref(),
            Some("http://127.0.0.1:19101")
        );
        assert_eq!(
            rendered.daemon_socket.as_deref(),
            Some(dir.path().join("tcfsd-fileprovider.sock").to_str().unwrap())
        );
        assert_eq!(rendered.master_key_file, master_key_path.to_string_lossy());

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["s3_secret"], "secret-key");
        assert_eq!(json["allow_insecure_http"], false);
        assert_eq!(
            json["master_key_file"].as_str(),
            Some(master_key_path.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn build_fileprovider_init_config_carries_explicit_http_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.storage.endpoint = "http://localhost:8333".into();
        config.storage.enforce_tls = false;
        let s3 = tcfs_secrets::S3Credentials {
            access_key_id: "access-key".into(),
            secret_access_key: secrecy::SecretString::from("secret-key".to_string()),
            endpoint: config.storage.endpoint.clone(),
            region: config.storage.region.clone(),
        };

        let rendered = build_fileprovider_init_config(
            &config,
            &s3,
            &dir.path().join("master.key"),
            "device-1",
        );
        let json = serde_json::to_value(&rendered).unwrap();

        assert!(rendered.allow_insecure_http);
        assert_eq!(json["allow_insecure_http"], true);
    }

    #[test]
    fn build_fileprovider_init_config_omits_per_device_keys_when_disabled() {
        // Default-off (wrap_mode = Master) must stay byte-identical to the legacy
        // master-only bootstrap: the per-device keys are absent from the JSON.
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        assert_eq!(config.crypto.wrap_mode, tcfs_core::config::WrapMode::Master);
        // Even with a registry path configured, Master => not emitted.
        config.sync.device_identity = Some(dir.path().join("devices.json"));

        let s3 = tcfs_secrets::S3Credentials {
            access_key_id: "access-key".into(),
            secret_access_key: secrecy::SecretString::from("secret-key".to_string()),
            endpoint: config.storage.endpoint.clone(),
            region: config.storage.region.clone(),
        };
        let master_key_path = dir.path().join("master.key");
        let rendered = build_fileprovider_init_config(&config, &s3, &master_key_path, "device-1");

        assert_eq!(rendered.wrap_mode, tcfs_core::config::WrapMode::Master);
        assert_eq!(rendered.device_registry_path, None);

        let json = serde_json::to_value(&rendered).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            !obj.contains_key("wrap_mode"),
            "wrap_mode must be omitted when Master (byte-identical default)"
        );
        assert!(
            !obj.contains_key("per_device_wrapping"),
            "legacy per_device_wrapping key must never be emitted"
        );
        assert!(
            !obj.contains_key("device_registry_path"),
            "device_registry_path must be omitted when wrapping is off"
        );
    }

    #[test]
    fn build_fileprovider_init_config_emits_per_device_keys_when_enabled() {
        // When wrap_mode is non-Master, the rendered FileProvider config must
        // carry the keys the read path consumes: `wrap_mode` and
        // `device_registry_path` (the configured registry).
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("devices.json");
        let mut config = test_config(dir.path());
        config.crypto.wrap_mode = tcfs_core::config::WrapMode::PerDevice;
        config.sync.device_identity = Some(registry_path.clone());

        let s3 = tcfs_secrets::S3Credentials {
            access_key_id: "access-key".into(),
            secret_access_key: secrecy::SecretString::from("secret-key".to_string()),
            endpoint: config.storage.endpoint.clone(),
            region: config.storage.region.clone(),
        };
        let master_key_path = dir.path().join("master.key");
        let rendered = build_fileprovider_init_config(&config, &s3, &master_key_path, "device-1");

        assert_eq!(rendered.wrap_mode, tcfs_core::config::WrapMode::PerDevice);
        assert_eq!(
            rendered.device_registry_path.as_deref(),
            Some(registry_path.to_string_lossy().as_ref())
        );

        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(
            json["wrap_mode"],
            serde_json::Value::String("per_device".to_string())
        );
        assert_eq!(
            json["device_registry_path"].as_str(),
            Some(registry_path.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn build_fileprovider_init_config_emits_dual_wrap_mode() {
        // Dual must also surface the registry path and emit wrap_mode = "dual".
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("devices.json");
        let mut config = test_config(dir.path());
        config.crypto.wrap_mode = tcfs_core::config::WrapMode::Dual;
        config.sync.device_identity = Some(registry_path.clone());

        let s3 = tcfs_secrets::S3Credentials {
            access_key_id: "access-key".into(),
            secret_access_key: secrecy::SecretString::from("secret-key".to_string()),
            endpoint: config.storage.endpoint.clone(),
            region: config.storage.region.clone(),
        };
        let master_key_path = dir.path().join("master.key");
        let rendered = build_fileprovider_init_config(&config, &s3, &master_key_path, "device-1");

        assert_eq!(rendered.wrap_mode, tcfs_core::config::WrapMode::Dual);
        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(
            json["wrap_mode"],
            serde_json::Value::String("dual".to_string())
        );
    }

    #[test]
    fn build_fileprovider_init_config_falls_back_to_default_registry_when_enabled() {
        // With wrapping on but no explicit registry, fall back to the shared
        // default registry path (matching the extension's own fallback).
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.crypto.wrap_mode = tcfs_core::config::WrapMode::PerDevice;
        config.sync.device_identity = None;

        let s3 = tcfs_secrets::S3Credentials {
            access_key_id: "access-key".into(),
            secret_access_key: secrecy::SecretString::from("secret-key".to_string()),
            endpoint: config.storage.endpoint.clone(),
            region: config.storage.region.clone(),
        };
        let master_key_path = dir.path().join("master.key");
        let rendered = build_fileprovider_init_config(&config, &s3, &master_key_path, "device-1");

        assert_eq!(rendered.wrap_mode, tcfs_core::config::WrapMode::PerDevice);
        assert_eq!(
            rendered.device_registry_path,
            Some(
                tcfs_secrets::device::default_registry_path()
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    #[test]
    fn resolve_fileprovider_device_id_prefers_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let resolved = resolve_fileprovider_device_id(&config, Some(" device-from-ci ")).unwrap();

        assert_eq!(resolved, "device-from-ci");
    }

    #[test]
    fn resolve_fileprovider_device_id_reads_registry_for_configured_device() {
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("devices.json");
        let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path).unwrap();
        let (device_id, _device_key) = registry.enroll_local("macbook", None);
        registry.save(&registry_path).unwrap();

        let mut config = test_config(dir.path());
        config.sync.device_identity = Some(registry_path);
        config.sync.device_name = Some("macbook".into());

        let resolved = resolve_fileprovider_device_id(&config, None).unwrap();

        assert_eq!(resolved, device_id);
    }

    #[test]
    fn resolve_fileprovider_device_id_falls_back_to_device_name_for_packaged_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.sync.device_name = Some("gha-macos-postinstall".into());

        let resolved = resolve_fileprovider_device_id(&config, None).unwrap();

        assert_eq!(resolved, "gha-macos-postinstall");
    }

    #[test]
    fn resolve_fileprovider_master_key_path_prefers_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let explicit = dir.path().join("explicit-master.key");

        let resolved = resolve_fileprovider_master_key_path(&config, Some(&explicit)).unwrap();

        assert_eq!(resolved, explicit);
    }

    #[test]
    fn write_fileprovider_config_file_refuses_existing_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("fileprovider/config.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "{}").unwrap();

        let err = write_fileprovider_config_file(&config_path, "{\"ok\":true}", false).unwrap_err();
        assert!(err
            .to_string()
            .contains("FileProvider config already exists"));

        write_fileprovider_config_file(&config_path, "{\"ok\":true}", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "{\"ok\":true}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_init_config_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let config = tcfs_core::config::TcfsConfig::default();

        write_init_config(&config_path, &config, false).unwrap();

        let mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn write_fileprovider_config_file_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("fileprovider/config.json");

        write_fileprovider_config_file(&config_path, "{\"ok\":true}", false).unwrap();

        let mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn session_token_interceptor_attaches_bearer_metadata() {
        let mut interceptor = SessionTokenInterceptor {
            token: Some("session-token-123".into()),
        };

        let request = interceptor.call(tonic::Request::new(())).unwrap();

        assert_eq!(
            request
                .metadata()
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer session-token-123"
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_token_interceptor_skips_missing_token() {
        let mut interceptor = SessionTokenInterceptor { token: None };

        let request = interceptor.call(tonic::Request::new(())).unwrap();

        assert!(request.metadata().get("authorization").is_none());
    }

    #[test]
    fn init_check_accepts_real_device_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let master_key_path = dir.path().join("master.key");
        let registry_path = dir.path().join("devices.json");
        std::fs::write(&config_path, "config = true\n").unwrap();
        std::fs::write(&master_key_path, [7u8; tcfs_crypto::KEY_SIZE]).unwrap();

        let mut registry = tcfs_secrets::device::DeviceRegistry::default();
        let (device_id, key) = registry.enroll_local("laptop", None);
        registry.save(&registry_path).unwrap();
        let key_path = tcfs_secrets::device::device_secret_key_path(&registry_path, &device_id);
        tcfs_secrets::device::save_device_secret_key(&key_path, &key.secret_key, false).unwrap();

        let paths = InitPaths {
            config_dir: dir.path().to_path_buf(),
            config_path,
            master_key_path,
            registry_path,
        };
        cmd_init_check(&paths).unwrap();
    }

    #[test]
    fn init_check_rejects_placeholder_device_keys() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let master_key_path = dir.path().join("master.key");
        let registry_path = dir.path().join("devices.json");
        std::fs::write(&config_path, "config = true\n").unwrap();
        std::fs::write(&master_key_path, [7u8; tcfs_crypto::KEY_SIZE]).unwrap();

        let mut registry = tcfs_secrets::device::DeviceRegistry::default();
        registry.enroll("legacy", "age1-device-deadbeef", None);
        registry.save(&registry_path).unwrap();

        let paths = InitPaths {
            config_dir: dir.path().to_path_buf(),
            config_path,
            master_key_path,
            registry_path,
        };
        let err = cmd_init_check(&paths).unwrap_err();
        assert!(
            err.to_string().contains("placeholder public key"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn repair_placeholder_device_key_preserves_device_id_and_writes_secret() {
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("devices.json");
        let mut registry = tcfs_secrets::device::DeviceRegistry::default();
        let device_id = registry.enroll("honey", "age1-device-6b746182", None);

        let key_path =
            repair_placeholder_device_key(&mut registry, &registry_path, "honey").unwrap();
        let repaired = registry.find("honey").unwrap();

        assert_eq!(repaired.device_id, device_id);
        assert!(tcfs_secrets::device::is_real_age_public_key(
            &repaired.public_key
        ));
        assert_eq!(
            key_path,
            tcfs_secrets::device::device_secret_key_path(&registry_path, &device_id)
        );
        assert!(key_path.exists());
    }

    #[test]
    fn merge_device_registry_prefers_real_key_over_placeholder() {
        let mut local = tcfs_secrets::device::DeviceRegistry::default();
        let device_id = local.enroll("honey", "age1-device-6b746182", None);
        let mut incoming = tcfs_secrets::device::DeviceRegistry::default();
        let (_incoming_id, key) = incoming.enroll_local("honey", None);
        incoming.devices[0].device_id = device_id.clone();

        let changed = merge_device_registry(&mut local, &incoming).unwrap();

        assert_eq!(changed, 1);
        let merged = local.find("honey").unwrap();
        assert_eq!(merged.device_id, device_id);
        assert_eq!(merged.public_key, key.public_key);
        assert!(tcfs_secrets::device::is_real_age_public_key(
            &merged.public_key
        ));
    }

    // ── TIN-1417 B4: unsigned-remote LAUNDERING bypass is BLOCKED ──────────────

    /// End-to-end attack reproduction: an attacker with object-store write access
    /// strips the signature off the remote `devices.json` and injects a hostile
    /// recipient. A normal master-holder running `tcfs device enroll --sync-remote`
    /// must REFUSE to merge it (so the injected recipient is never re-signed into a
    /// validly-signed registry), unless `--accept-unsigned-remote` is passed.
    #[tokio::test]
    async fn unsigned_remote_with_injected_recipient_is_refused_not_laundered() {
        let op = memory_op();
        let meta_prefix = "data";
        let master = master_key(0x42);

        // 1. Honest fleet publishes a SIGNED remote registry.
        let mut honest = tcfs_secrets::device::DeviceRegistry::default();
        honest.enroll(
            "alpha",
            &tcfs_secrets::device::generate_local_device_key().public_key,
            None,
        );
        honest
            .sync_to_remote_signed(&op, meta_prefix, master.as_bytes())
            .await
            .unwrap();

        // 2. Attacker rewrites the remote: injects a hostile recipient AND strips
        //    the signature envelope so it reads as UnsignedLegacy (not "tampered").
        let key = format!("{meta_prefix}/tcfs-meta/devices.json");
        let raw = op.read(&key).await.unwrap().to_bytes().to_vec();
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let attacker_pubkey = tcfs_secrets::device::generate_local_device_key().public_key;
        value["devices"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "name": "attacker",
                "device_id": "attacker-id",
                "public_key": attacker_pubkey,
                "enrolled_at": 1,
                "revoked": false
            }));
        let obj = value.as_object_mut().unwrap();
        obj.remove("registry_signature");
        obj.remove("signer_pubkey");
        obj.remove("sig_alg");
        op.write(&key, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();

        // 3. Master-holder loads + verifies the remote: it is UnsignedLegacy
        //    (signature stripped), NOT a hard tamper error.
        let (remote, trust) = tcfs_secrets::device::DeviceRegistry::load_remote_verified(
            &op,
            meta_prefix,
            master.as_bytes(),
        )
        .await
        .unwrap();
        assert_eq!(trust, tcfs_secrets::device::RegistryTrust::UnsignedLegacy);
        assert!(
            remote.devices.iter().any(|d| d.name == "attacker"),
            "sanity: the stripped remote really does carry the injected recipient"
        );

        // 4. The merge-path trust gate must REFUSE it (no laundering).
        let err = enforce_remote_merge_trust(trust.clone(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("UNSIGNED") && msg.contains("launder"),
            "unsigned remote must be refused on the merge path: {msg}"
        );

        // 5. Prove the laundering would have happened WITHOUT the gate: if we had
        //    merged + re-signed, the attacker's recipient would be in a validly
        //    signed registry. The gate is what prevents that, so we never merge.
        let mut local = tcfs_secrets::device::DeviceRegistry::default();
        // Gate refuses -> local stays clean; we never call merge.
        assert!(
            !local.devices.iter().any(|d| d.name == "attacker"),
            "local registry must NOT contain the laundered recipient"
        );
        // Demonstrate the counterfactual is real (defense-in-depth assertion):
        merge_device_registry(&mut local, &remote).unwrap();
        local.sign(master.as_bytes()).unwrap();
        assert!(
            local.find("attacker").is_some()
                && local.verify_signature(master.as_bytes()).unwrap()
                    == tcfs_secrets::device::RegistryTrust::Signed,
            "counterfactual: merging an unsigned remote then re-signing DOES launder \
             the recipient — which is exactly why the merge-path gate must refuse it"
        );
    }

    /// The explicit operator escape hatch (`--accept-unsigned-remote`) allows the
    /// unsigned remote through, for genuine one-time legacy migration.
    #[test]
    fn accept_unsigned_remote_flag_allows_merge() {
        enforce_remote_merge_trust(tcfs_secrets::device::RegistryTrust::UnsignedLegacy, true)
            .expect("--accept-unsigned-remote must allow an unsigned remote through");
    }

    /// A signed remote always passes the gate regardless of the flag.
    #[test]
    fn signed_remote_passes_merge_gate() {
        enforce_remote_merge_trust(tcfs_secrets::device::RegistryTrust::Signed, false)
            .expect("a signed remote must pass the merge gate");
    }

    #[test]
    fn merge_device_registry_rejects_conflicting_real_keys_for_same_device_id() {
        let mut local = tcfs_secrets::device::DeviceRegistry::default();
        let (device_id, _local_key) = local.enroll_local("honey", None);
        let mut incoming = tcfs_secrets::device::DeviceRegistry::default();
        incoming.enroll_local("honey", None);
        incoming.devices[0].device_id = device_id;

        let err = merge_device_registry(&mut local, &incoming).unwrap_err();

        assert!(
            err.to_string().contains("two real public keys differ"),
            "unexpected error: {err:#}"
        );
    }

    fn make_encrypted_manifest(
        old_master: &tcfs_crypto::MasterKey,
        manifest_hash: &str,
        rel_path: &str,
    ) -> tcfs_sync::manifest::SyncManifest {
        let file_key = tcfs_crypto::generate_file_key();
        let wrapped = tcfs_crypto::wrap_key(old_master, &file_key).unwrap();
        tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: manifest_hash.to_string(),
            file_size: 11,
            chunks: vec![],
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "test-device".into(),
            written_at: 0,
            rel_path: Some(rel_path.to_string()),
            mode: None,
            mtime: None,
            encrypted_file_key: Some(base64::engine::general_purpose::STANDARD.encode(wrapped)),
            wrapped_file_keys: Vec::new(),
        }
    }

    async fn read_manifest(op: &Operator, path: &str) -> tcfs_sync::manifest::SyncManifest {
        let data = op.read(path).await.unwrap().to_bytes();
        tcfs_sync::manifest::SyncManifest::from_bytes(&data).unwrap()
    }

    fn manifest_uses_key(
        manifest: &tcfs_sync::manifest::SyncManifest,
        master_key: &tcfs_crypto::MasterKey,
    ) -> bool {
        let wrapped_b64 = manifest.encrypted_file_key.as_ref().unwrap();
        let wrapped = base64::engine::general_purpose::STANDARD
            .decode(wrapped_b64)
            .unwrap();
        tcfs_crypto::unwrap_key(master_key, &wrapped).is_ok()
    }

    fn plan_with_actions(
        actions: Vec<tcfs_sync::reconcile::ReconcileAction>,
    ) -> tcfs_sync::reconcile::ReconcilePlan {
        tcfs_sync::reconcile::ReconcilePlan {
            actions,
            summary: tcfs_sync::reconcile::ReconcileSummary::default(),
            device_id: "test-device".into(),
            generated_at: 0,
        }
    }

    #[test]
    fn reconcile_cleanup_skips_pull_only_plans() {
        let plan = plan_with_actions(vec![tcfs_sync::reconcile::ReconcileAction::Pull {
            rel_path: "doc.txt".into(),
            manifest_hash: "hash".into(),
            size: 12,
            chunks: 1,
            reason: tcfs_sync::reconcile::PullReason::NewRemote,
            expected_kind: tcfs_sync::index_entry::RemoteEntryKind::RegularFile,
            expected_symlink_target: None,
        }]);

        assert!(!plan_may_orphan_remote_chunks(&plan));
    }

    #[test]
    fn reconcile_cleanup_runs_for_remote_overwrite_or_delete() {
        let overwrite = plan_with_actions(vec![tcfs_sync::reconcile::ReconcileAction::Push {
            local_path: PathBuf::from("doc.txt"),
            rel_path: "doc.txt".into(),
            reason: tcfs_sync::reconcile::PushReason::LocalNewer,
        }]);
        let delete = plan_with_actions(vec![tcfs_sync::reconcile::ReconcileAction::DeleteRemote {
            rel_path: "old.txt".into(),
        }]);

        assert!(plan_may_orphan_remote_chunks(&overwrite));
        assert!(plan_may_orphan_remote_chunks(&delete));
    }

    #[test]
    fn collect_config_from_sync_enables_symlink_preservation() {
        let mut config = tcfs_core::config::TcfsConfig::default();
        config.sync.sync_git_dirs = true;
        config.sync.git_sync_mode = "raw".into();
        config.sync.sync_hidden_dirs = true;
        config.sync.sync_symlinks = true;
        config.sync.sync_empty_dirs = true;

        let collect = collect_config_from_sync(&config);

        assert!(collect.sync_git_dirs);
        assert_eq!(collect.git_sync_mode, "raw");
        assert!(collect.sync_hidden_dirs);
        assert!(!collect.follow_symlinks);
        assert!(collect.preserve_symlinks);
        assert!(collect.sync_empty_dirs);
    }

    #[tokio::test]
    async fn load_config_reads_canary_sync_symlink_setting() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("tcfs-canary.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
[daemon]
socket = "{socket}"

[storage]
endpoint = "http://localhost:8333"
bucket = "tcfs"
remote_prefix = "git-repo-canary"
enforce_tls = false

[sync]
state_db = "{state_db}"
sync_root = "{sync_root}"
nats_url = "nats://localhost:4222"
nats_tls = false
sync_git_dirs = true
git_sync_mode = "raw"
sync_hidden_dirs = true
sync_symlinks = true
sync_empty_dirs = true

[crypto]
enabled = false
"#,
                socket = dir.path().join("no-daemon.sock").display(),
                state_db = dir.path().join("state.db").display(),
                sync_root = dir.path().join("shadow").display(),
            ),
        )
        .unwrap();

        let config = load_config(&config_path).await.unwrap();
        let collect = collect_config_from_sync(&config);

        assert!(config.sync.sync_symlinks);
        assert!(collect.preserve_symlinks);
        assert!(!collect.follow_symlinks);
    }

    #[tokio::test]
    async fn cli_push_status_pull_workflow_round_trips_file() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        std::fs::create_dir_all(sync_root.join("docs")).unwrap();
        let source = sync_root.join("docs/readme.txt");
        std::fs::write(&source, b"hello from tcfs").unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let config = test_config(&sync_root);

        cmd_push_with_operator(&config, &op, &source, None, &state_path, "test-device")
            .await
            .unwrap();

        let report = build_sync_status_report(&config, Some(&source), Some(&state_path)).unwrap();
        assert_eq!(report.tracked_files, 1);
        match report.file.unwrap() {
            SyncStatusPathReport::Tracked {
                remote_path,
                sync_status,
                needs_sync_reason,
                ..
            } => {
                assert!(remote_path.starts_with("data/manifests/"));
                assert_eq!(sync_status, tcfs_sync::state::FileSyncStatus::Synced);
                assert!(needs_sync_reason.is_none());
            }
            other => panic!("expected tracked status, got {other:?}"),
        }

        let pulled = dir.path().join("pulled.txt");
        cmd_pull_with_operator(
            &config,
            &op,
            &source.to_string_lossy(),
            Some(&pulled),
            None,
            &state_path,
            "test-device",
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&pulled).unwrap(), b"hello from tcfs");
    }

    #[tokio::test]
    async fn direct_manifest_fixed_rel_path_is_rejected_before_destination_or_cache_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        std::fs::create_dir_all(sync_root.join("docs")).unwrap();
        let source = sync_root.join("docs/readme.txt");
        std::fs::write(&source, b"direct manifest payload").unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let config = test_config(&sync_root);
        cmd_push_with_operator(&config, &op, &source, None, &state_path, "test-device")
            .await
            .unwrap();

        let remote_manifest = tcfs_sync::state::StateCache::open(&state_path)
            .unwrap()
            .get(&source)
            .unwrap()
            .remote_path
            .clone();
        let mut manifest = tcfs_sync::manifest::SyncManifest::from_bytes(
            &op.read(&remote_manifest).await.unwrap().to_vec(),
        )
        .unwrap();
        manifest.rel_path = Some(".env".into());
        op.write(&remote_manifest, manifest.to_bytes().unwrap())
            .await
            .unwrap();

        let destination = dir.path().join("safe-destination.txt");
        std::fs::write(&destination, b"keep-local-bytes").unwrap();
        let error = cmd_pull_with_operator(
            &config,
            &op,
            &remote_manifest,
            Some(&destination),
            None,
            &state_path,
            "test-device",
        )
        .await
        .expect_err("parsed direct-manifest rel_path must pass the fixed ingress boundary");

        let error_chain = format!("{error:#}");
        assert!(
            error_chain.contains("manifest-bound fixed-deny ingress path"),
            "{error_chain}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep-local-bytes");
        assert!(
            tcfs_sync::state::StateCache::open(&state_path)
                .unwrap()
                .get(&destination)
                .is_none(),
            "rejected direct manifest must not create destination cache state"
        );
        assert!(
            !dir.path().join(".env").exists(),
            "manifest rel_path must never select an alternate local destination"
        );
    }

    #[tokio::test]
    async fn cli_pull_by_file_path_without_dest_writes_to_file_path() {
        // Regression: `tcfs pull <file-path>` with no explicit destination must
        // write back to that file path, not to a hash-named file in the cwd.
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("sync");
        std::fs::create_dir_all(sync_root.join("docs")).unwrap();
        let source = sync_root.join("docs/readme.txt");
        std::fs::write(&source, b"hello from tcfs").unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let config = test_config(&sync_root);

        cmd_push_with_operator(&config, &op, &source, None, &state_path, "test-device")
            .await
            .unwrap();

        // Locally drift the file, then pull by file path with NO explicit dest.
        // (Keep the file present so manifest resolution by path still works.)
        std::fs::write(&source, b"locally drifted content").unwrap();
        cmd_pull_with_operator(
            &config,
            &op,
            &source.to_string_lossy(),
            None,
            None,
            &state_path,
            "test-device",
        )
        .await
        .unwrap();

        // The fix: pull wrote back to the file path (not a hash-named cwd file),
        // restoring the exact pushed bytes.
        assert_eq!(std::fs::read(&source).unwrap(), b"hello from tcfs");
    }

    #[tokio::test]
    async fn index_inspect_reports_missing_index_without_error() {
        let op = memory_op();

        let report = inspect_index_entry_with_operator(&op, "shared/alpha-test.txt", "data")
            .await
            .unwrap();

        assert_eq!(report.status, "missing_index");
        assert_eq!(report.index_key, "data/index/shared/alpha-test.txt");
        assert!(!report.index_exists);
        assert!(report.visible_entry.is_none());
    }

    #[tokio::test]
    async fn index_inspect_reports_visible_manifest() {
        let op = memory_op();
        op.write("data/manifests/hash123", b"{}".to_vec())
            .await
            .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            "data/index/shared/alpha-test.txt",
            &tcfs_sync::index_entry::RemoteIndexEntry::new("hash123", 46, 1),
        )
        .await
        .unwrap();

        let report = inspect_index_entry_with_operator(&op, "shared/alpha-test.txt", "data")
            .await
            .unwrap();

        assert_eq!(report.status, "visible");
        let visible = report.visible_entry.unwrap();
        assert_eq!(visible.manifest_hash, "hash123");
        assert_eq!(visible.manifest_key, "data/manifests/hash123");
        assert!(visible.manifest_exists);
        assert_eq!(visible.size, 46);
    }

    #[tokio::test]
    async fn index_inspect_rejects_pending_staged_manifest_outside_root() {
        let op = memory_op();
        let pending = tcfs_sync::index_entry::PendingIndexEntry::new(
            "hash123",
            46,
            1,
            "other/staging/manifests/550e8400-e29b-41d4-a716-446655440000-hash123.json",
        );
        tcfs_sync::index_entry::write_preparing_index_entry(
            &op,
            "data",
            "data/index/shared/alpha-test.txt",
            None,
            pending,
        )
        .await
        .unwrap();

        let err = inspect_index_entry_with_operator(&op, "shared/alpha-test.txt", "data")
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("escapes its root staging namespace"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn cache_evict_uses_remote_index_manifest_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let mut config = test_config(dir.path());
        config.fuse.cache_dir = cache_dir.clone();

        let op = memory_op();
        op.write("data/manifests/hash123", b"{}".to_vec())
            .await
            .unwrap();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            "data/index/shared/alpha-test.txt",
            &tcfs_sync::index_entry::RemoteIndexEntry::new("hash123", 46, 1),
        )
        .await
        .unwrap();

        let cache = tcfs_vfs::DiskCache::new(cache_dir, 1024 * 1024);
        cache.put("hash123", b"hydrated bytes").await.unwrap();
        assert!(cache.contains("hash123").await);

        let report = evict_cache_entry_with_operator(&config, &op, "shared/alpha-test.txt", "data")
            .await
            .unwrap();

        assert_eq!(report.rel_path, "shared/alpha-test.txt");
        assert_eq!(report.manifest_hash, "hash123");
        assert_eq!(report.bytes_freed, b"hydrated bytes".len() as u64);
        assert!(report.was_cached);
        assert!(!cache.contains("hash123").await);
    }

    #[tokio::test]
    async fn cache_evict_rejects_missing_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.fuse.cache_dir = dir.path().join("cache");
        let op = memory_op();

        let err = evict_cache_entry_with_operator(&config, &op, "missing.txt", "data")
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("remote index status is missing_index"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn storage_canary_key_is_scoped_under_prefix() {
        assert_eq!(
            storage_canary_key("data", "nonce"),
            "data/.tcfs-canary/nonce.txt"
        );
        assert_eq!(
            storage_canary_key("/tenant/a/", "nonce"),
            "tenant/a/.tcfs-canary/nonce.txt"
        );
        assert_eq!(storage_canary_key("", "nonce"), ".tcfs-canary/nonce.txt");
    }

    #[test]
    fn storage_canary_list_prefix_matches_daemon_health_scope() {
        assert_eq!(storage_canary_list_prefix("data"), "data/");
        assert_eq!(storage_canary_list_prefix("/tenant/a/"), "tenant/a/");
        assert_eq!(storage_canary_list_prefix(""), "/");
    }

    #[tokio::test]
    async fn storage_canary_writes_reads_deletes_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.storage.endpoint =
            "https://canary-user:CANARY-secret@storage.example.test:8333/CANARY-path?signature=CANARY-query#CANARY-fragment"
                .into();
        let op = memory_op();

        let report = run_storage_canary_with_operator(
            &config,
            &op,
            "data",
            None,
            "test-nonce",
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(report.key, "data/.tcfs-canary/test-nonce.txt");
        assert_eq!(report.list_prefix, "data/");
        assert_eq!(report.endpoint, "https://storage.example.test:8333");
        for forbidden in [
            "canary-user",
            "CANARY-secret",
            "CANARY-path",
            "CANARY-query",
            "CANARY-fragment",
        ] {
            assert!(
                !report.endpoint.contains(forbidden),
                "canary report leaked {forbidden}: {}",
                report.endpoint
            );
        }
        assert!(report.listed);
        assert!(report.list_count >= 1);
        assert!(report.deleted);
        assert!(report.scope_deny.is_none());
        assert!(!op.exists(&report.key).await.unwrap());
    }

    #[tokio::test]
    async fn storage_canary_rejects_same_scope_deny_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let op = memory_op();

        let err = run_storage_canary_with_operator(
            &config,
            &op,
            "data",
            Some("/data/"),
            "test-nonce",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("same canary key"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn storage_canary_fails_when_deny_prefix_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());
        let op = memory_op();

        let err = run_storage_canary_with_operator(
            &config,
            &op,
            "data",
            Some("outside"),
            "test-nonce",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("unexpectedly succeeded"),
            "unexpected error: {err}"
        );
        assert!(!op
            .exists("outside/.tcfs-canary/test-nonce.txt")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn index_inspect_reports_missing_manifest() {
        let op = memory_op();
        tcfs_sync::index_entry::write_committed_index_entry(
            &op,
            "data",
            "data/index/shared/alpha-test.txt",
            &tcfs_sync::index_entry::RemoteIndexEntry::new("missing", 46, 1),
        )
        .await
        .unwrap();

        let report = inspect_index_entry_with_operator(&op, "shared/alpha-test.txt", "data")
            .await
            .unwrap();

        assert_eq!(report.status, "missing_manifest");
        assert!(!report.visible_entry.unwrap().manifest_exists);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_directory_push_preserves_symlink_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        std::fs::write(sync_root.join("target.txt"), b"target").unwrap();
        std::os::unix::fs::symlink("target.txt", sync_root.join("link.txt")).unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let mut config = test_config(&sync_root);
        config.sync.sync_symlinks = true;

        cmd_push_with_operator(&config, &op, &sync_root, None, &state_path, "test-device")
            .await
            .unwrap();

        let index_bytes = op.read("data/index/link.txt").await.unwrap().to_bytes();
        let entry = tcfs_sync::index_entry::parse_index_entry(&index_bytes).unwrap();
        assert!(entry.is_symlink());
        assert_eq!(entry.symlink_target.as_deref(), Some("target.txt"));
    }

    #[tokio::test]
    async fn cli_directory_push_and_status_detect_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(sync_root.join("sub")).unwrap();
        let first = sync_root.join("alpha.txt");
        let second = sync_root.join("sub/beta.txt");
        std::fs::write(&first, b"alpha").unwrap();
        std::fs::write(&second, b"beta").unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let config = test_config(&sync_root);

        cmd_push_with_operator(&config, &op, &sync_root, None, &state_path, "test-device")
            .await
            .unwrap();

        assert!(op.read("data/index/alpha.txt").await.is_ok());
        assert!(op.read("data/index/sub/beta.txt").await.is_ok());

        std::fs::write(&first, b"alpha updated").unwrap();

        let report = build_sync_status_report(&config, Some(&first), Some(&state_path)).unwrap();
        assert_eq!(report.tracked_files, 2);
        match report.file.unwrap() {
            SyncStatusPathReport::Tracked {
                sync_status,
                needs_sync_reason,
                ..
            } => {
                assert_eq!(sync_status, tcfs_sync::state::FileSyncStatus::Synced);
                assert!(needs_sync_reason.is_some());
            }
            other => panic!("expected tracked status, got {other:?}"),
        }
    }

    #[test]
    fn cli_sync_status_reports_explicit_sync_state() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let tracked = sync_root.join("alpha.txt");
        std::fs::write(&tracked, b"alpha").unwrap();

        let state_path = dir.path().join("state.json");
        let config = test_config(&sync_root);
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        let mut entry = tcfs_sync::state::make_sync_state(
            &tracked,
            "abc123".to_string(),
            1,
            "data/manifests/abc123".to_string(),
        )
        .unwrap();
        entry.status = tcfs_sync::state::FileSyncStatus::NotSynced;
        state.set(&tracked, entry);
        state.flush().unwrap();

        let report = build_sync_status_report(&config, Some(&tracked), Some(&state_path)).unwrap();
        match report.file.unwrap() {
            SyncStatusPathReport::Tracked {
                sync_status,
                needs_sync_reason,
                ..
            } => {
                assert_eq!(sync_status, tcfs_sync::state::FileSyncStatus::NotSynced);
                assert!(needs_sync_reason.is_none());
            }
            other => panic!("expected tracked status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cli_unsync_marks_not_synced_and_reports_via_real_and_stub_paths() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let tracked = sync_root.join("alpha.txt");
        std::fs::write(&tracked, b"alpha").unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let mut config = test_config(&sync_root);
        config.sync.state_db = dir.path().join("state.db");

        cmd_push_with_operator(&config, &op, &tracked, None, &state_path, "test-device")
            .await
            .unwrap();

        cmd_unsync(&config, &tracked, false).await.unwrap();

        let stub_path = sync_root.join("alpha.txt.tc");
        let canonical_tracked = std::fs::canonicalize(&sync_root).unwrap().join("alpha.txt");
        assert!(
            !tracked.exists(),
            "hydrated file should be removed after unsync"
        );
        assert!(stub_path.exists(), "stub should be created after unsync");

        let state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        let entry = state
            .get(&tracked)
            .expect("tracked state should be preserved");
        assert_eq!(entry.status, tcfs_sync::state::FileSyncStatus::NotSynced);

        for lookup in [&tracked, &stub_path] {
            let report =
                build_sync_status_report(&config, Some(lookup), Some(&state_path)).unwrap();
            match report.file.unwrap() {
                SyncStatusPathReport::Tracked {
                    canonical,
                    sync_status,
                    needs_sync_reason,
                    ..
                } => {
                    assert_eq!(canonical, canonical_tracked);
                    assert_eq!(sync_status, tcfs_sync::state::FileSyncStatus::NotSynced);
                    assert!(needs_sync_reason.is_none());
                }
                other => panic!("expected tracked status after unsync, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn cli_pull_after_unsync_hydrates_latest_remote_and_removes_stub() {
        let dir = tempfile::tempdir().unwrap();
        let neo_root = dir.path().join("neo");
        let honey_root = dir.path().join("honey");
        std::fs::create_dir_all(&neo_root).unwrap();
        std::fs::create_dir_all(&honey_root).unwrap();
        let neo_file = neo_root.join("shared.txt");
        let honey_file = honey_root.join("shared.txt");
        std::fs::write(&neo_file, b"version from neo").unwrap();

        let op = memory_op();
        let neo_state = dir.path().join("neo-state.json");
        let honey_state = dir.path().join("honey-state.json");
        let mut neo_config = test_config(&neo_root);
        neo_config.sync.state_db = dir.path().join("neo-state.db");
        let mut honey_config = test_config(&honey_root);
        honey_config.sync.state_db = dir.path().join("honey-state.db");

        cmd_push_with_operator(&neo_config, &op, &neo_file, None, &neo_state, "neo-device")
            .await
            .unwrap();

        cmd_pull_with_operator(
            &honey_config,
            &op,
            "shared.txt",
            Some(&honey_file),
            Some("data"),
            &honey_state,
            "honey-device",
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&honey_file).unwrap(), b"version from neo");

        cmd_unsync(&neo_config, &neo_file, false).await.unwrap();
        let stub_path = neo_root.join("shared.txt.tc");
        assert!(
            !neo_file.exists(),
            "neo file should be removed after unsync"
        );
        assert!(stub_path.exists(), "neo stub should exist after unsync");

        std::fs::write(&honey_file, b"version from honey after neo unsynced").unwrap();
        cmd_push_with_operator(
            &honey_config,
            &op,
            &honey_file,
            None,
            &honey_state,
            "honey-device",
        )
        .await
        .unwrap();

        cmd_pull_with_operator(
            &neo_config,
            &op,
            "shared.txt",
            Some(&neo_file),
            Some("data"),
            &neo_state,
            "neo-device",
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(&neo_file).unwrap(),
            b"version from honey after neo unsynced"
        );
        assert!(
            !stub_path.exists(),
            "rehydrating a clean path should remove the adjacent stub"
        );

        let state = tcfs_sync::state::StateCache::open(&neo_state).unwrap();
        let entry = state.get(&neo_file).expect("neo state after rehydrate");
        assert_eq!(entry.status, tcfs_sync::state::FileSyncStatus::Synced);
    }

    #[tokio::test]
    async fn cli_pull_after_peer_delete_recreate_over_unsynced_stub_uses_recreated_remote() {
        let dir = tempfile::tempdir().unwrap();
        let neo_root = dir.path().join("neo");
        let honey_root = dir.path().join("honey");
        std::fs::create_dir_all(&neo_root).unwrap();
        std::fs::create_dir_all(&honey_root).unwrap();
        let neo_file = neo_root.join("shared.txt");
        let honey_file = honey_root.join("shared.txt");
        std::fs::write(&neo_file, b"version from neo").unwrap();

        let op = memory_op();
        let neo_state = dir.path().join("neo-state.json");
        let honey_state = dir.path().join("honey-state.json");
        let mut neo_config = test_config(&neo_root);
        neo_config.sync.state_db = dir.path().join("neo-state.db");
        let mut honey_config = test_config(&honey_root);
        honey_config.sync.state_db = dir.path().join("honey-state.db");

        cmd_push_with_operator(&neo_config, &op, &neo_file, None, &neo_state, "neo-device")
            .await
            .unwrap();
        cmd_pull_with_operator(
            &honey_config,
            &op,
            "shared.txt",
            Some(&honey_file),
            Some("data"),
            &honey_state,
            "honey-device",
        )
        .await
        .unwrap();

        cmd_unsync(&neo_config, &neo_file, false).await.unwrap();
        let stub_path = neo_root.join("shared.txt.tc");
        assert!(stub_path.exists(), "neo should keep only a physical stub");
        assert!(!neo_file.exists(), "neo hydrated file should be removed");

        let mut delete_state = tcfs_sync::state::StateCache::open(&honey_state).unwrap();
        tcfs_sync::engine::delete_remote_file(
            &op,
            "shared.txt",
            "data",
            &mut delete_state,
            Some(&honey_root),
        )
        .await
        .unwrap();

        cmd_pull_with_operator(
            &neo_config,
            &op,
            "shared.txt",
            Some(&neo_file),
            Some("data"),
            &neo_state,
            "neo-device",
        )
        .await
        .unwrap_err();
        assert!(
            stub_path.exists(),
            "remote delete should not remove local stub"
        );
        assert!(
            !neo_file.exists(),
            "failed pull should not hydrate local file"
        );

        std::fs::write(&honey_file, b"recreated after delete").unwrap();
        cmd_push_with_operator(
            &honey_config,
            &op,
            &honey_file,
            None,
            &honey_state,
            "honey-device",
        )
        .await
        .unwrap();

        cmd_pull_with_operator(
            &neo_config,
            &op,
            "shared.txt",
            Some(&neo_file),
            Some("data"),
            &neo_state,
            "neo-device",
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&neo_file).unwrap(), b"recreated after delete");
        assert!(
            !stub_path.exists(),
            "rehydrating recreated remote path should remove the adjacent stub"
        );
    }

    #[tokio::test]
    async fn cli_pull_adjacent_stub_cleanup_ignores_non_tcfs_files() {
        let dir = tempfile::tempdir().unwrap();
        let pulled = dir.path().join("notes.md");
        let adjacent = dir.path().join("notes.md.tc");
        std::fs::write(&pulled, b"hydrated bytes").unwrap();
        std::fs::write(&adjacent, b"user-owned sidecar, not a TCFS stub").unwrap();

        remove_adjacent_stub_after_pull(&pulled).await.unwrap();

        assert_eq!(
            std::fs::read(&adjacent).unwrap(),
            b"user-owned sidecar, not a TCFS stub"
        );

        let binary_pulled = dir.path().join("asset.bin");
        let binary_adjacent = dir.path().join("asset.bin.tc");
        std::fs::write(&binary_pulled, b"hydrated binary").unwrap();
        std::fs::write(&binary_adjacent, [0xff, 0x00, 0xfe, 0x01]).unwrap();

        remove_adjacent_stub_after_pull(&binary_pulled)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(&binary_adjacent).unwrap(),
            [0xff, 0x00, 0xfe, 0x01]
        );
    }

    #[tokio::test]
    async fn cli_unsync_force_uses_tracked_remote_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let tracked = sync_root.join("alpha.txt");
        std::fs::write(&tracked, b"alpha").unwrap();

        let op = memory_op();
        let state_path = dir.path().join("state.json");
        let mut config = test_config(&sync_root);
        config.sync.state_db = dir.path().join("state.db");

        cmd_push_with_operator(&config, &op, &tracked, None, &state_path, "test-device")
            .await
            .unwrap();

        let tracked_before = tcfs_sync::state::StateCache::open(&state_path)
            .unwrap()
            .get(&tracked)
            .cloned()
            .unwrap();

        std::fs::write(&tracked, b"alpha updated locally").unwrap();

        cmd_unsync(&config, &tracked, true).await.unwrap();

        let stub_path = sync_root.join("alpha.txt.tc");
        let stub =
            tcfs_vfs::StubMeta::parse(&std::fs::read_to_string(&stub_path).unwrap()).unwrap();
        assert_eq!(
            stub.blake3_hex(),
            Some(tracked_before.blake3.as_str()),
            "forced unsync should preserve tracked remote hash, not local dirty content"
        );
        assert_eq!(stub.size, tracked_before.size);
        assert_eq!(stub.chunks, tracked_before.chunk_count);
        assert!(
            stub.origin.ends_with("/alpha.txt"),
            "stub origin should point at the logical remote path"
        );
    }

    #[tokio::test]
    async fn cli_unsync_force_rejects_untracked_file() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(&sync_root).unwrap();
        let local = sync_root.join("never-pushed.txt");
        std::fs::write(&local, b"local only").unwrap();

        let mut config = test_config(&sync_root);
        config.sync.state_db = dir.path().join("state.db");

        let err = cmd_unsync(&config, &local, true).await.unwrap_err();
        assert!(
            err.to_string().contains("is not tracked"),
            "unexpected error: {err}"
        );
        assert!(local.exists(), "untracked file should be left in place");
        assert!(
            !sync_root.join("never-pushed.txt.tc").exists(),
            "force unsync must not create a fake stub for an untracked file"
        );
    }

    fn seed_tracked_file(
        state: &mut tcfs_sync::state::StateCache,
        file: &Path,
        remote_path: &str,
    ) -> tcfs_sync::state::SyncState {
        let data = std::fs::read(file).unwrap();
        let hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&data));
        let entry =
            tcfs_sync::state::make_sync_state(file, hash, 1, remote_path.to_string()).unwrap();
        state.set(file, entry.clone());
        entry
    }

    #[tokio::test]
    async fn rotation_freezes_indexed_root_before_manifest_mutation() {
        let op = memory_op();
        let manifest_path = "data/manifests/legacy-path";
        let original = b"legacy manifest bytes stay untouched".to_vec();
        op.write(manifest_path, original.clone()).await.unwrap();
        op.write(
            "data/index/docs/report.txt",
            b"manifest_hash=legacy-path\nsize=36\nchunks=0\n".to_vec(),
        )
        .await
        .unwrap();

        let err = legacy_manifest_mutation_permit(&op, "data", "scoped FileKey rotation", false)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("live path-index entries"),
            "unexpected freeze error: {message}"
        );
        assert!(
            message.contains("index-first copy-on-write"),
            "operator guidance must name the safe protocol: {message}"
        );
        assert_eq!(
            op.read(manifest_path).await.unwrap().to_vec(),
            original,
            "layout rejection must not touch manifest bytes"
        );

        let gc_err = legacy_manifest_mutation_permit(&op, "data", "scoped FileKey rotation", true)
            .await
            .unwrap_err();
        assert!(
            gc_err
                .to_string()
                .contains("--gc-immediate is disabled for indexed/multi-writer root"),
            "immediate GC needs a distinct fail-closed message: {gc_err}"
        );
    }

    #[tokio::test]
    async fn rotation_freezes_unindexed_byte_addressed_manifest() {
        let op = memory_op();
        let bytes = b"immutable manifest object".to_vec();
        let object_id = tcfs_sync::index_entry::manifest_object_id(&bytes);
        let path = format!("data/manifests/{object_id}");
        op.write(&path, bytes.clone()).await.unwrap();

        let err = legacy_manifest_mutation_permit(&op, "data", "master-key rotation", false)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("immutable byte-derived object ID"),
            "byte-address detection must not depend only on a surviving index: {message}"
        );
        assert_eq!(op.read(&path).await.unwrap().to_vec(), bytes);
    }

    #[tokio::test]
    async fn rotation_permit_accepts_legacy_manifest_only_layout() {
        let op = memory_op();
        op.write(
            "data/manifests/projects/legacy.txt",
            b"historical path-addressed manifest".to_vec(),
        )
        .await
        .unwrap();

        let permit = legacy_manifest_mutation_permit(&op, "data", "scoped FileKey rotation", false)
            .await
            .unwrap();
        permit
            .authorize_manifest_prefix("data/manifests/projects/")
            .unwrap();
        permit.authorize_remote_prefix("data").unwrap();
    }

    #[tokio::test]
    async fn cli_unsync_directory_converts_clean_tracked_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(sync_root.join("docs/deep")).unwrap();
        let alpha = sync_root.join("docs/alpha.txt");
        let beta = sync_root.join("docs/deep/beta.txt");
        let outside = sync_root.join("outside.txt");
        std::fs::write(&alpha, b"alpha").unwrap();
        std::fs::write(&beta, b"beta").unwrap();
        std::fs::write(&outside, b"outside").unwrap();

        let config = test_config(&sync_root);
        let state_path = resolve_state_path(&config, None);
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        seed_tracked_file(&mut state, &alpha, "data/index/docs/alpha.txt");
        seed_tracked_file(&mut state, &beta, "data/index/docs/deep/beta.txt");
        seed_tracked_file(&mut state, &outside, "data/index/outside.txt");
        state.flush().unwrap();

        cmd_unsync(&config, &sync_root.join("docs"), false)
            .await
            .unwrap();

        assert!(!alpha.exists(), "alpha should be dehydrated");
        assert!(!beta.exists(), "beta should be dehydrated");
        assert!(sync_root.join("docs/alpha.txt.tc").exists());
        assert!(sync_root.join("docs/deep/beta.txt.tc").exists());
        assert!(outside.exists(), "outside path is not a descendant");

        let state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert_eq!(
            state.get(&alpha).map(|entry| entry.status),
            Some(tcfs_sync::state::FileSyncStatus::NotSynced)
        );
        assert_eq!(
            state.get(&beta).map(|entry| entry.status),
            Some(tcfs_sync::state::FileSyncStatus::NotSynced)
        );
        assert_eq!(
            state.get(&outside).map(|entry| entry.status),
            Some(tcfs_sync::state::FileSyncStatus::Synced)
        );
    }

    #[tokio::test]
    async fn cli_unsync_directory_refuses_dirty_descendants_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(sync_root.join("docs")).unwrap();
        let clean = sync_root.join("docs/clean.txt");
        let dirty = sync_root.join("docs/dirty.txt");
        std::fs::write(&clean, b"clean").unwrap();
        std::fs::write(&dirty, b"before").unwrap();

        let config = test_config(&sync_root);
        let state_path = resolve_state_path(&config, None);
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        seed_tracked_file(&mut state, &clean, "data/index/docs/clean.txt");
        seed_tracked_file(&mut state, &dirty, "data/index/docs/dirty.txt");
        state.flush().unwrap();

        std::fs::write(&dirty, b"after local edit").unwrap();

        let err = cmd_unsync(&config, &sync_root.join("docs"), false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("dirty descendant"),
            "unexpected error: {err}"
        );
        assert!(clean.exists(), "clean file should not be converted");
        assert!(dirty.exists(), "dirty file should not be converted");
        assert!(!sync_root.join("docs/clean.txt.tc").exists());
        assert!(!sync_root.join("docs/dirty.txt.tc").exists());

        let state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert_eq!(
            state.get(&clean).map(|entry| entry.status),
            Some(tcfs_sync::state::FileSyncStatus::Synced)
        );
        assert_eq!(
            state.get(&dirty).map(|entry| entry.status),
            Some(tcfs_sync::state::FileSyncStatus::Synced)
        );
    }

    #[tokio::test]
    async fn cli_unsync_directory_force_converts_dirty_with_tracked_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let sync_root = dir.path().join("tree");
        std::fs::create_dir_all(sync_root.join("docs")).unwrap();
        let dirty = sync_root.join("docs/dirty.txt");
        std::fs::write(&dirty, b"before").unwrap();

        let config = test_config(&sync_root);
        let state_path = resolve_state_path(&config, None);
        let mut state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        let tracked = seed_tracked_file(&mut state, &dirty, "data/index/docs/dirty.txt");
        state.flush().unwrap();

        std::fs::write(&dirty, b"after local edit").unwrap();

        cmd_unsync(&config, &sync_root.join("docs"), true)
            .await
            .unwrap();

        let stub_path = sync_root.join("docs/dirty.txt.tc");
        assert!(!dirty.exists(), "dirty file should be removed after force");
        assert!(stub_path.exists(), "dirty file should be replaced by stub");
        let stub =
            tcfs_vfs::StubMeta::parse(&std::fs::read_to_string(&stub_path).unwrap()).unwrap();
        assert_eq!(stub.blake3_hex(), Some(tracked.blake3.as_str()));
        assert_eq!(stub.size, tracked.size);

        let state = tcfs_sync::state::StateCache::open(&state_path).unwrap();
        assert_eq!(
            state.get(&dirty).map(|entry| entry.status),
            Some(tcfs_sync::state::FileSyncStatus::NotSynced)
        );
    }

    #[test]
    fn atomic_write_normalizes_empty_relative_parent() {
        assert_eq!(
            Path::new("master.key").parent(),
            Some(Path::new("")),
            "this test must exercise Rust's empty relative parent"
        );
        assert_eq!(atomic_write_parent(Path::new("master.key")), Path::new("."));
        assert_eq!(
            atomic_write_temp_path(Path::new("master.key"), 1).parent(),
            Some(Path::new("."))
        );
    }

    #[test]
    fn generated_rotation_artifacts_match_fixed_blacklist_denies() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("custom-key-material.bin");
        let paths = key_rotation_paths(&key_path);
        let artifacts = [
            paths.pending_key_path.clone(),
            paths.state_path.clone(),
            atomic_write_temp_path(&key_path, 1),
            atomic_write_temp_path(&paths.pending_key_path, 2),
            atomic_write_temp_path(&paths.state_path, 3),
        ];
        let fixed = tcfs_sync::blacklist::Blacklist::default();

        for artifact in artifacts {
            assert!(
                fixed
                    .check_fixed_ingress_path_components(&artifact)
                    .is_some(),
                "generated sensitive artifact escaped the fixed deny-set: {}",
                artifact.display()
            );
        }
    }

    #[test]
    fn atomic_write_retries_collision_without_clobbering_and_sets_mode() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("master.key");
        let collision_nonce = 7;
        let success_nonce = 8;
        let collision_path = atomic_write_temp_path(&target, collision_nonce);
        let success_path = atomic_write_temp_path(&target, success_nonce);
        std::fs::write(&collision_path, b"owned-by-someone-else").unwrap();

        let mut nonces = [collision_nonce, success_nonce].into_iter();
        atomic_write_bytes_with_nonce_source(&target, b"new-key", Some(0o600), || {
            nonces.next().unwrap()
        })
        .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new-key");
        assert_eq!(
            std::fs::read(&collision_path).unwrap(),
            b"owned-by-someone-else",
            "create_new collisions must never be truncated or removed"
        );
        assert!(!success_path.exists(), "successful temp must be renamed");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn atomic_write_removes_temp_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("destination-is-a-directory");
        std::fs::create_dir(&target).unwrap();
        let nonce = 9;
        let temp_path = atomic_write_temp_path(&target, nonce);

        let error =
            atomic_write_bytes_with_nonce_source(&target, b"new-key", Some(0o600), || nonce)
                .unwrap_err();

        assert!(error.to_string().contains("renaming"), "{error:#}");
        assert!(!temp_path.exists(), "failed write must clean up its temp");
        assert!(target.is_dir(), "failed rename must preserve destination");
    }

    #[test]
    fn finalize_rotation_installs_key_before_cleaning_resume_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("master.key");
        let old_master = master_key(0x11);
        let new_master = master_key(0x22);
        let paths = key_rotation_paths(&key_path);
        write_master_key(&key_path, &old_master).unwrap();
        write_master_key(&paths.pending_key_path, &new_master).unwrap();
        write_rotation_state(
            &paths.state_path,
            &KeyRotationState::new("data/manifests/", &paths.pending_key_path),
        )
        .unwrap();

        finalize_key_rotation(&key_path, &new_master, &paths).unwrap();

        assert_eq!(
            read_master_key(&key_path).unwrap().as_bytes(),
            new_master.as_bytes()
        );
        assert!(!paths.state_path.exists());
        assert!(!paths.pending_key_path.exists());
    }

    #[test]
    fn cleanup_keeps_pending_key_when_state_cannot_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = key_rotation_paths(&dir.path().join("master.key"));
        std::fs::create_dir(&paths.state_path).unwrap();
        std::fs::write(&paths.pending_key_path, b"recoverable-pending-key").unwrap();

        cleanup_rotation_artifacts(&paths);

        assert!(paths.state_path.is_dir());
        assert_eq!(
            std::fs::read(&paths.pending_key_path).unwrap(),
            b"recoverable-pending-key",
            "cleanup must never leave state without its pending key"
        );
    }

    #[test]
    fn cleanup_keeps_pending_key_when_state_removal_cannot_be_synced() {
        let dir = tempfile::tempdir().unwrap();
        let paths = key_rotation_paths(&dir.path().join("master.key"));
        std::fs::write(&paths.state_path, b"resume-state").unwrap();
        std::fs::write(&paths.pending_key_path, b"recoverable-pending-key").unwrap();
        let mut sync_calls = 0;

        cleanup_rotation_artifacts_with_sync(&paths, |_| {
            sync_calls += 1;
            anyhow::bail!("simulated directory sync failure")
        });

        assert_eq!(sync_calls, 1, "cleanup must stop after the failed sync");
        assert!(!paths.state_path.exists());
        assert_eq!(
            std::fs::read(&paths.pending_key_path).unwrap(),
            b"recoverable-pending-key",
            "pending key must remain until state removal is durable"
        );
    }

    #[tokio::test]
    async fn rotate_manifests_can_resume_after_interruption() {
        let op = memory_op();
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("master.key");
        let old_master = master_key(0x11);
        let new_master = master_key(0x22);
        let paths = key_rotation_paths(&key_path);

        write_master_key(&key_path, &old_master).unwrap();
        write_master_key(&paths.pending_key_path, &new_master).unwrap();

        op.write(
            "data/manifests/a",
            make_encrypted_manifest(&old_master, "hash-a", "a.txt")
                .to_bytes()
                .unwrap(),
        )
        .await
        .unwrap();
        op.write(
            "data/manifests/b",
            make_encrypted_manifest(&old_master, "hash-b", "b.txt")
                .to_bytes()
                .unwrap(),
        )
        .await
        .unwrap();

        let permit = legacy_manifest_mutation_permit(&op, "data", "master-key rotation", false)
            .await
            .unwrap();

        let mut state = KeyRotationState::new("data/manifests/", &paths.pending_key_path);
        write_rotation_state(&paths.state_path, &state).unwrap();

        let err = rotate_manifests_with_resume(
            &op,
            &permit,
            "data/manifests/",
            &old_master,
            &new_master,
            &mut state,
            &paths.state_path,
            Some(1),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("simulated interruption"));

        let persisted = read_rotation_state(&paths.state_path).unwrap();
        assert_eq!(persisted.rotated_manifests, 1);
        assert_eq!(persisted.status, KeyRotationStatus::RewritingManifests);

        let manifest_a = read_manifest(&op, "data/manifests/a").await;
        let manifest_b = read_manifest(&op, "data/manifests/b").await;
        let rotated_count = [manifest_a.clone(), manifest_b.clone()]
            .iter()
            .filter(|manifest| manifest_uses_key(manifest, &new_master))
            .count();
        let old_count = [manifest_a.clone(), manifest_b.clone()]
            .iter()
            .filter(|manifest| manifest_uses_key(manifest, &old_master))
            .count();
        assert_eq!(rotated_count, 1);
        assert_eq!(old_count, 1);

        let mut resumed_state = read_rotation_state(&paths.state_path).unwrap();
        rotate_manifests_with_resume(
            &op,
            &permit,
            "data/manifests/",
            &old_master,
            &new_master,
            &mut resumed_state,
            &paths.state_path,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resumed_state.status, KeyRotationStatus::ReadyToSwap);
        assert_eq!(resumed_state.rotated_manifests, 1);
        assert_eq!(resumed_state.already_rotated_manifests, 1);

        let manifest_a = read_manifest(&op, "data/manifests/a").await;
        let manifest_b = read_manifest(&op, "data/manifests/b").await;
        assert!(manifest_uses_key(&manifest_a, &new_master));
        assert!(manifest_uses_key(&manifest_b, &new_master));
        assert!(!manifest_uses_key(&manifest_a, &old_master));
        assert!(!manifest_uses_key(&manifest_b, &old_master));
    }

    #[test]
    fn prepare_key_rotation_cleans_stale_state_after_key_swap() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("master.key");
        let current_master = master_key(0x33);
        let paths = key_rotation_paths(&key_path);

        write_master_key(&key_path, &current_master).unwrap();
        write_master_key(&paths.pending_key_path, &current_master).unwrap();
        write_rotation_state(
            &paths.state_path,
            &KeyRotationState::new("data/manifests/", &paths.pending_key_path),
        )
        .unwrap();

        let prepared =
            prepare_key_rotation(&key_path, "data/manifests/", false, true, None).unwrap();
        assert!(prepared.is_none());
        assert!(!paths.state_path.exists());
        assert!(!paths.pending_key_path.exists());
    }

    // ── TIN-2856: rotate-key --new-key-file (exact externally-derived key) ──

    #[test]
    fn rotate_key_new_key_file_parses_raw_32_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.key");
        std::fs::write(&path, [0x5au8; tcfs_crypto::KEY_SIZE]).unwrap();

        let key = read_exact_new_master_key(&path).unwrap();
        assert_eq!(key.as_bytes(), &[0x5au8; tcfs_crypto::KEY_SIZE]);
    }

    #[test]
    fn rotate_key_new_key_file_parses_hex_with_optional_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.key.hex");
        let expected: Vec<u8> = (0..tcfs_crypto::KEY_SIZE as u8).collect();
        let hex: String = expected.iter().map(|b| format!("{b:02x}")).collect();

        // `sha256sum`-style: hex digest with a trailing newline.
        std::fs::write(&path, format!("{hex}\n")).unwrap();
        let key = read_exact_new_master_key(&path).unwrap();
        assert_eq!(key.as_bytes().as_slice(), expected.as_slice());

        // Bare 64-char hex (no newline) parses identically.
        std::fs::write(&path, &hex).unwrap();
        let key = read_exact_new_master_key(&path).unwrap();
        assert_eq!(key.as_bytes().as_slice(), expected.as_slice());
    }

    #[test]
    fn rotate_key_new_key_file_rejects_wrong_length_and_non_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.key");

        // 31 raw bytes: neither raw-sized nor hex-sized.
        std::fs::write(&path, [0u8; 31]).unwrap();
        let err = read_exact_new_master_key(&path).unwrap_err();
        assert!(err.to_string().contains("32 raw bytes"));

        // 64 chars but not hex.
        std::fs::write(&path, "zz".repeat(32)).unwrap();
        assert!(read_exact_new_master_key(&path).is_err());

        // 62 hex chars (one byte short).
        std::fs::write(&path, "ab".repeat(31)).unwrap();
        assert!(read_exact_new_master_key(&path).is_err());
    }

    #[test]
    fn rotate_key_new_key_file_conflicts_with_password_flag() {
        let err = Cli::try_parse_from([
            "tcfs",
            "rotate-key",
            "--password",
            "--new-key-file",
            "/tmp/k",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

        // Each flag alone still parses.
        assert!(Cli::try_parse_from(["tcfs", "rotate-key", "--new-key-file", "/tmp/k"]).is_ok());
        assert!(Cli::try_parse_from(["tcfs", "rotate-key", "--password"]).is_ok());
    }

    #[test]
    fn prepare_key_rotation_uses_exact_new_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("master.key");
        write_master_key(&key_path, &master_key(0x11)).unwrap();

        let new_key_path = dir.path().join("new.key");
        std::fs::write(&new_key_path, [0x77u8; tcfs_crypto::KEY_SIZE]).unwrap();

        let prepared = prepare_key_rotation(
            &key_path,
            "data/manifests/",
            false,
            true,
            Some(&new_key_path),
        )
        .unwrap()
        .expect("fresh rotation prepared");
        assert!(!prepared.resumed);
        assert_eq!(
            prepared.new_master.as_bytes(),
            &[0x77u8; tcfs_crypto::KEY_SIZE]
        );
        // The pending key file carries the exact key, so an interrupted run
        // resumes to (and finally swaps in) the operator-supplied key.
        let pending = read_master_key(&prepared.paths.pending_key_path).unwrap();
        assert_eq!(pending.as_bytes(), &[0x77u8; tcfs_crypto::KEY_SIZE]);
    }

    #[test]
    fn prepare_key_rotation_rejects_mismatched_new_key_file_on_resume() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("master.key");
        let paths = key_rotation_paths(&key_path);
        write_master_key(&key_path, &master_key(0x11)).unwrap();
        write_master_key(&paths.pending_key_path, &master_key(0x22)).unwrap();
        write_rotation_state(
            &paths.state_path,
            &KeyRotationState::new("data/manifests/", &paths.pending_key_path),
        )
        .unwrap();

        let other_key_path = dir.path().join("other.key");
        std::fs::write(&other_key_path, [0x33u8; tcfs_crypto::KEY_SIZE]).unwrap();

        let err = prepare_key_rotation(
            &key_path,
            "data/manifests/",
            false,
            true,
            Some(&other_key_path),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not match --new-key-file"));
    }

    // ── TIN-1899: scoped per-device key rotation tests ──────────────────────

    use tcfs_sync::engine::{DeviceUnwrapIdentity, EncryptionContext, WrapMode as EngWrapMode};

    /// A throwaway age X25519 device identity for tests.
    struct TestDevice {
        device_id: String,
        recipient: String,
        secret: String,
    }

    fn test_device(device_id: &str) -> TestDevice {
        // Reuse the production age X25519 keypair generator (avoids a direct
        // `age` dev-dependency in tcfs-cli).
        let key = tcfs_secrets::device::generate_local_device_key();
        TestDevice {
            device_id: device_id.to_string(),
            recipient: key.public_key.clone(),
            secret: key.secret_key.expose_secret().to_string(),
        }
    }

    fn recipient_of(d: &TestDevice) -> tcfs_crypto::AgeFileKeyRecipient {
        tcfs_crypto::AgeFileKeyRecipient {
            device_id: d.device_id.clone(),
            recipient: d.recipient.clone(),
        }
    }

    fn identity_of(d: &TestDevice) -> DeviceUnwrapIdentity {
        DeviceUnwrapIdentity {
            device_id: d.device_id.clone(),
            secret: d.secret.clone(),
        }
    }

    /// Build a per-device (v3) EncryptionContext: per-device-only wraps, this
    /// device's unwrap identity, and the given recipient set.
    fn per_device_ctx(
        master: &tcfs_crypto::MasterKey,
        recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
        identity: DeviceUnwrapIdentity,
    ) -> EncryptionContext {
        EncryptionContext::new(master.clone()).with_wrap_mode(
            EngWrapMode::PerDevice,
            recipients,
            Some(identity),
        )
    }

    /// Seed a real encrypted file (chunks + manifest) into the operator using the
    /// given context's wrap shape. Returns the published manifest path.
    async fn seed_encrypted_file(
        op: &Operator,
        remote_prefix: &str,
        rel_path: &str,
        plaintext: &[u8],
        ctx: &EncryptionContext,
    ) -> String {
        let file_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(plaintext));
        let file_id: [u8; 32] = *tcfs_chunks::hash_from_hex(&file_hash).unwrap().as_bytes();
        let file_key = tcfs_crypto::generate_file_key();

        // One chunk for test simplicity (the production path chunks via FastCDC;
        // re-keying is per-chunk-index either way).
        let ciphertext = tcfs_crypto::encrypt_chunk(&file_key, 0, &file_id, plaintext).unwrap();
        let chunk_hash = tcfs_chunks::hash_to_hex(&tcfs_chunks::hash_bytes(&ciphertext));
        op.write(&format!("{remote_prefix}/chunks/{chunk_hash}"), ciphertext)
            .await
            .unwrap();

        let (encrypted_file_key, wrapped_file_keys, version) =
            wrap_rotated_file_key(ctx, &file_key).unwrap();

        let manifest = tcfs_sync::manifest::SyncManifest {
            version,
            file_hash,
            file_size: plaintext.len() as u64,
            chunks: vec![chunk_hash],
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "seed-device".into(),
            written_at: 0,
            rel_path: Some(rel_path.to_string()),
            mode: None,
            mtime: None,
            encrypted_file_key,
            wrapped_file_keys,
        };
        let manifest_path = format!("{remote_prefix}/manifests/{rel_path}");
        op.write(&manifest_path, manifest.to_bytes().unwrap())
            .await
            .unwrap();
        manifest_path
    }

    /// Decrypt a published manifest's content end-to-end with the given context.
    async fn decrypt_published(
        op: &Operator,
        remote_prefix: &str,
        manifest_path: &str,
        ctx: &EncryptionContext,
    ) -> Result<Vec<u8>> {
        let data = op.read(manifest_path).await.unwrap().to_vec();
        let manifest = tcfs_sync::manifest::SyncManifest::from_bytes(&data).unwrap();
        let fk = unwrap_manifest_file_key(&manifest, ctx, manifest_path)?
            .ok_or_else(|| anyhow::anyhow!("keyless manifest"))?;
        let file_id: [u8; 32] = *tcfs_chunks::hash_from_hex(&manifest.file_hash)
            .unwrap()
            .as_bytes();
        let mut out = Vec::new();
        for (i, hash) in manifest.chunks.iter().enumerate() {
            let ct = op
                .read(&format!("{remote_prefix}/chunks/{hash}"))
                .await
                .unwrap()
                .to_vec();
            let pt = tcfs_crypto::decrypt_chunk(&fk, i as u64, &file_id, &ct)?;
            out.extend_from_slice(&pt);
        }
        Ok(out)
    }

    async fn chunk_count(op: &Operator, remote_prefix: &str) -> usize {
        op.list_with(&format!("{remote_prefix}/chunks/"))
            .recursive(true)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| !e.metadata().is_dir() && !e.path().ends_with('/'))
            .count()
    }

    /// (a) After revoke X + scoped rotate, device X (absent from the new
    /// recipient set) gets a HARD unwrap error on the re-keyed manifest, while a
    /// still-current device decrypts fine.
    #[tokio::test]
    async fn scoped_rotate_revokes_per_device_read() {
        let op = memory_op();
        let master = master_key(0x42);
        let keep = test_device("device-keep");
        let revoke = test_device("device-revoke");

        // Before rotation: BOTH devices are recipients (per-device-only v3).
        let writer_ctx = per_device_ctx(
            &master,
            vec![recipient_of(&keep), recipient_of(&revoke)],
            identity_of(&keep),
        );
        let plaintext = b"top secret payload that must rotate";
        let manifest_path =
            seed_encrypted_file(&op, "data", "secret/a.txt", plaintext, &writer_ctx).await;
        let permit = legacy_manifest_mutation_permit(&op, "data", "scoped FileKey rotation", false)
            .await
            .unwrap();

        // Sanity: the revoked device CAN read pre-rotation content.
        let revoke_reader = per_device_ctx(
            &master,
            vec![recipient_of(&keep), recipient_of(&revoke)],
            identity_of(&revoke),
        );
        assert_eq!(
            decrypt_published(&op, "data", &manifest_path, &revoke_reader)
                .await
                .unwrap(),
            plaintext
        );

        // Post-revocation recipient set: ONLY the kept device.
        let rotate_ctx = per_device_ctx(&master, vec![recipient_of(&keep)], identity_of(&keep));
        let outcome = rekey_one_manifest(&op, &permit, "data", &manifest_path, &rotate_ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, RekeyOutcome::Rotated { .. }));

        // The kept device still decrypts the re-keyed manifest.
        let keep_reader = per_device_ctx(&master, vec![recipient_of(&keep)], identity_of(&keep));
        assert_eq!(
            decrypt_published(&op, "data", &manifest_path, &keep_reader)
                .await
                .unwrap(),
            plaintext
        );

        // The revoked device gets a HARD unwrap error on the re-keyed manifest.
        let revoke_after =
            per_device_ctx(&master, vec![recipient_of(&revoke)], identity_of(&revoke));
        let err = decrypt_published(&op, "data", &manifest_path, &revoke_after)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unwrap") || msg.contains("no decryptable") || msg.contains("file key"),
            "expected a hard unwrap error for the revoked device, got: {msg}"
        );

        // The re-keyed manifest carries NO wrap addressed to the revoked device.
        let data = op.read(&manifest_path).await.unwrap().to_vec();
        let m = tcfs_sync::manifest::SyncManifest::from_bytes(&data).unwrap();
        assert!(
            !m.wrapped_file_keys
                .iter()
                .any(|w| w.recipient_device_id == "device-revoke"),
            "re-keyed manifest must not wrap to the revoked device"
        );
        assert!(m
            .wrapped_file_keys
            .iter()
            .any(|w| w.recipient_device_id == "device-keep"));
    }

    /// (c) Orphaned old chunks are GC'd ONLY after publish, and referenced chunks
    /// are never deleted.
    #[tokio::test]
    async fn scoped_rotate_gcs_orphans_only_after_publish() {
        let op = memory_op();
        let master = master_key(0x42);
        let keep = test_device("device-keep");

        let ctx = per_device_ctx(&master, vec![recipient_of(&keep)], identity_of(&keep));
        let manifest_path =
            seed_encrypted_file(&op, "data", "secret/a.txt", b"payload one", &ctx).await;
        let permit = legacy_manifest_mutation_permit(&op, "data", "scoped FileKey rotation", false)
            .await
            .unwrap();

        // Capture the original chunk address.
        let before = op.read(&manifest_path).await.unwrap().to_vec();
        let old_hash = tcfs_sync::manifest::SyncManifest::from_bytes(&before)
            .unwrap()
            .chunks[0]
            .clone();
        assert_eq!(chunk_count(&op, "data").await, 1);

        // Re-key: writes a NEW chunk; the OLD chunk is still present (orphan).
        rekey_one_manifest(&op, &permit, "data", &manifest_path, &ctx)
            .await
            .unwrap();
        let after = op.read(&manifest_path).await.unwrap().to_vec();
        let new_hash = tcfs_sync::manifest::SyncManifest::from_bytes(&after)
            .unwrap()
            .chunks[0]
            .clone();
        assert_ne!(
            old_hash, new_hash,
            "re-key must produce a new content address"
        );
        assert_eq!(
            chunk_count(&op, "data").await,
            2,
            "old chunk is NOT deleted before GC"
        );
        assert!(op.exists(&format!("data/chunks/{old_hash}")).await.unwrap());

        // GC SAFETY (reference correctness): after publish, the OLD chunk is no
        // longer referenced by ANY live manifest and is identified as orphaned,
        // while the NEW (referenced) chunk is NEVER classified as orphaned.
        //
        // NOTE: the opendal Memory backend reports `last_modified: None`, so the
        // grace-gated *deletion* path conservatively keeps timestamp-less chunks
        // (verified in tcfs-sync's plan_orphaned_chunk_cleanup_respects_grace_period).
        // We therefore assert the orphan-identification invariant the rotation
        // depends on for safety, rather than physical deletion on this backend.
        let report = tcfs_sync::reconcile::find_orphaned_chunks(&op, "data")
            .await
            .unwrap();
        assert_eq!(
            report.orphaned_chunks,
            vec![old_hash.clone()],
            "exactly the old chunk is orphaned after publish"
        );
        assert_eq!(report.referenced_chunks, 1, "the new chunk is referenced");
        assert!(
            !report.orphaned_chunks.contains(&new_hash),
            "the referenced new chunk must NEVER be classified as orphaned"
        );

        // The cleanup call runs cleanly and never deletes a referenced chunk; on
        // a timestamp-bearing backend it would also delete the orphan.
        let cleanup = tcfs_sync::reconcile::cleanup_legacy_orphaned_chunks(
            &op,
            "data",
            Duration::from_secs(0),
            SystemTime::now(),
        )
        .await
        .unwrap();
        assert_eq!(
            cleanup.orphaned_chunks_found, 1,
            "GC found exactly the old orphan"
        );
        assert!(
            !cleanup.deleted_chunks.contains(&new_hash),
            "GC must never delete the referenced new chunk"
        );
        assert!(
            op.exists(&format!("data/chunks/{new_hash}")).await.unwrap(),
            "the live (referenced) chunk survives GC"
        );
    }

    /// (b) Resumability: a kill mid-run leaves published manifests in
    /// done_manifests, and the resume skips them without losing data.
    #[tokio::test]
    async fn scoped_rotate_state_resumes_published_manifests() {
        let op = memory_op();
        let master = master_key(0x42);
        let keep = test_device("device-keep");
        let ctx = per_device_ctx(&master, vec![recipient_of(&keep)], identity_of(&keep));

        let m_a = seed_encrypted_file(&op, "data", "secret/a.txt", b"alpha", &ctx).await;
        let m_b = seed_encrypted_file(&op, "data", "secret/b.txt", b"bravo", &ctx).await;
        let permit = legacy_manifest_mutation_permit(&op, "data", "scoped FileKey rotation", false)
            .await
            .unwrap();

        // Simulate a kill after publishing only manifest A.
        let mut state = ScopedRotationState::new("data/manifests/secret/");
        rekey_one_manifest(&op, &permit, "data", &m_a, &ctx)
            .await
            .unwrap();
        state.mark_done(&m_a);
        state.rotated_manifests += 1;
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(".key-rotate.json");
        write_scoped_rotation_state(&state_path, &state).unwrap();

        // Resume: A is skipped (idempotent), B is freshly re-keyed.
        let resumed = read_scoped_rotation_state(&state_path).unwrap();
        assert!(resumed.is_done(&m_a));
        assert!(!resumed.is_done(&m_b));

        let manifests = list_scoped_manifests(&op, "data/manifests/secret/")
            .await
            .unwrap();
        let mut state = resumed;
        for path in &manifests {
            if state.is_done(path) {
                state.already_done_manifests += 1;
                continue;
            }
            rekey_one_manifest(&op, &permit, "data", path, &ctx)
                .await
                .unwrap();
            state.mark_done(path);
            state.rotated_manifests += 1;
        }
        assert_eq!(state.already_done_manifests, 1);
        assert!(state.is_done(&m_b));

        // Both manifests decrypt cleanly with the kept device after resume.
        assert_eq!(
            decrypt_published(&op, "data", &m_a, &ctx).await.unwrap(),
            b"alpha"
        );
        assert_eq!(
            decrypt_published(&op, "data", &m_b, &ctx).await.unwrap(),
            b"bravo"
        );
    }

    /// (d) The renamed master-rotate counter distinguishes genuinely-plaintext
    /// manifests from per-device-only manifests.
    #[tokio::test]
    async fn master_rotate_counter_distinguishes_plaintext_from_per_device() {
        let op = memory_op();
        let old_master = master_key(0x11);
        let new_master = master_key(0x22);
        let device = test_device("device-keep");

        // 1) genuinely keyless (plaintext) manifest.
        let plaintext_manifest = tcfs_sync::manifest::SyncManifest {
            version: 2,
            file_hash: "hash-plain".into(),
            file_size: 0,
            chunks: vec![],
            vclock: tcfs_sync::conflict::VectorClock::new(),
            written_by: "t".into(),
            written_at: 0,
            rel_path: Some("plain.txt".into()),
            mode: None,
            mtime: None,
            encrypted_file_key: None,
            wrapped_file_keys: Vec::new(),
        };
        op.write(
            "data/manifests/plain",
            plaintext_manifest.to_bytes().unwrap(),
        )
        .await
        .unwrap();

        // 2) per-device-only (v3) manifest: no master wrap, has device wraps.
        let pd_ctx = per_device_ctx(
            &old_master,
            vec![recipient_of(&device)],
            identity_of(&device),
        );
        seed_encrypted_file(&op, "data", "perdev.txt", b"secret", &pd_ctx).await;

        // 3) a normal master-wrapped manifest (gets rotated).
        op.write(
            "data/manifests/master",
            make_encrypted_manifest(&old_master, "hash-m", "master.txt")
                .to_bytes()
                .unwrap(),
        )
        .await
        .unwrap();

        let permit = legacy_manifest_mutation_permit(&op, "data", "master-key rotation", false)
            .await
            .unwrap();

        let mut state = KeyRotationState::new("data/manifests/", Path::new("/tmp/pending"));
        let _dir = tempfile::tempdir().unwrap();
        let state_path = _dir.path().join("rotate-state.json");
        rotate_manifests_with_resume(
            &op,
            &permit,
            "data/manifests/",
            &old_master,
            &new_master,
            &mut state,
            &state_path,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            state.skipped_keyless_manifests, 1,
            "exactly one genuinely-plaintext manifest"
        );
        assert_eq!(
            state.skipped_per_device_manifests, 1,
            "exactly one per-device-only manifest (NOT counted as plaintext)"
        );
        assert_eq!(state.rotated_manifests, 1, "the master-wrapped one rotated");
    }

    // ── TIN-1899 must-fix: forward-secrecy messaging is gated on wrap_mode ──

    /// Build a DEFAULT Master-wrap context: this is what `cmd_key_rotate`
    /// constructs via `build_encryption_context` when `crypto.wrap_mode` is the
    /// default `Master`. Re-keyed content is re-wrapped to the unchanged shared
    /// master key — NO per-device forward secrecy.
    fn master_ctx(master: &tcfs_crypto::MasterKey) -> EncryptionContext {
        EncryptionContext::new(master.clone())
    }

    /// Build a Dual context: master wrap + per-device wraps. The master wrap is
    /// retained, so a revoked master-key holder still decrypts — also NO
    /// per-device forward secrecy.
    fn dual_ctx(
        master: &tcfs_crypto::MasterKey,
        recipients: Vec<tcfs_crypto::AgeFileKeyRecipient>,
        identity: DeviceUnwrapIdentity,
    ) -> EncryptionContext {
        EncryptionContext::new(master.clone()).with_wrap_mode(
            EngWrapMode::Dual,
            recipients,
            Some(identity),
        )
    }

    /// The must-fix: under DEFAULT Master wrap, `cmd_key_rotate`'s closing
    /// summary must NOT claim forward secrecy. It must instead emit the LOUD
    /// warning that re-keyed content was re-wrapped to the UNCHANGED shared
    /// master key and a revoked master-key holder still decrypts. This drives the
    /// exact decision logic `cmd_key_rotate` prints (see
    /// `forward_secrecy_summary_lines`).
    #[test]
    fn master_wrap_rotation_warns_no_forward_secrecy() {
        let master = master_key(0x42);
        let ctx = master_ctx(&master);

        assert!(
            !rotation_grants_forward_secrecy(&ctx),
            "Master wrap must NOT be reported as granting forward secrecy"
        );

        let summary = forward_secrecy_summary_lines(&ctx).join("\n");
        // The LOUD warning is present...
        assert!(
            summary.contains("WARNING: NO per-device forward secrecy"),
            "Master-mode summary must warn that NO forward secrecy was gained: {summary}"
        );
        assert!(
            summary.contains("UNCHANGED shared master key"),
            "Master-mode summary must name the unchanged shared master key: {summary}"
        );
        assert!(
            summary.contains("can STILL decrypt"),
            "Master-mode summary must state a revoked holder STILL decrypts: {summary}"
        );
        assert!(
            summary.contains("wrap_mode=PerDevice"),
            "Master-mode summary must point at PerDevice as the fix: {summary}"
        );
        // ...and the PerDevice reassurance is ABSENT.
        assert!(
            !summary.contains("can no longer decrypt the re-keyed content"),
            "Master-mode summary must NOT print the PerDevice forward-secrecy reassurance: \
             {summary}"
        );
    }

    /// Dual wrap (master wrap retained alongside per-device wraps) is ALSO not
    /// forward-secret: the master wrap is an unchanged shared-secret path back to
    /// the FileKey. It must get the same warning as Master.
    #[test]
    fn dual_wrap_rotation_warns_no_forward_secrecy() {
        let master = master_key(0x42);
        let keep = test_device("device-keep");
        let ctx = dual_ctx(&master, vec![recipient_of(&keep)], identity_of(&keep));

        assert!(
            !rotation_grants_forward_secrecy(&ctx),
            "Dual wrap retains the master wrap and must NOT be reported as forward-secret"
        );
        let summary = forward_secrecy_summary_lines(&ctx).join("\n");
        assert!(
            summary.contains("WARNING: NO per-device forward secrecy"),
            "Dual-mode summary must warn that NO forward secrecy was gained: {summary}"
        );
        assert!(
            !summary.contains("can no longer decrypt the re-keyed content"),
            "Dual-mode summary must NOT print the PerDevice reassurance: {summary}"
        );
    }

    /// The ONLY path that earns the reassurance: PerDevice with a real recipient
    /// set (per-device-only wraps, no master wrap, manifest v3).
    #[test]
    fn per_device_wrap_rotation_reports_forward_secrecy() {
        let master = master_key(0x42);
        let keep = test_device("device-keep");
        let ctx = per_device_ctx(&master, vec![recipient_of(&keep)], identity_of(&keep));

        assert!(
            rotation_grants_forward_secrecy(&ctx),
            "PerDevice wrap with recipients MUST be reported as granting forward secrecy"
        );
        let summary = forward_secrecy_summary_lines(&ctx).join("\n");
        assert!(
            summary.contains("can no longer decrypt the re-keyed content"),
            "PerDevice summary must print the forward-secrecy reassurance: {summary}"
        );
        assert!(
            !summary.contains("WARNING: NO per-device forward secrecy"),
            "PerDevice summary must NOT print the no-forward-secrecy warning: {summary}"
        );
    }

    /// Defense in depth: PerDevice with an EMPTY recipient set is not a real
    /// forward-secrecy guarantee (no one is a recipient); it must NOT earn the
    /// reassurance. (In practice the write path rejects an empty PerDevice set,
    /// but the messaging gate must not depend on that.)
    #[test]
    fn per_device_wrap_without_recipients_does_not_claim_forward_secrecy() {
        let master = master_key(0x42);
        let ctx = EncryptionContext::new(master.clone()).with_wrap_mode(
            EngWrapMode::PerDevice,
            Vec::new(),
            None,
        );
        assert!(
            !rotation_grants_forward_secrecy(&ctx),
            "PerDevice with no recipients must NOT be reported as forward-secret"
        );
        let summary = forward_secrecy_summary_lines(&ctx).join("\n");
        assert!(
            summary.contains("WARNING: NO per-device forward secrecy"),
            "empty-recipient PerDevice must warn rather than reassure: {summary}"
        );
    }

    /// TIN-2853: `tcfs conflicts` summarizes repo HEADs with `git log`. A
    /// roamed repository can arm `log.showSignature=true` plus
    /// `gpg.program=<attacker binary>` in its synced `.git/config`, which an
    /// unsanitized `git log` executes. The sanitized spawn must return the
    /// same benign summary while never running the repository-configured
    /// program.
    #[cfg(unix)]
    #[test]
    fn git_head_oneline_does_not_execute_repository_gpg_program() {
        use std::os::unix::fs::PermissionsExt;

        fn run_git(repo: &Path, args: &[&str]) {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        run_git(&repo, &["config", "user.email", "tcfs@example.invalid"]);
        run_git(&repo, &["config", "user.name", "TCFS Test"]);

        // The armed program doubles as a fake signer (SIG_CREATED status plus
        // a signature block, the shape `git commit -S` expects) so the fixture
        // commit carries a signature for `git log` to verify.
        let gpg = dir.path().join("evil-gpg");
        let sentinel = dir.path().join("evil-gpg.ran");
        std::fs::write(
            &gpg,
            format!(
                "#!/bin/sh\n\
                 printf ran > \"{}\"\n\
                 printf '[GNUPG:] SIG_CREATED \\n' >&2\n\
                 printf -- '-----BEGIN PGP SIGNATURE-----\\nfake\\n-----END PGP SIGNATURE-----\\n'\n",
                sentinel.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gpg, std::fs::Permissions::from_mode(0o700)).unwrap();
        run_git(&repo, &["config", "gpg.program", gpg.to_str().unwrap()]);
        run_git(&repo, &["config", "log.showSignature", "true"]);

        std::fs::write(repo.join("file.txt"), b"base\n").unwrap();
        run_git(&repo, &["add", "file.txt"]);
        run_git(&repo, &["commit", "--quiet", "-S", "-m", "base"]);
        assert!(
            sentinel.exists(),
            "signing through the armed gpg.program did not run it"
        );
        std::fs::remove_file(&sentinel).unwrap();

        // Prove the fixture is armed: an ordinary `git log` executes the
        // repository-configured gpg.program via log.showSignature.
        let control = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["log", "--oneline", "-1"])
            .output()
            .unwrap();
        assert!(
            control.status.success(),
            "ordinary git log failed: {}",
            String::from_utf8_lossy(&control.stderr)
        );
        assert!(sentinel.exists(), "the armed control did not run");
        std::fs::remove_file(&sentinel).unwrap();

        let head = git_head_oneline(&repo).expect("sanitized HEAD read must succeed");
        assert!(head.contains("base"), "unexpected oneline output: {head}");
        assert!(
            !sentinel.exists(),
            "git_head_oneline must not execute the repository-configured gpg.program"
        );
    }
}
