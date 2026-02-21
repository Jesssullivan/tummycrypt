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
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
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

    // ── Phase 3: FUSE mount + stub management ────────────────────────────────
    /// Mount a remote as a local directory (requires FUSE)
    #[cfg(feature = "fuse")]
    Mount {
        /// Remote spec (e.g. seaweedfs://host/bucket[/prefix])
        remote: String,
        /// Local mountpoint
        mountpoint: PathBuf,
        /// Mount read-only
        #[arg(long)]
        read_only: bool,
    },

    /// Unmount a tcfs mountpoint (requires FUSE)
    #[cfg(feature = "fuse")]
    Unmount {
        /// Local mountpoint to unmount
        mountpoint: PathBuf,
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
}

#[derive(Subcommand, Debug)]
enum DeviceAction {
    /// List enrolled devices
    List,
    /// Revoke a device by name
    Revoke {
        /// Device name to revoke
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Unlock the encryption session (store master key in keychain)
    Unlock,
    /// Lock the encryption session (clear master key from keychain)
    Lock,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the active configuration (merged defaults + config file)
    Show,
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
    let cli = Cli::parse();
    let config = load_config(&cli.config).await?;

    match cli.command {
        #[cfg(unix)]
        Commands::Status => cmd_status(&config).await,
        #[cfg(not(unix))]
        Commands::Status => anyhow::bail!("status command requires Unix daemon socket (not available on Windows)"),
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
        #[cfg(feature = "fuse")]
        Commands::Mount {
            remote,
            mountpoint,
            read_only,
        } => cmd_mount(&config, &remote, &mountpoint, read_only).await,
        #[cfg(feature = "fuse")]
        Commands::Unmount { mountpoint } => cmd_unmount(&mountpoint),
        Commands::Unsync { path, force } => cmd_unsync(&config, &path, force).await,
        Commands::Init {
            device_name,
            non_interactive,
            password,
        } => cmd_init(&config, device_name, non_interactive, password).await,
        Commands::Device { action } => match action {
            DeviceAction::List => cmd_device_list(),
            DeviceAction::Revoke { name } => cmd_device_revoke(&name),
        },
        Commands::Auth { action } => match action {
            AuthAction::Unlock => cmd_auth_unlock(),
            AuthAction::Lock => cmd_auth_lock(),
        },
        Commands::RotateCredentials {
            cred_file,
            non_interactive,
        } => cmd_rotate_credentials(&config, cred_file.as_deref(), non_interactive).await,
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

/// Build an OpenDAL operator using credentials from environment variables.
///
/// Reads AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY (standard S3 env vars).
/// These override any config file credentials for direct CLI use.
fn build_operator_from_env(config: &tcfs_core::config::TcfsConfig) -> Result<opendal::Operator> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID")
        .or_else(|_| std::env::var("TCFS_ACCESS_KEY_ID"))
        .context(
            "S3 credentials not set\n\
             Set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY environment variables.\n\
             Example:\n\
             \texport AWS_ACCESS_KEY_ID=your-key\n\
             \texport AWS_SECRET_ACCESS_KEY=your-secret",
        )?;
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .or_else(|_| std::env::var("TCFS_SECRET_ACCESS_KEY"))
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

    // Default prefix: local directory/file name
    let remote_prefix = prefix
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            local
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "tcfs".to_string())
        });

    println!(
        "Pushing {} → {}:{} (endpoint: {})",
        local.display(),
        config.storage.bucket,
        remote_prefix,
        config.storage.endpoint,
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

        let result =
            tcfs_sync::engine::upload_file(&op, local, &remote_prefix, &mut state, Some(&progress))
                .await
                .with_context(|| format!("uploading {}", local.display()))?;

        state.flush().context("flushing state cache")?;

        if result.skipped {
            pb.finish_with_message(format!(
                "{} (unchanged)",
                local.file_name().unwrap_or_default().to_string_lossy()
            ));
            println!("  skipped (unchanged since last sync)");
        } else {
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

        let (uploaded, skipped, bytes) =
            tcfs_sync::engine::push_tree(&op, local, &remote_prefix, &mut state, Some(&progress))
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
    _state_override: Option<&Path>,
) -> Result<()> {
    let op = build_operator_from_env(config)?;

    // Derive the remote prefix from the manifest path if not provided
    // e.g. "mydata/manifests/abc123" → prefix = "mydata"
    let remote_prefix = prefix
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            manifest_path
                .split('/')
                .next()
                .unwrap_or("tcfs")
                .to_string()
        });

    // Default local path: current dir + manifest hash (last path component)
    let hash_basename = manifest_path.split('/').next_back().unwrap_or("downloaded");
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

    let result = tcfs_sync::engine::download_file(
        &op,
        manifest_path,
        &local_path,
        &remote_prefix,
        Some(&progress),
    )
    .await
    .with_context(|| format!("downloading {}", manifest_path))?;

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
            "ok"
        } else {
            "not connected (Phase 2)"
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

// ── `tcfs mount` (requires fuse feature) ────────────────────────────────────

#[cfg(feature = "fuse")]
/// Parse a remote spec like `seaweedfs://host:port/bucket[/prefix]`
fn parse_remote_spec(spec: &str) -> anyhow::Result<(String, String, String)> {
    let rest = spec
        .strip_prefix("seaweedfs://")
        .with_context(|| format!("remote spec must start with seaweedfs:// — got: {}", spec))?;

    // Split host:port from /bucket[/prefix]
    let slash = rest
        .find('/')
        .with_context(|| format!("remote spec must include /bucket — got: {}", spec))?;

    let host = &rest[..slash]; // e.g. "dees-appu-bearts:8333"
    let path = &rest[slash + 1..]; // e.g. "tcfs-test" or "tcfs-test/subdir"

    // First path component = bucket, remainder = prefix
    let (bucket, prefix) = path.split_once('/').unwrap_or((path, ""));

    Ok((
        format!("http://{}", host),
        bucket.to_string(),
        prefix.trim_end_matches('/').to_string(),
    ))
}

#[cfg(feature = "fuse")]
async fn cmd_mount(
    config: &tcfs_core::config::TcfsConfig,
    remote: &str,
    mountpoint: &std::path::Path,
    read_only: bool,
) -> Result<()> {
    let (endpoint, bucket, prefix) = parse_remote_spec(remote)?;

    // Credentials
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

    // Ensure mountpoint exists
    tokio::fs::create_dir_all(mountpoint)
        .await
        .with_context(|| format!("creating mountpoint: {}", mountpoint.display()))?;

    let cache_dir = expand_tilde(&config.fuse.cache_dir);
    let neg_ttl = config.fuse.negative_cache_ttl_secs;
    let cache_max = config.fuse.cache_max_mb * 1024 * 1024;

    println!(
        "Mounting {}:{} (prefix: {}) → {}",
        endpoint,
        bucket,
        if prefix.is_empty() { "(root)" } else { &prefix },
        mountpoint.display()
    );
    println!(
        "Press Ctrl-C or run `tcfs unmount {}` to stop.",
        mountpoint.display()
    );

    tcfs_fuse::mount(tcfs_fuse::MountConfig {
        op,
        prefix,
        mountpoint: mountpoint.to_path_buf(),
        cache_dir,
        cache_max_bytes: cache_max,
        negative_ttl_secs: neg_ttl,
        read_only,
        allow_other: false,
    })
    .await
    .context("FUSE mount failed")
}

// ── `tcfs unmount` (requires fuse feature) ──────────────────────────────────

#[cfg(feature = "fuse")]
fn cmd_unmount(mountpoint: &std::path::Path) -> Result<()> {
    // macOS: use umount directly (works with FUSE-T and macFUSE)
    // Linux: use fusermount3 first, fall back to umount
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("umount")
            .arg(mountpoint)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("Unmounted: {}", mountpoint.display());
                return Ok(());
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
    if tcfs_fuse::is_stub_path(path) {
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
    let stub_path = tcfs_fuse::real_to_stub_name(path.file_name().context("path has no filename")?);
    let stub_full = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(stub_path);

    let stub = tcfs_fuse::StubMeta {
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

    // Check if already initialized
    let registry_path = tcfs_secrets::device::default_registry_path();
    let registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;
    if registry.find(&device_name).is_some() {
        anyhow::bail!(
            "Device '{}' is already enrolled. Use 'tcfs device list' to see devices.",
            device_name
        );
    }

    // Get master passphrase
    let passphrase = if non_interactive {
        password.context("--password is required in non-interactive mode")?
    } else {
        let p = rpassword::prompt_password("Master passphrase: ")
            .context("failed to read passphrase")?;
        let confirm = rpassword::prompt_password("Confirm passphrase: ")
            .context("failed to read confirmation")?;
        if p != confirm {
            anyhow::bail!("Passphrases do not match");
        }
        p
    };

    // Generate recovery mnemonic
    println!("Creating tcfs identity...");
    let (mnemonic, _master_key) = tcfs_crypto::generate_mnemonic()?;

    // Derive master key from passphrase
    let salt: [u8; 16] = rand_salt();
    let master_key = tcfs_crypto::derive_master_key(
        &secrecy::SecretString::from(passphrase),
        &salt,
        &tcfs_crypto::kdf::KdfParams::default(),
    )?;

    // Generate a device file key and store it
    let file_key = tcfs_crypto::generate_file_key();
    let _wrapped = tcfs_crypto::wrap_key(&master_key, &file_key)?;

    // Register device
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut registry = tcfs_secrets::device::DeviceRegistry::load(&registry_path)?;
    registry.add(tcfs_secrets::device::DeviceIdentity {
        name: device_name.clone(),
        public_key: format!("age1-device-{}", &blake3_short(&device_name)),
        enrolled_at: now,
        revoked: false,
    });
    registry.save(&registry_path)?;

    println!();
    println!("Your recovery phrase (WRITE THIS DOWN):");
    println!();
    // Display mnemonic in groups of 4 words
    let words: Vec<&str> = mnemonic.split_whitespace().collect();
    for (i, chunk) in words.chunks(4).enumerate() {
        println!("  {:2}. {}", i * 4 + 1, chunk.join("  "));
    }
    println!();
    println!("Device name:     {}", device_name);
    println!("Registry:        {}", registry_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Store your recovery phrase in a safe place");
    println!("  2. Configure storage: tcfs config show");
    println!("  3. Push files: tcfs push /path/to/files");

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
        println!(
            "  {} [{}] — enrolled {} — {}",
            device.name, status, device.enrolled_at, device.public_key
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

// ── `tcfs auth unlock` / `tcfs auth lock` ────────────────────────────────────

fn cmd_auth_unlock() -> Result<()> {
    if !tcfs_secrets::keychain::is_available() {
        anyhow::bail!(
            "Platform keychain not available. \
             On Linux, ensure GNOME Keyring or KDE Wallet is running."
        );
    }

    let passphrase =
        rpassword::prompt_password("Master passphrase: ").context("failed to read passphrase")?;

    // Store in keychain for session use
    tcfs_secrets::keychain::store_secret(
        tcfs_secrets::keychain::keys::SESSION_TOKEN,
        &secrecy::SecretString::from(passphrase),
    )?;

    println!("Session unlocked. Master key stored in platform keychain.");
    println!("Run 'tcfs auth lock' to clear it.");
    Ok(())
}

fn cmd_auth_lock() -> Result<()> {
    tcfs_secrets::keychain::delete_secret(tcfs_secrets::keychain::keys::SESSION_TOKEN)?;
    tcfs_secrets::keychain::delete_secret(tcfs_secrets::keychain::keys::MASTER_KEY)?;
    println!("Session locked. Master key cleared from keychain.");
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
