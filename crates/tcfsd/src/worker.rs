//! Stateless NATS JetStream consumer mode for Kubernetes pods.
//!
//! Enabled via `--features k8s-worker`.  This mode runs tcfsd as a
//! horizontally-scalable worker that consumes SYNC_TASKS from NATS
//! JetStream rather than serving a local FUSE mount.
//!
//! Architecture:
//!   - One NATS pull consumer per pod (durable "sync-workers" consumer)
//!   - Parallel Tokio task pool (configurable concurrency, default = CPU count)
//!   - Each task: fetch → upload_file / download_file → ack / nak
//!   - Prometheus metrics exposed on :9100/metrics (scraped by kube-prometheus)
//!   - Graceful shutdown on SIGTERM (drain in-flight tasks, then exit 0)

#[cfg(feature = "k8s-worker")]
pub use inner::run;

#[cfg(feature = "k8s-worker")]
mod inner {
    use anyhow::{Context, Result};
    use axum::extract::State;
    use futures::StreamExt;
    use prometheus_client::{
        encoding::text::encode,
        metrics::{counter::Counter, family::Family, histogram::Histogram},
        registry::Registry,
    };
    use std::sync::{Arc, Mutex};
    use tokio::{
        signal::unix::{signal, SignalKind},
        sync::{Mutex as TokioMutex, Semaphore},
    };
    use tracing::{error, info, warn};

    use tcfs_sync::nats::{NatsClient, SyncTask};

    // ── Metrics ───────────────────────────────────────────────────────────────

    #[derive(Clone)]
    struct WorkerMetrics {
        tasks_processed: Family<Vec<(String, String)>, Counter>,
        tasks_failed: Family<Vec<(String, String)>, Counter>,
        task_duration: Family<Vec<(String, String)>, Histogram>,
    }

    impl WorkerMetrics {
        fn new(registry: &mut Registry) -> Self {
            let tasks_processed = Family::default();
            let tasks_failed = Family::default();
            let task_duration =
                Family::<Vec<(String, String)>, Histogram>::new_with_constructor(|| {
                    Histogram::new([0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 120.0])
                });

            registry.register(
                "tcfs_worker_tasks_processed_total",
                "Total sync tasks processed successfully",
                tasks_processed.clone(),
            );
            registry.register(
                "tcfs_worker_tasks_failed_total",
                "Total sync tasks that failed after all retries",
                tasks_failed.clone(),
            );
            registry.register(
                "tcfs_worker_task_duration_seconds",
                "Task processing duration in seconds",
                task_duration.clone(),
            );

            WorkerMetrics {
                tasks_processed,
                tasks_failed,
                task_duration,
            }
        }

        fn task_labels(task_type: &str) -> Vec<(String, String)> {
            vec![("task_type".to_string(), task_type.to_string())]
        }
    }

    // ── run() ─────────────────────────────────────────────────────────────────

    pub async fn run(config: tcfs_core::config::TcfsConfig) -> Result<()> {
        info!("tcfsd starting in worker mode (NATS consumer)");

        // Prometheus registry
        let mut registry = Registry::default();
        let metrics = WorkerMetrics::new(&mut registry);
        let registry = Arc::new(Mutex::new(registry));

        // Start metrics HTTP server on :9100/metrics
        let metrics_addr = config
            .daemon
            .metrics_addr
            .clone()
            .unwrap_or_else(|| "0.0.0.0:9100".to_string());
        tokio::spawn(metrics_server(metrics_addr, registry.clone()));

        // Build OpenDAL operator from env credentials
        let access_key = std::env::var("AWS_ACCESS_KEY_ID")
            .or_else(|_| std::env::var("TCFS_ACCESS_KEY_ID"))
            .context("S3 credentials not set: export AWS_ACCESS_KEY_ID")?;
        let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .or_else(|_| std::env::var("TCFS_SECRET_ACCESS_KEY"))
            .context("AWS_SECRET_ACCESS_KEY not set")?;

        let op = tcfs_storage::operator::build_from_core_config(
            &config.storage,
            &access_key,
            &secret_key,
        )
        .context("building storage operator")?;

        // State cache (JSON, shared across tasks with Arc<TokioMutex>)
        let state_path = config.sync.state_db.with_extension("json");
        let state = Arc::new(TokioMutex::new(
            tcfs_sync::state::StateCache::open(&state_path)
                .with_context(|| format!("opening state cache: {}", state_path.display()))?,
        ));

        // Connect to NATS
        let nats: NatsClient = NatsClient::connect(&config.sync.nats_url).await?;
        nats.ensure_streams().await?;

        // Concurrency limit: configurable via TCFS_WORKER_CONCURRENCY or CPU count
        let concurrency = std::env::var("TCFS_WORKER_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
            });
        info!(concurrency, "worker pool ready");
        let semaphore = Arc::new(Semaphore::new(concurrency));

        // Shutdown signal
        let mut sigterm = signal(SignalKind::terminate()).context("registering SIGTERM handler")?;
        let mut sigint = signal(SignalKind::interrupt()).context("registering SIGINT handler")?;
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

        // Task processor loop
        let op_clone = op.clone();
        let state_clone = state.clone();
        let metrics_clone = metrics.clone();
        let sem_clone = semaphore.clone();
        let _shutdown_tx_clone = shutdown_tx.clone();

        let processor = tokio::spawn(async move {
            let task_stream = match nats.task_stream().await {
                Ok(s) => s,
                Err(e) => {
                    error!("failed to open NATS task stream: {e}");
                    return;
                }
            };
            tokio::pin!(task_stream);

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => {
                        info!("worker: shutdown signal received, draining...");
                        break;
                    }
                    Some(msg_result) = task_stream.next() => {
                        let msg = match msg_result {
                            Ok(m) => m,
                            Err(e) => {
                                warn!("error reading NATS message: {e}");
                                continue;
                            }
                        };

                        let permit = sem_clone.clone().acquire_owned().await
                            .expect("semaphore closed");
                        let op = op_clone.clone();
                        let state = state_clone.clone();
                        let metrics = metrics_clone.clone();

                        tokio::spawn(async move {
                            let _permit = permit; // released when task completes
                            execute_task(msg, op, state, metrics).await;
                        });
                    }
                }
            }

            // Drain: wait for all in-flight tasks
            let _ = sem_clone.acquire_many(concurrency as u32).await;
            info!("worker: all in-flight tasks complete");
        });

        // Wait for SIGTERM or SIGINT
        tokio::select! {
            _ = sigterm.recv() => info!("received SIGTERM"),
            _ = sigint.recv() => info!("received SIGINT"),
        }
        let _ = shutdown_tx.send(());
        let _ = processor.await;

        info!("worker exiting cleanly");
        Ok(())
    }

    // ── Task execution ────────────────────────────────────────────────────────

    async fn execute_task(
        msg: tcfs_sync::nats::TaskMessage,
        op: opendal::Operator,
        state: Arc<TokioMutex<tcfs_sync::state::StateCache>>,
        metrics: WorkerMetrics,
    ) {
        let task_type = msg.task.type_name().to_string();
        let task_id = msg.task.task_id().to_string();
        let start = std::time::Instant::now();

        // Send periodic in-progress acks for long-running tasks
        // (not needed for short tasks — ack_wait = 60s)

        let result = dispatch_task(&msg.task, &op, &state).await;
        let elapsed = start.elapsed().as_secs_f64();
        let labels = WorkerMetrics::task_labels(&task_type);

        match result {
            Ok(()) => {
                metrics.tasks_processed.get_or_create(&labels).inc();
                metrics
                    .task_duration
                    .get_or_create(&labels)
                    .observe(elapsed);
                tracing::debug!(task_id, task_type, elapsed_secs = elapsed, "task ok");
                if let Err(e) = msg.ack().await {
                    warn!(task_id, "ack failed: {e}");
                }
            }
            Err(e) => {
                metrics.tasks_failed.get_or_create(&labels).inc();
                error!(task_id, task_type, error = %e, elapsed_secs = elapsed, "task failed");
                if let Err(nak_err) = msg.nak().await {
                    warn!(task_id, "nak failed: {nak_err}");
                }
            }
        }
    }

    async fn dispatch_task(
        task: &SyncTask,
        op: &opendal::Operator,
        state: &Arc<TokioMutex<tcfs_sync::state::StateCache>>,
    ) -> anyhow::Result<()> {
        match task {
            SyncTask::Push {
                local_path,
                remote_prefix,
                ..
            } => {
                let local = std::path::Path::new(local_path);
                let mut guard = state.lock().await;
                if local.is_file() {
                    tcfs_sync::engine::upload_file(op, local, remote_prefix, &mut guard, None)
                        .await
                        .map(|_| ())?;
                } else if local.is_dir() {
                    tcfs_sync::engine::push_tree(op, local, remote_prefix, &mut guard, None)
                        .await
                        .map(|_| ())?;
                } else {
                    anyhow::bail!("push: path not found: {local_path}");
                }
                guard.flush().context("flushing state cache")
            }
            SyncTask::Pull {
                manifest_path,
                remote_prefix,
                local_path,
                ..
            } => {
                let local = std::path::Path::new(local_path);
                tcfs_sync::engine::download_file(op, manifest_path, local, remote_prefix, None)
                    .await
                    .map(|_| ())
            }
            SyncTask::Unsync { local_path, .. } => {
                // Basic unsync: if file exists and is not already a stub, remove it
                // (stub creation is a CLI concern; worker just evicts the local copy)
                let path = std::path::Path::new(local_path);
                if path.exists() && !tcfs_fuse::is_stub_path(path) {
                    tokio::fs::remove_file(path)
                        .await
                        .with_context(|| format!("removing file: {local_path}"))?;
                }
                Ok(())
            }
        }
    }

    // ── Metrics HTTP server ───────────────────────────────────────────────────

    async fn metrics_server(addr: String, registry: Arc<Mutex<Registry>>) {
        use axum::{routing::get, Router};

        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .with_state(registry);

        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!("metrics server: failed to bind {addr}: {e}");
                return;
            }
        };
        info!("metrics: listening on http://{addr}/metrics");
        let _ = axum::serve(listener, app).await;
    }

    async fn metrics_handler(State(registry): State<Arc<Mutex<Registry>>>) -> String {
        let mut buf = String::new();
        let guard = registry.lock().expect("registry lock poisoned");
        encode(&mut buf, &guard).unwrap_or_default();
        buf
    }
}
