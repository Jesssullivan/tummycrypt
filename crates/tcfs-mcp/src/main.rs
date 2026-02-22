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

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}
