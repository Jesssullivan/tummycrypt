//! Native ordinary-file transport and offline provider composition on Unix.
//!
//! # Scope (plan workstream G, R32-R35)
//!
//! The agent walks a corpus, transfers verified chunks in bounded frames,
//! preserves divergent destinations, and composes retained `SQLite` candidates.
//! It still enumerates the source on each run; resumable content completion is
//! not a no-rewalk guarantee. Ordinary-file transfer does not provide Git-native
//! divergent union or raw live `SQLite` copying. It does not talk to object storage, does
//! not speak HTTP or gRPC, and has no async runtime -- see the dependency wall
//! in `Cargo.toml` and the `dep_graph` integration test that enforces it.
//!
//! # Freshness
//!
//! The walker is written against the local [`freshness::FreshnessCache`]
//! trait rather than any concrete cache. PR #586 lands a `freshness.rs` in
//! `tcfs-sync`; the trait here keeps M3 unblocked either way and lets the M0
//! bench swap in a null cache.

pub mod estate;
pub mod freshness;
pub mod git_carry;
pub mod hash;
pub mod materialize;
pub mod provider_sqlite;
pub mod transfer;
pub mod transfer_store;
pub mod walk;

pub use tcfs_bulkload_proto::{BulkloadRefusal, Frame, FrameKind, Result, RowSchema};
