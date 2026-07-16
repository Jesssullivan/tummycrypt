# ADR: Roots are named identities (interim registry = the reconcile-state dir)

- **Date:** 2026-07-14
- **Status:** Accepted (interim; durable form lands with TIN-1556)
- **Refs:** TIN-2658 (per-root reconcile conflicts unresolvable), TIN-1556 (root
  scale-out + `root_id`), per-root state isolation (`docs/ops`, git-divergent
  keep-both design)

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
