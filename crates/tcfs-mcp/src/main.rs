//! tcfs MCP server — exposes daemon capabilities as MCP tools for AI agents
//!
//! Communicates with tcfsd over Unix domain socket gRPC, then translates
//! responses into MCP tool results. Runs over stdio for Claude Code integration.

use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::{self, EnvFilter};

mod server;

#[tokio::main]
async fn main() -> Result<()> {
    // Logging MUST go to stderr — stdout is reserved for JSON-RPC messages
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("tcfs-mcp starting");

    let socket_path = std::env::var("TCFS_SOCKET").unwrap_or_else(|_| {
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                std::path::PathBuf::from(home).join(".local/state")
            });
        state_dir
            .join("tcfsd/tcfsd.sock")
            .to_string_lossy()
            .to_string()
    });

    let config_path = std::env::var("TCFS_CONFIG").ok();

    // Parent death detection: if the parent process (e.g. Claude Code) exits,
    // we should exit too to avoid orphaned MCP servers.
    let parent_pid = std::os::unix::process::parent_id();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let current_ppid = std::os::unix::process::parent_id();
            if current_ppid != parent_pid {
                tracing::warn!(
                    original_ppid = parent_pid,
                    current_ppid,
                    "parent process changed (reparented to init), exiting"
                );
                std::process::exit(0);
            }
        }
    });

    let server = server::TcfsMcp::new(socket_path.into(), config_path.map(|p| p.into()));

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}
