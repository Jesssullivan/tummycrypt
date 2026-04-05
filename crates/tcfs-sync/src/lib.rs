//! tcfs-sync: sync engine with state cache, NATS JetStream, and conflict resolution

pub mod auto_unsync;
pub mod blacklist;
pub mod conflict;
pub mod engine;
pub mod git_safety;
pub mod manifest;
pub mod nats;
pub mod policy;
pub mod reconcile;
pub mod scheduler;
pub mod state;
pub mod watcher;

// Re-export key NATS types for convenience
#[cfg(feature = "nats")]
pub use nats::{NatsClient, StateEvent};
