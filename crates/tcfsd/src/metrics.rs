//! Prometheus /metrics + health check HTTP endpoints
//!
//! Endpoints:
//!   GET /metrics  — Prometheus text format
//!   GET /healthz  — Liveness probe (always 200 if process is running)
//!   GET /readyz   — Readiness probe (200 if storage is reachable)

use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, gauge::Gauge},
    registry::Registry as PRegistry,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

pub type Registry = PRegistry;

/// Daemon-wide Prometheus metrics.
///
/// All fields are atomic — safe to clone and increment from any async task.
#[derive(Clone)]
pub struct DaemonMetrics {
    pub files_pushed: Counter,
    pub files_pulled: Counter,
    pub sync_conflicts: Counter,
    pub nats_events_published: Counter,
    pub nats_events_received: Counter,
    pub storage_health: Gauge,
}

impl DaemonMetrics {
    /// Create a new metrics set and register all counters with the given registry.
    pub fn new(registry: &mut Registry) -> Self {
        let metrics = Self {
            files_pushed: Counter::default(),
            files_pulled: Counter::default(),
            sync_conflicts: Counter::default(),
            nats_events_published: Counter::default(),
            nats_events_received: Counter::default(),
            storage_health: Gauge::default(),
        };

        registry.register(
            "tcfsd_files_pushed_total",
            "Total files pushed to remote storage",
            metrics.files_pushed.clone(),
        );
        registry.register(
            "tcfsd_files_pulled_total",
            "Total files pulled from remote storage",
            metrics.files_pulled.clone(),
        );
        registry.register(
            "tcfsd_sync_conflicts_total",
            "Total sync conflicts detected",
            metrics.sync_conflicts.clone(),
        );
        registry.register(
            "tcfsd_nats_events_published_total",
            "Total NATS state events published",
            metrics.nats_events_published.clone(),
        );
        registry.register(
            "tcfsd_nats_events_received_total",
            "Total NATS state events received",
            metrics.nats_events_received.clone(),
        );
        registry.register(
            "tcfsd_storage_health",
            "Storage backend health (1=healthy, 0=unreachable)",
            metrics.storage_health.clone(),
        );

        metrics
    }
}

/// Shared health state updated by the daemon
#[derive(Clone)]
pub struct HealthState {
    pub registry: Arc<Registry>,
    pub operator: Arc<TokioMutex<Option<opendal::Operator>>>,
}

/// Serve Prometheus metrics and health endpoints on `addr` (e.g. "127.0.0.1:9100")
pub async fn serve(addr: String, state: HealthState) -> Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("metrics bind {addr}: {e}"))?;

    tracing::info!(addr = %addr, "metrics: listening on /metrics, /healthz, /readyz");

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("metrics server: {e}"))
}

async fn metrics_handler(State(state): State<HealthState>) -> impl IntoResponse {
    let mut body = String::new();
    match encode(&mut body, &state.registry) {
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

/// Liveness probe: returns 200 if the process is running.
async fn healthz_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Readiness probe: returns 200 if storage is reachable, 503 otherwise.
async fn readyz_handler(State(state): State<HealthState>) -> impl IntoResponse {
    let op = state.operator.lock().await;
    match op.as_ref() {
        Some(op) => match tcfs_storage::check_health(op).await {
            Ok(()) => (StatusCode::OK, "ready"),
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "storage unreachable"),
        },
        None => (StatusCode::SERVICE_UNAVAILABLE, "no storage operator"),
    }
}
