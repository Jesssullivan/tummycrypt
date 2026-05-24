# TCFS Git Repo Canary Evidence

Created: 2026-05-24T22:26:24Z

This bundle inventories one git worktree read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
source repo and does not claim Finder/FileProvider production readiness,
`~/Documents`, dotfiles, `.local`, broad `~/git`, or home-directory takeover.

- Canary: `linux-xr-fast`
- Source: `/Users/jess/git/linux-xr-fast`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-fast-shadow-20260524T222550Z`
- Remote: `seaweedfs://localhost:8333/tcfs/git-repo-canary-linux-xr-fast-20260524T222550Z`
- Branch: `xr/main`
- Head: `d362a939112e40d0dd0217ae34b0f63dbc862b11`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-20260524T222550Z/state/tcfs-git-repo-canary.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-20260524T222550Z/state/push-state.json`

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Plan-only packets should report `full-project-parity-not-claimed` until push,
honey mounted traversal/hydration, mounted symlink verification, and the Linux
lifecycle companion run.

Contents:

- `git-repo-canary-policy.env`: shadow-first claim boundaries and source git
  metadata
- `git-repo-canary-summary.md`: short human-readable dogfood summary
- `source-inventory/`: branch, remotes, dirty status, counts, hidden dirs,
  symlinks with targets, unsupported special files, and bounded tree listing
- `shadow-inventory/`: same inventory after the isolated copy
- `symlink-shadow-compare.log`: local source/shadow symlink target comparison
- `state/tcfs-git-repo-canary.toml`: generic alias for the disposable config
  with raw `.git`, hidden-dir, symlink, and empty-dir sync enabled
- `push.log` or `push.log.gz`: shadow push transcript when `--push` ran
- `honey-git-repo-canary-commands.txt`: generic alias for the honey mounted
  proof command packet for traversal, selected hydration, and mounted symlink
  checks
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
