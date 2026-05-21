//! Storage health check

use anyhow::Result;
use opendal::Operator;
use std::time::Duration;

/// Default upper bound for storage health probes.
pub const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Verify the storage endpoint is reachable by listing the root.
pub async fn check_health(op: &Operator) -> Result<()> {
    check_health_with_timeout(op, DEFAULT_HEALTH_TIMEOUT).await
}

/// Verify storage health with an explicit timeout.
pub async fn check_health_with_timeout(op: &Operator, timeout: Duration) -> Result<()> {
    check_health_path_with_timeout(op, "/", timeout).await
}

/// Verify the storage endpoint is reachable using a scoped remote prefix.
///
/// Production credentials may be limited to a tenant or run prefix. In that
/// case bucket-root `ListBucket` can be denied even though the configured tcfs
/// prefix is fully usable.
pub async fn check_health_for_prefix(op: &Operator, prefix: &str) -> Result<()> {
    check_health_for_prefix_with_timeout(op, prefix, DEFAULT_HEALTH_TIMEOUT).await
}

/// Verify storage health for a scoped remote prefix with an explicit timeout.
pub async fn check_health_for_prefix_with_timeout(
    op: &Operator,
    prefix: &str,
    timeout: Duration,
) -> Result<()> {
    let path = health_probe_path(prefix);
    check_health_path_with_timeout(op, &path, timeout).await
}

fn health_probe_path(prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        "/".to_string()
    } else {
        format!("{prefix}/")
    }
}

async fn check_health_path_with_timeout(
    op: &Operator,
    path: &str,
    timeout: Duration,
) -> Result<()> {
    let probe = async {
        op.list(path)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("storage health check failed at {path}: {e}"))
    };

    tokio::time::timeout(timeout, probe)
        .await
        .map_err(|_| anyhow::anyhow!("storage health check timed out after {timeout:?}"))?
}

/// Returns true if storage is reachable, false otherwise (non-panicking)
pub async fn is_healthy(op: &Operator) -> bool {
    check_health(op).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Memory;

    fn memory_operator() -> Operator {
        Operator::new(Memory::default()).unwrap().finish()
    }

    #[tokio::test]
    async fn memory_operator_is_healthy() {
        let op = memory_operator();

        check_health(&op).await.expect("memory health check");
        assert!(is_healthy(&op).await);
    }

    #[tokio::test]
    async fn explicit_health_timeout_is_supported() {
        let op = memory_operator();

        check_health_with_timeout(&op, Duration::from_secs(1))
            .await
            .expect("memory health check with explicit timeout");
    }

    #[tokio::test]
    async fn prefix_health_check_lists_scoped_prefix() {
        let op = memory_operator();
        op.write("tenant-a/index/file.txt", "ok")
            .await
            .expect("seed scoped object");

        check_health_for_prefix(&op, "tenant-a")
            .await
            .expect("prefix health check");
        check_health_for_prefix_with_timeout(&op, "/tenant-a/", Duration::from_secs(1))
            .await
            .expect("prefix health check with explicit timeout");
    }

    #[test]
    fn health_probe_path_normalizes_prefixes() {
        assert_eq!(health_probe_path(""), "/");
        assert_eq!(health_probe_path("/"), "/");
        assert_eq!(health_probe_path("tenant-a"), "tenant-a/");
        assert_eq!(health_probe_path("/tenant-a/nested/"), "tenant-a/nested/");
    }
}
