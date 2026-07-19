# ADR: Roots are named identities (interim registry = the reconcile-state dir)

- **Date:** 2026-07-14
- **Status:** Superseded; retained as the rejected CLI-local design
- **Refs:** TIN-2658 (per-root reconcile conflicts unresolvable), TIN-1556 (root
  scale-out + `root_id`), TIN-2853 (daemon-trusted stable-root routing),
  PR #551, TIN-2863 (versioned read-only registry), per-root state isolation
  (`docs/ops`, git-divergent keep-both design)

## Supersession

This ADR records the rejected CLI-local interim design. The accepted successor
chain is:

1. [TIN-2853 / PR #551](stable-root-routing-2026-07-14.md) replaces it with a
   daemon-selected, conflict-only registered-root route. Mutating resolve
   accepts a bounded root identity and never a client-supplied state path or
   remote prefix.
2. [TIN-2863/B0a](versioned-root-registry-status-b0a-2026-07-19.md) adds a
   separate strict V1 registry for authorized immutable list/status. It does
   not reinterpret the PR #551 entries or add mutation, reconcile, or MCP.

The operator ratified the fail-closed retirement of unrooted `keep_local`,
`keep_remote`, and `keep_both` mutation paths, plus removal of the unrooted MCP
`resolve_conflict` tool, on 2026-07-16.

`tcfs conflicts --state` remains a legacy read-only diagnostic surface. It does
not authorize mutation. The historical decision below is retained only to
explain why the daemon-trusted boundary replaced the CLI-local approach.

## Context

Scheduled `tcfsd-reconcile-<name>` units each reconcile one enrolled root into an
**isolated** per-root state cache at `~/.local/state/tcfsd/reconcile/<name>.json`
— deliberately never shared with the primary `sync_root` state (per-root state
isolation). The primary daemon never sees these caches, so its `resolve_conflict`
RPC cannot clear conflicts recorded there. TIN-2658 is the live symptom: honey's
`git-roam-tool-daemon` root has a `.git`-group conflict re-recorded every cycle
(900+ times) with no resolution path.

The mechanism fix (resolve a repo-group keep-both in-process against an
operator-supplied state cache) is real, but exposing it only as a raw
`--state <path>` flag would be "another path-specific patch": the operator must
know an internal file layout, and every future verb would re-invent path
plumbing. We want the addressing model, not just the plumbing.

## Decision

1. **A root is a named identity.** CLI verbs address roots by name
   (`--root <name>`), not by internal path.
2. **The reconcile-state directory is the interim registry.** `--root <name>`
   resolves to `<tcfsd state dir>/reconcile/<name>.json`, where the tcfsd state
   dir is the daemon socket's directory (`~/.local/state/tcfsd/` by default — the
   same `XDG_STATE_HOME` anchor `config.rs` uses). Listing that directory's
   `*.json` stems enumerates the roots; an unknown `--root` errors with that
   list. No new registry file is introduced yet.
3. **`--state <path>` stays as a low-level escape hatch**, mutually exclusive
   with `--root`, for ad-hoc or non-registered caches.
4. **`resolve` and `conflicts` share the root-identity resolution** so inspect
   and act use one UX; prefix/undo derivation is identical for both flags (a root
   is just a named state file today).
5. **The durable `root_id`/registry design is deferred to TIN-1556.** When roots
   gain stable identifiers and a real registry, verbs keep the `--root <name>`
   surface and the resolution swaps underneath.

## Consequences

- `tcfs resolve` and `tcfs conflicts` gain `--root <name>` now; the operator never
  types an internal state path for the common case.
- Future root-addressed verbs address roots, not paths — the addressing seam is
  established before the scale-out work.
- **Daemon-side root routing is deferred until `root_id` exists.** An RPC that
  accepts an arbitrary client-supplied state path was evaluated and **rejected**
  on write-primitive-safety grounds: `StateCache::flush()` unconditionally
  writes to whatever path it was opened with, and the RPC is gated only by
  ordinary `push` permission — that would turn "has a push session" into
  "overwrite any JSON the daemon can reach". The CLI-local resolution keeps the
  write primitive on the human-invoked binary (structurally operator-only) and
  needs no session/TOTP, so it also runs on headless hosts (TIN-2653).
- The interim registry is a convention on a directory, not a guarantee: names are
  filenames, and there is no `root_id` yet. That is acceptable for the enrolled
  reconcile units and is the explicit thing TIN-1556 replaces.
