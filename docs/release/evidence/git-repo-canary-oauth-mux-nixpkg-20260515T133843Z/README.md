# TCFS Git Repo Canary Evidence

Created: 2026-05-15T13:48:55Z

This bundle inventories one git worktree read-only, copies it to an isolated
shadow, and roots TCFS state/config at that shadow. It does not mutate the live
source repo and does not claim Finder/FileProvider production readiness,
`~/Documents`, dotfiles, `.local`, broad `~/git`, or home-directory takeover.

- Canary: `oauth-mux-nixpkg`
- Source: `/Users/jess/git/oauth-mux`
- Shadow: `/Users/jess/TCFS Pilot/real-canaries/oauth-mux-nixpkg-shadow-20260515T133844Z`
- Remote: `seaweedfs://100.64.48.53:8333/tcfs/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z`
- Branch: `main`
- Head: `ef1d8ea3571fb107b78fc5c83e7c8d7c48d2d420`
- Config: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/state/tcfs-git-repo-canary.toml`
- State JSON: `/Users/jess/git/tummycrypt/docs/release/evidence/git-repo-canary-oauth-mux-nixpkg-20260515T133843Z/state/push-state.json`
- Local tcfs: `/nix/store/srcrzf7y7jbliw8r8cwydjmdp8l47nm7-tcfs-cli-0.12.12/bin/tcfs`
  (`tcfs 0.12.12`, SHA-256 `3b14a3a1dee2b076f03a1703ab37d3488f9ebc91d56a2e6b2fe3a83dfcdf6035`)
- Honey tcfs: `/nix/store/xq14ldwyl6bbabcggfvc4vrg9ml7pw1c-tcfs-cli-0.12.12/bin/tcfs`
  (`tcfs 0.12.12`, SHA-256 `dc9b1758274b9c19d4ed470537486e989a23a07bb78e2f004d91eac56e946e43`)

Truth gate: scoped project-tree parity is claimable only when
`parity-gates.env` reports `scoped-project-tree-parity-evidence-complete`.
Plan-only packets should report `full-project-parity-not-claimed` until push,
honey mounted traversal/hydration, mounted symlink verification, and the Linux
lifecycle companion run.

Contents:

- `git-repo-canary-policy.env`: shadow-first claim boundaries and source git
  metadata
- `git-repo-canary-summary.md`: short human-readable dogfood summary
- `neo-nix-build.log` and `honey-nix-build.log`: package build/fetch commands
  that produced the explicit current Nix package binaries without installing
  either into a user profile
- `neo-nix-tcfs-version.txt` and `honey-nix-tcfs-version.txt`: exact tcfs
  version and SHA-256 proof for the local producer and honey consumer binaries
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
- `restore-proof/`: fresh-tree restore attempt using `tcfs reconcile`; current
  result is a blocker, not a content-restore claim. `restore-proof.env` reports
  `proof=fresh-tree-restore-blocked` because dry-run remote-index scanning timed
  out after 120s before restore execution.
