//! tcfs-sync: sync engine with state cache, NATS JetStream, and conflict resolution

pub mod auto_unsync;
pub mod blacklist;
pub mod conflict;
pub mod conflict_git;
pub mod engine;
pub mod git_safety;
pub mod index_entry;
pub mod manifest;
pub mod nats;
pub mod path_acl;
pub mod policy;
pub mod reconcile;
// Internal acquisition artifact until a held root anchor revalidates inventory C
// across state/remote reads and is the only path that can mint a complete plan.
#[allow(dead_code)]
pub(crate) mod registered_local_snapshot;
pub mod registered_reconcile;
// Diagnostic key-only repeated listing; never a complete namespace snapshot
// or plan-digest input. The bound reader must discard this artifact and rerun
// fresh list+bind work inside each complete source-only pass.
#[allow(dead_code)]
pub(crate) mod registered_remote_observation;
pub mod scheduler;
pub mod state;
pub mod watcher;

// Re-export key NATS types for convenience
#[cfg(feature = "nats")]
pub use nats::{NatsClient, StateEvent};
