//! tcfsd: TummyCrypt filesystem daemon
//!
//! Usage:
//!   tcfsd [--config /etc/tcfs/config.toml] [--mode daemon|worker]
//!
//! Modes:
//!   daemon  - Full local daemon (FUSE + gRPC + sync) [default]
//!   worker  - Stateless NATS consumer for K8s pods (feature: k8s-worker)

mod cred_store;
mod daemon;
mod grpc;
mod metrics;
mod worker;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "tcfsd", version, about = "TummyCrypt filesystem daemon")]
struct Cli {
    /// Path to tcfs.toml configuration file
    #[arg(
        long,
        short = 'c',
        env = "TCFS_CONFIG",
        default_value = "/etc/tcfs/config.toml"
    )]
    config: PathBuf,

    /// Daemon mode
    #[arg(long, default_value = "daemon")]
    mode: Mode,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, env = "TCFS_LOG", default_value = "info")]
    log: String,

    /// Log format (json, text)
    #[arg(long, env = "TCFS_LOG_FORMAT", default_value = "text")]
    log_format: LogFormat,
}

#[derive(Clone, Debug, ValueEnum, PartialEq)]
enum Mode {
    /// Full local daemon with FUSE + gRPC (default)
    Daemon,
    /// Stateless NATS consumer mode for Kubernetes pods
    Worker,
}

#[derive(Clone, Debug, ValueEnum)]
enum LogFormat {
    Json,
    Text,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    init_logging(&cli.log, &cli.log_format);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        mode = ?cli.mode,
        config = %cli.config.display(),
        "tcfsd starting"
    );

    // Load configuration
    let config = load_config(&cli.config).await?;

    match cli.mode {
        Mode::Daemon => daemon::run(config).await,
        Mode::Worker => {
            #[cfg(feature = "k8s-worker")]
            return worker::run(config).await;
            #[cfg(not(feature = "k8s-worker"))]
            anyhow::bail!(
                "worker mode requires the k8s-worker feature: cargo build --features k8s-worker"
            )
        }
    }
}

async fn load_config(path: &PathBuf) -> Result<tcfs_core::config::TcfsConfig> {
    if path.exists() {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))
    } else {
        tracing::warn!(
            "config file not found: {}  (using defaults)",
            path.display()
        );
        Ok(tcfs_core::config::TcfsConfig::default())
    }
}

fn init_logging(level: &str, format: &LogFormat) {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    match format {
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().json())
                .init();
        }
        LogFormat::Text => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer())
                .init();
        }
    }
}
