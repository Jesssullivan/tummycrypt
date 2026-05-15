# TCFS Git Repo Canary Blocker Evidence

Created: 2026-05-15T00:37:24Z

This bundle inventories one git worktree read-only, copies it to an isolated
shadow, and records a stopped live push attempt. It does not mutate the live
source repo and does not claim Finder/FileProvider production readiness,
`~/Documents`, dotfiles, `.local`, broad `~/git`, or home-directory takeover.

- Source: `/Users/jess/git/oauth-mux`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-shadow-20260515T003543Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-oauth-mux-concurrent-20260515T003542Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-oauth-mux-20260515T003543Z/state/tcfs-git-repo-canary.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-oauth-mux-20260515T003543Z/state/push-state.json`

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Symlink-enabled packets must prove source symlinks rehydrate as symlinks with
matching targets, and `push.log` must not contain skipped symlink rows. This
packet is a blocker because `push.log` contains skipped symlink rows. See
`symlink-push-blocker.md`.

Contents:

- `source-inventory/`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- `shadow-inventory/`: same inventory after the isolated copy
- `symlink-shadow-compare.log`: local source/shadow symlink target comparison
- `state/tcfs-git-repo-canary.toml`: generic alias for the disposable config
  with raw `.git`, hidden-dir, symlink, and empty-dir sync enabled
- `tcfs-linux-xr-shadow.toml` under `state/`: inherited harness config with
  `sync_git_dirs = true`, `sync_hidden_dirs = true`,
  `git_sync_mode = "raw"`, `sync_symlinks = true`, and
  `sync_empty_dirs = true`
- `symlink-push-blocker.md`: short blocker summary for the stopped live push
- `push.log` or `push.log.gz`: shadow push transcript when `--push` ran
- `push-storage-summary.env` and `push-storage-summary.md`: storage-facing
  totals extracted from `push.log` when push evidence is present
- `honey-linux-xr-shadow-commands.txt`: honey mounted traversal/hydration
  commands for the selected file, `.git` traversal, and mounted symlink
  target verification
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
