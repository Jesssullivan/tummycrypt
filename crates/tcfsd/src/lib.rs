//! Library surface for the tcfsd binary.
//!
//! This crate is primarily a binary (`main.rs` ships as `tcfsd`), but we also
//! expose its modules here so integration tests under `tests/` can exercise
//! free functions that would otherwise be unreachable from outside the crate.
//!
//! Keep the surface minimal — only re-export what tests or tooling require.

pub mod cred_store;
pub mod daemon;
pub mod grpc;
pub mod metrics;
pub mod worker;
