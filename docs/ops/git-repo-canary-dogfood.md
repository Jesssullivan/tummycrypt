# TCFS Git Repo Canary Dogfood

This runbook is the next step between isolated fixture proof and real
`~/git` management. It is intentionally shadow-first: the source repo is
inventoried read-only, copied into `~/TCFS Pilot/real-canaries/`, and only the
shadow is pushed or exercised through TCFS.

## Current Readiness

Use the Linux/FUSE lane for the first real repo dogfood. The strongest current
proof is the archived `linux-xr` isolated-shadow lifecycle packet, which proves
remote traversal, selected hydration, mounted write/readback, cache
clear/rehydrate, recursive safe-unsync refusal/success, and symlink target
preservation on `honey`.

The generic `oauth-mux` canary is now green when both hosts use either
source-built binaries from current source or explicit current Nix flake package
binaries. The packet
`docs/release/evidence/git-repo-canary-oauth-mux-sourcebin-fresh-20260515T014640Z/`
uses source-built `tcfs` on `neo` for the push and a Nix-built current-source
Linux `tcfs` on `honey` for mounted traversal/hydration, mounted symlink target
checks, and the Linux lifecycle companion. It proves the shadow-first workflow
for one clean repo; it does not make live `~/git/oauth-mux` managed by TCFS.
The package-backed packet
`docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/`
uses current Nix flake package binaries on both `neo` and `honey`, publishes
4,601 regular files / 356,520,343 bytes with 0 skipped symlinks, verifies all 9
mounted symlink targets on honey, and passes the same Linux lifecycle companion.

The remaining package blocker is Homebrew. The installed Homebrew
`tcfs 0.12.12` binary skips symlinks even when the canary config sets
`sync_symlinks = true`; current-checkout Nix and source-built `tcfs 0.12.12`
preserve the same tiny symlink fixture in
`docs/release/evidence/tcfs-symlink-package-probe-20260515T041947Z/`. A follow-up
tiny mounted probe,
`docs/release/evidence/tcfs-symlink-package-probe-20260515T051126Z/`, proves a
neo current-checkout Nix producer can be mounted and read on honey with the
current source-built Linux binary, including `link.txt -> target.txt`. A second
follow-up, `docs/release/evidence/tcfs-symlink-package-probe-20260515T060330Z/`,
proves the same tiny mounted parse/target check with current Nix flake packages
on both neo and honey. This is still not Homebrew proof: installed Homebrew
continues to skip symlinks. The next gates before moving live repos into TCFS
are Homebrew rebuild/publish if Homebrew is the client lane, `linux-xr-fast` as
the larger clean stress canary, and fresh-tree restore/rollback proof.

Use `task lazy:tcfs-symlink-package-probe` to recheck packaged or candidate
binaries before repeating the real repo canary. The helper writes a fresh
evidence packet with each candidate binary path, version, SHA-256, config, push
log, and a `preserved` / `skipped` / `push_failed` symlink verdict. Add
`--run-honey-mount` when the candidate should also prove mounted parse and
target verification on honey. A package candidate is not dogfood-ready until
this probe reports `overall_status=passed` and the honey mount can parse and
verify the same symlink index format using the packaged/current consumer binary.
Current Nix flake producer/consumer proof is green for both the tiny fixture and
the clean `oauth-mux` shadow canary; Homebrew is the remaining package-current
blocker.

Do not use the current `neo` Finder/CloudStorage root for active repos yet. The
local Provider registration is still a diagnostic surface until a published
`.pkg` is installed into `/Applications`, stale user/build registrations are
cleaned after inventory, backend reachability is fixed, and strict production
signing preflight passes.

## Default Canary Order

1. `~/git/oauth-mux` shadow: small, clean, low-risk first proof.
2. `~/git/linux-xr-fast` shadow: large clean stress proof after the small lane
   is boring.
3. One expendable live repo: only after the shadow packet proves restore from
   remote, cross-host rehydrate, and safe-unsync behavior.
4. Selected `~/git` or `~/Documents` subtrees: only after several repo canaries
   have boring transcripts on at least two machines.

Keep `~/git/linux-xr` inventory-only unless direct live mutation is explicitly
approved. Keep `finances`, secrets, package caches, dotfiles, `.local`,
keychains, and broad home-directory takeover out of this lane.

## Task Surface

Plan-only inventory and shadow packet for the default small repo:

```bash
task lazy:git-repo-canary
```

Explicit small repo run:

```bash
SOURCE="$HOME/git/oauth-mux" \
NAME=oauth-mux \
REMOTE=seaweedfs://HOST:8333/tcfs/git-repo-canary-oauth-mux-manual \
task lazy:git-repo-canary
```

Cross-host proof after the disposable remote and honey credentials are ready:

```bash
SOURCE="$HOME/git/oauth-mux" \
NAME=oauth-mux \
REMOTE=seaweedfs://HOST:8333/tcfs/git-repo-canary-oauth-mux-manual \
PUSH=1 \
RUN_HONEY=1 \
HONEY_START_MOUNT=1 \
RUN_LINUX_LIFECYCLE=1 \
task lazy:git-repo-canary
```

Until the package lane is rebuilt, pass explicit current-source binaries:

```bash
TCFS_BIN="$PWD/target/codex-verify/debug/tcfs" \
HONEY_TCFS_BIN=/path/on/honey/to/current-source/tcfs \
TCFS_HONEY_EXPECTED_SHA256=<sha256> \
PUSH=1 \
RUN_HONEY=1 \
HONEY_START_MOUNT=1 \
RUN_LINUX_LIFECYCLE=1 \
task lazy:git-repo-canary
```

Package/current symlink probe:

```bash
CANDIDATES="homebrew=/opt/homebrew/opt/tcfs/bin/tcfs source_built=$PWD/target/codex-verify/debug/tcfs" \
ENDPOINT=http://HOST:8333 \
BUCKET=tcfs \
task lazy:tcfs-symlink-package-probe
```

Tiny mounted parse/target proof after choosing one preserved producer:

```bash
scripts/tcfs-symlink-package-probe.sh \
  --endpoint http://HOST:8333 \
  --bucket tcfs \
  --candidate nix_current=/nix/store/...-tcfs-cli-0.12.12/bin/tcfs \
  --run-honey-mount \
  --mount-label nix_current \
  --honey-tcfs-bin /path/on/honey/to/tcfs
```

Large clean repo stress pass:

```bash
SOURCE="$HOME/git/linux-xr-fast" \
NAME=linux-xr-fast \
REMOTE=seaweedfs://HOST:8333/tcfs/git-repo-canary-linux-xr-fast-manual \
task lazy:git-repo-canary
```

The helper refuses dirty worktrees unless `ALLOW_DIRTY_SOURCE=1` or
`--allow-dirty-source` is set. Dirty snapshots are allowed only as explicit
evidence; they are not a default dogfood target.

## Evidence Boundary

Every packet writes `git-repo-canary-policy.env` and
`git-repo-canary-summary.md` beside the inherited source/shadow inventory,
TCFS config, and optional push/honey/lifecycle transcripts.
When a later resume pass runs honey or lifecycle proof, `run-metadata.env`
describes the final pass and `push-run-metadata.env` preserves the original
push-time binary/concurrency settings.

Claims allowed from this lane:

- one git worktree can be copied to a shadow and represented with TCFS config
- if `PUSH=1`, the shadow can publish to a disposable prefix
- if `RUN_HONEY=1`, a second Linux host can traverse and hydrate selected files
- if `RUN_LINUX_LIFECYCLE=1`, the mounted write/readback, cache
  clear/rehydrate, and safe-unsync lifecycle passed for that prefix
- scoped project-tree parity only when `parity-gates.env` reports
  `scoped-project-tree-parity-evidence-complete`; if `push.log` contains
  skipped symlink rows, the packet remains a blocker even if the file push
  itself exits successfully
- symlink preservation means remote/mounted symlink target proof. Local
  `sync-status` and recursive `unsync` remain regular-file-oriented for now, so
  do not use symlink status rows as a readiness claim.

Claims not allowed:

- the live source repo was managed by TCFS
- Finder/FileProvider is production ready
- broad `~/git`, `~/Documents`, dotfiles, `.local`, or full home takeover is
  ready
- production S3 posture is closed

## Exit Bar Before Moving A Live Repo

A live repo can become a candidate only after a shadow packet proves all of:

1. source inventory, shadow inventory, and unsupported-file policy are archived
2. exact remote push and pullback pass
3. honey traversal before full hydration passes
4. selected file hydration returns exact source bytes
5. mounted edit/readback preserves exact bytes
6. clean recursive `tcfs unsync` succeeds and dirty recursive unsync refuses
7. exact rehydrate after unsync/cache-clear passes
8. rollback is demonstrated by recreating a fresh local tree from the remote
   prefix
9. source symlinks are not skipped during push and rehydrate as symlinks with
   exact matching targets
