//! Library surface for the `tcfs` CLI binary.
//!
//! Most of the CLI lives in `main.rs` for historical reasons. This library
//! exposes narrowly-scoped, testable helpers that exercise ordering /
//! crash-safety invariants of individual subcommands. The binary does not
//! depend on these helpers at runtime — they duplicate the critical path
//! shape so integration tests can drive it without spinning up gRPC/FUSE/NATS.

pub mod commands;
