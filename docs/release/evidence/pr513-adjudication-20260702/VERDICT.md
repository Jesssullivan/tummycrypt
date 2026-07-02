# PR #513 adjudication — one-page verdict packet (2026-07-02)

Per the 2026-07-01 operator decision (ledger item 10): #515 fence and #506 harness landed
first; agents ran the expected-red flipflop canary + adversarial review; this packet is the
operator rubber-stamp input. **The merge itself stays operator-gated.**

## VERDICT: DO NOT MERGE YET — BLOCKED on two must-fix findings

The adversarial final-gate review (posted on PR #513 at head `a12e430`) found two blockers.
This packet independently **re-verified both in the PR-head source** before adopting them.

## Evidence run in this packet (2026-07-02, dated, reproducible)

| Run | Where | Result |
|---|---|---|
| FACET 6 harness (`scripts/git-dotgit-fsck-conflict-harness.sh`, local-fixture) | `main` @ `2f3c046` (post-#514/#515/#506/#516) | `harness-main-2f3c046/`: **clean flip-flop invariant holds**; midwrite `index.lock` gate recorded; conflict row **CORRUPTION-RISK CONFIRMED** (`invalid sha1 pointer` after per-file `.git` resolution) — the expected-RED baseline stands on main |
| `cargo test -p tcfs-sync --test git_ff_resolution` | PR #513 head @ `a12e430` | `git-ff-tests-a12e430.txt`: **7/7 pass** (FF converge both directions, divergent-stays-conflict, concurrent-clock push, RemoteAhead reclassify, object-failure barrier ×2, fabricates-nothing) |

## G5-git-5 grade: stays **expected-red**. No flip to green.

The harness red row is "per-file `.git` resolution corrupts". #513 fixes that mechanism for
*proven head refs*, but BLOCKER-1 means divergence in every other ref-valued file is now
resolved silently (fail-open) instead of corrupting loudly — a contract violation, not a green.

## Blockers (re-verified line-by-line at `a12e430`)

- **B1 — unproven refs ride the FF group-win.** `decide_repo_fast_forward`
  (`reconcile.rs:1147`) proves ancestry **only** for `.git/refs/heads/*`
  (`head_ref_for_git_path`, `git_safety.rs:303` — returns `None` for packed-refs, tags,
  stash, remotes). The apply loop (`reconcile.rs:1083–1124`) then rewrites **every** `.git/*`
  conflict in the repo group toward the winning direction, pushes carrying `GitFastForward`
  dominance. Divergent tags / stash / packed-refs are silently clobbered and the loser pulls
  the clobber down as plain RemoteNewer. Violates the PR's own "divergent stays Conflict,
  zero writes" contract. Fix: veto the repo when the group contains a non-head ref-valued
  conflict, or extend the proof to packed-refs/tags (equality required for tags).
- **B2 — plan-time proof, execute-time spend; no local re-verify.** Pull direction:
  reclassified pulls are plain `PullReason::RemoteNewer` (`reconcile.rs:1116`) — zero
  execute-time guard; a `git commit` on the losing device during the object wave gets its ref
  + reflog overwritten (silent lost work). Push direction: the upload guard
  (`engine.rs:~1633`) re-checks only the **remote** index entry, never that the local ref
  still equals the proven SHA. Fix: carry the plan-time local ref SHA in both directions and
  byte-compare immediately before overwrite/dominate; mismatch → defer + re-plan.

## Mediums (same-PR or immediate follow-up tickets)

- **M-3:** `.git/tcfs.lock` is not in the deny set (verified: zero hits in `blacklist.rs`) —
  a leaked lock roams fleet-wide and can become permanently unstealable on foreign hosts.
- **M-4:** submodule gitdirs (`.git/modules/**`) match neither object- nor ref-class needles
  → no ordering, no barrier for raw-roamed submodules.
- **Deploy precondition to state in the PR:** `TCFS_ASSUME_FRESH_PREFIX=1` (live on neo per
  fleet state docs) skips the entire conflict/dominance guard block (`engine.rs:1564`).

## What is genuinely sound (keep; do not re-litigate)

HIGH-1 (temp-dir isolation — planning fabricates nothing under any sync root), HIGH-2
remote-side guard (dominate only while the remote entry equals the proven manifest),
MEDIUM-1 objects-before-refs as a real per-repo barrier in both executors, lock staleness
(fail-safe on PID reuse), all five new tests are non-vacuous, #515 fence interplay clean,
feature flag default-off with byte-identical non-git planning.

## Operator checklist to unblock

1. Land B1 + B2 fixes on the PR branch (+ regression tests: head-FF + divergent tag in the
   same repo must stay Conflict; mid-window local commit must defer, not clobber).
2. M-3/M-4 same-PR or ticketed immediately.
3. Re-run: `cargo test -p tcfs-sync --test git_ff_resolution` + full CI + this harness.
4. Then rubber-stamp merge (fence #515 ✅ and harness #506 ✅ already merged).
