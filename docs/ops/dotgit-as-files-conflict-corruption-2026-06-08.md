# TCFS `.git`-as-files Conflict / Corruption Safety - 2026-06-08

Status: read-only research note plus a local-only test harness. This is the
**precision layer** the existing large-workdir ladder already asks for — it does
not introduce a new workstream and does not move goalposts.

Tracker: `TIN-1620` (G5, one expendable live repo). FACET 6 of the repo-roam
enrollment review.

## What this grounds in (not new)

The repo-roam / "single live repo" target is the **extant** Phase 2 of
[large-workdir-onboarding-design-2026-05-25.md](large-workdir-onboarding-design-2026-05-25.md)
and **Gate G5 / `TIN-1620`** of
[large-workdir-daily-driver-sequencing-2026-05-30.md](large-workdir-daily-driver-sequencing-2026-05-30.md).
G5's test gate already names the conflict rows **T10/T11** plus rollback /
fresh-tree restore. This note adds the `.git`-specific corruption and
flip-flop-safety checks **on top of** those rows; it does not replace them.

The operator chose `git_sync_mode = "raw"` (`.git`-as-files), not `bundle`. Every
large-workdir shadow packet to date runs `raw` + `conflict_mode = "auto"` (see
`docs/release/evidence/large-workdir-*/tcfs-linux-xr-shadow.toml` and the daemon
`conflict_mode=auto` startup lines). This note is about that exact combination.

## Code anchors

- `crates/tcfs-sync/src/conflict.rs` — `AutoResolver` resolves **per file path**
  by lexicographic device tie-break (`local_device <= remote_device`). It has
  **no notion of `.git` as an atomic unit**.
- `crates/tcfsd/src/daemon.rs:1792-1819` — under `conflict_mode = "auto"` the
  daemon applies `AutoResolver` per inbound file event. Each `.git/*` object
  (`refs/heads/*`, `packed-refs`, `index`, loose objects) is a separate event
  resolved independently.
- `crates/tcfs-sync/src/engine.rs:3331-3359` — in `raw` mode the collector
  checks `git_is_safe(.git)` **once** at the start of collection, then recurses
  and uploads every `.git/*` file individually.
- `crates/tcfs-sync/src/git_safety.rs` — `git_is_safe` (lock/in-progress gate),
  `acquire_git_lock` (cooperative `.git/tcfs.lock`), and the bundle helpers.
- `crates/tcfs-cli/src/main.rs:3299-3598` — `cmd_unsync` / `build_unsync_plan` /
  `apply_unsync_conversion`: whole-directory unsync with dirty-child refusal.

## Finding 1 — Concurrent `.git` writes under `conflict_mode=auto` CAN corrupt

`AutoResolver` is path-scoped. When two machines diverge the same repo's `.git`,
the daemon resolves each `.git/*` path on its own. There is **no transaction**
that keeps `refs/heads/main`, `packed-refs`, the `index`, and the object store
mutually consistent. The lexicographic tie-break can keep device A's loose
`refs/heads/main` while keeping device B's `packed-refs` / `index` / object
store — and B's object store need not contain the commit A's ref points at.

The harness reproduces exactly this interleave on a throwaway repo and `git fsck`
reports a **half-applied ref**:

```
error: refs/heads/main: invalid sha1 pointer e7b1326...
error: HEAD: invalid sha1 pointer e7b1326...
```

This is not a hypothetical: per-file resolution gives **no atomicity guarantee**
across the files that make up a single git ref update. Corruption risk under
concurrent same-repo `.git` editing with `conflict_mode=auto` is **real**.

Aggravating factor (TOCTOU): in `raw` mode `git_is_safe` is checked once, then
collection/upload streams `.git/*` over many seconds. A `git commit` / `gc` /
`fetch` that **starts after** the check but during the upload window produces a
**torn snapshot** even on a single machine. `acquire_git_lock` exists but is
**not called** on the raw collection path — the cooperative lock is effectively
dead code for uploads.

## Finding 2 — The SAFE flip-flop (`tcfs unsync <repo>`) is the right primitive

`cmd_unsync_directory` walks `children_with_prefix(repo)`, which **includes every
tracked `.git/*` path**, and refuses the whole directory if **any** descendant is
dirty (`needs_sync` → `Some(reason)` without `--force`). So the intended
handoff — `unsync` on neo, work on honey, rehydrate on neo — does dehydrate the
whole repo incl. `.git` and, when neo's tree is clean, removes neo as a writer of
those `.git` paths before honey edits them. That is the correct way to avoid the
Finding-1 concurrent-write race: **don't let two machines hold the same `.git`
hydrated and writable at once.**

Two caveats the flip-flop does **not** yet cover:

1. **Non-atomic dehydration.** `flush_unsync_state_first` flips all state to
   `NotSynced`, then loops `apply_unsync_conversion` (write `.tc` stub, remove
   file) **one file at a time**. A crash mid-loop leaves a **partially stubbed
   `.git`** (some refs/objects are stubs, some are real) — locally fsck-broken
   until rehydrate completes. There is no all-or-nothing swap.
2. **No `git_is_safe` / lock during unsync.** Unsync neither checks
   `git_is_safe` nor takes `acquire_git_lock`. A `git` process writing the repo
   **during** unsync races the stub conversion. The flip-flop is only safe if the
   operator quiesces git on the source host first.

## Finding 3 — Atomicity of a mid-sync `.git`

A `.git` captured/restored mid-operation fails `git fsck`:

- **Upload side (raw):** torn snapshot per Finding 1's TOCTOU — `packed-refs`
  uploaded before a concurrent `commit`, the new loose object after, etc.
- **Restore side (raw):** rehydrate is per-file; a peer that reads the tree
  before all `.git/*` objects land sees a `.git` missing objects its refs point
  at. (The bundle path's `restore_git_bundle_into` is atomic-ish by contrast —
  `git fetch +refs/*:refs/*` into a real object store — but the operator did not
  choose bundle.)

There is currently **no `git fsck` assertion anywhere in the test suite**. The
only `.git` roundtrip test, `crates/tcfs-sync/tests/git_bundle_roundtrip.rs`,
covers **bundle** mode — the path **not** in use. Raw-mode `.git` correctness is
**untested**.

## What the test MUST check (precision rows on top of T10/T11/G5)

For `TIN-1620` / G5 the live-repo conflict and rollback proof must add, for
`git_sync_mode = "raw"`:

- **G5-git-1 (peer fsck):** after push-on-A → rehydrate-on-B, `git -C <repo>
  fsck --full` is **clean** and `git status` / `git log` succeed on B. No
  dangling/invalid ref pointers; HEAD resolves to a present commit.
- **G5-git-2 (no torn snapshot):** a repo with `.git/index.lock` (or an
  in-progress rebase/merge) is **skipped** that cycle by `git_is_safe`, never
  uploaded half-applied. Retried once the lock clears.
- **G5-git-3 (flip-flop is clean + exact):** `tcfs unsync <repo>` on a clean
  repo dehydrates the whole tree incl. `.git`; rehydrate restores an
  **fsck-clean, byte-exact** repo (HEAD, branches, staged, untracked all
  round-trip).
- **G5-git-4 (dirty-child refusal incl. `.git`):** `tcfs unsync <repo>` with a
  dirty `.git` child (e.g. a fresh commit not yet synced) **refuses** without
  `--force`. No silent loss, no partial dehydration.
- **G5-git-5 (concurrent-write corruption row):** the per-file `.git` conflict
  interleave under `conflict_mode=auto` must be **detected** — `git fsck` reports
  the half-applied ref. Until resolution is made `.git`-aware (resolve a repo's
  `.git/*` as one unit, or fall back to bundle on `.git` conflict), this row is
  expected to FAIL and stands as the corruption-risk gate that must be closed
  before G5 can claim concurrent-edit safety.

## Harness

`scripts/git-dotgit-fsck-conflict-harness.sh` (+ `test-…`) implements G5-git-1..5
locally and safely:

- builds a **throwaway** canary git repo under a temp dir (committed + staged +
  untracked + branch/HEAD + packed-refs); never touches real `~/git` repos;
- asserts the baseline and full-tree `.git`-as-files mirror are **fsck-clean and
  exact** (G5-git-3 invariant);
- records the `index.lock` skip contract (G5-git-2);
- **reproduces the per-file `.git` conflict interleave and proves `git fsck`
  flags a half-applied ref** (G5-git-5 corruption evidence);
- the optional `--run-push` stage is a thin pointer to the existing
  `scripts/git-repo-canary.sh` (shadow-first, disposable prefix) — it does **not**
  duplicate that scaffold, and it refuses non-disposable remotes;
- requires **no daemon, no backbone, no fleet mutation**; default run is a pure
  local-fixture proof.

Run: `bash scripts/test-git-dotgit-fsck-conflict-harness.sh`.

## Recommended fix direction (out of scope for this note)

1. Make conflict resolution **`.git`-aware**: when a conflicting path is under a
   `.git/`, resolve the **whole repo's `.git`** as one unit (pick one device's
   `.git` wholesale, or drop to bundle for that repo) rather than per file.
2. On the raw upload path, take `acquire_git_lock` (already implemented) and/or
   re-check `git_is_safe` immediately before finalizing, to close the TOCTOU.
3. Make unsync `.git` dehydration **atomic** (stage all stubs, swap last) or
   bracket it with the git lock + a `git_is_safe` precheck.
4. Add the `git fsck` assertions above to the live-repo (`TIN-1620`) packet so
   G5 cannot be claimed while G5-git-5 is red.
