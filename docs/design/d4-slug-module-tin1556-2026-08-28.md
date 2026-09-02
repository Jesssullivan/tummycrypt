# Design note: the D4 slug module (TIN-1556)

- **Date:** 2026-08-28
- **Status:** Landed as a pure module with zero call sites
- **Module:** `crates/tcfs-core/src/slug.rs`
- **Parent ADR:** [Stable root lifecycle and broad-directory ownership
  (TIN-1556)](stable-root-lifecycle-tin1556-2026-07-28.md), Decision **D4**
- **Ruling this implements:** ADR Q1, ruled 2026-08-26 — the uniform absolute
  prefix is `/tcfs/<root_id>`
- **Tracker:** TIN-1556 (related: TIN-2301, TIN-2306, TIN-2801)

## What this is

D4 says agent-session and roam-first roots bind at *one absolute path that is
identical on every host*, so the Claude-style path slug, the
`~/.claude.json` `projects{}` registry key, and every absolute path embedded
in a transcript all agree with **zero rewriting**. In-place transcript
rewriting stays rejected — it would break the byte-exact convergence
invariant roam enrollment depends on.

This module is the arithmetic of that convention: encode, decode, validate,
heal. It is pure — no filesystem probing, no daemon state, no I/O — and this
change wires it to **nothing**. The consumers come later: the D1 adopt
dry-run inventory (which must report slug collisions before it will execute)
and the TIN-2301 resume proof (which must show a session resuming against a
uniform-prefix root).

## The encoding is measured, not assumed

The host slug rule is: **replace every character that is not an ASCII
alphanumeric with `-`, preserving case.**

That is not a guess. It was derived from 96 real host project directories on
neo, each paired with the absolute working directory recorded inside its own
session records, and it matched **96/96 with zero mismatches**. The two
plausible weaker rules were measured and are wrong:

| Candidate rule | Mismatches |
|---|---|
| replace `/` only | 75 / 96 |
| replace `/` and `.` only | 3 / 96 |
| **replace every non-alphanumeric** | **0 / 96** |

The discriminating rows are worth naming, because a hand-written encoder gets
them wrong in exactly these two places:

- `/Users/jess/git/spear_resumes` → `-Users-jess-git-spear-resumes`
  (`_` collapses too)
- `/Users/jess/git/GloriousFlywheel/.worktrees/tin-2609-…` →
  `-Users-jess-git-GloriousFlywheel--worktrees-tin-2609-…`
  (a leading-dot segment produces the doubled separator)

Both rows are pinned in the test table.

## Decode is deliberately partial

`/`, `.`, `_`, and `-` all collapse to `-`, so decoding is ambiguous in
general. The module refuses to guess, and offers two decoders instead:

- `decode_path_slug` — defined only on the **reversible subset**: slugs whose
  every segment is a non-empty ASCII alphanumeric run. Everything else
  returns a typed `SlugDecodeError` naming the offending segment index. This
  is what makes `/Users/jess/x ↔ -Users-jess-x` and `/home/jess/x ↔
  -home-jess-x` exact.
- `decode_uniform_slug(slug, root_id)` — **root-anchored**. It strips the
  known `-tcfs-<root_id>` prefix at a segment boundary and only then decodes
  the tail, which is what makes `/tcfs/<id>/x ↔ -tcfs-<id>-x` exact even when
  `<id>` itself contains `-` (e.g. `agent-state-v1`).

Prefix matching is segment-boundary-aware throughout: `-Users-jess-git` does
**not** match `-Users-jess-gitfoo`, and `-tcfs-r1` does not match
`-tcfs-r10-x`. A naive `starts_with` accepts both and would silently graft
one root's state onto another.

## The finding: registry-legal root ids can alias under D4

`validate_registered_root_id` (in `crates/tcfs-core/src/config.rs`) admits
`.` and `_`. Both encode to `-`. So under a uniform binding, the distinct
roots `foo.bar`, `foo_bar`, and `foo-bar` **share the slug prefix
`-tcfs-foo-bar`** — three roots, one agent-state tree, silently.

`validate_uniform_root_id` is therefore a strict narrowing of the registry
rule, not a restatement of it: a root opting into a uniform binding must be
*slug-stable* (`encode_path_slug(id) == id`, i.e. `[a-z0-9-]` only) and must
not end in `-` (a trailing separator makes the prefix boundary ambiguous
against the first relative segment). Roots that cannot honor this stay
`UNBOUND` and fail closed, exactly as the ADR requires.

`validate_uniform_root_binding` is the sibling of
`validate_registered_root_id` that D4 needs: `local_root` must be *exactly*
`/tcfs/<root_id>` (one trailing `/` tolerated). Near misses — `~/tcfs/r1`,
`/TCFS/r1`, `/tcfs/r1/nested`, a relative path — are rejected rather than
normalized, because a near miss does not fail loudly at bind time; it
produces a divergent slug tree that only shows up as silent non-convergence
weeks later.

## The healing map, and why it is the whole point

`heal_host_native_slug(host_native_slug, host_local_root, root_id)` rewrites
a slug a host produced under *its own* local root into the slug the same
directory has under `/tcfs/<root_id>`:

```
macOS:  -Users-jess-git-tummycrypt   (host root /Users/jess)  ┐
                                                              ├─→ -tcfs-r1-git-tummycrypt
Linux:  -home-jess-git-tummycrypt    (host root /home/jess)   ┘
```

Unlike host roots, one uniform slug. That convergence is the property D4
buys, and the test asserting it (`healing_converges_macos_and_linux_onto_one_uniform_slug`)
is the module's load-bearing test.

The tail is carried across **verbatim**, not parsed, so healing also works
for tails that are not themselves decodable — `--worktrees-…` heals fine even
though `decode_uniform_slug` rejects it. Healing maps registry keys and
directory names only.

## Non-ASCII

`encode_path_slug` accepts non-ASCII but is *not* guaranteed to agree with
the reference host encoder byte-for-byte: the reference replaces per UTF-16
code unit, so a non-BMP scalar yields two separators there and one here. The
validators and the healing map refuse non-ASCII inputs outright rather than
emit a slug that might diverge across implementations.

## Sequencing this feeds

Per the ADR's fence correction (PR #575): **`~/.claude/projects` enrollment
must not widen before D4 lands**, because widening first produces disjoint
slug trees per OS — non-convergence that is silent, not loud. This module is
the first of the D4 pieces. It does not itself provision `/tcfs`, rewrite a
transcript, enroll a device, deploy, reconcile, or start a crypto ceremony;
each of those needs its own authorization.

## Test posture

`proptest` is not a dev-dependency of `tcfs-core`, so the cross-OS properties
are covered by exhaustive table tests instead: a fixed encode table (with the
two discriminating real-host rows), an exhaustive round trip over every path
built from a 4-symbol segment alphabet at depths 1–3 (84 paths, asserted both
directions), and accept/reject tables for both validators. Introducing
`proptest` here would add a dev-dependency to the crate every other crate in
the workspace depends on; the exhaustive enumeration covers the same ground
at this alphabet size.
