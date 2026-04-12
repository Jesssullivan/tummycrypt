//! Prometheus /metrics + health check HTTP endpoints
//!
//! Endpoints:
//!   GET /metrics  — Prometheus text format
//!   GET /healthz  — Liveness probe (always 200 if process is running)
//!   GET /readyz   — Readiness probe (200 if storage is reachable)
//!   GET /livez   — FUSE mount probe (200 if sync_root is stat-able within 5s)

use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, gauge::Gauge},
    registry::Registry as PRegistry,
};
use std::path::PathBuf;
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
    pub fuse_health: Gauge,
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
            fuse_health: Gauge::default(),
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
        registry.register(
            "tcfsd_fuse_health",
            "FUSE mount health (1=responsive, 0=stale/unresponsive)",
            metrics.fuse_health.clone(),
        );

        metrics
    }
}

/// Shared health state updated by the daemon
#[derive(Clone)]
pub struct HealthState {
    pub registry: Arc<Registry>,
    pub operator: Arc<TokioMutex<Option<opendal::Operator>>>,
    /// Sync root path for FUSE mount liveness probing
    pub sync_root: Option<PathBuf>,
}

/// Serve Prometheus metrics and health endpoints on `addr` (e.g. "127.0.0.1:9100")
pub async fn serve(addr: String, state: HealthState) -> Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .route("/livez", get(livez_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("metrics bind {addr}: {e}"))?;

    tracing::info!(addr = %addr, "metrics: listening on /metrics, /healthz, /readyz, /livez");

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

/// FUSE mount liveness probe: returns 200 if sync_root is stat-able within 5 seconds.
///
/// A stale FUSE mount (transport endpoint disconnected) will cause stat() to hang
/// indefinitely. This handler wraps the stat in a timeout to detect that condition.
async fn livez_handler(State(state): State<HealthState>) -> impl IntoResponse {
    let Some(ref sync_root) = state.sync_root else {
        return (StatusCode::OK, "live (no sync_root configured)");
    };

    let path = sync_root.clone();
    let probe = tokio::task::spawn_blocking(move || std::fs::metadata(&path));

    match tokio::time::timeout(std::time::Duration::from_secs(5), probe).await {
        Ok(Ok(Ok(_meta))) => (StatusCode::OK, "live"),
        Ok(Ok(Err(_io_err))) => {
            // stat failed (e.g. path doesn't exist yet) — daemon is alive, mount may not be up
            (StatusCode::OK, "live (sync_root not mounted)")
        }
        Ok(Err(_join_err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "live probe task panicked",
        ),
        Err(_timeout) => {
            if let Some(ref root) = state.sync_root {
                tracing::warn!(path = %root.display(),
                    "FUSE mount probe timed out — mount likely stale");
            }
            (StatusCode::SERVICE_UNAVAILABLE, "fuse mount unresponsive")
        }
    }
}
