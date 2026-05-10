# TCFS Home Canary linux-xr Shadow Evidence

Created: 2026-05-10T15:15:26Z

This bundle inventories the live source read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
`/Users/jess/git/linux-xr` tree and does not claim `~/Documents`, `~/.local`,
dotfiles, or broad `~/git` takeover.

- Source: `/Users/jess/git/linux-xr`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-shadow-20260510T002604Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/home-canary-linux-xr-shadow-20260510T023938Z`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-shadow-20260510T023938Z/state/tcfs-linux-xr-shadow.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/home-canary-linux-xr-shadow-20260510T023938Z/state/push-state.json`

Truth gate: full project parity is not claimed while symlink preservation is
unsupported by the push path. See `parity-gates.env`,
`source-inventory/symlinks.txt`, and
`source-inventory/unsupported-special-files.txt`.

Contents:

- `source-inventory/`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks, unsupported special files, and bounded tree listing
- `shadow-inventory/`: same inventory after the isolated copy
- `tcfs-linux-xr-shadow.toml` under `state/`: disposable config with
  `sync_git_dirs = true`, `sync_hidden_dirs = true`,
  `git_sync_mode = "raw"`, and `sync_empty_dirs = true`
- `push.log`: shadow push transcript when `--push` ran
- `honey-linux-xr-shadow-commands.txt`: honey mounted traversal/hydration
  commands for the selected file and `.git` traversal
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
