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
    let probe = async {
        op.list("/")
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("storage health check failed: {e}"))
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
}
