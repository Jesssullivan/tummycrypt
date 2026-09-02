//! The tcfs bulkload agent: the thin darwin-side half of the rebuild.
//!
//! # Scope (plan workstream G, R32-R35)
//!
//! The agent walks a corpus, stats and optionally hashes each seat, and emits
//! [`tcfs_bulkload_proto::Frame`]s. It does not talk to object storage, does
//! not speak HTTP or gRPC, and has no async runtime -- see the dependency wall
//! in `Cargo.toml` and the `dep_graph` integration test that enforces it.
//!
//! # Freshness
//!
//! The walker is written against the local [`freshness::FreshnessCache`]
//! trait rather than any concrete cache. PR #586 lands a `freshness.rs` in
//! `tcfs-sync`; the trait here keeps M3 unblocked either way and lets the M0
//! bench swap in a null cache.

pub mod freshness;
pub mod hash;
pub mod walk;

pub use tcfs_bulkload_proto::{BulkloadRefusal, Frame, FrameKind, Result, RowSchema};
