# ADR: Stable root lifecycle and broad-directory ownership (TIN-1556, B phase)

- **Date:** 2026-07-28
- **Status:** Proposed design (design spike only — no implementation, no live
  claim; TIN-2856/TIN-2801 freeze on live resolver/enrollment/deploy/crypto
  ceremonies remains in force)
- **Scope:** the B-phase root lifecycle — adopt, remove, daemon-owned
  reconcile, profile behavior, uniform roam-root — built on the landed
  B0a contract
- **Predecessors:** [Versioned root registry/status ADR
  (B0a)](versioned-root-registry-status-b0a-2026-07-19.md), [Stable root
  routing ADR](stable-root-routing-2026-07-14.md)
- **Historical rejected design:** [CLI-local root
  identity](root-identity-adr-2026-07-14.md)
- **Tracker:** TIN-1556 (executes through the TIN-2859 B0 umbrella: B0b, B0c)

## Context

TIN-1556's identity problem is already solved. `RootSpecV1` (fleet-stable:
`version | root_id | remote_prefix | profile | generation` →
`identity_fingerprint`, equal across hosts) and `RootBindingV1` (host-local:
canonical `local_root`/`state_path` + policies → `binding_fingerprint`) landed
with TIN-2863, along with authorized `tcfs roots list/status`. A root with no
binding is a valid fleet state (`UNBOUND`) — the "same root, unlike local
paths per host" model is representable today.

What is *not* solved is the lifecycle around that identity. The central
as-built fact this design addresses:

**The thing that reconciles a root and the thing that identifies a root are
two disconnected systems.** Live multi-root reconcile happens through
per-root launchd/systemd units rendered by lab (`extraReconcileRoots`), each
running `tcfs reconcile --path --prefix --state` against
`~/.local/state/tcfsd/reconcile/<name>.json`. The daemon registry
(`[sync.roots]` / `[sync.root_registry]`) is a hand-maintained mirror of
those unit tuples; the `<root_id>.json` filename is the only structural
join. `RootLifecyclePolicyV1::Reconcile` parses and is ignored
(`ReconcileSupport::None` is hardcoded and client-asserted);
`RootProfileV1` has zero behavioral consumers; `tcfs reconcile` has no
`--root` flag.

The B-phase acceptance sentence (PRODUCT.md) names the target directly:

> B succeeds when a fresh host can discover its authorized roots, map them to
> valid local paths, hydrate one, work, unsync it, and recover without
> editing a unit file or copying a state cache.

"Without editing a unit file" is the per-root unit pattern's retirement
notice.

## Ratified constraints this design inherits (not re-litigated)

1. Root-ID-only addressing on every mutation RPC; the daemon selects all
   paths/prefixes; client-supplied state/prefix paths are never accepted.
2. New root-addressed verbs are new RPCs so daemon downgrade fails
   `Unimplemented` — never optional routing on legacy RPCs
   (`ResolveConflictRequest` field 4 is a permanent `reserved "root_id"`
   tombstone).
3. The compat-break retiring unrooted `keep_local`/`keep_remote`/`keep_both`
   and the unrooted MCP `resolve_conflict` tool is operator-final.
4. `RootSpecV1`/`RootBindingV1` and both fingerprint constructions are the
   identity contract; this design extends them and changes neither.
5. MCP/TUI exposure of root surfaces requires its own disclosure and
   authorization review; nothing here inherits B0a approval.
6. Crypto-first gate sequencing (G1/TIN-1417 before enrolling machines);
   agent-state directories lead repos as live targets; `~/.claude`,
   `~/.codex`, opencode caches, live SQLite/WAL, and raw transcript roots are
   not enrollable as beachhead substitutes.

## Decision D1 — lifecycle verbs are registry transactions, not path surgery

Add `tcfs root adopt | remove` (extending the existing `roots list/status`)
backed by dedicated RPCs (`AdoptRegisteredRoot`, `RemoveRegisteredRoot`),
each returning the same server-selected route evidence as
`ResolveRegisteredRoot`.

**Adopt** is a two-phase transaction on the V1 registry:

- *Phase 1 — dry-run inventory (mandatory, default, and the only mode until
  explicitly executed):* the daemon walks the candidate `local_root` under
  `ReadOnlyInventory` scope and produces an adoption report: file/dir/byte
  counts, blacklist hits (secrets/WAL deny-set), git topology findings
  (linked worktrees, gitfiles, alternates — today's fail-closed fences),
  hardlink findings (link count > 1 — see Gates), symlink escapes, xattr
  presence, namespace-reservation preview, and the resolved
  `(root_id, remote_prefix, profile, generation)` spec with both
  fingerprints. The report is the artifact TIN-1556's acceptance names.
- *Phase 2 — execute:* refused by default when the inventory is *dirty*
  (any blacklist hit, fence hit, or unresolvable path) or *ambiguous*
  (prefix/local-root overlap with an existing root, slug collision,
  `root_id` present in the legacy `[sync.roots]` table). Execute writes the
  registry row + binding, creates the state cache at the fenced
  `<root_state_dir>/<root_id>.json` location, and performs the initial
  namespace reservation — atomically enough that a crash leaves either no
  root or a `READY`-checkable root, never a half-adopted one.

**Remove** is the documented rollback path: it tombstones the binding
(state cache retained under a `removed/` sub-namespace for a bounded
retention window), never deletes remote objects (namespace reservations are
monotonic by contract), and reports what was retained where. A
`--purge-state` escalation deletes the local state cache after the window.

Adoption never mutates the tree being adopted. The existing config-file
enrollment path remains valid; `adopt` is its supersession, not its
replacement — B0c renders the same registry rows from fleet configuration.

## Decision D2 — the daemon becomes the multi-root reconcile driver

Replace the N-units pattern with one daemon-owned scheduler iterating
registry rows whose `lifecycle_policy = "reconcile"`:

- The reconcile engine is already root-agnostic (every root-varying value is
  a parameter); the driver is additive.
- Delivery follows the B0b/B0c gates already named by the B0a ADR:
  `ReconcileRegisteredRoot` ships **plan-only** first
  (`ReconcileSupport::PlanOnly`), execution is a separate gate
  (`PlanAndExecute`). The client keeps asserting the daemon's declared
  support level.
- A third validation scope (`Reconcile`) sits between `ReadOnlyInventory`
  and `Mutation`: full route fencing per cycle, without re-probing peer
  isolation on every file.
- **Fair-share:** a global concurrency cap (default: min(4, cores/2) roots
  reconciling at once), per-root interval + jitter, and a back-pressure rule
  (a root whose last cycle overran its interval skips a beat rather than
  queueing). Per-root cycle metrics land in the status surface.
- The launchd/systemd units are retired per-root as roots migrate into the
  registry; the unit pattern remains the documented fallback for hosts
  running daemonless.

## Decision D3 — profiles gain behavior (root classes)

`RootProfileV1` stops being a label. Each profile binds the currently-global
knobs (`sync_git_dirs`, `git_sync_mode`, `conflict_mode`,
`sync_hidden_dirs`, `exclude_patterns`) as a per-root policy bundle resolved
at route-selection time, eliminating the config-file-per-root workaround:

| Profile | Ignore preset | Git handling | Conflict posture |
|---|---|---|---|
| `git-raw-v1` (exists) | project-tree preset | `.git`-as-files, raw mode, topology fences | repo-group keep-both only |
| `agent-static-v1` (exists) | agent-state preset: quiesced JSONL allowed; live SQLite/WAL, lock, and socket patterns denied (G0 deny-set) | n/a | per-file, gated on manifest identity |
| `home-macos-v1` (new) | macOS home preset (Library caches, keychains, `.Trash`, FileProvider internals denied) | subtree repos surfaced in inventory, not auto-adopted | inspect-only until proven |
| `home-linux-v1` (new) | Linux home preset (`.cache`, XDG runtime, sockets denied) | same | inspect-only until proven |

This satisfies the four-preset acceptance criterion. New profiles require
their own validation contract per the B0a rule; the two home profiles ship
inventory-and-shadow-only in B (live home subtrees are C-phase claims).

## Decision D4 — uniform roam-root is the agent-session on-ramp

The TIN-2301 probe falsified the symlink shim as a complete answer:
Claude-style agent state keys sessions by absolute-path slug *and* by a
literal-path registry (`~/.claude.json` `projects{}`), and 32–57% of
in-transcript path references are embedded absolute paths no shim reaches.
The recorded escalation — **a uniform absolute prefix, identical on every
host** — is adopted here as the binding convention for agent-session and
roam-first roots:

- A reserved conventional prefix (proposal: `/tcfs/<root_id>`, with
  `~/tcfs/<root_id>` as the non-root-privilege fallback — **operator
  question Q1**) at which the `RootBindingV1.local_root` is *identical on
  every host*, so slug, registry key, and embedded cwd all match with zero
  rewriting. In-place transcript rewriting stays rejected — it would break
  the byte-exact convergence invariant roam enrollment depends on.
- Uniform-prefix bindings are a *convention on top of* the binding contract,
  not a change to it: hosts that cannot honor the prefix stay `UNBOUND` and
  fail closed.
- This is the designed unlock for the R7 fence ("sting = sole writer, no
  roam until TIN-2301/1556"): agent-session roam resumes only via roots
  adopted under this convention, after TIN-2301's resume proof passes
  against a uniform-prefix root.

## Decision D5 — scale posture for 100+ roots

The as-built surface has measured ceilings; B commits to the following and
defers the rest:

- **Registry validation:** the O(N²) overlap check moves to a sorted
  interval structure computed once per config generation and reused per
  request (validation epoch), replacing the per-RPC full-table re-scan.
- **`roots list`:** per-root blocking canonicalize + full state-cache parse
  inside the async handler is replaced by a daemon-maintained status cache
  with an explicit `observed_at` staleness field (B0a's no-repair-on-read
  contract is preserved — the cache is advisory; `roots status <id>`
  remains the authoritative slow path).
- **State layout:** the flat `<root_state_dir>/<root_id>.json` contract is
  kept for B (it is a fence, and 64-char slugs suffice at hundreds of
  roots); sharding is a C concern. Whole-snapshot flush is kept but bounded:
  a per-root entry-count advisory threshold surfaces in status before flush
  cost becomes pathological.
- **Undo bundles:** keep-both bundles gain a retention policy (count + age
  per repo-root hash, GC'd by the driver between cycles) — today they
  accumulate full-history bundles forever in one shared directory on a disk
  already near capacity.
- **Deferred to C:** per-root storage endpoints/credentials (one global
  transport is a stated invariant), root-scoped NATS subjects (events for
  registered roots stay unpublished until subjects carry root identity —
  designing that schema is a B deliverable, shipping it is C), vector-clock
  root identity, and namespace-reservation reclamation.

## Gates and sequencing

1. **TIN-2889 (hardlink egress) is a hard precondition for any adopt
   execute on repo/home profiles**: Push must reject link count > 1 with
   re-validation at the anchored read boundary. Adopt's inventory reports
   hardlinks from day one; execute on a tree containing them is refused
   until the fence lands.
2. **TIN-2890 (atomic `.git` bootstrap/restore contract) gates hydrate of
   any `git-raw-v1` root on a fresh host** — adopt/reconcile on the origin
   host does not depend on it.
3. Delivery order: **B0b** (plan-only driver + adopt dry-run inventory) →
   **B0c** (execute + fleet-rendered registry rows) → uniform-prefix
   agent-static adoption + TIN-2301 resume proof → repo adoption after the
   TIN-2306 two-repo stop rule passes. Broad `~/git`/home claims stay out
   until the C phase, per the standing claim boundary.
4. Everything above is source/tests/docs until the TIN-2856/TIN-2801 freeze
   clears; no step in this ADR authorizes a live ceremony.

## Acceptance mapping (TIN-1556)

| Acceptance criterion | Where satisfied |
|---|---|
| Stable root IDs independent of host path | landed (B0a fingerprints); D4 adds the uniform-prefix convention on top |
| Manifests/index rooted by `(root_id, relative_path)` | remote keys already live under `remote_prefix`; D5's NATS-subject schema design extends identity to events; ordinary-file resolution returns only once manifest identity is root-bound (B0b/B0c) |
| `tcfs root adopt/list/status` CLI | D1 (adopt/remove) + landed `roots list/status` |
| Adopt requires dry-run inventory, refuses dirty/ambiguous | D1 phase structure |
| Ignore presets: project-tree, git-repo, macOS home, Linux home | D3 profile table |
| Cross-machine convergence, different local paths, no collisions | UNBOUND/binding model + D2 driver; proven by the B0c two-host test (and the uniform-prefix variant for agent roots) |
| Rollback/removal documented and tested | D1 remove semantics |

## Operator questions (blocking decisions, batched for one interview)

- **Q1 — uniform roam-root prefix:** `/tcfs/<root_id>` (needs a root-owned
  directory created once per host) vs `~/tcfs/<root_id>` (no privilege, but
  `~` differs across OSes so only the *suffix* is uniform — breaks the
  full-path-slug healing for cross-OS pairs). A third option is a per-OS
  synthetic mount point unified by the daemon. D4 recommends `/tcfs`.
- **Q2 — remove retention window:** how long removed-root state caches are
  retained before `--purge-state` is allowed (proposal: 30 days).
- **Q3 — driver default posture:** should migrated roots default
  `lifecycle_policy = "reconcile"` (units retired eagerly) or
  `"inspect-only"` (operator flips each root after observing plan output)?
  D2 recommends inspect-only defaults with explicit per-root promotion.
- **Q4 — home profiles in B:** confirm inventory-and-shadow-only scope for
  `home-*-v1`, with live home subtrees deferred to C.

## Explicit non-goals of this ADR

Live rollout of anything; TOTP/enrollment/crypto ceremonies; MCP or TUI
root surfaces; per-root storage endpoints; subscription selective sync
(TIN-1416 consumes this registry but is its own gate); linked-worktree
reconstruction (stays fail-closed, G5-wt-1 expected-red); in-place
transcript rewriting; any change to the landed fingerprint constructions or
the retired-compat rulings.
