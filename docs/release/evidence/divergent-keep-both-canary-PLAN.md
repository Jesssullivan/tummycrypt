# LIVE Divergent Keep-Both Canary — Runbook (neo ⇄ honey)

**Status:** EXECUTED 2026-07-08 — PASS. Results:
`docs/release/evidence/divergent-keep-both-canary-20260707T071335Z/RESULTS.md`.
Convergence proven via the automatic loser-guard (see §6); the operator resolve
VERB (§5) is NOT claimed — blocked by TIN-2657 (daemon `sync.state_db` →
`state.json` remap) and TIN-2653 (headless session token). The run also fixed
TIN-2584 (#540) and TIN-2652 (#541) as prerequisites.
**Flips:** harness row `G5-git-13` red→green (LIVE-PROVEN); closes gate
`G5-git-5` end-to-end (T10/T11, `docs/ops/repo-roam-test-plan-2026-06-08.md:127`,
§6 R5 `:328-333`).
**Ticket:** TIN-2552 / PR #534 (loser-side no-loss guard).

---

## 0. Grounding — what is actually true right now (read this first)

This plan is maintained from current `origin/main`. Run it only from a fresh
checkout that contains #513, #529, and #534, and after the lab/fleet deploy has
installed a build newer than merge commit `4c61da4`.

### Surprises vs. the task brief

1. **PR #534 is already MERGED**, not "at merge gate." `gh pr view 534` →
   `state: MERGED`, `mergedAt: 2026-07-06T01:16:49Z`, merge commit `4c61da4`
   (`tcfs: loser-side no-loss guard for git keep-both (TIN-2552)`). The brief's
   premise ("once merged...") is already satisfied on `origin/main` — what is
   **not** yet true is fleet deployment (see below).
2. **The repo-roam test plan and FF canary evidence are now present on main.** Use
   `docs/ops/repo-roam-test-plan-2026-06-08.md` for the G5/T10/T11 rows and
   `docs/release/evidence/bidirectional-ff-canary-20260705T225429Z/RESULTS.md`
   as the fast-forward baseline.
3. **`docs/ops/current-workstream-truth-2026-07-06.md` is the current operator
   checkpoint.** It says #534 is merged, but the fleet deployment and divergent
   live canary are still pending.
4. **Neither `tcfs resolve` nor `tcfs conflicts --json`'s CLI surface has the
   `--theirs-as-branch`, `--allow-dirty`, or `--restore-undo` flags** that
   `docs/design/git-divergent-keep-both-2026-07-02.md` describes as UX skin (§2.2,
   §2.3 step 10). Only `tcfs resolve <path> --strategy keep-both [--execute]` and
   `tcfs conflicts [--json] [--state <path>]` shipped. Undo restoration is a manual
   `git bundle` operation (§8 below), not a CLI flag.
5. **Architectural gotcha (verified live, not in any doc): `tcfs resolve` talks
   only to the running daemon over gRPC, and the daemon's `ResolveConflict`
   handler resolves against the daemon's OWN configured `sync.state_db`** — it does
   **not** accept a `--state` override (unlike `tcfs conflicts`, which does). The
   existing enrolled `git-roam/tool-daemon` scheduled unit on honey (systemd timer
   `tcfsd-reconcile-git-roam-tool-daemon`) deliberately uses an **isolated** state
   file (`~/.local/state/tcfsd/reconcile/git-roam-tool-daemon.json`, comment: *"Per-root
   state isolation: never share sync state with the primary sync_root"*,
   `home-manager`-generated wrapper script). A conflict recorded in that isolated
   file is **invisible to `tcfs resolve`**, because the daemon only
   `reload_from_disk()`s its own `sync.state_db`
   (`~/.local/share/tcfsd/state.db` on both hosts, verified live). **This canary
   must therefore point its manual reconcile invocations at the daemon's own
   `sync.state_db` path** (§2), not an isolated file, or `tcfs resolve` will see
   zero conflicts and the whole canary silently no-ops. Verify this live with the
   dry-run sanity check in §4 before trusting `--execute`.
6. **Fleet is on tcfs 0.12.16 on both hosts** (verified live: `tcfs --version` on
   neo and `ssh jess@honey 'tcfs --version'` both return `tcfs 0.12.16`) — matching
   the FF canary's fleet line. `Cargo.toml` on `origin/main` is **still** `0.12.16`
   (unreleased, no version bump yet for #534/#535), and the binary has no embedded
   build SHA (`tcfs --version` prints only the semver). **A version number alone
   cannot distinguish pre-#534 from post-#534 fleet builds** — see §0.1 for the
   actual check.
7. Both hosts' deployed `config.toml` already have `sync.sync_git_dirs = true` and
   `sync.git_sync_mode = "raw"` **globally** (verified: `cat ~/.config/tcfs/config.toml`
   on neo, `ssh jess@honey cat ~/.config/tcfs/config.toml` on honey). No config
   change is needed for raw `.git`-as-files mode.

### 0.1 Precondition: is the fleet build post-#534?

There is no `tcfs --version --build-sha` surface. Use the nix store path identity
as the practical proxy — a rebuild after `origin/main` moves past `4c61da4`
necessarily gets a new store path hash:

```sh
# BEFORE deploy — record current store paths
which tcfs                                  # neo
readlink -f "$(which tcfs)"                 # neo — note the hash
ssh jess@honey 'readlink -f "$(which tcfs)"'  # honey — note the hash

# cross-check the deployed derivation is from a rev that IS-ANCESTOR 4c61da4
# (run against whatever repo/rev the fleet's flake pins tcfs-cli to — this
# repo is the source, not the deploy flake; find the deploy flake's lock rev
# and run from a clone of THIS repo):
git merge-base --is-ancestor 4c61da4 <pinned-rev> && echo "post-#534" || echo "PRE-#534 — redeploy required"
```

As of this grounding pass, **both hosts are pre-#534** (0.12.16, same store paths
seen in the 2026-07-05 FF canary, which predates the #534 merge timestamp). **A
deploy is required before any step in §2 onward.** Follow this repo's normal
fleet-deployment flow (`docs/ops/fleet-deployment.md`) to cut and roll a build
containing `4c61da4`, then re-run the check above on both hosts until it prints
`post-#534` and the store-path hash has changed from the recorded baseline.

---

## 1. What T10 / T11 require (origin/main, `docs/ops/repo-roam-test-plan-2026-06-08.md`)

Quoted verbatim (line numbers from `origin/main`):

> `:127` — **T10 / T11 + M5 / M5-R** — same-file conflict visible; keep-both
> recovery — for a repo this means BOTH `.git` states preserved and each
> fsck-clean.
>
> `:328-333` (§6 R5) — conflict visible, local bytes preserved, manual keep-both
> → both `.git` states preserved, each fsck-clean and each fingerprint-able.

Design doc mapping (`docs/design/git-divergent-keep-both-2026-07-02.md` §4.1):

- **T10** = visibility (`tcfs conflicts`) + record-only preservation (zero writes
  until resolve).
- **T11** = both histories rehydratable git-natively
  (`git checkout refs/tcfs/theirs/<device>/heads/<b>` or
  `git worktree add ../theirs refs/tcfs/theirs/...`), one fsck-clean repo, then
  ordinary convergence.
- **G5-git-13** (harness `scripts/git-dotgit-fsck-conflict-harness.sh` Stage 6,
  `origin/main`) is the acceptance row that flips **G5-git-5** green: "two-daemon
  convergence after G5-git-9 — loser's `refs/heads/main` == A's SHA; loser's old
  SHA reachable via parked theirs-ref on the loser too; both fsck clean; next
  cycle on BOTH sides records zero conflicts."

---

## 2. Canary repo — use a disposable fixture, not `tinyland-tool-daemon`

The existing enrolled roam repo (`/home/jess/git/tinyland-tool-daemon` on honey,
prefix `git-roam/tool-daemon`, systemd timer `tcfsd-reconcile-git-roam-tool-daemon`)
is **live production infra with its own isolated state file** (§0 surprise 6) and
its own continuously-running timer. Do not run a destructive-shaped canary against
it. Mirror the 2026-07-05 FF canary's own convention ("disposable prefix
`git-roam/bidi-ff-*`"): create a throwaway repo + throwaway prefix, torn down after.

```sh
TS=$(date -u +%Y%m%dT%H%M%SZ)
REPO_NAME="divergent-kb-${TS}"
PREFIX="git-roam/${REPO_NAME}"

# neo's daemon-owned state.db — THIS is what tcfs resolve will read (§0 surprise 6)
NEO_STATE="/Users/jess/.local/share/tcfsd/state.db"
HONEY_STATE="/home/jess/.local/share/tcfsd/state.db"
```

**Do not use an isolated `--state` file for this canary.** Point every manual
`tcfs reconcile --execute` invocation below at the host's own `sync.state_db`
(the same file the daemon reloads), or `tcfs resolve` will report zero conflicts.

---

## 3. T10 — create genuine divergence, verify conflict visibility

### 3.1 Seed the repo on neo, push baseline

```sh
mkdir -p ~/git/${REPO_NAME} && cd ~/git/${REPO_NAME}
git init --quiet
printf 'base\n' > base.txt
git add -A
git commit --quiet -m "base" --no-gpg-sign   # harmless on neo even if signing works
BASE_SHA=$(git rev-parse HEAD)

tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} \
  --state "$NEO_STATE" --execute
git fsck --full   # expect clean
```

### 3.2 Clone/pull the baseline onto honey

```sh
ssh jess@honey "mkdir -p ~/git/${REPO_NAME}"
# reconcile pulls it down via the same prefix
ssh jess@honey "tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} \
  --state ${HONEY_STATE} --execute"
ssh jess@honey "cd ~/git/${REPO_NAME} && git rev-parse HEAD && git fsck --full"
# must equal $BASE_SHA and be fsck-clean
```

### 3.3 Diverge — DIFFERENT commits on each host while the other is stale

```sh
# --- neo: commit A, but do NOT reconcile yet ---
cd ~/git/${REPO_NAME}
printf 'neo work\n' > neo-work.txt
git add -A && git commit --quiet -m "neo diverges" --no-gpg-sign
NEO_SHA=$(git rev-parse HEAD)

# --- honey: commit B, independently, before honey has seen neo's commit ---
ssh jess@honey "cd ~/git/${REPO_NAME} && printf 'honey work\n' > honey-work.txt \
  && git add -A && git -c commit.gpgsign=false commit --quiet -m 'honey diverges' \
  && git rev-parse HEAD"
# record as HONEY_SHA
```

### 3.4 Both sides reconcile — this records the Conflict, zero writes either way

```sh
tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} --state "$NEO_STATE" --execute
ssh jess@honey "tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} --state ${HONEY_STATE} --execute"
```

Expected: `NotFastForward` on both sides (`git_safety.rs:203-219`), the repo group
stays `Conflict` (`reconcile.rs:1737-1746` arm), **zero local or remote writes**,
`refs/heads/main` unchanged on both hosts (T10's "local bytes preserved").

### 3.5 Verify conflict visibility (T10 acceptance)

```sh
tcfs conflicts --state "$NEO_STATE"
tcfs conflicts --json --state "$NEO_STATE"
ssh jess@honey "tcfs conflicts --state ${HONEY_STATE}"
ssh jess@honey "tcfs conflicts --json --state ${HONEY_STATE}"
```

Pass bar (mirrors harness row **G5-git-6**): exactly one repo group for
`${REPO_NAME}`; both local HEAD SHAs shown (own side accurate, peer side honest
"objects not local yet" degradation is fine if objects haven't roamed); repo-level
grouping, not per-file. Confirm on **both** hosts before proceeding — if either
side shows zero conflicts, stop: the state-file/daemon-visibility gotcha in §0
surprise 6 has bitten you.

---

## 4. Sanity check before `--execute` (do this — do not skip)

```sh
tcfs resolve ~/git/${REPO_NAME} --strategy keep-both
```

No `--execute` → dry-run by default (`main.rs` Resolve struct: `execute: bool`,
default false). Expect printed plan naming the parked ref
`refs/tcfs/theirs/honey/heads/main` at `$HONEY_SHA`, and the line:
`Dry-run only. Re-run with --execute to mutate refs and clear conflicts.`

If this instead errors with "no conflict recorded" or similar, the daemon's
own `sync.state_db` did not pick up the conflict written by §3.4 — re-check that
`$NEO_STATE` really is neo's daemon-configured `sync.state_db` path (grep
`~/.config/tcfs/config.toml` for `state_db`), and that the daemon is running
(`ls -la /Users/jess/.local/state/tcfsd/tcfsd.sock`).

---

## 5. T11 — resolve on neo (designated winner), verify parking + undo bundle

Neo is the operator-chosen winner for this canary (arbitrary, stated up front —
either host could be chosen; the design is symmetric).

```sh
EVID="docs/release/evidence/divergent-keep-both-canary-${TS}"
mkdir -p "$EVID"/{neo,honey}

tcfs resolve ~/git/${REPO_NAME} --strategy keep-both --execute 2>&1 | tee "$EVID/neo/resolve-execute.log"
```

Immediately capture:

```sh
cd ~/git/${REPO_NAME}
git show-ref | tee "$EVID/neo/show-ref-post-resolve.txt"
git fsck --full | tee "$EVID/neo/fsck-post-resolve.txt"
git log --all --oneline -10 | tee "$EVID/neo/log-all-post-resolve.txt"
tcfs conflicts --state "$NEO_STATE" | tee "$EVID/neo/conflicts-post-resolve.txt"
```

### 5.1 Verify winner side (matches harness **G5-git-9**)

- `refs/tcfs/theirs/honey/heads/main` == `$HONEY_SHA` (git_show-ref grep)
- `refs/heads/main` == `$NEO_SHA` (neo's own committed work untouched)
- `git fsck --full` clean
- `git log --all` shows both `neo-work.txt` and `honey-work.txt` reachable
- `tcfs conflicts` on neo now shows the group cleared

### 5.2 Locate + verify the undo bundle (design §2.3 step 5 / S6)

```sh
find /Users/jess/.local/share/tcfsd/keep-both-undo -name '*.bundle' -newer /tmp -print
```

(the dir is keyed `state_dir/keep-both-undo/<blake3(repo_root)[..16]>/keep-both-<uuid>.bundle`,
`conflict_git.rs:558-568` — never in-tree; `blacklist.rs` fail-closed fences any
legacy in-tree `.git/tcfs-undo/**` path so this can never roam and re-conflict).

```sh
BUNDLE=$(find /Users/jess/.local/share/tcfsd/keep-both-undo -name '*.bundle' | sort | tail -1)
git bundle verify "$BUNDLE" | tee "$EVID/neo/undo-bundle-verify.txt"
```

Pass bar: `git bundle verify` clean.

---

## 6. Convergence — the loser side (honey) auto-parks and pulls (G5-git-13)

Do **not** run `tcfs resolve` on honey — PR-4's loser-side guard is automatic,
triggered by the normal reconcile pull path (`reconcile.rs` loser-guard, ~lines
1994-2260 on `origin/main`: `loser_guard_ref_target`, park-before-overwrite,
`deferred_git_refs` fallback).

```sh
# neo pushes: now LocalNewer (new theirs-ref + ticked clock, design §2.4)
tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} --state "$NEO_STATE" --execute \
  2>&1 | tee "$EVID/neo/reconcile-push-post-resolve.log"

# honey pulls: loser-guard should park honey's own old head before the incoming
# neo-dominant clock overwrites refs/heads/main
ssh jess@honey "tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} \
  --state ${HONEY_STATE} --execute" 2>&1 | tee "$EVID/honey/reconcile-pull-loser.log"
```

### 6.1 Verify loser side (matches harness **G5-git-13**)

```sh
ssh jess@honey "cd ~/git/${REPO_NAME} && git show-ref" | tee "$EVID/honey/show-ref-post-pull.txt"
ssh jess@honey "cd ~/git/${REPO_NAME} && git fsck --full" | tee "$EVID/honey/fsck-post-pull.txt"
ssh jess@honey "cd ~/git/${REPO_NAME} && git log --all --oneline -10" | tee "$EVID/honey/log-all-post-pull.txt"
```

Pass bar:

- honey's `refs/heads/main` now == `$NEO_SHA` (converged to the winner)
- honey's OWN self-device theirs-ref (`refs/tcfs/theirs/<honey-device-id>/heads/main`)
  == `$HONEY_SHA` — honey's committed work parked automatically, not lost
- both `$NEO_SHA` and `$HONEY_SHA` commit objects present and reachable
  (`git cat-file -e <sha>^{commit}` for each, on honey)
- `git fsck --full` clean on honey
- honey's own pre-overwrite undo bundle exists under
  `/home/jess/.local/share/tcfsd/keep-both-undo/**` (same S6 mechanism, this
  time written by the loser-guard rather than the resolve verb)

### 6.2 Converges-never-again check — one more cycle on both sides

```sh
tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} --state "$NEO_STATE" --execute
ssh jess@honey "tcfs reconcile --path ~/git/${REPO_NAME} --prefix ${PREFIX} --state ${HONEY_STATE} --execute"
tcfs conflicts --state "$NEO_STATE"
ssh jess@honey "tcfs conflicts --state ${HONEY_STATE}"
```

Pass bar: **zero conflicts recorded on either side** — the forever-Conflict gap
(design §1a) is closed. `git rev-parse HEAD` on neo and honey are identical.

---

## 7. Zero-loss assertion (the actual claim under test)

Run on **both** hosts:

```sh
for sha in $NEO_SHA $HONEY_SHA; do
  git cat-file -e "${sha}^{commit}" && echo "$sha: reachable" || echo "$sha: MISSING (FAIL)"
done
git rev-list --all --count
```

Pass bar: both SHAs reachable on both machines; commit counts match between neo
and honey (`git rev-list --all | sort | diff - <(ssh jess@honey "cd ~/git/${REPO_NAME} && git rev-list --all | sort")`
empty diff). This is the literal "zero commits lost on either host" claim.

Rehydration proof (T11's "both versions can be preserved and rehydrated"):

```sh
ssh jess@honey "cd ~/git/${REPO_NAME} && git worktree add /tmp/theirs-check refs/tcfs/theirs/<honey-device-id>/heads/main && cat /tmp/theirs-check/honey-work.txt"
```

---

## 8. Rollback / abort

If fsck-after fails, or the operator needs to back out mid-canary, the pre-resolve
bundle is the escape hatch (design §2.3 step 8, S6/S7):

```sh
# on the host whose refs need restoring
cd ~/git/${REPO_NAME}
git fsck --full   # confirm current damage
git bundle verify "$BUNDLE"
# restore each ref from the bundle (bundle contains --all refs at pre-resolve state)
git bundle list-heads "$BUNDLE"
git fetch "$BUNDLE" 'refs/*:refs/*'   # or targeted per-ref fetch, then update-ref
git fsck --full   # confirm clean after restore
```

There is no `--restore-undo` CLI flag (§0 surprise 5) — this is manual `git
bundle`/`git fetch`/`git update-ref`, same as `rollback_refs` does internally on
an in-process failure (`conflict_git.rs` `rollback_refs`).

Full abort / cleanup after the canary regardless of pass/fail:

```sh
tcfs unsync ~/git/${REPO_NAME} 2>/dev/null || true   # if such a verb exists in your build; else rm remote prefix manually
rm -rf ~/git/${REPO_NAME}
ssh jess@honey "rm -rf ~/git/${REPO_NAME}"
# remove the disposable remote prefix per your storage backend's admin path
```

---

## 9. Evidence capture

All commands above already `tee` into `$EVID/{neo,honey}/*.log`. After the run,
write a `RESULTS.md` mirroring `bidirectional-ff-canary-20260705T225429Z/RESULTS.md`'s
structure:

```
$EVID/RESULTS.md
$EVID/neo/{resolve-execute.log,show-ref-post-resolve.txt,fsck-post-resolve.txt,
           log-all-post-resolve.txt,conflicts-post-resolve.txt,
           undo-bundle-verify.txt,reconcile-push-post-resolve.log}
$EVID/honey/{show-ref-post-pull.txt,fsck-post-pull.txt,log-all-post-pull.txt,
             reconcile-pull-loser.log}
```

`RESULTS.md` header should state: fleet build (post-#534 store-path check from
§0.1), both hosts' `tcfs --version`, the disposable prefix used, and the six-step
result table (seed/push, pull-baseline, diverge, record-conflict, resolve+park,
loser-converge) — same shape as the FF canary's table.

---

## 10. G5-git-13 flip criteria (what makes this row green)

Per `scripts/git-dotgit-fsck-conflict-harness.sh` Stage 6 assertions
(`origin/main`), this LIVE canary flips **G5-git-13** green when ALL of:

1. The fixture genuinely diverged (`$NEO_SHA != $HONEY_SHA`) — confirmed §3.3.
2. Both `.git` states are fsck-clean after convergence (§6.1, §5.1).
3. `refs/heads/main` on the loser (honey) equals the winner's SHA (§6.1).
4. The loser's own old head is reachable via its own parked theirs-ref on the
   loser itself (§6.1) — not just on the winner.
5. Both the winner's and loser's commit objects are present and reachable on
   both machines (§7).
6. A second reconcile cycle on both sides records **zero** conflicts (§6.2) —
   the forever-Conflict gap actually closes, not just "looks resolved once."

All six hold → update `scripts/git-dotgit-fsck-conflict-harness.sh` Stage 6 run
log / the #506 harness tracking doc to flip `G5-git-13` from the expected-fail
placeholder to PASS, and close gate **G5-git-5** end-to-end (both the FF half,
already closed by the 2026-07-05 canary, and the divergent half, closed here).
