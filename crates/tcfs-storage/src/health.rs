//! Storage health check

use anyhow::Result;
use opendal::Operator;

/// Verify the storage endpoint is reachable by listing the root
pub async fn check_health(op: &Operator) -> Result<()> {
    // A simple stat on the root is the lightest health check
    op.list("/")
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("storage health check failed: {e}"))
}

/// Returns true if storage is reachable, false otherwise (non-panicking)
pub async fn is_healthy(op: &Operator) -> bool {
    check_health(op).await.is_ok()
}
