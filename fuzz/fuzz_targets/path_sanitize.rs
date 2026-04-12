#![no_main]
//! Fuzz target: normalize_rel_path must never panic on arbitrary path strings.
//!
//! Properties checked:
//! - Never panics
//! - Output never starts with '/' (leading slash always stripped)
//! - Output never contains backslashes (normalized to forward slash)

use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    // Test without sync_root
    let result = tcfs_sync::engine::normalize_rel_path(Path::new(input), None);
    assert!(
        !result.starts_with('/'),
        "output must not start with /: {result}"
    );
    assert!(
        !result.contains('\\'),
        "output must not contain backslash: {result}"
    );

    // Test with a sync_root
    let tmp = std::env::temp_dir();
    let result2 = tcfs_sync::engine::normalize_rel_path(Path::new(input), Some(&tmp));
    assert!(
        !result2.starts_with('/'),
        "output must not start with / (with root): {result2}"
    );
    assert!(
        !result2.contains('\\'),
        "output must not contain backslash (with root): {result2}"
    );
});
