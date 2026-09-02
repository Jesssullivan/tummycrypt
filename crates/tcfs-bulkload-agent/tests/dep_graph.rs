//! M1 acceptance gate: the agent's dependency wall (R34).
//!
//! The agent half is the thin darwin-side process. It must not acquire an
//! async runtime, an object-store abstraction, an HTTP client, a TLS stack, or
//! a gRPC stack -- not directly and not transitively through a well-meaning
//! shared crate. This test shells out to `cargo tree` and reads the real
//! resolved graph rather than trusting the manifest, because the hazard is
//! precisely the dependency nobody wrote down.
//!
//! If this test fails, the fix is to remove the dependency, not to widen the
//! list.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

/// Crates that must never appear in the agent's normal dependency graph.
///
/// A match is exact on the package name, or on the name plus a `-` (so
/// `tokio-util` is caught alongside `tokio`, while `unicode-normalization`
/// is not mistaken for `ring`).
const FORBIDDEN: &[&str] = &["tokio", "opendal", "reqwest", "ring", "tonic"];

fn is_forbidden(name: &str) -> Option<&'static str> {
    FORBIDDEN.iter().copied().find(|forbidden| {
        name == *forbidden || name.starts_with(&format!("{forbidden}-"))
    })
}

/// Pull the package name out of one `cargo tree` line.
///
/// Lines look like `│   ├── serde v1.0.210` or `└── tokio v1.40.0 (*)`.
fn package_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(|c: char| {
        matches!(c, '│' | '├' | '└' | '─' | ' ' | '|' | '`' | '-' | '+')
    });
    let name = trimmed.split_whitespace().next()?;
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[test]
fn agent_dependency_graph_has_no_forbidden_crates() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let output = Command::new(&cargo)
        .args([
            "tree",
            "-p",
            "tcfs-bulkload-agent",
            "-e",
            "normal",
            "--prefix",
            "none",
            "--no-dedupe",
        ])
        .current_dir(manifest_dir)
        .output()
        .expect("failed to run `cargo tree`; the dep-graph wall cannot be checked");

    assert!(
        output.status.success(),
        "`cargo tree` failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut packages = 0_usize;
    let mut violations: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let Some(name) = package_name(line) else {
            continue;
        };
        packages += 1;
        if let Some(forbidden) = is_forbidden(name) {
            violations.push(format!("{name} (matches forbidden `{forbidden}`)"));
        }
    }

    assert!(
        packages > 1,
        "`cargo tree` produced no packages; the wall was not actually checked:\n{stdout}"
    );

    violations.sort_unstable();
    violations.dedup();
    assert!(
        violations.is_empty(),
        "tcfs-bulkload-agent must not depend on {FORBIDDEN:?}, but its graph contains:\n  {}\n\n\
         full tree:\n{stdout}",
        violations.join("\n  ")
    );
}

/// Guard the parser itself: a matcher that silently matched nothing would make
/// the wall above pass unconditionally.
#[test]
fn forbidden_matcher_is_neither_too_broad_nor_too_narrow() {
    assert_eq!(is_forbidden("tokio"), Some("tokio"));
    assert_eq!(is_forbidden("tokio-util"), Some("tokio"));
    assert_eq!(is_forbidden("ring"), Some("ring"));
    assert_eq!(is_forbidden("tonic-prost"), Some("tonic"));

    assert_eq!(is_forbidden("unicode-normalization"), None);
    assert_eq!(is_forbidden("blake3"), None);
    assert_eq!(is_forbidden("ignore"), None);
    assert_eq!(is_forbidden("rusqlite"), None);
    assert_eq!(is_forbidden("stringprep"), None);

    assert_eq!(package_name("├── serde v1.0.210"), Some("serde"));
    assert_eq!(package_name("│   └── tokio v1.40.0 (*)"), Some("tokio"));
    assert_eq!(package_name("tcfs-bulkload-agent v0.12.14"), Some("tcfs-bulkload-agent"));
    assert_eq!(package_name("   "), None);
}
