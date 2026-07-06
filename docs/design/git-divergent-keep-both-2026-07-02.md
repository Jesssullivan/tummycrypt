# Divergent `.git` keep-both: operator resolution for genuinely diverged repos

- **Status:** Design-of-record — PR-1 through PR-4 are merged; live divergent
  fleet canary pending deploy
- **Date:** 2026-07-02
- **Code baseline:** all `file:line` references verified at `origin/main` `c40f075` (#513 merged)
- **Predecessors:** #513 (`.git`-aware FF conflict resolution), #506 harness
  (`scripts/git-dotgit-fsck-conflict-harness.sh`),
  `docs/ops/repo-roam-test-plan-2026-06-08.md`,
  `docs/ops/large-workdir-onboarding-design-2026-05-25.md`,
  `docs/ops/lazy-traversal-qa-permutation-matrix-2026-05-09.md`
- **Tickets:** TIN-1549 (conflict/status UX), G5-git-5 (repo-roam-test-plan §7)

---

## 1. Problem

#513 closed the fast-forward case: ref conflicts whose heads are provably
FF-related are reclassified and applied atomically per repo, with plan-time
ref-SHA pins re-verified at execute, an objects-before-refs barrier, and a
fail-closed veto. Two gaps remain, both by design of #513's fail-closed
posture:

**1a. Genuinely diverged `.git` state never converges (T10/T11).**
When both devices commit different work on the same branch, there is no FF
direction: `classify_fast_forward` returns `NotFastForward`
(`crates/tcfs-sync/src/git_safety.rs:203-219`), `decide_repo_fast_forward`
fails closed (`crates/tcfs-sync/src/reconcile.rs:1310-1312`), and the whole
repo group stays `Conflict`. The Conflict arm at execute
(`reconcile.rs:1737-1746`) only writes `ConflictInfo` into the state cache —
zero local writes, zero remote writes — so the conflict re-records every
300s cycle, forever. The operator has **no verb** to resolve it: the only
conflict verb is per-file `tcfs resolve` (`crates/tcfs-cli/src/main.rs:329-335`
→ daemon `ResolveConflict`, `crates/tcfsd/src/grpc.rs:1526-1826`), which is
strictly per-file, acquires no `.git/tcfs.lock`, does no fsck, and would
actively corrupt a `.git` group if used (see 1c).

The acceptance bar this must meet is already written down:

> `docs/ops/large-workdir-onboarding-design-2026-05-25.md:235-236`
> "| T10 | same-file conflict | Conflict state is visible and local bytes are preserved |"
> "| T11 | keep-both recovery | Both versions can be preserved and rehydrated |"

repo-tightened by `docs/ops/repo-roam-test-plan-2026-06-08.md:127`:

> "| **T10 / T11 + M5 / M5-R** | same-file conflict visible; keep-both
> recovery | for a repo this means BOTH `.git` states preserved and each
> fsck-clean |"

and §6 R5 (`repo-roam-test-plan-2026-06-08.md:328-333`): conflict visible,
local bytes preserved, manual keep-both → both `.git` states preserved, each
fsck-clean and each fingerprint-able. Note the plan promises **manual**
keep-both (M5-R, `lazy-traversal-qa-permutation-matrix-2026-05-09.md:53`);
the operator verb is the designed-but-missing piece. M5-D (line 54) — a
daemon-backed `tcfs resolve --strategy keep-both` — is BLOCKED on record
(RPC timed out with partial side effects); M5-D2 (line 178) specs its
closure bar.

**1b. The over-veto.** #513's fail-closed rule — any ref-class conflict path
for which `head_ref_for_git_path` returns `None` vetoes the WHOLE repo group
(`reconcile.rs:1265-1278`; `is_git_ref_class_path` at `reconcile.rs:741-749`;
`head_ref_for_git_path` at `git_safety.rs:303-313`, matching only
`.git/refs/heads/*`) — means one benign divergent non-head ref (a stale
remote-tracking ref, a divergent tag, a submodule ref) blocks the entire repo
from converging even when the heads could cleanly FF. Correct for safety,
costly for convergence. Keep-both/per-ref parking is the designed unlock;
this doc builds the parking primitive but defers the plan-time auto-relaxation
(§6).

**1c. Live corruption vectors that predate this design.** Two per-file paths
bypass all of #513's group discipline today:

- Daemon `ResolveConflict` `keep_remote`/`keep_both`
  (`grpc.rs:1650-1713`, `1714-1825`): `keep_both` on `.git/refs/heads/main`
  renames it to `main.conflict-{device}` (`grpc.rs:1733-1745`) — a
  syntactically **legal loose ref** that pollutes the branch namespace and is
  uploaded remotely; `keep_remote` splices one device's ref/index over the
  other device's object store — the exact G5-git-5 fsck corruption. No lock,
  no `git_is_safe`, no objects-before-refs ordering, no fsck. The MCP
  `resolve_conflict` tool (`crates/tcfs-mcp/src/server.rs:237-267`) is a thin
  passthrough to the same RPC, so agents inherit the hazard.
- The daemon's NATS-event auto path (`crates/tcfsd/src/daemon.rs:1808-1836`)
  applies `AutoResolver` (lexicographic KeepLocal/KeepRemote,
  `crates/tcfs-sync/src/conflict.rs:150-160`) **per file** with zero `.git`
  awareness — the G5-git-5 interleave vector.

Additionally, the executor's cooperative `.git/tcfs.lock` is **best-effort**:
the comment at `reconcile.rs:1539-1547` says outright that if a repo's lock
cannot be acquired, "we still run". Any resolve verb that assumes the lock
fences a concurrent sync cycle is wrong until this is hardened (§3, PR-2).

---

## 2. Design

### 2.1 Core insight: the loser's objects are already local

While a repo's refs are in Conflict, its `.git/objects/**` files are
content-addressed, never conflict, and keep roaming in both directions — each
side's new pack/loose objects are brand-new paths that classify
LocalNewer/RemoteNewer and flow normally. This is documented in the engine
itself: `classify_fast_forward`'s doc comment (`git_safety.rs:187-203`) states
that a conflicted repo's `.git/objects/**` "still roam". So after a cycle or
two of steady-state divergence, **both devices hold both object sets**; only
the pointer files (refs, HEAD, index, logs, packed-refs) stay conflicted.

Consequence: materializing the other side's history needs **no bundle
transport and no remote version history** — just `git update-ref` against
objects already on disk, gated by a `git cat-file -e <sha>^{commit}` presence
check (fail closed: objects not yet roamed → "retry after next sync cycle",
zero writes). This also matches the retrievability reality: in the divergent
steady state neither side pushes the conflicted paths, the remote holds at
most one side per path, and `ConflictInfo` (`conflict.rs:106-123`) carries
only blake3 content hashes — the loser's bytes are only guaranteed to exist
on the loser's disk. The objects-roam property is what makes them exist on
the *winner's* disk too.

The remote head SHA is readable the way planning already reads it:
`read_remote_ref_sha` (private fn in `reconcile.rs`, used by
`decide_repo_fast_forward` at ~`reconcile.rs:1305-1309`, downloads the tiny
ref blob to an ephemeral temp dir) — promote to `pub(crate)` and reuse.

### 2.2 Keep-both = namespaced theirs-refs

Keep-both for a diverged repo means materializing the losing side's committed
history as namespaced refs inside the winning repo:

```
refs/tcfs/theirs/<remote_device>/heads/<branch>
refs/tcfs/theirs/<remote_device>/tags/<tag>
refs/tcfs/theirs/<remote_device>/stash
```

The namespace mirrors the full ref layout, so `git log --all`, fsck
reachability, and `git merge refs/tcfs/theirs/honey/heads/main` all just
work. Parking theirs-refs also makes the loser's otherwise-dangling objects
gc-reachable on the winner. Both histories become fsck-reachable in **one**
repo; combining them stays ordinary `git merge`/`rebase`/`cherry-pick` done
by a human — TCFS never invents a merge commit.

Collision rule: if a theirs-ref already exists at a different SHA (repeated
divergence epochs), write `...<name>-<sha12>` instead — write-once targets.
Identical-content same-name writes classify UpToDate in `compare_clocks`
(`conflict.rs:163-199` — content-equal short-circuit), so theirs-refs
themselves essentially never conflict.

UX skin: `--theirs-as-branch <name>` (auto-name
`tcfs/conflict/<remote_device>-<shortsha>`) is a flag over the same
primitive — park at a real `refs/heads/*` branch instead of the
`refs/tcfs/theirs/**` namespace. Same mechanics, one naming decision for the
operator (§6, Q1).

### 2.3 The resolve operation (winner side)

New `pub async fn resolve_git_keep_both(repo_root, ...)` in a new
`crates/tcfs-sync/src/conflict_git.rs`. Runs on the device the operator
chooses — that device's heads win. Sequence, single commit point:

1. **Lock + quiesce.** `acquire_git_lock` (`git_safety.rs:411-425`,
   cooperative `.git/tcfs.lock`, dead-owner staleness recovery at
   `:461-494`) held for the whole apply; `git_is_safe`
   (`git_safety.rs:29-79`) refuses mid-rebase/merge/lockfile repos. Graft
   from the ux-first review: also refuse on a dirty working tree
   (`git status --porcelain` non-empty) unless `--allow-dirty`.
   **Prerequisite:** executor ref-class writes must hard-respect a foreign
   lock holder first (PR-2) — otherwise the lock fences nothing.
2. **Enumerate the group.** All state-cache entries under this repo root with
   `entry.conflict = Some(_)` (the records written at
   `reconcile.rs:1737-1746`). Classify each with `head_ref_for_git_path` /
   `is_git_ref_class_path`. Group by `repo_root_for_git_path`
   (`git_safety.rs:275-297`).
3. **Non-parkable veto (inherits #513's shape — mandatory).** If the group
   contains **any ref-class conflict this verb cannot prove AND park** —
   submodule refs (`.git/modules/<n>/refs/heads/*`, which
   `head_ref_for_git_path`'s `.git/refs/heads/` needle never matches,
   `git_safety.rs:303-313`), a divergent `.git/HEAD`, packed-refs deltas —
   **refuse the whole group**, zero writes, with an error naming the
   unparkable paths. This mirrors `decide_repo_fast_forward`'s veto
   (`reconcile.rs:1244-1279`). Without this, step 8's tick-and-clear would
   winner-dominate a divergent submodule head with no parking, orphaning the
   loser's committed submodule work. Module-gitdir parking is future work.
4. **Object-presence gate (fail closed).** For every conflicted head ref,
   read the remote SHA via `read_remote_ref_sha`; require
   `git cat-file -e <remote_sha>^{commit}` locally. Any miss → retryable
   error "objects still roaming, retry after next sync cycle", zero writes.
   No new peer-side export verb in v1: objects roam while refs conflict
   (§2.1), so absence is brief or peer-offline; `conflict-export` is
   deferred with a named trigger (§6).
5. **Pre-resolve undo bundle + fsck-before.** `snapshot_git_for_sync`
   variant (`git_safety.rs:85-100`, `git bundle create ... --all`) writing to
   the daemon state dir — `~/.local/state/tcfs/resolve-bundles/<repo-id>-<ts>.bundle`
   — **never** the in-tree `GIT_BUNDLE_REL_PATH` (`git_safety.rs:13`), which
   is deliberately in-tree so it roams in bundle mode; a resolve artifact
   must NOT roam and re-conflict. `git bundle verify` it. Then
   `git fsck --no-dangling` gate BEFORE any mutation — do not resolve on top
   of pre-existing corruption.
6. **Pin + re-verify.** Pin every conflicted head ref's local SHA
   (`GitRefPin` pattern, `reconcile.rs:43-49`) and re-verify immediately
   before the first write (mirror of `git_ff_pins_still_valid`,
   `reconcile.rs:829-836`). A local commit landing mid-resolve → abort the
   whole group pre-write.
7. **Materialize theirs.** For each divergent head:
   `git update-ref refs/tcfs/theirs/<remote_device>/heads/<b> <remote_sha>`.
   Divergent tags: local `refs/tags/<t>` kept, theirs parked at
   `.../tags/<t>`. Divergent `refs/stash`: parked at `.../stash`. Stale
   remote-tracking refs: local wins silently (re-derivable from origin).
   Local `refs/heads/*`, `HEAD`, `index`, `logs/**`, `packed-refs` all keep
   local bytes (T10).
8. **fsck-after gate.** `git fsck --full` must pass, and
   `classify_fast_forward`/`is_ancestor` (`git_safety.rs:203-242`) proves
   each parked theirs SHA is reachable. Failure → roll refs back from the
   step-5 bundle (`restore_git_bundle_into` semantics,
   `git_safety.rs:117-158`, or targeted `update-ref` rollback), return
   error, conflicts stay recorded.
9. **Commit point — clock tick.** For every conflicted `.git` path in the
   group: `clock = merge(local_clock, remote_clock_from_ConflictInfo);
   clock.tick(local_device)`; clear `entry.conflict` — all-or-nothing.
   **No upload from the verb** — this avoids the daemon `keep_local`
   chunkless-manifest hazard (`grpc.rs:1600` rebuilds a `SyncManifest` with
   `chunks: vec![]`, overwriting the remote manifest with an unhydratable
   stub). The next reconcile cycle pushes real bytes through the normal
   engine path, under the existing objects-before-refs barrier
   (`reconcile.rs:1549-1566`, `787-805`) and FF-pin discipline.
10. **Release lock; print the undo line**:
    `undo: tcfs resolve <repo> --restore-undo <bundle-path>`.

### 2.4 Convergence story (why the forever-Conflict actually settles)

After the tick, the winner's clocks dominate the exact remote clocks the
conflict was recorded against → next cycle classifies LocalNewer → pushes
ref/index/log files plus the brand-new `refs/tcfs/theirs/**` files (new
paths, no conflict). The loser sees RemoteNewer on everything → pulls. Cycle
after: clocks equal, content equal → UpToDate on both sides. Both repos
contain both lines under identical refs — genuine convergence, merge at
leisure. If the remote index moved between resolve and push, clocks are
concurrent again → ordinary Conflict, zero writes — safe re-run.

**Loser-side no-loss guard (PR-4, the piece that makes "no committed work is
lost" hold on BOTH machines).** In the execute loop, beside the FF-pin
re-verify (`reconcile.rs:1576-1584`): before overwriting a local ref-class
head file (`.git/refs/heads/*`, `refs/stash` — and explicitly covering or
deferring on module-gitdir refs) whose current SHA differs from the incoming
one and is not an ancestor of it (`is_ancestor`, `git_safety.rs:227-242`),
park the old SHA at `refs/tcfs/theirs/<self_device>/...` first and
bundle-snapshot the pre-overwrite `.git` to the state dir. If parking fails →
DEFER the pull, riding the existing `deferred_git_refs` machinery
(`reconcile.rs:180-184`), re-planned next cycle. This closes the accepted
crash window where path ordering pulls `refs/heads/*` before
`refs/tcfs/...` and a mid-pull crash briefly leaves the loser's old head
unreferenced. It also covers the race where the loser commits more work
after the winner resolved.

N>2 devices: the per-device namespace is collision-free; each additional
divergent device costs one more resolve (or gets auto-parked by the
loser-side guard). Honest cost, no corruption.

### 2.5 Surfaces (extend, don't reinvent)

- **CLI.** `cmd_resolve` (`main.rs:6668-6722`) gains routing: path is a repo
  root, or is inside a repo with recorded `.git`-internal conflicts →
  repo-group mode; any attempt to resolve a single `.git`-internal path
  per-file is refused with "git-internal path; resolve the repo group:
  `tcfs resolve <root>`". Repo-group mode is **dry-run by default** with
  `--execute`, matching the Reconcile convention (`main.rs:288-296`): the
  dry run prints the exact plan (refs to be parked, names, undo-bundle path,
  fsck gates). Fix the interactive prompt while there: today
  `resolve_conflict_interactive` (`main.rs:6726`) is called with a DUMMY
  `ConflictInfo` (empty hashes/clocks) — fetch the real record.
- **New read verb: `tcfs conflicts`** (new clap variant beside Resolve,
  `main.rs:325-335`). Data source: the state cache directly, same pattern as
  `tcfs reconcile` (`StateCache::open`, `main.rs:6448-6450`) — no daemon RPC.
  Groups `.git`-internal conflicts by `repo_root_for_git_path`; non-.git
  conflicts list flat. Per repo group: local HEAD via `local_ref_sha`
  (`git_safety.rs:318-339`) + `git log --oneline -5`; remote HEAD via the
  `read_remote_ref_sha` pattern; **honest degradation** — if the remote
  SHA's objects are not in the local odb, show the SHA + "objects not local"
  instead of a fake log. Age from `ConflictInfo.detected_at`
  (`conflict.rs:122`); cycle count from a new `times_recorded: u64`
  (serde-default) bumped at `reconcile.rs:1737-1746`. `--json` for
  agents/TUI (the TUI conflicts widget,
  `crates/tcfs-tui/src/ui/widgets/conflicts.rs`, consumes the same grouping
  later). Suggested-command line per group.
- **Cadence.** First detection = one WARN with the repo-group summary +
  suggested command; thereafter one aggregated line per cycle; escalation
  WARN at 24h/7d. `tcfs sync-status` and the reconcile summary
  (`main.rs:6589-6594`) gain a conflict banner.
- **MCP.** The daemon fence (§3, I1) covers the `resolve_conflict`
  passthrough (`server.rs:237-267`) automatically; update its description to
  name the repo-group rule; add a `list_conflicts` tool mirroring
  `tcfs conflicts --json`. Repo-group resolution stays CLI/library-driven in
  v1 — M5-D already proved routing a long apply through `ResolveConflict`
  times out with partial side effects; don't repeat it.
- **Strategy surface.** For `.git` groups: `--keep-both` ≡
  `--theirs-as-branch` (the per-file rename semantics are nonsensical for
  refs). Bare group `--keep-local`/`--keep-remote` ship **only as sugar over
  the same parking primitive** (park the losing line first, then dominate)
  **or not at all** — as specified without parking they orphan the
  non-resolving device's committed line: the undo bundle exists only on the
  machine that ran resolve, so "recoverable via undo" silently substitutes
  for "no committed work is lost". `--defer` stays a no-op that resets the
  escalation timer.
- **Data-model graft.** `ConflictInfo.remote_manifest_key: Option<String>`
  (serde-default, back-compat; `conflict.rs:106-123`), populated where the
  conflict is recorded (`reconcile.rs:1737-1746` has the index entry in
  hand). Evidence rendering and per-file keep-remote stop depending on the
  incidental, unversioned `SyncState.remote_path` (`state.rs:192`).
  Best-effort evidence only: manifests persisting under `manifests/` is
  incidental durability (orphan GC's `scan_remote_chunks` lists all of
  `manifests/`, `reconcile.rs:445-462`), not a promised surface.

### 2.6 Parallel track: plain-file keep-both hardening (independent)

Orthogonal to the `.git` story; closes M5-D2 for ordinary files. The current
daemon `keep_both` renames the local file away BEFORE downloading
(`grpc.rs:1733-1757`, rename-back only on a returned error ~`1818-1820`) —
a crash mid-download leaves the original path absent (the recorded M5-D
failure). Rework:

1. Download remote manifest bytes to `{original}.tcfs-partial`, blake3-verify
   against the manifest's file hash; failure → delete temp, zero net writes.
2. **Copy** (not rename) local → `{stem}.tcfs-conflict-{device}-{utcstamp}{ext}`,
   fsync. Extension stays LAST so blacklist deny semantics hold
   (`blacklist.rs:104-125` suffix matching — a copy of `notes.env` is still
   `.env`-denied; degradation to local-only preservation is surfaced, not
   hidden).
3. Atomic rename temp → original. No window where the original is absent;
   second resolve is idempotent. Add `.tcfs-partial` to the builtin deny
   list (`blacklist.rs:76-77` pattern). Cascade fence: refuse keep-both on a
   path already matching the conflict-copy pattern. Fix `keep_local`'s
   chunkless-manifest stub (`grpc.rs:1600`) to upload real bytes. Copies
   roam deliberately (T11 cross-device rehydration; current code already
   uploads for this reason, `grpc.rs:1783`). No automatic GC of copies in
   v1; optional `tcfs conflicts prune --older-than` refuses unless the
   original exists and is Synced.

---

## 3. Safety invariants

Inherit #513's discipline; every rule below is fail-closed.

| # | Invariant | Mechanism |
|---|-----------|-----------|
| S1 | Per-file resolution never touches `.git` internals | Daemon `ResolveConflict` entry guard: `repo_root_for_git_path(path).is_some()` → refuse keep_local/keep_remote/keep_both with repo-group guidance (`defer` allowed — it's a no-op, `grpc.rs:1566-1573`). Covers MCP passthrough automatically. |
| S2 | Auto-resolution never touches `.git` internals | NATS auto path (`daemon.rs:1808-1836`): `.git`-internal conflict under `conflict_mode=auto` → log once per repo + defer; never `do_auto_download`. |
| S3 | Resolve only mutates a quiesced, locked, clean repo | `git_is_safe` + dirty-tree refusal + `acquire_git_lock` held across the apply; **prerequisite:** executor ref-class writes hard-fail when a foreign holder owns `.git/tcfs.lock` (fixes the best-effort comment at `reconcile.rs:1539-1547`). Until that lands, no resolve verb ships. |
| S4 | Non-parkable group → whole-group refusal | Step-3 veto, mirror of `reconcile.rs:1244-1279`: any ref-class conflict that is not a provable, parkable top-level head (submodule refs, divergent HEAD, packed-refs deltas) vetoes the group. Same shape as #513's over-veto, applied to the verb. |
| S5 | Never point a ref at objects not present | `git cat-file -e <sha>^{commit}` gate before any `update-ref`; miss → retryable error, zero writes. |
| S6 | Every mutation is undoable | `git bundle verify`-clean pre-resolve bundle in the state dir (never in-tree) BEFORE the first write; fsck-after failure → bundle/targeted rollback, conflicts retained. |
| S7 | fsck gates both sides of the mutation | `git fsck --no-dangling` before; `git fsck --full` + ancestry reachability proof after; only then the clock tick. |
| S8 | Plan-time pins re-verified at write time | `GitRefPin` re-check immediately pre-write (pattern of `reconcile.rs:829-836`); mismatch → abort whole group. |
| S9 | Single commit point; idempotent re-run | All filesystem effects before the tick are idempotent ref writes + a state-dir bundle; a crash leaves either "nothing" or "refs written, still conflicted" — both re-runnable (`update-ref` to same SHA is a no-op). Meets the M5-D2 bar structurally: no partial state on timeout, second resolve idempotent. |
| S10 | Convergence never orphans the non-resolving side | PR-4 loser guard: pre-overwrite parking + state-dir bundle for any non-ancestor head overwrite; parking failure → defer via `deferred_git_refs` (`reconcile.rs:180-184`). |
| S11 | Concurrent resolves degrade safely | Both tick → clocks concurrent → ordinary Conflict, zero writes (`compare_clocks` untouched, `conflict.rs:163-199`); device-namespaced parking cannot collide; one extra round, never corruption. |
| S12 | Raw mode only | Matches #513's gate: engaged only when `git_sync_mode == "raw"` (`reconcile.rs:651-656`; CLI wiring `main.rs:6456`). Bundle-mode repos are whole-file-atomic and untouched. |

Known residual (stated, not hand-waved): until S3's executor hardening lands,
a concurrent daemon cycle can write ref files while resolve holds the lock;
resolve re-checks pins after acquiring the lock, shrinking but not
eliminating the window. That is why the hardening is PR-2, a prerequisite,
not an optional follow-up.

---

## 4. Acceptance rows

### 4.1 Existing rows this satisfies

- **T10** (`large-workdir-onboarding-design-2026-05-25.md:235`): visibility
  is what `tcfs conflicts` + the status banner add (today: per-file
  sync-status rows and daemon logs only); preservation strengthened — until
  resolve, behavior unchanged (record-only, zero writes); at resolve, every
  local byte the winner holds is kept; the per-file `.git` splice paths that
  could violate T10 are hard-refused (S1/S2).
- **T11** (`:236`) under the repo-tightened bar
  (`repo-roam-test-plan-2026-06-08.md:127`, §6 R5 `:328-333`): both
  histories rehydratable git-natively —
  `git checkout refs/tcfs/theirs/<device>/heads/<b>` or
  `git worktree add ../theirs refs/tcfs/theirs/...` — in one fsck-clean,
  fingerprint-able repo that then converges by ordinary sync; loser's line
  parked on both machines (S10). No filesystem duplication, no conflict-copy
  files inside `.git`.
- **M5-R → M5-D trajectory** (`lazy-traversal-qa-permutation-matrix-2026-05-09.md:53-54,178`):
  the plan promised manual keep-both; this ships the operator verb and meets
  the M5-D2 closure bar by construction (CLI-driven — no RPC timeout;
  idempotent second resolve; single commit point ⇒ no partial state). The
  parallel track (§2.6) closes M5-D2 for ordinary files.
- **G5-git-5** (`repo-roam-test-plan-2026-06-08.md:359-361`, "EXPECTED TO
  FAIL until .git-aware resolution lands"): flipped red→green by
  G5-git-7/-8 (fence) + G5-git-9 (verb) + G5-git-13 (loser convergence) —
  the full end-to-end row is G5-git-13.

### 4.2 New harness rows

Extend `scripts/git-dotgit-fsck-conflict-harness.sh` (same throwaway-fixture,
evidence-dir, gated-remote pattern; row naming per
`docs/ops/dotgit-as-files-conflict-corruption-2026-06-08.md:115-146`; the
two-device diverged fixture already exists in stage 4).

| Row | Fixture / action | Pass bar |
|-----|------------------|----------|
| G5-git-6 | diverged fixture → `tcfs conflicts --json` | exactly one repo group; both HEAD SHAs; honest "objects not local" degradation when applicable; age; `times_recorded` ≥ 1 |
| G5-git-7 | per-file `tcfs resolve <repo>/.git/refs/heads/main --strategy keep-remote` (and keep-both/keep-local; also via MCP) | daemon refuses with repo-group guidance; ZERO filesystem writes; `git fsck --full` byte-identical before/after |
| G5-git-8 | `conflict_mode=auto` + `.git` conflict event | AutoResolver defers; no `do_auto_download` fires; flips stage 4 from evidence-row to pass/fail |
| G5-git-9 | keep-both on device A (winner) | `refs/tcfs/theirs/B/heads/main` == B's old SHA; `refs/heads/main` == A's SHA; `git fsck --full` clean; `git log --all` shows both lines; group conflicts cleared all-or-nothing; second resolve = no-op |
| G5-git-10 | remote SHA's commit absent from local odb | resolve refuses pre-write; zero ref writes; conflicts intact; retry-after-cycle message |
| G5-git-11 | object deleted post-bundle (fsck-after fails) | refs rolled back from pre-resolve bundle; conflicts intact; non-zero exit; bundle `git bundle verify`-clean |
| G5-git-12 | group contains a divergent submodule head (`.git/modules/<n>/refs/heads/*`) | whole-group refusal naming the unparkable path; zero writes (locks in the S4 veto) |
| G5-git-13 | two-daemon convergence after G5-git-9 | loser's `refs/heads/main` == A's SHA; loser's old SHA reachable via parked theirs-ref on the loser too; both fsck clean; next cycle on BOTH sides records zero conflicts (the converges-never gap closed) — **this row flips G5-git-5 green** |
| G5-git-14 | resolve on dirty tree / with `index.lock` present | refused pre-write; `.git` byte-identical; conflict retained |
| B-1..B-3 (parallel track) | plain-file keep-both: happy path / crash injection between temp-download, copy, rename / cascade fence | loser bytes at `{stem}.tcfs-conflict-*` with blake3 proof; original path always present under any crash; no stray `.tcfs-partial` after retry; copy-of-copy refused |

---

## 5. Increments (smallest shippable first; each independently green)

**PR-1 — fence + see (ships alone, zero write-path risk).**
Daemon `ResolveConflict` `.git`-internal refusal (`grpc.rs:1526` entry; S1) —
covers MCP automatically; NATS auto-path defer (`daemon.rs:1808-1836`; S2);
`tcfs conflicts` read verb with repo grouping, both-HEAD evidence, honest
degradation, `--json`; `times_recorded` counter; escalating cadence +
sync-status banner; MCP description update + `list_conflicts` tool.
*Evidence bar:* harness rows G5-git-6, -7, -8 green. This alone removes the
live G5-git-5 splice vector and the silent-re-record hole, before any new
capability exists.

**PR-2 — prerequisite hardening.**
Executor ref-class writes hard-respect a foreign `.git/tcfs.lock` holder
(fixes `reconcile.rs:1539-1547` best-effort; S3); add
`ConflictInfo.remote_manifest_key` (serde-default) populated at
`reconcile.rs:1737-1746`. Small, independently green.
*Evidence bar:* unit/integration test — a held foreign lock defers ref-class
writes for that repo, non-ref writes proceed; serde round-trip compat test on
old state caches.

**PR-3 — the verb (winner side).**
`conflict_git.rs::resolve_git_keep_both` (§2.3 steps 1-10, including the S4
submodule/non-parkable veto and dirty-tree refusal); `read_remote_ref_sha` →
`pub(crate)`; new git_safety helpers (`theirs_ref_name`, `objects_present`,
`fsck_clean`, `park_ref`) on `run_git` (`git_safety.rs:163`); CLI routing in
`cmd_resolve` (dry-run default + `--execute`), real-ConflictInfo prompt fix;
`--theirs-as-branch` naming skin; keep-local/keep-remote as park-first sugar
(or omitted, per Q2).
*Evidence bar:* harness rows G5-git-9, -10, -11, -12, -14 green. After this
PR the operator can resolve; the winner converges outward.

**PR-4 — loser-side no-loss guard.**
Pre-overwrite parking + state-dir bundle in the execute loop beside the
FF-pin re-verify (`reconcile.rs:1576-1584`; S10), explicitly covering or
deferring on module gitdirs.
*Evidence bar:* harness row G5-git-13 green — the two-daemon convergence row
that flips G5-git-5 end-to-end.

**PARALLEL (independent, any time) — plain-file keep-both hardening (§2.6).**
*Evidence bar:* rows B-1..B-3; M5-D2 checklist
(`lazy-traversal-qa-permutation-matrix-2026-05-09.md:178`) green for
ordinary files.

---

## 6. Non-goals and open operator questions

### Non-goals

- **No automatic merging.** The verb makes both lines present and reachable;
  combining them is human git work. TCFS never invents a merge commit.
- **Committed work only.** The loser's index (staged-but-uncommitted hunks),
  reflog depth, and stash stack beyond the parked `refs/stash` tip are not
  byte-preserved cross-device after convergence — `git bundle --all` cannot
  carry reflog/index (git limitation, stated not hidden). The acceptance bar
  is "no committed work is lost"; the loser's full pre-overwrite `.git` is
  bundled to its own state dir as the escape hatch.
- **No remote version history.** No manifest-by-hash fetch API; the loser's
  bytes come from the loser's disk (guaranteed) via already-roamed objects.
- **No `conflict-export` peer verb in v1.** Its premise ("peer's commits
  often exist only on the peer's disk") is wrong for objects — they roam
  while refs conflict (§2.1). Deferred trigger: peer-offline resolves become
  a real operator need.
- **No plan-time over-veto relaxation.** Auto-parking divergent tags /
  stale remote-tracking refs at plan time so provable heads FF without
  operator involvement (the §1b unlock) resolves divergence **without
  explicit operator intent** — it gets its own design note + rows, built on
  the parking primitives this doc proves. Explicitly not needed for T10/T11.
- **No daemon RPC for group resolve** (the M5-D lineage) — deliberate, not
  deferred-by-accident.
- **Non-`.git` conflict semantics unchanged** beyond the §2.6 hardening;
  `compare_clocks` and #513's reclassification untouched.
- **TIN-1549's full desktop status/progress/error vocabulary** — only the
  CLI/log/banner surfaces named here.
- **Raw mode only** (S12); bundle-mode conflict story untouched.

### Open operator questions (Jess decides before PR-3)

1. **Q1 — parking namespace default:** `refs/tcfs/theirs/<device>/**`
   (hidden from branch listings, mirrors full ref layout) vs real branches
   `tcfs/conflict/<device>-<shortsha>` (visible in `git branch`, roams as an
   ordinary FF push). Proposal: namespace by default, `--theirs-as-branch`
   opt-in. Confirm?
2. **Q2 — bare group `--keep-local`/`--keep-remote`:** ship as park-first
   sugar over the same primitive, or omit entirely in v1 (theirs-parking +
   plain git covers every case)? Red-team verdict: never ship them without
   parking.
3. **Q3 — dirty-tree refusal default:** hard refuse with `--allow-dirty`
   escape, or warn-and-proceed? Proposal: hard refuse.
4. **Q4 — state-dir bundle retention:** keep resolve/undo bundles forever, or
   prune with `tcfs conflicts prune` after N days? (They can be large for
   big repos; neo is disk-constrained.)
5. **Q5 — escalation cadence:** 24h/7d WARN thresholds acceptable, or should
   a conflicted repo page harder given the 236-repo enrollment plan?
6. **Q6 — ticket routing:** fold into TIN-1549 (conflict/status UX) vs a new
   ticket for the verb with TIN-1549 taking only the `tcfs conflicts` /
   banner surface?

---

## 7. Linear-ready summary

1. Post-#513, genuinely diverged `.git` repos stay Conflict forever (record-only arm, `reconcile.rs:1737-1746`) with no operator verb; the only existing verb (per-file `ResolveConflict`) actively corrupts `.git` groups (`grpc.rs:1650-1825`) and the NATS auto path (`daemon.rs:1808-1836`) is a live splice vector — G5-git-5.
2. Design: operator-invoked repo-group keep-both that parks the losing side's heads as `refs/tcfs/theirs/<device>/**` via `git update-ref` against objects that already roam while refs conflict (`git_safety.rs:187-203`), gated fail-closed on object presence, lock+quiesce, fsck before/after, pin re-verify, state-dir undo bundle, single clock-tick commit point; loser-side pre-overwrite parking guard makes "no committed work lost" hold on both machines and closes convergence.
3. Increments: PR-1 fence+`tcfs conflicts` (ships alone, kills the splice vector) → PR-2 hard lock respect + ConflictInfo manifest pin (prerequisite) → PR-3 the verb (winner side, submodule veto inherited from #513's shape) → PR-4 loser guard (flips G5-git-5/T10/T11 green, harness row G5-git-13); parallel: plain-file keep-both crash-ordering rework (M5-D2).
4. Acceptance: T10/T11 + repo-tightened R5 bar ("BOTH `.git` states preserved and each fsck-clean", repo-roam-test-plan `:127`) via new harness rows G5-git-6..14 on the #506 harness pattern.
5. Open operator questions before PR-3: parking namespace default, whether bare keep-local/keep-remote ship at all, dirty-tree policy, bundle retention, escalation cadence, ticket routing (TIN-1549 vs new).

---

## Implementation status (updated 2026-07-05)

This design is now the design-of-record; PR-1 through PR-4 are merged. The
remaining work is fleet deploy plus the live divergent keep-both canary.

| Rung | PR | Merge commit | Status |
|------|----|--------------|--------|
| **PR-1** — fence per-file `.git` resolution (CLI+MCP) + `tcfs conflicts` read verb | #526 | `afd84b2` | ✅ merged |
| (hardening) — fence paths + persistence | #527 | `449846e` | ✅ merged |
| **PR-2** — executor hard-respects a foreign `.git/tcfs.lock`; `ConflictInfo.remote_manifest_key` | #528 | `1e41a23` | ✅ merged |
| **PR-3** — repo-group keep-both resolver (`resolve_repo_keep_both`): parks losing heads at `refs/tcfs/theirs/<device>/**`, fsck-gated both sides, dry-run default, state-dir undo bundle, **operator-CLI-only** (MCP/auto excluded via the `operator_cli` provenance gate) | #529 | `831d363b` | ✅ merged |
| **PR-4** — loser-side no-loss guard (pre-overwrite parking; flips harness G5-git-13 / T10/T11 live after deploy/canary) | #534 | `4c61da4` | ✅ merged |

**PR-4 is merged, not live-proven.** The remaining gate is deploy + canary:
builds must not run locally on neo, and PZM cannot be used for TCFS offload
until lab's PZM directory-health/Nix-context/denial-log/remote-builder verifier
passes again. Until deploy + canary run, the honest claim is:
**divergent `.git` conflicts are safely fenced, visible (`tcfs conflicts`), and
operator-resolvable (`tcfs resolve … --execute`), but the two-machine
live-convergence proof (G5-git-5 T10/T11 green) is pending.**

**Ratified operator §6 answers (2026-07-05):** parking namespace default = `refs/tcfs/theirs/**` (not real branches); bare `keep-local`/`keep-remote` = omitted (park-first only); dirty-tree = hard refuse; ticket routing = new ticket for the verb + TIN-1549 keeps the `tcfs conflicts`/banner UX surface. Bundle retention and escalation cadence remain open (non-blocking; PR-4-adjacent).

**Security note (from the PR-3 adversarial review, #529):** the repo-group execute path is reachable **only** via the operator CLI (`operator_cli` proto flag); MCP rejects `git_keep_both*` before connect and the daemon refuses the dispatch without the flag — the agent/auto/NATS threat is closed. The undo bundle is written to the machine-local state dir (never in-tree), with `.git/tcfs-undo/**` fail-closed in the blacklist as belt-and-suspenders.
