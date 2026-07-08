# Divergent keep-both canary — LIVE — 2026-07-08 (fixture 20260707T071335Z)

**Closes the divergent (non-FF) half of G5-git-5 (TIN-1620 / TIN-1908) LIVE.**
Two hosts committed *different* work on the same `.git` repo while each was stale;
the R5 divergent `.git`-conflict now **CONVERGES** — loser parks its own head and
adopts the winner's, with zero committed work lost on either machine — where the
2026-06-09 canary stalled at 5 conflicts and where the fast-forward resolver
(#513, proven in `bidirectional-ff-canary-20260705T225429Z/`) does not apply
because neither head is an ancestor of the other.

The run **found and drove out four product defects before it could pass** — that
narrative is the substance of this packet, not a footnote (see §Findings). Two are
now fixed and proven live (#540, #541); two remain open (TIN-2653, TIN-2657) and
are honestly scoped below.

## Fixture

- Disposable repo `divergent-kb-20260707T071335Z`, disposable prefix
  `git-roam/divergent-kb-20260707T071335Z` (mirrors the FF canary's throwaway
  `git-roam/bidi-ff-*` convention — never run against enrolled roam infra).
- neo (macOS, device `03d8a0bd-36de-4df8-9b88-a923a9dd2c7a`) ⇄ honey (Rocky,
  device `d1176e5d-8baa-413e-8d50-68c1dbd36506`).
- SHAs (`shas.env`): `BASE=38468dbc`, `NEO=88b217fa` (winner), `HONEY=df41b693`
  (loser).

## Fleet

- Both hosts' **daemons** run a post-#534 build (`4c61da4`, TIN-2552 loser-side
  no-loss guard) — the guard under test is the deployed daemon code, not a local
  reproduction.
- The manual reconcile/resolve **CLIs** were built from post-#541 `main`
  (`e9950a99`, which includes #540 and #541): neo
  `/nix/store/ws81ydsk...`, honey `/nix/store/7ya5i625...`. The CLI/daemon split
  is what surfaced TIN-2657 (§Findings 4).
- Raw `.git`-as-files (`sync_git_dirs=true`, `git_sync_mode=raw`) — see
  §Config caveat: enabled via `TCFS_CONFIG` overlays, **not** deployed host
  config.

## Result — divergent conflict converges, zero committed work lost

| Step | Action | Evidence | Outcome |
|------|--------|----------|---------|
| 1 | neo seeds baseline `38468dbc`, pushes | `neo/reconcile-seed.log`, `neo/reconcile-seed-git.log` | pushed, fsck clean |
| 2 | honey pulls baseline | `honey/reconcile-pull-baseline.log` | honey at `38468dbc`, 0 conflicts |
| 3 | **each host commits different work while stale** — neo `88b217fa`, honey `df41b693` | `shas.env` | genuine divergence, neither an ancestor of the other |
| 4 | first divergent reconcile (pre-#540 CLI) | `neo/reconcile-diverge.log` (9 pushed, **0 conflicts**), `honey/reconcile-diverge.log` (1 pushed / 4 pulled, **0 conflicts**) | **DEFECT: divergence silently absorbed** → TIN-2584 |
| 5 | re-reconcile with #540-fixed CLI | `honey/rerun-fixed-cli-conflict.log` | **5-conflict repo group recorded** (`.git/refs/heads/main` [head] + index/COMMIT_EDITMSG/logs) — fix proven live |
| 6 | winner-side resolve attempt (T11 §5 verb) | `honey/resolve-dryrun.log`, `honey/resolve-final-run.log` | resolve VERB blocked by TIN-2657 (daemon reads a different state file) — **not claimed** |
| 7 | **loser-guard convergence** — honey fresh-state reconcile pull | `honey/loser-guard-verify.txt`, `neo/show-ref-final.txt` | **guard fires in production**: honey parks its head at `refs/tcfs/theirs/d1176e5d.../heads/main` **before** overwriting `main` → `88b217fa`; undo bundle written |
| 8 | parked ref + winner main roam to neo | `neo/show-ref-final.txt`, `neo/reconcile-pull-parked.log`, `neo/reconcile-pull-final.log` | both commits present + reachable on **both** hosts |
| 9 | second reconcile cycle both sides | `neo/second-cycle.log`, `honey/second-cycle.log` | **0 conflicts, 0 errors both** — forever-Conflict gap closed |

## Convergence path (what actually made it green)

The #534 loser-side no-loss guard fired **in production** during honey's
fresh-state reconcile pull. Before overwriting `refs/heads/main` (`df41b693` →
`88b217fa`) it:

1. Parked honey's live head at
   `refs/tcfs/theirs/d1176e5d-8baa-413e-8d50-68c1dbd36506/heads/main` = `df41b693`
   (honey's own device namespace — reachable on the loser itself), and
2. Wrote a machine-local undo bundle
   `keep-both-undo/584df06defdb84b1/keep-both-1160f3ad-0627-443c-a2e9-976d45b230a1.bundle`
   — `git bundle verify` clean, `fsck-rc=0` (`honey/loser-guard-verify.txt`).

That parked theirs-ref then **roamed to neo** (`neo/show-ref-final.txt` shows both
`refs/heads/main = 88b217fa` and `refs/tcfs/theirs/d1176e5d.../heads/main =
df41b693`), so honey's committed work is present and reachable on **both**
machines. `zero-loss-final.txt`: on honey both SHAs reachable, `rev-list --all` =
3 on both hosts. `T11` rehydration proof: a worktree checked out from the
parked theirs-ref reads back `honey work` (`zero-loss-final.txt` tail).

> The **operator resolve VERB** path (T11 §5 flavor: `tcfs resolve … --execute`)
> remains blocked by TIN-2657 and is **explicitly NOT claimed here**. The G5-git-13
> / T10 / T11 acceptance criteria are **convergence-based** (both heads reachable,
> both fsck-clean, second cycle zero-conflict on both machines) and are **fully
> met** by the loser-guard path above.

## Findings — four product defects driven out live

| # | Ticket | Defect | State | Proof |
|---|--------|--------|-------|-------|
| 1 | **TIN-2584** | Divergence **silently absorbed**: an out-of-band commit never ticks the vclock, so the dominated clock is *structurally* conflict-unreachable and the LIST race returns `UpToDate` — the first divergent reconcile recorded **0 conflicts** and quietly moved on. | **FIXED — #540**, proven live | `neo/reconcile-diverge.log` + `honey/reconcile-diverge.log` show the silent absorb (0 conflicts); after #540 honey re-recorded the **5-conflict repo group** — `honey/rerun-fixed-cli-conflict.log` |
| 2 | **TIN-2652** | Plan-path conflicts were recorded with `status=synced`, leaving them **invisible to the resolver** (the resolver only walks `status=conflict` entries). | **FIXED — #541**, proven at state layer | `honey/resolve-final-run.log`: re-reconcile with #541 CLI flips the five conflict-entry statuses `synced → conflict` (`['conflict', 'conflict', 'conflict', 'conflict', 'conflict']`) |
| 3 | **TIN-2653** | Headless session token is **write-only**: TOTP provenance is unusable over ssh, so the operator resolve verb cannot present a valid session. Resolve was exercised via the repo-precedent `require_session=false` bypass window, **re-locked immediately after**. | **OPEN** | `honey/resolve-final-run.log`: "bypass window + guarded resolve" then "re-lock → active" |
| 4 | **TIN-2657** | Daemon remaps `sync.state_db` → `state.json` (`crates/tcfs-daemon/src/daemon.rs:315`); the CLI (`tcfs conflicts`/`resolve`) and the daemon operate on **different files**. This is why the resolve VERB path returned **0 refs** even with the post-#541 CLI. | **OPEN** | `honey/resolve-final-run.log`: guarded `keep-both` dry-run reports "0 ref(s)" → "GUARD FAILED — not executing" |

Findings 1 and 2 had to be fixed just to make the conflict *recordable* and
*resolver-visible*; 3 and 4 block the operator VERB but not the automatic
loser-guard convergence path that this canary accepts on.

## G5-git-13 — six criteria (runbook §10 / harness Stage 6)

| # | Criterion | Result | Evidence |
|---|-----------|--------|----------|
| 1 | Fixture genuinely diverged (`NEO ≠ HONEY`, neither an ancestor) | ✓ | `shas.env` (`88b217fa` vs `df41b693`) |
| 2 | Both `.git` fsck-clean after convergence (rc=0) | ✓ both | `honey/loser-guard-verify.txt` (`fsck-rc=0`); neo clean (`zero-loss-final.txt`) |
| 3 | Loser `refs/heads/main` == winner SHA | ✓ | honey `main = 88b217fa`; `neo/show-ref-final.txt` |
| 4 | Loser's old head reachable via its **own** parked theirs-ref on the loser + undo bundle verified | ✓ | `refs/tcfs/theirs/d1176e5d.../heads/main = df41b693`; bundle sha1/clean — `honey/loser-guard-verify.txt` |
| 5 | Both commits present + reachable on **both** machines | ✓ | parked ref roamed to neo (`neo/show-ref-final.txt`); `rev-list --all` 3==3 (`zero-loss-final.txt`) |
| 6 | Second cycle **0 conflicts / 0 errors** on both sides | ✓ both | `neo/second-cycle.log`, `honey/second-cycle.log` |

- **T10 conflict-visibility:** proven with the recorded 5-conflict repo group +
  honest peer degradation ("`<objects not local yet>`") — `honey/rerun-fixed-cli-conflict.log`.
- **T11 rehydration:** worktree from the parked theirs-ref reads back "honey work"
  — `zero-loss-final.txt`.

## Significance

The 2026-06-09 canary proved forward dev-env zero-diff roam but the reverse
stalled ("5 conflicts, 0 pushed") — the G5-git-5 gap. #513 closed the
**fast-forward** half (proven 2026-07-05). This packet closes the **divergent
(non-FF)** half: when two hosts commit incompatible work on the same repo, the
loser-side no-loss guard (#534) parks the loser's head before adopting the
winner's, both commit lines stay reachable on both machines, both `.git` stay
fsck-clean, and the next reconcile cycle records zero conflicts on both sides.
**G5-git-5 is closed end-to-end** (FF half 2026-07-05 + divergent half
2026-07-08). Harness row **G5-git-13** — the two-daemon convergence row — is
green, now backed by a live fleet canary rather than only the local pure-git
fixture reproduction in `scripts/git-dotgit-fsck-conflict-harness.sh` Stage 6.

## Config caveat

Git sync was enabled for this canary via `TCFS_CONFIG` overlays
(`sync_git_dirs=true`, `git_sync_mode=raw`) — these are **NOT** present in the
deployed host `config.toml` files. The runbook §0.7 claim that they were already
global on both hosts was **wrong**; verified live. The overlays mirror the
scheduled roam-unit isolation pattern and were used only for this disposable
fixture.

## Teardown

Fixture repos removed on both hosts post-evidence. The disposable remote prefix
`git-roam/divergent-kb-20260707T071335Z` was left to the storage backend's
orphan-sweep (no admin delete path invoked).
