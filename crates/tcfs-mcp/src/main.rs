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

    let socket_path =
        std::env::var("TCFS_SOCKET").unwrap_or_else(|_| "/run/tcfsd/tcfsd.sock".to_string());

    let config_path = std::env::var("TCFS_CONFIG").ok();

    let server = server::TcfsMcp::new(socket_path.into(), config_path.map(|p| p.into()));

    // Spawn parent process death watcher (prevents orphaned MCP processes)
    tokio::spawn(async move {
        watch_parent_exit().await;
        tracing::info!("parent process exited, shutting down MCP server");
        std::process::exit(0);
    });

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}

/// Watch for parent process exit to prevent orphaned MCP processes.
///
/// MCP servers communicate over stdio. When the parent process exits,
/// stdin is closed (broken pipe). We also enforce a 1-hour inactivity
/// timeout as a safety net.
async fn watch_parent_exit() {
    // The MCP runtime owns stdin for JSON-RPC. We can't read from it directly.
    // Instead, use a simple ppid polling approach.
    #[cfg(unix)]
    {
        let initial_ppid = std::os::unix::process::parent_id();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let current_ppid = std::os::unix::process::parent_id();
            if current_ppid != initial_ppid {
                return; // Parent changed → original parent died
            }
        }
    }

    #[cfg(not(unix))]
    {
        // 1-hour safety timeout on non-Unix
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
