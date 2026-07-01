# Repo-roam dev-env zero-diff canary — 2026-06-09

Live two-machine proof (`neo` macOS source, `honey` Linux peer) that a real repo's
full in-progress developer state roams across machines, byte- and semantically
identical, on the deployed `tcfs 0.12.14`. Repo under test: a small, clean,
expendable scaffold repo under `~/git` (raw `.git`-as-files mode).

## Mechanism

Scheduled `tcfs reconcile --path ~/git/<repo> --prefix git-roam/<name> --execute`
unit, with a per-root config setting `[sync] sync_git_dirs = true`,
`git_sync_mode = "raw"`, `sync_hidden_dirs = true`. The daemon's own config is
unchanged (no fleet-wide flip). The fail-closed deny-set excludes secrets / `.env`
/ live DB+WAL files.

## Result — forward roam: PASS

| Step | Outcome |
|------|---------|
| R0 source fingerprint | branch `wip` + staged + unstaged + untracked + 1 stash; `fsck=clean` |
| R1 source PUSH | **126 files pushed, 0 errors** — raw `.git` (objects, index, refs, `logs/refs/stash`) + working tree |
| R2 peer PULL + compare | peer fingerprint **identical** to source → `dev-env-zero-diff=pass`, `git fsck` clean both sides |

Branch, staged change, unstaged change, untracked file, and the stash all roamed
exact. A developer can `ssh` the peer host, `cd` into the repo, and resume the
identical uncommitted work.

## Boundary — bidirectional concurrent edit: gated (expected)

When both hosts edit the same repo concurrently, the peer reconcile reports
`5 conflicts, 0 pushed` — divergent `.git` refs/index trip vector-clock conflict
detection that is not yet `.git`-aware (the documented G5-git-5 gate). Forward
roam is clean; concurrent two-way editing needs `.git`-aware conflict resolution
or a one-writer-at-a-time unsync/rehydrate handoff.

## Reproduce

See the README section "Roam an in-progress repo across machines" and
`docs/ops/repo-roam-test-plan-2026-06-08.md`. The pass/fail gate is
`scripts/repo-roam-fingerprint.sh` (`capture` / `compare`).

## Raw run record (2026-06-09, as captured on neo)

Verbatim operator run log; raw fingerprint artifacts are committed alongside in
`neo-source-fingerprint/`.

> Deployed: B1 reconcile units (raw .git-as-files) on current tcfs 0.12.14, hm gen 386 (neo).
>
> R0 source fingerprint (neo): 5b24b9f95065576e4f664141fe837e8e1d9e4586c2a5765720b34ed8b05ee0b3 (branch roam-canary-wip + staged AGENTS.md + unstaged README.md + untracked scratch + 1 stash, fsck=clean)
> R1 neo PUSH: 126 pushed, 0 errors (raw .git incl objects/index/refs/logs+stash + working tree)
> R2 honey PULL + compare: honey fingerprint = 5b24b9f9... IDENTICAL → dev-env-zero-diff=PASS (T13-Z), fsck clean both sides. Branch/staged/unstaged/untracked/stash all roamed exact.
> R3 bidirectional (honey commit→neo): honey reconcile = 5 conflicts, 0 pushed → G5-git-5 concurrent-.git gate CONFIRMED expected-fail (AutoResolver not .git-aware). Forward roam clean; concurrent bidirectional needs .git-aware conflict resolution (future work) or unsync/rehydrate safe-handoff.
