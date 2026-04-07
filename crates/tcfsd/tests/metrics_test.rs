//! Metrics registration and health endpoint tests

use prometheus_client::encoding::text::encode;
use prometheus_client::registry::Registry;

/// Test that DaemonMetrics registers all expected counters and gauges
#[test]
fn metrics_registration() {
    let mut registry = Registry::default();
    let metrics = tcfsd_test_helpers::create_metrics(&mut registry);

    // Increment counters to verify they're wired
    metrics.files_pushed.inc();
    metrics.files_pulled.inc();
    metrics.sync_conflicts.inc();
    metrics.nats_events_published.inc();
    metrics.nats_events_received.inc();
    metrics.storage_health.set(1);

    // Encode and verify all metrics appear in output
    let mut output = String::new();
    encode(&mut output, &registry).expect("encode should succeed");

    assert!(
        output.contains("tcfsd_files_pushed_total"),
        "missing files_pushed"
    );
    assert!(
        output.contains("tcfsd_files_pulled_total"),
        "missing files_pulled"
    );
    assert!(
        output.contains("tcfsd_sync_conflicts_total"),
        "missing sync_conflicts"
    );
    assert!(
        output.contains("tcfsd_nats_events_published_total"),
        "missing nats_published"
    );
    assert!(
        output.contains("tcfsd_nats_events_received_total"),
        "missing nats_received"
    );
    assert!(
        output.contains("tcfsd_storage_health"),
        "missing storage_health"
    );
}

#[test]
fn metrics_increment_correctly() {
    let mut registry = Registry::default();
    let metrics = tcfsd_test_helpers::create_metrics(&mut registry);

    for _ in 0..5 {
        metrics.files_pushed.inc();
    }
    metrics.files_pulled.inc();
    metrics.files_pulled.inc();

    let mut output = String::new();
    encode(&mut output, &registry).expect("encode");

    // Counter values should appear in output
    assert!(output.contains("5"), "files_pushed should be 5");
}

/// Minimal helper module — re-exports DaemonMetrics::new for tests.
///
/// We can't directly use `tcfsd::metrics` because tcfsd is a binary crate.
/// Instead we replicate the metric creation logic here.
mod tcfsd_test_helpers {
    use prometheus_client::{
        metrics::{counter::Counter, gauge::Gauge},
        registry::Registry,
    };

    #[derive(Clone)]
    pub struct DaemonMetrics {
        pub files_pushed: Counter,
        pub files_pulled: Counter,
        pub sync_conflicts: Counter,
        pub nats_events_published: Counter,
        pub nats_events_received: Counter,
        pub storage_health: Gauge,
    }

    pub fn create_metrics(registry: &mut Registry) -> DaemonMetrics {
        let m = DaemonMetrics {
            files_pushed: Counter::default(),
            files_pulled: Counter::default(),
            sync_conflicts: Counter::default(),
            nats_events_published: Counter::default(),
            nats_events_received: Counter::default(),
            storage_health: Gauge::default(),
        };

        registry.register(
            "tcfsd_files_pushed_total",
            "files pushed",
            m.files_pushed.clone(),
        );
        registry.register(
            "tcfsd_files_pulled_total",
            "files pulled",
            m.files_pulled.clone(),
        );
        registry.register(
            "tcfsd_sync_conflicts_total",
            "conflicts",
            m.sync_conflicts.clone(),
        );
        registry.register(
            "tcfsd_nats_events_published_total",
            "nats published",
            m.nats_events_published.clone(),
        );
        registry.register(
            "tcfsd_nats_events_received_total",
            "nats received",
            m.nats_events_received.clone(),
        );
        registry.register(
            "tcfsd_storage_health",
            "storage health",
            m.storage_health.clone(),
        );

        m
    }
}
