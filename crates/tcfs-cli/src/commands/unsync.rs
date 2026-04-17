//! `tcfs unsync` ordering harness.
//!
//! The full `cmd_unsync` lives in `main.rs` and entangles config loading,
//! hash verification, and stub metadata construction. This helper captures
//! only the critical ordering invariant covered by Greptile P2 #3 on PR #301:
//!
//!   1. Flip persisted state to `NotSynced` and flush to disk.
//!   2. Drop the state handle.
//!   3. Perform destructive fs ops (write stub, remove original).
//!
//! If step 3 fails, the on-disk state already reflects reality
//! (`NotSynced` with a possibly-missing stub), which is recoverable by
//! re-hydration. The previous ordering (fs ops first, state flush after)
//! could leave `Synced` on disk even though the hydrated file was gone —
//! the CLI would then lie to the daemon.
//!
//! This shim is only exercised by integration tests under
//! `tests/cmd_unsync_state_ordering.rs` and deliberately takes minimal
//! inputs (no config, no hash check) so the ordering invariant can be
//! asserted without booting the rest of the CLI.

use std::path::Path;

/// Test-only helper mirroring the ordering of the real `cmd_unsync`.
///
/// Flips the state entry for `path` to `NotSynced`, flushes, drops the
/// handle, then writes a placeholder stub at `stub_full` and removes the
/// hydrated original at `path`. Any error from the fs ops is returned to
/// the caller so tests can verify the persisted state regardless.
pub async fn run_for_test(path: &Path, stub_full: &Path, state_path: &Path) -> anyhow::Result<()> {
    // Flip state BEFORE destructive fs ops. If the fs ops later fail, the
    // persisted state already reflects "this file is not hydrated" so a
    // re-hydration pass can recover correctly.
    let mut state = tcfs_sync::state::StateCache::open(state_path)?;
    state.set_status(path, tcfs_sync::state::FileSyncStatus::NotSynced);
    state.flush()?;
    drop(state);

    // Placeholder stub body — the real command writes a StubMeta-encoded
    // payload; for ordering verification any byte slice suffices.
    let stub_bytes = vec![0u8; 16];

    tokio::fs::write(stub_full, &stub_bytes).await?;
    tokio::fs::remove_file(path).await?;
    Ok(())
}
