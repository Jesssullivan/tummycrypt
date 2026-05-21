//! Storage health check

use anyhow::Result;
use opendal::Operator;
use std::time::{Duration, Instant};

/// Default upper bound for storage health probes.
pub const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Machine-readable storage health failure class.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HealthCheckFailureKind {
    Timeout,
    PermissionDenied,
    NotFound,
    RateLimited,
    Backend,
}

impl HealthCheckFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Backend => "backend_error",
        }
    }
}

impl std::fmt::Display for HealthCheckFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Successful storage health probe details.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HealthCheckReport {
    pub path: String,
    pub elapsed_ms: u128,
    pub entry_count: usize,
}

/// Typed storage health probe failure.
#[derive(Debug, thiserror::Error)]
#[error("storage health check {kind} at {path} after {elapsed_ms} ms: {message}")]
pub struct HealthCheckError {
    kind: HealthCheckFailureKind,
    path: String,
    elapsed_ms: u128,
    message: String,
    backend_kind: Option<String>,
}

impl HealthCheckError {
    pub fn kind(&self) -> HealthCheckFailureKind {
        self.kind
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn elapsed_ms(&self) -> u128 {
        self.elapsed_ms
    }

    pub fn backend_kind(&self) -> Option<&str> {
        self.backend_kind.as_deref()
    }

    fn timeout(path: &str, timeout: Duration, elapsed: Duration) -> Self {
        Self {
            kind: HealthCheckFailureKind::Timeout,
            path: path.to_string(),
            elapsed_ms: elapsed.as_millis(),
            message: format!("timed out after {timeout:?}"),
            backend_kind: None,
        }
    }

    fn from_opendal(path: &str, elapsed: Duration, err: opendal::Error) -> Self {
        let backend_kind = err.kind().to_string();
        let kind = match err.kind() {
            opendal::ErrorKind::PermissionDenied => HealthCheckFailureKind::PermissionDenied,
            opendal::ErrorKind::NotFound => HealthCheckFailureKind::NotFound,
            opendal::ErrorKind::RateLimited => HealthCheckFailureKind::RateLimited,
            _ => HealthCheckFailureKind::Backend,
        };

        Self {
            kind,
            path: path.to_string(),
            elapsed_ms: elapsed.as_millis(),
            message: err.to_string(),
            backend_kind: Some(backend_kind),
        }
    }
}

/// Verify the storage endpoint is reachable by listing the root.
pub async fn check_health(op: &Operator) -> Result<()> {
    check_health_with_timeout(op, DEFAULT_HEALTH_TIMEOUT).await
}

/// Verify storage health with an explicit timeout.
pub async fn check_health_with_timeout(op: &Operator, timeout: Duration) -> Result<()> {
    check_health_detailed_with_timeout(op, timeout).await?;
    Ok(())
}

/// Verify storage health and return probe details.
pub async fn check_health_detailed(
    op: &Operator,
) -> std::result::Result<HealthCheckReport, HealthCheckError> {
    check_health_detailed_with_timeout(op, DEFAULT_HEALTH_TIMEOUT).await
}

/// Verify storage health with an explicit timeout and return probe details.
pub async fn check_health_detailed_with_timeout(
    op: &Operator,
    timeout: Duration,
) -> std::result::Result<HealthCheckReport, HealthCheckError> {
    check_health_path_detailed_with_timeout(op, "/", timeout).await
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
    check_health_for_prefix_detailed_with_timeout(op, prefix, timeout).await?;
    Ok(())
}

/// Verify scoped storage health and return probe details.
pub async fn check_health_for_prefix_detailed(
    op: &Operator,
    prefix: &str,
) -> std::result::Result<HealthCheckReport, HealthCheckError> {
    check_health_for_prefix_detailed_with_timeout(op, prefix, DEFAULT_HEALTH_TIMEOUT).await
}

/// Verify scoped storage health with an explicit timeout and return probe details.
pub async fn check_health_for_prefix_detailed_with_timeout(
    op: &Operator,
    prefix: &str,
    timeout: Duration,
) -> std::result::Result<HealthCheckReport, HealthCheckError> {
    let path = health_probe_path(prefix);
    check_health_path_detailed_with_timeout(op, &path, timeout).await
}

fn health_probe_path(prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        "/".to_string()
    } else {
        format!("{prefix}/")
    }
}

async fn check_health_path_detailed_with_timeout(
    op: &Operator,
    path: &str,
    timeout: Duration,
) -> std::result::Result<HealthCheckReport, HealthCheckError> {
    let start = Instant::now();

    match tokio::time::timeout(timeout, op.list(path)).await {
        Ok(Ok(entries)) => Ok(HealthCheckReport {
            path: path.to_string(),
            elapsed_ms: start.elapsed().as_millis(),
            entry_count: entries.len(),
        }),
        Ok(Err(err)) => Err(HealthCheckError::from_opendal(path, start.elapsed(), err)),
        Err(_elapsed) => Err(HealthCheckError::timeout(path, timeout, start.elapsed())),
    }
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

    #[tokio::test]
    async fn detailed_prefix_health_records_path_latency_and_entry_count() {
        let op = memory_operator();
        op.write("tenant-a/index/file.txt", "ok")
            .await
            .expect("seed scoped object");

        let report = check_health_for_prefix_detailed(&op, "tenant-a")
            .await
            .expect("prefix health report");

        assert_eq!(report.path, "tenant-a/");
        assert!(report.entry_count >= 1);
    }

    #[test]
    fn permission_denied_errors_are_classified() {
        let err = HealthCheckError::from_opendal(
            "tenant-a/",
            Duration::from_millis(12),
            opendal::Error::new(opendal::ErrorKind::PermissionDenied, "denied"),
        );

        assert_eq!(err.kind(), HealthCheckFailureKind::PermissionDenied);
        assert_eq!(err.path(), "tenant-a/");
        assert_eq!(err.elapsed_ms(), 12);
        assert_eq!(err.backend_kind(), Some("PermissionDenied"));
    }

    #[test]
    fn rate_limited_errors_are_classified() {
        let err = HealthCheckError::from_opendal(
            "tenant-a/",
            Duration::from_millis(9),
            opendal::Error::new(opendal::ErrorKind::RateLimited, "slow down"),
        );

        assert_eq!(err.kind(), HealthCheckFailureKind::RateLimited);
        assert_eq!(err.backend_kind(), Some("RateLimited"));
    }

    #[test]
    fn timeout_errors_are_classified() {
        let err = HealthCheckError::timeout(
            "tenant-a/",
            Duration::from_secs(5),
            Duration::from_millis(5001),
        );

        assert_eq!(err.kind(), HealthCheckFailureKind::Timeout);
        assert_eq!(err.path(), "tenant-a/");
        assert_eq!(err.elapsed_ms(), 5001);
        assert_eq!(err.backend_kind(), None);
    }

    #[test]
    fn health_probe_path_normalizes_prefixes() {
        assert_eq!(health_probe_path(""), "/");
        assert_eq!(health_probe_path("/"), "/");
        assert_eq!(health_probe_path("tenant-a"), "tenant-a/");
        assert_eq!(health_probe_path("/tenant-a/nested/"), "tenant-a/nested/");
    }
}
