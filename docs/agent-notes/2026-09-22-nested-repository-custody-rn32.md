# 2026-09-22 — foreign nested repositories and gitlinks become custody (R-N32)

Ruling: R-N32 (operator, 2026-09-22, Linear TIN-3692). Also honours R25
(zero-reread resume: nothing below a nest is read, and a repository without a
nest keeps its exact capture key so retained captures stay reusable).

Branch `feat/bulkload-nested-repository-custody-20260922`, stacked on
`feat/bulkload-rebuildable-omission-20260922` (PR #597). Must be rebased after
the drift-tolerant capture PR (`feat/bulkload-drift-tolerant-capture-20260922`)
lands, since both touch the same walk.

## What changed

- `git_carry::NestedRepository { rel_path, kind: Directory | Gitlink,
  head_oid: Option<String>, gitdir_kind: Directory | PointerFile | None }`
  beside `NestedWorktree`; the census gains a sorted `nested_repositories`.
- Walk: a `.git` directory, or a pointer file resolving to administration of a
  different repository, is custody: no row, no descent. HEAD is read through
  `git --git-dir=.git rev-parse --verify -q HEAD` under the hardened invocation;
  exit 1 (unborn) is `None`, anything else Git cannot read still refuses.
  A symlinked `.git`, an unresolvable pointer, and a pointer into our own
  common dir that is not a registered worktree keep `GIT_INVENTORY_MALFORMED`.
- `source_index()` no longer refuses mode `160000`; each gitlink is recorded
  with its index oid and removed from the private index copy before
  `write-tree`, so the staged tree never names a commit the bundle lacks.
- Key: sidecar hashed under `tcfs-git-nested-repositories-v1\0` only when
  non-empty. The nested HEAD is inside the before/after census equality (a
  foreign object moving mid-pass is drift we name, not ignore).
- Bundle: `refs/carry-export/nested-repositories-v1` (postcard
  `Vec<NestedRepository>`), written only when non-empty. Receipt untouched;
  the ref is the custody surface. `Export` gains `nested_repositories`;
  `git-export` prints one `nested-repository ...` stderr line per nest.
- No RowSchema, refusal-code, or estate `Capture` postcard change.

## Measured motivating cases (neo)

medical-massage-specialists-infra (`.terraform-data/.../.git`), printstack
(`.tmp/uv-cache/git-v0/{db,checkouts}/.../.git`), asfirewire-legalab (CMake
`_deps` and SwiftPM checkouts), crs310-8g-2s-in (one `160000` gitlink). All
four are synthetic fixtures in `git_carry::tests`.
