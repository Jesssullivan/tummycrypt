//! D4 uniform-root slug encoding (TIN-1556).
//!
//! Agent-session state (Claude-style) keys a working directory two ways at
//! once: by a *slug* derived from the absolute path (the
//! `~/.claude/projects/<slug>/` directory name) and by the literal absolute
//! path recorded inside session records. The TIN-2301 probe falsified the
//! symlink shim as a complete answer, so the
//! [stable-root-lifecycle ADR][adr] adopts **D4**: bind roam-first roots at a
//! *uniform absolute prefix that is identical on every host*, so the slug, the
//! registry key, and every embedded path agree with zero rewriting.
//!
//! Operator ruling (2026-08-26, ADR Q1): the uniform prefix is
//! [`UNIFORM_ROOT_PREFIX`]`/<root_id>` — i.e. `/tcfs/<root_id>`. `~/tcfs/...`
//! was rejected precisely because `~` differs across operating systems, so
//! only the *suffix* would be uniform and full-path-slug healing would break
//! for exactly the macOS↔Linux pairs D4 exists to serve.
//!
//! This module is **pure**: no filesystem probing, no I/O, no daemon state,
//! and (in this change) no call sites. It supplies the encode/decode/validate
//! primitives that the D1 adopt inventory and the TIN-2301 resume proof will
//! consume later.
//!
//! # The encoding
//!
//! The host-native slug rule is *replace every character that is not an ASCII
//! alphanumeric with `-`*, preserving case:
//!
//! | absolute path | slug |
//! |---|---|
//! | `/Users/jess/x` | `-Users-jess-x` |
//! | `/home/jess/x` | `-home-jess-x` |
//! | `/tcfs/r1/x` | `-tcfs-r1-x` |
//! | `/Users/jess/git/gf/.worktrees/a-b` | `-Users-jess-git-gf--worktrees-a-b` |
//!
//! That rule was derived from, and checked against, 96 real host slugs paired
//! with the absolute working directory recorded inside their own session
//! records (0 mismatches). The two weaker rules one might guess — "replace
//! `/` only", and "replace `/` and `.` only" — mismatch 75/96 and 3/96
//! respectively, so they are wrong and are not implemented here. The `_` and
//! `.` cases are the ones that discriminate: `/Users/jess/git/spear_resumes`
//! slugs as `-Users-jess-git-spear-resumes`, and a leading-dot segment such
//! as `/.worktrees` produces the doubled `--worktrees`.
//!
//! # Why decode is partial
//!
//! Encoding is deliberately lossy: `/`, `.`, `_` and `-` all collapse to `-`.
//! Two decoders are therefore offered, and neither guesses:
//!
//! * [`decode_path_slug`] is defined only on the *reversible subset* — slugs
//!   whose every segment is a non-empty ASCII alphanumeric run. It fails
//!   closed on anything else rather than inventing a separator.
//! * [`decode_uniform_slug`] is *root-anchored*: given the `root_id` it
//!   strips the known `-tcfs-<root_id>` prefix at a segment boundary and only
//!   then decodes the remainder. This is what makes `/tcfs/<id>/x ↔
//!   -tcfs-<id>-x` exact even when `<id>` itself contains `-`.
//!
//! # Why `root_id` must be slug-stable
//!
//! [`crate::config::validate_registered_root_id`] admits `.` and `_`, which
//! are legal registry identifiers but **encode to `-`**. A root named
//! `foo.bar` and a root named `foo-bar` would share the slug prefix
//! `-tcfs-foo-bar`, silently aliasing two distinct roots' agent state.
//! [`validate_uniform_root_id`] therefore narrows the alphabet for roots that
//! opt into a uniform binding: the id must satisfy `encode_path_slug(id) ==
//! id`. Roots that cannot honor this stay `UNBOUND` and fail closed, per the
//! ADR.
//!
//! [adr]: ../../../docs/design/stable-root-lifecycle-tin1556-2026-07-28.md

use crate::config::validate_registered_root_id;

/// The reserved uniform absolute prefix (ADR D4, Q1 ruling 2026-08-26).
///
/// A root-owned directory created once per host at enrollment. Bindings live
/// at `{UNIFORM_ROOT_PREFIX}/{root_id}`.
pub const UNIFORM_ROOT_PREFIX: &str = "/tcfs";

/// The single character every non-alphanumeric byte collapses to.
pub const SLUG_SEPARATOR: char = '-';

/// Encode an absolute path into a host-native agent-session slug.
///
/// Total and infallible, matching the observed host rule exactly: every
/// character that is not an ASCII alphanumeric becomes [`SLUG_SEPARATOR`];
/// case is preserved. A leading `/` therefore yields the leading `-` that
/// makes slugs self-identifying.
///
/// Non-ASCII input is accepted but is **not** guaranteed to agree with the
/// host encoder byte-for-byte: the reference implementation replaces per
/// UTF-16 code unit, so a non-BMP scalar yields two separators there and one
/// here. Guard with [`path_is_ascii`] before relying on cross-implementation
/// agreement; [`validate_uniform_root_binding`] enforces that guard.
pub fn encode_path_slug(path: &str) -> String {
    path.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                SLUG_SEPARATOR
            }
        })
        .collect()
}

/// True when every character is ASCII, i.e. when [`encode_path_slug`] is
/// guaranteed to agree with the reference host encoder.
pub fn path_is_ascii(path: &str) -> bool {
    path.is_ascii()
}

/// True when `path` is an absolute path that [`decode_path_slug`] can recover
/// exactly: at least one segment, every segment a non-empty ASCII
/// alphanumeric run.
pub fn path_is_slug_reversible(path: &str) -> bool {
    match path.strip_prefix('/') {
        Some(body) if !body.is_empty() => body.split('/').all(segment_is_reversible),
        _ => false,
    }
}

/// True when `relative` is a non-empty relative path whose every segment is a
/// non-empty ASCII alphanumeric run (so it survives a slug round trip).
pub fn relative_is_slug_reversible(relative: &str) -> bool {
    !relative.is_empty() && relative.split('/').all(segment_is_reversible)
}

fn segment_is_reversible(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

/// Why a slug could not be decoded back to an absolute path.
///
/// Decoding fails closed rather than guessing which separator a `-` stood
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlugDecodeError {
    /// The slug was the empty string.
    Empty,
    /// The slug did not begin with [`SLUG_SEPARATOR`], so it does not encode
    /// an absolute path.
    MissingLeadingSeparator,
    /// Segment `index` (0-based, after the leading separator) was empty — the
    /// signature of a collapsed `/.`, `//`, or trailing separator, which
    /// cannot be reconstructed.
    EmptySegment { index: usize },
    /// Segment `index` contained a character that [`encode_path_slug`] would
    /// itself have replaced, so the slug was not produced by this encoder.
    NonAlphanumericSegment { index: usize, segment: String },
}

impl std::fmt::Display for SlugDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "empty slug"),
            Self::MissingLeadingSeparator => write!(
                formatter,
                "slug must start with '{SLUG_SEPARATOR}' to encode an absolute path"
            ),
            Self::EmptySegment { index } => write!(
                formatter,
                "slug segment {index} is empty: the original separator run is not recoverable"
            ),
            Self::NonAlphanumericSegment { index, segment } => write!(
                formatter,
                "slug segment {index} '{segment}' contains a character the slug encoder would have replaced"
            ),
        }
    }
}

impl std::error::Error for SlugDecodeError {}

/// Decode a host-native slug back into an absolute path.
///
/// Defined only on the reversible subset described in the module docs; see
/// [`decode_uniform_slug`] for the root-anchored decoder that handles ids and
/// relatives containing `-`.
pub fn decode_path_slug(slug: &str) -> Result<String, SlugDecodeError> {
    if slug.is_empty() {
        return Err(SlugDecodeError::Empty);
    }
    let body = slug
        .strip_prefix(SLUG_SEPARATOR)
        .ok_or(SlugDecodeError::MissingLeadingSeparator)?;
    if body.is_empty() {
        return Err(SlugDecodeError::EmptySegment { index: 0 });
    }
    let segments = decode_segments(body)?;
    Ok(format!("/{}", segments.join("/")))
}

fn decode_segments(body: &str) -> Result<Vec<&str>, SlugDecodeError> {
    let mut segments = Vec::new();
    for (index, segment) in body.split(SLUG_SEPARATOR).enumerate() {
        if segment.is_empty() {
            return Err(SlugDecodeError::EmptySegment { index });
        }
        if !segment
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        {
            return Err(SlugDecodeError::NonAlphanumericSegment {
                index,
                segment: segment.to_string(),
            });
        }
        segments.push(segment);
    }
    Ok(segments)
}

/// The uniform absolute binding path for `root_id`: `/tcfs/<root_id>`.
///
/// Identical on every host by construction — that identity is the whole point
/// of D4.
pub fn uniform_root_path(root_id: &str) -> String {
    format!("{UNIFORM_ROOT_PREFIX}/{root_id}")
}

/// The slug of [`uniform_root_path`]: `-tcfs-<root_id>`.
pub fn uniform_root_slug(root_id: &str) -> String {
    encode_path_slug(&uniform_root_path(root_id))
}

/// Validate that `root_id` may carry a uniform `/tcfs/<root_id>` binding.
///
/// Applies every registry rule from
/// [`crate::config::validate_registered_root_id`] and then the D4-specific
/// narrowing: the id must be *slug-stable* (`encode_path_slug(id) == id`, so
/// no `.` and no `_`) and must not end in [`SLUG_SEPARATOR`], because a
/// trailing separator makes the prefix boundary ambiguous against the first
/// relative segment.
pub fn validate_uniform_root_id(root_id: &str) -> Result<(), String> {
    validate_registered_root_id(root_id)?;
    if encode_path_slug(root_id) != root_id {
        return Err(format!(
            "root id '{root_id}' is not slug-stable: '.' and '_' both encode to '{SLUG_SEPARATOR}', so a uniform /tcfs/<root_id> binding would alias distinct roots; use only lowercase ASCII letters, digits and '{SLUG_SEPARATOR}'"
        ));
    }
    if root_id.ends_with(SLUG_SEPARATOR) {
        return Err(format!(
            "root id '{root_id}' must not end with '{SLUG_SEPARATOR}': the uniform slug prefix boundary would be ambiguous"
        ));
    }
    Ok(())
}

/// Validate a uniform-prefix binding: `local_root` must be exactly
/// `/tcfs/<root_id>` for a slug-stable `root_id`.
///
/// This is the D4 sibling of
/// [`crate::config::validate_registered_root_id`] — that one validates the
/// fleet-stable identity, this one validates the host-local *convention on
/// top of* the binding contract. A host that cannot honor the prefix must
/// stay `UNBOUND`; it must never be bound to a near-miss path, because a
/// near-miss silently produces a divergent slug tree.
///
/// A single trailing `/` is tolerated and normalized away; anything else
/// (relative paths, `~`-relative paths, `..`, extra segments, case drift) is
/// rejected with both the given and expected paths in the message.
pub fn validate_uniform_root_binding(local_root: &str, root_id: &str) -> Result<(), String> {
    validate_uniform_root_id(root_id)?;
    if !path_is_ascii(local_root) {
        return Err(format!(
            "uniform root binding '{local_root}' must be ASCII: non-ASCII paths are not guaranteed to slug identically across hosts"
        ));
    }
    let expected = uniform_root_path(root_id);
    let normalized = match local_root.strip_suffix('/') {
        Some(trimmed) if !trimmed.is_empty() => trimmed,
        Some(_) => local_root,
        None => local_root,
    };
    if normalized != expected {
        return Err(format!(
            "uniform root binding for '{root_id}' must be exactly '{expected}', got '{local_root}'"
        ));
    }
    Ok(())
}

/// Build the uniform slug for `relative` under `root_id`.
///
/// `relative` is a path *relative to the root* (no leading or trailing `/`,
/// no empty, `.` or `..` segments). An empty `relative` yields the root's own
/// slug.
pub fn uniform_slug_for(root_id: &str, relative: &str) -> Result<String, String> {
    validate_uniform_root_id(root_id)?;
    if relative.is_empty() {
        return Ok(uniform_root_slug(root_id));
    }
    validate_relative_path(relative)?;
    Ok(encode_path_slug(&format!(
        "{}/{relative}",
        uniform_root_path(root_id)
    )))
}

fn validate_relative_path(relative: &str) -> Result<(), String> {
    if !relative.is_ascii() {
        return Err(format!(
            "relative path '{relative}' must be ASCII to slug identically across hosts"
        ));
    }
    if relative.starts_with('/') || relative.ends_with('/') {
        return Err(format!(
            "relative path '{relative}' must not start or end with '/'"
        ));
    }
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!(
                "relative path '{relative}' must not contain empty, '.' or '..' segments"
            ));
        }
    }
    Ok(())
}

/// Strip `prefix` from `slug` at a segment boundary.
///
/// Returns the remainder, which is either empty (exact match) or begins with
/// [`SLUG_SEPARATOR`]. Returns `None` when `slug` is not under `prefix` —
/// crucially, `-Users-jess-git` does **not** prefix-match
/// `-Users-jess-gitfoo`, which a naive `starts_with` would wrongly accept.
pub fn strip_slug_prefix<'slug>(slug: &'slug str, prefix: &str) -> Option<&'slug str> {
    if prefix.is_empty() {
        return None;
    }
    let remainder = slug.strip_prefix(prefix)?;
    if remainder.is_empty() || remainder.starts_with(SLUG_SEPARATOR) {
        Some(remainder)
    } else {
        None
    }
}

/// Decode a uniform slug back to the root-relative path it names.
///
/// Root-anchored, so it is exact for any slug-stable `root_id` — including
/// ids containing `-`, which the general [`decode_path_slug`] cannot
/// disambiguate. Returns the empty string when the slug names the root
/// itself. The remainder must still lie in the reversible subset.
pub fn decode_uniform_slug(slug: &str, root_id: &str) -> Result<String, String> {
    validate_uniform_root_id(root_id)?;
    let prefix = uniform_root_slug(root_id);
    let remainder = strip_slug_prefix(slug, &prefix).ok_or_else(|| {
        format!("slug '{slug}' is not under the uniform prefix '{prefix}' for root '{root_id}'")
    })?;
    if remainder.is_empty() {
        return Ok(String::new());
    }
    let body = remainder.strip_prefix(SLUG_SEPARATOR).unwrap_or(remainder);
    let segments = decode_segments(body).map_err(|error| {
        format!("slug '{slug}' remainder under root '{root_id}' is not decodable: {error}")
    })?;
    Ok(segments.join("/"))
}

/// The healing map: rewrite a host-native slug into the uniform slug.
///
/// Given the slug a host produced for a path under its *own* local root (e.g.
/// `/Users/jess` on macOS, `/home/jess` on Linux) and the `root_id` that root
/// is registered as, return the slug the same directory would have under the
/// uniform `/tcfs/<root_id>` binding. Two hosts with unlike local roots heal
/// to the *same* uniform slug — that convergence is the property D4 buys, and
/// the reason `~/.claude/projects` enrollment must not widen before D4 lands
/// (widening first produces disjoint slug trees per OS).
///
/// The rewrite is a prefix substitution at a segment boundary; the relative
/// tail is carried across verbatim, so it heals tails that are not themselves
/// decodable (`--worktrees-...`) without inventing separators. It maps
/// *registry keys and directory names only* — in-place rewriting of absolute
/// paths embedded in transcripts stays rejected by the ADR, since it would
/// break the byte-exact convergence invariant roam enrollment depends on.
pub fn heal_host_native_slug(
    host_native_slug: &str,
    host_local_root: &str,
    root_id: &str,
) -> Result<String, String> {
    validate_uniform_root_id(root_id)?;
    if !host_local_root.starts_with('/') {
        return Err(format!(
            "host local root '{host_local_root}' must be an absolute path"
        ));
    }
    if !host_local_root.is_ascii() {
        return Err(format!(
            "host local root '{host_local_root}' must be ASCII to slug identically across hosts"
        ));
    }
    let trimmed = host_local_root.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("host local root '/' is too broad to heal against".to_string());
    }
    let host_prefix = encode_path_slug(trimmed);
    let remainder = strip_slug_prefix(host_native_slug, &host_prefix).ok_or_else(|| {
        format!(
            "slug '{host_native_slug}' is not under host local root '{trimmed}' (slug prefix '{host_prefix}')"
        )
    })?;
    Ok(format!("{}{remainder}", uniform_root_slug(root_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute paths whose slug we assert exactly. The last three rows are
    /// verbatim from the 96-sample host corpus the encoding rule was derived
    /// from, and are the rows that falsify the weaker "`/` only" and
    /// "`/` and `.` only" rules.
    const ENCODE_TABLE: &[(&str, &str)] = &[
        ("/Users/jess/x", "-Users-jess-x"),
        ("/home/jess/x", "-home-jess-x"),
        ("/tcfs/r1/x", "-tcfs-r1-x"),
        ("/tcfs/agent-state/x", "-tcfs-agent-state-x"),
        ("/", "-"),
        ("/Users/jess", "-Users-jess"),
        ("/Users/jess/git/tummycrypt", "-Users-jess-git-tummycrypt"),
        ("/Users/jess/git/tinyland.dev", "-Users-jess-git-tinyland-dev"),
        ("/Users/jess/git/spear_resumes", "-Users-jess-git-spear-resumes"),
        (
            "/Users/jess/git/GloriousFlywheel/.worktrees/tin-2609-application-release-v0-5-0-20260816",
            "-Users-jess-git-GloriousFlywheel--worktrees-tin-2609-application-release-v0-5-0-20260816",
        ),
    ];

    #[test]
    fn encode_matches_the_observed_host_rule() {
        for (path, expected) in ENCODE_TABLE {
            assert_eq!(&encode_path_slug(path), expected, "encoding {path}");
        }
    }

    #[test]
    fn encode_preserves_case_and_collapses_every_non_alphanumeric() {
        assert_eq!(encode_path_slug("/A_b.c-d/E"), "-A-b-c-d-E");
        assert_eq!(encode_path_slug(""), "");
        assert_eq!(encode_path_slug("relative/x"), "relative-x");
    }

    #[test]
    fn the_weaker_encoding_rules_are_wrong() {
        // "/ only" would keep the dot; "/ and . only" would keep the
        // underscore. Both were measured wrong against the host corpus.
        let dotted = "/Users/jess/git/tinyland.dev";
        assert_ne!(encode_path_slug(dotted), dotted.replace('/', "-"));
        let scored = "/Users/jess/git/spear_resumes";
        assert_ne!(
            encode_path_slug(scored),
            scored.replace('/', "-").replace('.', "-")
        );
    }

    #[test]
    fn decode_round_trips_the_reversible_rows() {
        for (path, slug) in ENCODE_TABLE {
            if !path_is_slug_reversible(path) {
                continue;
            }
            assert_eq!(
                decode_path_slug(slug).as_deref(),
                Ok(*path),
                "decoding {slug}"
            );
        }
    }

    /// Exhaustive round trip over every path built from these segments at
    /// depths 1..=3 — 4 + 16 + 64 = 84 paths, each asserted both ways.
    #[test]
    fn round_trip_is_exhaustive_over_the_reversible_alphabet() {
        const SEGMENTS: &[&str] = &["Users", "home", "tcfs", "r1"];
        let mut checked = 0usize;
        let mut frontier: Vec<String> = SEGMENTS
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect();
        let mut paths: Vec<String> = frontier.clone();
        for _ in 0..2 {
            let mut next = Vec::new();
            for path in &frontier {
                for segment in SEGMENTS {
                    next.push(format!("{path}/{segment}"));
                }
            }
            paths.extend(next.iter().cloned());
            frontier = next;
        }
        for path in &paths {
            let slug = encode_path_slug(path);
            assert_eq!(slug, path.replace('/', "-"), "alphanumeric-only fast path");
            assert_eq!(decode_path_slug(&slug).as_deref(), Ok(path.as_str()));
            checked += 1;
        }
        assert_eq!(checked, 84);
    }

    #[test]
    fn decode_fails_closed_instead_of_guessing() {
        assert_eq!(decode_path_slug(""), Err(SlugDecodeError::Empty));
        assert_eq!(
            decode_path_slug("Users-jess"),
            Err(SlugDecodeError::MissingLeadingSeparator)
        );
        assert_eq!(
            decode_path_slug("-"),
            Err(SlugDecodeError::EmptySegment { index: 0 })
        );
        // The doubled separator from a leading-dot segment is irrecoverable.
        assert_eq!(
            decode_path_slug("-Users-jess--worktrees"),
            Err(SlugDecodeError::EmptySegment { index: 2 })
        );
        // Trailing separator, likewise.
        assert_eq!(
            decode_path_slug("-Users-jess-"),
            Err(SlugDecodeError::EmptySegment { index: 2 })
        );
        assert_eq!(
            decode_path_slug("-Users-je.ss"),
            Err(SlugDecodeError::NonAlphanumericSegment {
                index: 1,
                segment: "je.ss".to_string(),
            })
        );
    }

    #[test]
    fn reversibility_predicates_agree_with_the_decoder() {
        for path in ["/Users/jess/x", "/home/jess/x", "/tcfs/r1/x", "/a"] {
            assert!(path_is_slug_reversible(path), "{path} should be reversible");
            assert!(decode_path_slug(&encode_path_slug(path)).is_ok());
        }
        for path in ["", "/", "relative/x", "/Users/jess/.claude", "/Users//jess"] {
            assert!(
                !path_is_slug_reversible(path),
                "{path} should not be reversible"
            );
        }
        assert!(relative_is_slug_reversible("git/tummycrypt"));
        assert!(!relative_is_slug_reversible(""));
        assert!(!relative_is_slug_reversible("git/"));
        assert!(!relative_is_slug_reversible("git/.claude"));
    }

    #[test]
    fn uniform_prefix_is_the_ruled_one() {
        assert_eq!(UNIFORM_ROOT_PREFIX, "/tcfs");
        assert_eq!(uniform_root_path("r1"), "/tcfs/r1");
        assert_eq!(uniform_root_slug("r1"), "-tcfs-r1");
        assert_eq!(uniform_root_slug("agent-state"), "-tcfs-agent-state");
    }

    #[test]
    fn uniform_slug_round_trips_including_ids_containing_separators() {
        for (root_id, relative, slug) in [
            ("r1", "x", "-tcfs-r1-x"),
            ("r1", "", "-tcfs-r1"),
            ("agent-state", "x", "-tcfs-agent-state-x"),
            (
                "agent-state-v1",
                "git/tummycrypt",
                "-tcfs-agent-state-v1-git-tummycrypt",
            ),
            (
                "r1",
                "git/tummycrypt/crates",
                "-tcfs-r1-git-tummycrypt-crates",
            ),
        ] {
            assert_eq!(
                uniform_slug_for(root_id, relative).as_deref(),
                Ok(slug),
                "encoding {root_id}:{relative}"
            );
            assert_eq!(
                decode_uniform_slug(slug, root_id).as_deref(),
                Ok(relative),
                "decoding {slug} under {root_id}"
            );
        }
    }

    #[test]
    fn uniform_decode_is_anchored_not_prefix_matched() {
        // A different root whose slug is a raw string prefix must not match.
        assert!(decode_uniform_slug("-tcfs-r10-x", "r1").is_err());
        assert!(decode_uniform_slug("-Users-jess-x", "r1").is_err());
        // Nor may an undecodable tail be silently accepted.
        assert!(decode_uniform_slug("-tcfs-r1--worktrees", "r1").is_err());
    }

    #[test]
    fn strip_slug_prefix_respects_segment_boundaries() {
        assert_eq!(
            strip_slug_prefix("-Users-jess-git-tummycrypt", "-Users-jess-git"),
            Some("-tummycrypt")
        );
        assert_eq!(strip_slug_prefix("-Users-jess", "-Users-jess"), Some(""));
        assert_eq!(
            strip_slug_prefix("-Users-jess-gitfoo", "-Users-jess-git"),
            None
        );
        assert_eq!(strip_slug_prefix("-Users-jess", ""), None);
    }

    #[test]
    fn uniform_root_id_rejects_slug_unstable_ids() {
        for accepted in ["r1", "agent-state-v1", "a", "0abc", "a--b"] {
            assert!(
                validate_uniform_root_id(accepted).is_ok(),
                "{accepted} should be accepted"
            );
        }
        // '.' and '_' are legal registry ids but alias under the slug map.
        for rejected in ["foo.bar", "foo_bar", "a-", "Foo", "primary", "", "-a"] {
            assert!(
                validate_uniform_root_id(rejected).is_err(),
                "{rejected} should be rejected"
            );
        }
        // The aliasing this rule prevents, made explicit:
        assert_eq!(encode_path_slug("foo.bar"), encode_path_slug("foo-bar"));
    }

    #[test]
    fn uniform_binding_must_be_exactly_the_uniform_path() {
        assert!(validate_uniform_root_binding("/tcfs/r1", "r1").is_ok());
        assert!(validate_uniform_root_binding("/tcfs/r1/", "r1").is_ok());
        for rejected in [
            "/tcfs/r2",
            "/tcfs/r1/nested",
            "/TCFS/r1",
            "tcfs/r1",
            "~/tcfs/r1",
            "/Users/jess/tcfs/r1",
            "/tcfs/r1/..",
        ] {
            assert!(
                validate_uniform_root_binding(rejected, "r1").is_err(),
                "{rejected} should be rejected"
            );
        }
        assert!(validate_uniform_root_binding("/tcfs/foo.bar", "foo.bar").is_err());
    }

    /// The property D4 exists for: unlike host roots, identical uniform slug.
    #[test]
    fn healing_converges_macos_and_linux_onto_one_uniform_slug() {
        const RELATIVES: &[&str] = &[
            "",
            "-git-tummycrypt",
            "-git-GloriousFlywheel--worktrees-tin-2609",
            "-git-tinyland-dev",
        ];
        for tail in RELATIVES {
            let macos = heal_host_native_slug(&format!("-Users-jess{tail}"), "/Users/jess", "r1");
            let linux = heal_host_native_slug(&format!("-home-jess{tail}"), "/home/jess", "r1");
            assert_eq!(macos, linux, "healing must converge for tail '{tail}'");
            assert_eq!(macos.as_deref(), Ok(format!("-tcfs-r1{tail}").as_str()));
        }
    }

    #[test]
    fn healing_matches_encode_of_the_uniform_path_for_reversible_tails() {
        let healed = heal_host_native_slug("-Users-jess-git-tummycrypt", "/Users/jess", "r1")
            .expect("healable");
        assert_eq!(healed, encode_path_slug("/tcfs/r1/git/tummycrypt"));
        assert_eq!(
            uniform_slug_for("r1", "git/tummycrypt").as_deref(),
            Ok(healed.as_str())
        );
        assert_eq!(
            decode_uniform_slug(&healed, "r1").as_deref(),
            Ok("git/tummycrypt")
        );
    }

    #[test]
    fn healing_carries_undecodable_tails_across_verbatim() {
        // A leading-dot segment slugs to a doubled separator that cannot be
        // decoded, but heals fine because the tail is copied, not parsed.
        let healed = heal_host_native_slug(
            "-Users-jess-git-gf--worktrees-a-b",
            "/Users/jess",
            "agent-state-v1",
        )
        .expect("healable");
        assert_eq!(healed, "-tcfs-agent-state-v1-git-gf--worktrees-a-b");
        assert!(decode_uniform_slug(&healed, "agent-state-v1").is_err());
    }

    #[test]
    fn healing_refuses_slugs_outside_the_host_root() {
        assert!(heal_host_native_slug("-home-jess-x", "/Users/jess", "r1").is_err());
        // Segment-boundary safety: /Users/jess must not swallow /Users/jessica.
        assert!(heal_host_native_slug("-Users-jessica-x", "/Users/jess", "r1").is_err());
        assert!(heal_host_native_slug("-Users-jess-x", "Users/jess", "r1").is_err());
        assert!(heal_host_native_slug("-Users-jess-x", "/", "r1").is_err());
        assert!(heal_host_native_slug("-Users-jess-x", "/Users/jess", "foo.bar").is_err());
    }

    #[test]
    fn healing_tolerates_a_trailing_slash_on_the_host_root() {
        assert_eq!(
            heal_host_native_slug("-Users-jess-x", "/Users/jess/", "r1"),
            heal_host_native_slug("-Users-jess-x", "/Users/jess", "r1")
        );
    }

    #[test]
    fn non_ascii_paths_are_refused_rather_than_silently_diverging() {
        assert!(!path_is_ascii("/Users/josé/x"));
        assert!(heal_host_native_slug("-Users-jos--x", "/Users/josé", "r1").is_err());
        assert!(uniform_slug_for("r1", "josé").is_err());
        assert!(validate_uniform_root_binding("/tcfs/r1\u{00e9}", "r1").is_err());
    }

    #[test]
    fn relative_paths_are_shape_checked() {
        for rejected in ["/x", "x/", "a//b", "a/./b", "a/../b", ".."] {
            assert!(
                uniform_slug_for("r1", rejected).is_err(),
                "{rejected} should be rejected"
            );
        }
    }

    #[test]
    fn decode_error_messages_name_the_problem() {
        assert_eq!(
            SlugDecodeError::EmptySegment { index: 2 }.to_string(),
            "slug segment 2 is empty: the original separator run is not recoverable"
        );
        assert!(SlugDecodeError::MissingLeadingSeparator
            .to_string()
            .contains("absolute path"));
    }
}
