# TCFS Home Canary linux-xr Shadow Evidence

Created: 2026-05-14T00:00:16Z

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
`/Users/jess/git/linux-xr` tree and does not claim `~/Documents`, `~/.local`,
dotfiles, or broad `~/git` takeover.

- Source: `/Users/jess/git/linux-xr`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-storage-posture-20260513T220442Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-storage-posture-20260513T220441Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-storage-posture-20260513T220442Z/state/tcfs-linux-xr-shadow.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-storage-posture-20260513T220442Z/state/push-state.json`

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Symlink-enabled packets must prove source symlinks rehydrate as symlinks with
matching targets. See `source-inventory/symlink-targets.tsv` and
`source-inventory/unsupported-special-files.txt`.

Contents:

- `source-inventory/`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- `shadow-inventory/`: same inventory after the isolated copy
- `symlink-shadow-compare.log`: local source/shadow symlink target comparison
- `tcfs-linux-xr-shadow.toml` under `state/`: disposable config with
  `sync_git_dirs = true`, `sync_hidden_dirs = true`,
  `git_sync_mode = "raw"`, `sync_symlinks = true`, and
  `sync_empty_dirs = true`
- `push.log.gz`: compressed shadow push transcript from the `--push` run
- `push-storage-summary.env` and `push-storage-summary.md`: storage-facing
  totals extracted from `push.log` before compression when push evidence is
  present
- `storage-posture-live-observations.md`: claim boundary and follow-up notes
  from the completed push-only storage run
- `honey-linux-xr-shadow-commands.txt`: honey mounted traversal/hydration
  commands for the selected file, `.git` traversal, and mounted symlink
  target verification
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
