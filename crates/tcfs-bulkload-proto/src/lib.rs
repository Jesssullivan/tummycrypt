//! Shared wire types for the tcfs bulkload rebuild (plan workstream G, R32-R35).
//!
//! This crate is the contract between the thin darwin-side agent
//! (`tcfs-bulkload-agent`) and whatever consumes its stream. It holds three
//! things and deliberately nothing else:
//!
//! * [`frame`] -- the postcard frame codec skeleton.
//! * [`row`] -- the scanned-row schema the agent emits.
//! * [`refusal`] -- the [`BulkloadRefusal`] taxonomy.
//!
//! Everything here is `no_std`-friendly in spirit and panic-free in practice:
//! the crate carries the R33 deny-panics wall, so every fallible path returns
//! a `Result` carrying a [`BulkloadRefusal`].

pub mod frame;
pub mod refusal;
pub mod row;

pub use frame::{Frame, FrameKind, PROTO_VERSION};
pub use refusal::BulkloadRefusal;
pub use row::{FileKind, RowSchema};

/// Convenience alias: every fallible bulkload operation refuses with a code.
pub type Result<T> = core::result::Result<T, BulkloadRefusal>;
