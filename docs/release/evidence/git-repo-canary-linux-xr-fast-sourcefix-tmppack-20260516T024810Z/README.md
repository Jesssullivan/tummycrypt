# TCFS Git Repo Canary Evidence

Created: 2026-05-16T03:15:10Z

This bundle inventories one git worktree read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
source repo and does not claim Finder/FileProvider production readiness,
`~/Documents`, dotfiles, `.local`, broad `~/git`, or home-directory takeover.

- Canary: `linux-xr-fast`
- Source: `/Users/jess/git/linux-xr-fast`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/linux-xr-fast-nixpkg-shadow-20260516T005236Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-linux-xr-fast-sourcefix-tmppack-20260516T024810Z`
- Branch: `xr/main`
- Head: `dbfcd3938a2f38cd1020716e98aad245452f51e1`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-tmppack-20260516T024810Z/state/tcfs-git-repo-canary.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-linux-xr-fast-sourcefix-tmppack-20260516T024810Z/state/push-state.json`

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Plan-only packets should report `full-project-parity-not-claimed` until push,
honey mounted traversal/hydration, mounted symlink verification, and the Linux
lifecycle companion run.

Status: blocker evidence only. The source-built temp-pack fix reduced
extensionless Git temp packs to 51 / 18 / 8 chunks, then exposed `.git/index`
as the max-chunk path at 1,767 chunks before the current `.git/index` source
fix. See `blocker-notes.md`.

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
- `blocker-notes.md`: raw-Git metadata object-count findings and claim
  boundaries
- `honey-git-repo-canary-commands.txt`: generic alias for the honey mounted
  proof command packet for traversal, selected hydration, and mounted symlink
  checks
- `linux-lifecycle-companion.log` and `linux-lifecycle/`: optional mounted
  write/readback, cache clear/rehydrate, dirty safe-unsync refusal, clean
  recursive unsync, and exact rehydrate companion evidence
