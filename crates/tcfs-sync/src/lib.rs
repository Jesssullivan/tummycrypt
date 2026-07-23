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
// Descriptor-held, two-pass GitRaw topology/ref evidence. This is a bounded
// local prerequisite only and has no digest, plan, action, or serialization
// surface.
#[allow(dead_code)]
pub(crate) mod registered_git_topology;
// Internal acquisition artifact until a held root anchor revalidates inventory C
// across state/remote reads and is the only path that can mint a complete plan.
#[allow(dead_code)]
pub(crate) mod registered_local_snapshot;
pub mod registered_reconcile;
// Strict remote-observation stages. The diagnostic key-only artifact is never
// a snapshot or plan input; the bound reader independently reruns fresh
// list+bind work in each non-atomic source-only evidence pass.
#[allow(dead_code)]
pub(crate) mod registered_remote_observation;
// Immutable catalog-head closure. This validates only the catalog inventory;
// writer fencing, bootstrap truth, and every named object remain mandatory
// before the artifact can satisfy complete-or-no-digest.
#[allow(dead_code)]
pub(crate) mod registered_remote_catalog;
// Held-window composition of selected-root diagnostic and catalog-bound
// observation evidence. Even the internally closed catalog revision lacks
// writer-fence/bootstrap/currentness authority, so neither path has a plan
// digest, action conversion, or serialization surface.
#[allow(dead_code)]
pub(crate) mod registered_source_composition;
pub mod scheduler;
pub mod state;
pub mod watcher;

// Re-export key NATS types for convenience
#[cfg(feature = "nats")]
pub use nats::{NatsClient, StateEvent};
