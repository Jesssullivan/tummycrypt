//! Prometheus /metrics HTTP endpoint

use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use prometheus_client::{encoding::text::encode, registry::Registry as PRegistry};
use std::sync::Arc;

pub type Registry = PRegistry;

/// Serve Prometheus metrics on `addr` (e.g. "127.0.0.1:9100")
pub async fn serve(addr: String, registry: Arc<Registry>) -> Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(registry);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("metrics bind {addr}: {e}"))?;

    tracing::info!(addr = %addr, "metrics: listening on /metrics");

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("metrics server: {e}"))
}

async fn metrics_handler(State(registry): State<Arc<Registry>>) -> impl IntoResponse {
    let mut body = String::new();
    match encode(&mut body, &registry) {
        Ok(()) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4")],
            body,
        ),
        Err(e) => {
            tracing::error!("metrics encode failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [("content-type", "text/plain")],
                e.to_string(),
            )
        }
    }
}
