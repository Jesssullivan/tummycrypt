#![no_main]
//! Fuzz target: SyncManifest::from_bytes() must never panic on arbitrary input.
//!
//! Valid inputs parse to a manifest; invalid inputs return Err — but never panic.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // from_bytes must not panic regardless of input
    let _ = tcfs_sync::manifest::SyncManifest::from_bytes(data);
});
