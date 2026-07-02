# `~/git` Roam Daily-Driver Acceptance - 2026-06-08

Status: acceptance plan for the "machine does not matter" TCFS user story.

Related trackers: `TIN-1617`, `TIN-1620`, `TIN-1738`, `TIN-1899`.

Related runbooks:

- [large-workdir-daily-driver-sequencing-2026-05-30.md](large-workdir-daily-driver-sequencing-2026-05-30.md)
- [claude-projects-roam-enrollment-2026-06-08.md](claude-projects-roam-enrollment-2026-06-08.md)
- [git-repo-canary-dogfood.md](git-repo-canary-dogfood.md)

## Golden Objective

An enrolled operator can start work on any enrolled host, leave that work
uncommitted, SSH into another enrolled host, browse the same logical `~/git`
namespace, hydrate the needed repo and agent context, continue work, then
optionally unsync either host without losing the remote encrypted copy.

The claim is not "every byte under `~/git` is blindly mirrored." The claim is:

1. enrolled repo roots are discoverable from every enrolled host;
2. active working-tree state can be continued from any enrolled host;
3. the Git history/refs needed to understand that work are restored by the
   Git-safety path, not by naive live `.git/objects` mirroring;
4. agent context for the same work is present through the `~/.claude/projects`
   reconcile root; and
5. secrets, live databases, caches, generated outputs, and large disposable
   artifacts stay denied or snapshot-only.

## What Must Be True

The daily-driver story is only proven when all of these hold together for the
same repo/session pair:

- **Any origin host:** work can begin on `neo`, `honey`, or the next enrolled
  host such as `bumble`.
- **Any continuation host:** another enrolled host can traverse the repo path
  before full hydration, hydrate only what is needed, and continue.
- **Dirty WIP:** modified tracked files, untracked files, deletes, renames, file
  mode changes, and symlink targets survive the round trip.
- **Git metadata:** `git status --porcelain=v1 -b`, `git rev-parse HEAD`,
  current branch, local refs, and the restored object graph match the source
  snapshot after the Git bundle restore path runs.
- **Agent continuity:** the matching `~/.claude/projects` session/subtree
  converges with the repo so an agent can resume with the same transcript and
  path references.
- **Unsync/cloud-only:** either host can evict local bodies while preserving
  stubs/metadata, and a later open or explicit hydrate restores exact bytes.
- **Conflict visibility:** concurrent edits to the same file produce an
  explicit conflict/recovery state, while independent sibling edits converge.
- **Policy safety:** denied paths never enter a push plan, evidence packet, or
  remote prefix.

## Enrollment Model

Use repo-by-repo enrollment. Do not flip broad `~/git` ownership as the first
move.

Recommended root classes:

| Class | Default posture | Notes |
| --- | --- | --- |
| Working tree files | Sync | Normal source files, docs, tests, configs. |
| `.git` | Git-safety bundle/restore | Restore real history and refs; do not rely on naive live object mirroring. |
| Agent context | Scheduled reconcile | `~/.claude/projects` under `agent/claude-projects`; keep auth roots out. |
| Generated output | Deny | `target`, `node_modules`, `.svelte-kit`, `build`, `.venv`, caches. |
| Secrets/auth | Deny fail-closed | `.env*`, `auth.json`, `.credentials.json`, SOPS secret trees, SSH/GPG material. |
| Live DB/WAL | Deny or snapshot-only | `*.sqlite`, `*.db`, `*-wal`, `*-shm`; never live-mirror open WAL streams. |
| Large artifacts | Repo-specific | Example: `rockies/.artifacts` needs explicit policy. |

### Git Metadata Mode Boundary

`sync_git_dirs` is currently a global `sync` config field consumed by
`tcfs reconcile` through `Blacklist::from_sync_config`, not a per-root
`extraReconcileRoots` knob. The default Git mode is `bundle`: raw `.git`
internals are skipped, a `.git-tcfs-bundle` object is synced, and pull-side
restore reconstructs Git history/refs from that bundle.

Raw `.git` as ordinary files (`git_sync_mode = "raw"`) is a separate stress
mode. It may become useful for exact index/stash/worktree experiments, but it
is not the default daily-driver claim and must prove index/mtime, lockfile,
symlink, mode, and concurrent-operation safety before it can replace the
bundle/restore path.

### Linked Worktrees

`git worktree` metadata is fail-closed fenced by the blacklist
(`crates/tcfs-sync/src/blacklist.rs`), independent of `sync_git_dirs` and
`git_sync_mode`:

- A non-directory named `.git` — a linked worktree's (or submodule's) gitfile
  pointer containing `gitdir: <absolute path>` — is never collected or roamed
  (`BlacklistReason::GitFilePointer`).
- Any path containing a `.git/worktrees/` segment — the per-worktree admin
  area holding `HEAD`, `index`, and `gitdir` files with absolute host paths —
  is never collected or roamed (`BlacklistReason::GitWorktreesAdmin`), even
  under `sync_git_dirs = true` with `git_sync_mode = "raw"`.

Why: roamed raw, both shapes dangle on the destination host, and under
bidirectional roam a roamed `git worktree prune` deletion would sync back and
destroy the origin host's live worktree. The fence trades pointer fidelity for
safety: a roamed copy of a linked worktree (or a submodule checkout) arrives
without its `.git` pointer and reads as a plain directory. Real worktree roam
is design-first work, tracked as an expected-red gate:

| Gate | Scenario | Expected |
| --- | --- | --- |
| G5-wt-1 | Real linked-worktree roam: pointer + admin area re-established on the destination host | red (future, design-first, H3) |

## Acceptance Gates

### R0 - Policy And Inventory

For each candidate repo, archive:

- root path, machine, hostname, timestamp, TCFS version, wrap mode, device list;
- `git status --porcelain=v1 -b`;
- `git rev-parse HEAD`, current branch, local refs summary;
- untracked file inventory;
- symlink inventory and targets;
- unsupported special files;
- generated-output and secret deny matches; and
- expected remote prefix.

Pass condition: denied paths are listed as denied, not silently omitted from the
evidence.

### R1 - Single-Origin Dirty WIP

Start on `neo` with one small candidate repo and create a dirty snapshot:

- modify a tracked source file;
- add one untracked file;
- delete one tracked file;
- rename one file;
- include one symlink or mode-change fixture when the repo supports it; and
- append to the matching `~/.claude/projects` session.

Pass condition: `honey` hydrates the repo and agent session, then reports the
same dirty state and exact plaintext hashes.

### R2 - Reverse Origin

Repeat R1 with the mutation originating on `honey` and continuing on `neo`.

Pass condition: the direction does not matter. This is the minimum proof for
"work can start anywhere" across two hosts.

### R3 - Unsync And Cloud-Only

After R1/R2 converge:

- unsync the source host;
- verify local bodies are evicted while stubs/metadata remain where applicable;
- hydrate on the continuation host;
- reopen on the source host and rehydrate exact bytes.

Pass condition: unsync is a local storage decision, not a data-loss event.

### R4 - Git Bundle Restore

Restore the repo into a fresh tree on the continuation host using the Git-safety
bundle path.

Pass condition:

- `git fsck` passes where practical;
- `git status --porcelain=v1 -b` matches the source snapshot;
- `git rev-parse HEAD` matches;
- local refs/branches needed by the WIP are present; and
- the working tree and untracked files match the source evidence.

### R5 - Conflict And Independent Edits

Run both cases:

- same-file concurrent edit on two hosts; and
- independent sibling edits on two hosts.

Pass condition: same-file conflict is explicit and recoverable; independent
sibling edits converge without manual object surgery.

### R6 - Third-Host Readiness

Before claiming `bumble` or any new server:

- enroll the device through the hardened enrollment path;
- prove the same remote prefix and device registry converge;
- run R1/R3 from `bumble` as origin or continuation; and
- confirm no host-specific absolute paths are baked into repo state except
  expected local config.

Pass condition: the test matrix expands by adding one host row, not by writing a
new special-case flow.

## Candidate Repo Order

Start with repos that expose the behavior without turning scale into the first
failure mode:

1. `ci-templates` - small operational repo, good first repo.
2. `site.scaffold` - useful generated-output excludes (`node_modules`,
   `.svelte-kit`, `build`).
3. `dell-7810` - already useful in the bounded agent-session proof.
4. `yt-text` / Tubebrain checkout - Rust target-dir exclude coverage.
5. `xoxdwm` - review secret-feature fixture names before enrollment.
6. `rockies` - only after `.artifacts` policy is explicit.
7. `linux-xr` / `linux-xr-fast` - stress/WIP gate, not first daily-driver repo.

## Evidence Packet Contract

Every live repo-roam packet should contain:

- `source.env`: origin host, continuation host, repo path, remote prefix, TCFS
  binary path/version/hash, wrap mode, device ids.
- `git-source.txt` and `git-continuation.txt`: status, branch, HEAD, refs
  summary, and `git fsck` result when run.
- `tree-source.sha256` and `tree-continuation.sha256`: plaintext file hashes
  after applying the deny policy.
- `agent-source.sha256` and `agent-continuation.sha256`: matching
  `~/.claude/projects` session/subtree hashes.
- `policy-deny.txt`: denied candidates observed during inventory.
- `unsync.log`: local eviction / rehydrate transcript.
- `conflict.log`: conflict or independent-edit transcript when that gate runs.
- `result.env`: one machine-readable status with the allowed claim boundary.

The packet must name whether it proves shadow-only, live repo, two-host, or
three-host behavior. A packet that proves only shadow restore must not be cited
as broad live `~/git` readiness.

## Harness Gap List

Existing helpers already cover important pieces:

- `task lazy:git-roam-daily-driver-plan`
- `task lazy:git-repo-canary`
- `task lazy:git-repo-restore-proof`
- `task lazy:neo-honey-unsynced-rehydrate-plan`
- `task lazy:neo-honey-reverse-unsynced-rehydrate-plan`
- `task lazy:neo-honey-delete-rename-unsynced-plan`
- `task lazy:neo-honey-conflict-plan`

Missing combined harnesses before the golden objective can be claimed:

The plan-only harness bounds hash collection by default so a real agent
transcript tree cannot become an accidental long-running scan. Use
`MAX_HASH_FILES=0 MAX_HASH_FILE_BYTES=0` only when the live canary intentionally
needs full plaintext hash manifests.

1. `git-roam-dirty-wip` - upgrades the plan-only packet into an executed R1
   dirty snapshot and asserts exact
   continuation state.
2. `git-roam-reverse-origin` - same test with `honey` as origin.
3. `git-roam-agent-coupled` - ties repo evidence to the matching
   `~/.claude/projects` session evidence.
4. `git-roam-third-host` - parameterizes the host matrix so `bumble` is another
   row, not a new script family.
5. `git-roam-policy-leak` - fails if any denied secret/live-DB/generated path
   appears in push plans, evidence manifests, or remote listing samples.

## Stop Rules

- Do not bulk-enroll all of `~/git` before two small repos pass R0-R5 in both
  directions.
- Do not treat `~/.claude/projects` roam as proof that repo WIP roams; it only
  proves agent context.
- Do not treat Git shadow restore as proof that a live repo can be managed until
  dirty WIP, unsync, and conflict gates pass.
- Do not claim per-device revoke/rotate field security until the live fleet is
  in `PerDevice` wrap mode and the revoked-device canary proves denial after
  `tcfs key rotate <prefix>`.
- Do not enroll live SQLite/WAL, auth files, `.env*`, SSH/GPG material, or SOPS
  secret trees.

## Next Implementation Slice

The next low-risk code slice is a plan-only harness that emits the R0-R5 packet
shape for one selected repo without mutating TCFS state. After that, wire
execution to the existing `git-repo-canary`, reverse-origin, delete/rename,
conflict, and restore-proof helpers so the combined user story is tested as one
claim instead of five unrelated proofs.
