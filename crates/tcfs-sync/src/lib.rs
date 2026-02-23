//! tcfs-sync: sync engine with state cache, NATS JetStream, and conflict resolution

pub mod conflict;
pub mod engine;
pub mod git_safety;
pub mod manifest;
pub mod nats;
pub mod scheduler;
pub mod state;
pub mod watcher;

// Re-export key NATS types for convenience
#[cfg(feature = "nats")]
pub use nats::{NatsClient, StateEvent};
