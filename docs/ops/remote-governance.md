# Remote Governance

Policy document for how `tcfs` manages its git remotes, branches, and tracker
state across the `origin` / `tinyland` / `yoga` topology.

Anchor: the [Canonical Home](../../README.md#canonical-home) section of `README.md`
declares `Jesssullivan/tummycrypt` as the canonical source repository. This
document makes the consequences of that declaration operational.

## Current Remote Topology

As of 2026-04-17:

| Remote | URL | Branches | `main` ahead of `origin/main` | `main` behind `origin/main` | Role |
|--------|-----|----------|-------------------------------|------------------------------|------|
| `origin` | `https://github.com/Jesssullivan/tummycrypt.git` | 25 | 0 | 0 | canonical source + release authority |
| `tinyland` | `git@github.com:tinyland-inc/tummycrypt.git` | 73 | 19 | 106 | active downstream dev surface |
| `yoga` | `yoga:git/tummycrypt` (bare SSH) | 10 | 137 | 328 | retired legacy mirror |

Divergence points:

- `origin/main` ↔ `tinyland/main`: merge-base is `3b30df6` (#178 streaming
  chunker, 2026-04-10). Divergence is 7 days old at time of writing. Both
  sides have merged real work since the split; the two histories are
  reconcilable but not identical.
- `origin/main` ↔ `yoga/main`: severely diverged; yoga represents a
  `v0.9.x`-era tcfs snapshot and is not a current development branch.

## Declared Roles

### `origin` — canonical source and release authority

- All **release tags** (`v0.12.x`) are cut from `origin/main`. No release tag
  is authoritative on any other remote.
- All **planning issues** (roadmap, milestones, sprint goals) live on
  `origin`. Issues filed on `tinyland` or `yoga` are downstream-specific
  (infra, package signing, distribution channels) and must not duplicate
  planning state.
- All **contributor PRs** targeting the canonical product land on `origin`.
  A PR that only exists on `tinyland` is staging-only until a corresponding
  `origin` PR is opened.
- **Branch protection** and **required reviews** are configured on `origin`
  as the gate of record. `tinyland` and `yoga` may have weaker or different
  protections; those do not substitute for `origin`'s.

### `tinyland` — active downstream dev surface

`tinyland-inc/tummycrypt` is not a passive mirror. It carries real feature
work from the `tinyland-inc` org's internal development cadence, often
merged there first and upstreamed to `origin` afterward.

- `tinyland/main` may **lead** `origin/main` by a small number of commits
  (integration drift). A lead of **greater than 20 commits** is a
  governance smell and should open a tracker issue for reconciliation.
- `tinyland/main` must **not fall behind** `origin/main` indefinitely.
  Reconcile at release-cut cadence **or** weekly, whichever is more frequent.
- Reconciliation is done **into `origin` first**, then `origin → tinyland`
  via a sync. Never the reverse (never force-push `tinyland` onto `origin`).
- `tinyland`-specific infrastructure (binary-signing identifiers such as
  `io.tinyland.tcfsd`, binary cache host `nix-cache.fuzzy-dev.tinyland.dev`)
  is legitimate downstream coupling, but it is **externalization-sensitive**:
  see [v0.12.2 evidence matrix §Blocker A](../release/v0.12.2-evidence-matrix.md#blocker-a--nix-cache-externality)
  for the cache-reachability implication, and #307 for the concrete decision.

### `yoga` — retired legacy mirror

`yoga` is a bare SSH remote on a laptop host. It carries a `v0.9.x`-era
snapshot that predates the current `v0.12.x` line by 328 commits.

- `yoga` is **not pushed to** from this point forward.
- `yoga/main` and its 10 branches remain as **read-only historical
  reference** only.
- When the host is decommissioned the remote can be dropped with
  `git remote remove yoga` without information loss, provided anything
  worth preserving has been audited first.
- Any attempt to land new work on `yoga` should be redirected to a PR on
  `origin`.

## Branch Lifecycle Rules

These rules apply to `origin`. `tinyland` has its own branch tranche (73 at
time of writing) which is triaged separately and tracked under the
[tinyland branch triage issue](#related-trackers).

- **Feature branches** land via squash-merge to `main` and are deleted
  promptly after merge. Both the local and remote branch delete steps are
  expected; `gh pr merge --auto --squash --delete-branch` is the canonical
  incantation.
- **Release branches** are not used today; releases are cut from tags on
  `main` directly.
- **Long-lived codex branches** (`codex/*` originating from Codex agent
  runs) are permitted during active work but must be closed or merged
  within the week they were opened. Stale codex branches past 14 days
  should be audited for supersession.
- **No force-push to `main`** on any remote. Force-push on feature branches
  is acceptable only by the branch author and only before review is
  requested.

## Sync Policy

### `origin → tinyland/main`

Preferred path: open a PR on `tinyland` whose base is `tinyland/main` and
whose head is a branch tracking `origin/main`. Merge to land the `origin`
history on `tinyland/main`. Conflicts get resolved in the sync PR, not in
a separate commit against `main`.

Mechanical equivalent:

```bash
git fetch origin main
git checkout tinyland/main -b sync/origin-$(date +%Y-%m-%d)
git merge --no-ff origin/main
git push tinyland sync/origin-$(date +%Y-%m-%d)
# open PR against tinyland/main
```

### `tinyland → origin`

Do not fast-forward `origin/main` to `tinyland/main`. Instead:

1. For each tinyland-unique commit that is a real upstream candidate,
   open an `origin` PR.
2. For each tinyland-unique commit that is downstream-specific (infra,
   org-specific wiring), leave it on tinyland and annotate in a
   persistent downstream-only list.
3. Squash-merge `origin` PRs normally. After all upstream candidates are
   landed, `tinyland/main` can resync via the `origin → tinyland` path.

### yoga

No sync policy. `yoga` is archived.

## Tracker State Policy

- **Planning issues**, **roadmap issues**, and **release-surface issues**
  live on `origin` only.
- **Downstream-specific issues** (binary signing, package channels,
  infrastructure externalization) may live on `tinyland` if they concern
  the `tinyland-inc` substrate and cannot be acted on via `origin`.
  Otherwise they belong on `origin`.
- **Bulk Codex migration issues** on tinyland should be closed in batches
  once their upstream `origin` equivalents are identified; see
  [related trackers](#related-trackers).

## Related Trackers

Execution work derived from this policy is tracked as separate issues so
the governance document itself stays a policy statement and not a punch
list:

- **Upstream current tinyland-unique commits to `origin`**: 19 commits
  on `tinyland/main` ahead of `origin/main` as of 2026-04-17 (merge-base
  `3b30df6`). Features include `feat(sync)` trash + bandwidth throttling,
  `feat(sync)` selective sync policies + D-Bus gRPC backend, `feat`
  unsync dehydration + auto-unsync with disk pressure, `fix(vfs)` OOM
  prevention + bounded negative cache, `fix(fuse)` flush via VFS fsync,
  `fix(daemon)` race guards + deferred vclock merge, `fix(sync)` panic
  elimination. A tracker issue enumerates per-commit upstream disposition.

- **Triage the tinyland branch tranche**: 73 branches under categories
  `feat/*` (~30), `chore/bump-*` (~15), `sid/*` (~15), and migration /
  sprint (~4). A tracker issue proposes per-category disposition
  (mergeable, supersede, close stale).

- **Retire `yoga` formally**: the remote is already de facto retired.
  A tracker issue decides whether to archive the bare SSH repo,
  decommission the host, or leave both in place.

- **Nix cache externalization** (#307): the existing tracker for
  `nix-cache.fuzzy-dev.tinyland.dev` externality. The infrastructure
  question is cross-cut; resolving it informs both the release-surface
  Nix blocker and the tinyland-vs-origin infra coupling.

## Relationship To Other Documents

- [README § Canonical Home](../../README.md#canonical-home) — upstream
  declaration; this document makes it operational
- [Contributing](../CONTRIBUTING.md) — the PR-based workflow that all
  `origin`-targeted work follows
- [v0.12.2 Evidence Matrix](../release/v0.12.2-evidence-matrix.md) —
  release-surface consequences of tinyland-hosted infrastructure
- [Product Reality and Priority](product-reality-and-priority.md) —
  current-state summary across all lanes

## Revision History

- 2026-04-17 — Initial governance doc. Captures the 3-remote topology at
  its current divergence state (`tinyland` 19 ahead / 106 behind,
  `yoga` 137 ahead / 328 behind) and declares roles + sync policy.
